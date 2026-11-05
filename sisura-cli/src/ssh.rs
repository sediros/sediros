//! `sisura shell`: replace this process with an ssh client connected to the VM.

use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::ShellArgs;
use crate::paths::{validate_vm_name, Paths};
use crate::vm::VmState;

pub fn shell(paths: &Paths, args: &ShellArgs) -> Result<()> {
    validate_vm_name(&args.name)?;
    let vm = VmState::load(&paths.vm_dir(&args.name))
        .with_context(|| format!("VM '{}' not found", args.name))?;
    if !vm.is_running() {
        bail!(
            "VM '{0}' is not running; start it with `sisura launch {0}`",
            vm.name
        );
    }

    let identity = args.identity.clone().or_else(|| {
        paths
            .project()
            .ok()
            .map(|p| p.join("id_sisura"))
            .filter(|p| p.is_file())
    });

    let mut cmd = Command::new("ssh");
    cmd.arg("-p")
        .arg(vm.ssh_port.to_string())
        // Per-VM known_hosts: host keys are generated on the VM's first boot,
        // pinned on first contact, and deleted together with the VM on purge.
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", vm.known_hosts().display()))
        .args(["-o", "StrictHostKeyChecking=accept-new"])
        .args(["-o", "LogLevel=ERROR"])
        .args(["-o", "ConnectTimeout=5"])
        .args(["-o", "ServerAliveInterval=30"]);

    if let Some(id) = identity {
        if !id.is_file() {
            bail!("SSH key not found: {}", id.display());
        }
        cmd.arg("-i").arg(id).args(["-o", "IdentitiesOnly=yes"]);
    }

    cmd.arg(format!("{}@127.0.0.1", args.user));
    cmd.args(&args.command);

    // exec() only returns on failure.
    let err = cmd.exec();
    Err(err).context("failed to exec ssh")
}
