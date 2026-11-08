//! `sediros shell` and `sediros transfer`: replace this process with an ssh or
//! scp client connected to the VM, sharing the CLI's key and known_hosts.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::{ShellArgs, TransferArgs};
use crate::paths::{validate_vm_name, Paths};
use crate::vm::VmState;

pub fn shell(paths: &Paths, args: &ShellArgs) -> Result<()> {
    let vm = running_vm(paths, &args.name)?;
    let mut cmd = Command::new("ssh");
    client_options(&mut cmd, paths, &vm, args.identity.as_deref())?;
    cmd.arg(format!("{}@127.0.0.1", args.user));
    cmd.args(&args.command);

    // exec() only returns on failure.
    let err = cmd.exec();
    Err(err).context("failed to exec ssh")
}

pub fn transfer(paths: &Paths, args: &TransferArgs) -> Result<()> {
    let (dest, sources) = args
        .paths
        .split_last()
        .context("need at least one source and a destination")?;
    if sources.is_empty() {
        bail!("need at least one source and a destination");
    }
    let sources: Vec<Endpoint> = sources.iter().map(|s| Endpoint::parse(s)).collect();
    let dest = Endpoint::parse(dest);

    // Exactly one side lives in the VM: host -> VM or VM -> host.
    let vm_name = match &dest {
        Endpoint::Vm { vm, .. } => {
            if sources.iter().any(|s| matches!(s, Endpoint::Vm { .. })) {
                bail!("copy between the host and a VM: only one side can be <vm>:<path>");
            }
            vm.clone()
        }
        Endpoint::Host(_) => {
            let mut names = sources.iter().map(|s| match s {
                Endpoint::Vm { vm, .. } => Some(vm.as_str()),
                Endpoint::Host(_) => None,
            });
            let first = names.next().flatten().context(
                "no VM path given: write the VM side as <vm>:<path>, e.g. dev:/tmp or dev:",
            )?;
            if !names.all(|n| n == Some(first)) {
                bail!("all sources must be in the same VM, or all on the host");
            }
            first.to_string()
        }
    };

    let vm = running_vm(paths, &vm_name)?;
    let mut cmd = Command::new("scp");
    client_options(&mut cmd, paths, &vm, args.identity.as_deref())?;
    if args.recursive {
        cmd.arg("-r");
    }
    // Paths are passed on literally (no "--" handling needed for the remote
    // side: it always starts with user@host:).
    cmd.arg("--");
    for ep in sources.iter().chain(std::iter::once(&dest)) {
        cmd.arg(ep.scp_arg(&args.user));
    }

    let err = cmd.exec();
    Err(err).context("failed to exec scp")
}

/// Run a command in a running VM as the default user, without a terminal.
/// Returns whether it succeeded; its output goes to our stderr.
pub fn run_in_vm(paths: &Paths, vm: &VmState, command: &str) -> Result<bool> {
    let mut cmd = Command::new("ssh");
    client_options(&mut cmd, paths, vm, None)?;
    cmd.args(["-o", "BatchMode=yes", "-T"])
        .arg(format!("{DEFAULT_USER}@127.0.0.1"))
        .arg(command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    Ok(cmd.status().context("running ssh")?.success())
}

/// Guest user of SedirOS VM images.
const DEFAULT_USER: &str = "sediros";

fn running_vm(paths: &Paths, name: &str) -> Result<VmState> {
    validate_vm_name(name)?;
    let vm =
        VmState::load(&paths.vm_dir(name)).with_context(|| format!("VM '{name}' not found"))?;
    if !vm.is_running() {
        bail!(
            "VM '{0}' is not running; start it with `sediros launch {0}`",
            vm.name
        );
    }
    Ok(vm)
}

/// Options shared by ssh and scp: port, per-VM known_hosts, key.
fn client_options(
    cmd: &mut Command,
    paths: &Paths,
    vm: &VmState,
    identity: Option<&Path>,
) -> Result<()> {
    // -o Port works for both ssh (-p) and scp (-P).
    cmd.arg("-o")
        .arg(format!("Port={}", vm.ssh_port))
        // Per-VM known_hosts: host keys are generated on the VM's first boot,
        // pinned on first contact, and deleted together with the VM on purge.
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", vm.known_hosts().display()))
        .args(["-o", "StrictHostKeyChecking=accept-new"])
        .args(["-o", "LogLevel=ERROR"])
        .args(["-o", "ConnectTimeout=5"])
        .args(["-o", "ServerAliveInterval=30"]);

    if let Some(id) = identity_file(paths, identity) {
        if !id.is_file() {
            bail!("SSH key not found: {}", id.display());
        }
        cmd.arg("-i").arg(id).args(["-o", "IdentitiesOnly=yes"]);
    }
    Ok(())
}

/// The CLI's key (given to the VM at launch); a project id_sediros only
/// matters for images built before keys were passed at boot.
fn identity_file(paths: &Paths, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(id) = explicit {
        return Some(id.to_path_buf());
    }
    let own = paths.home.join("id_ed25519");
    if own.is_file() {
        return Some(own);
    }
    paths
        .project()
        .ok()
        .map(|p| p.join("id_sediros"))
        .filter(|p| p.is_file())
}

/// One side of a transfer: `<vm>:<path>` or a host path.
#[derive(Debug, PartialEq, Eq)]
enum Endpoint {
    Host(String),
    Vm { vm: String, path: String },
}

impl Endpoint {
    /// `dev:/tmp` and `dev:` are VM paths; anything else is a host path.
    /// A host path containing ':' before any '/' can be written as `./a:b`.
    fn parse(s: &str) -> Self {
        if let Some((vm, path)) = s.split_once(':') {
            if validate_vm_name(vm).is_ok() {
                return Endpoint::Vm {
                    vm: vm.to_string(),
                    path: path.to_string(),
                };
            }
        }
        Endpoint::Host(s.to_string())
    }

    fn scp_arg(&self, user: &str) -> String {
        match self {
            Endpoint::Host(p) => p.clone(),
            Endpoint::Vm { path, .. } => format!("{user}@127.0.0.1:{path}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Endpoint;

    fn vm(vm: &str, path: &str) -> Endpoint {
        Endpoint::Vm {
            vm: vm.into(),
            path: path.into(),
        }
    }

    #[test]
    fn parses_endpoints() {
        assert_eq!(Endpoint::parse("dev:/tmp/x"), vm("dev", "/tmp/x"));
        assert_eq!(Endpoint::parse("dev:"), vm("dev", ""));
        assert_eq!(Endpoint::parse("web-1:a b"), vm("web-1", "a b"));
        assert_eq!(Endpoint::parse("./a:b"), Endpoint::Host("./a:b".into()));
        assert_eq!(
            Endpoint::parse("/abs/path"),
            Endpoint::Host("/abs/path".into())
        );
        assert_eq!(Endpoint::parse("Dev:x"), Endpoint::Host("Dev:x".into()));
        assert_eq!(
            Endpoint::parse("file.txt"),
            Endpoint::Host("file.txt".into())
        );
    }

    #[test]
    fn scp_args() {
        assert_eq!(vm("dev", "").scp_arg("sediros"), "sediros@127.0.0.1:");
        assert_eq!(
            vm("dev", "/tmp").scp_arg("sediros"),
            "sediros@127.0.0.1:/tmp"
        );
    }
}
