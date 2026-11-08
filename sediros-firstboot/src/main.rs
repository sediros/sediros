//! sediros-firstboot: sets up a SedirOS cloud image for the instance it runs
//! on, from the provider's metadata (NoCloud seed or EC2-style service).
//!
//! Runs at every boot, before sshd. The first boot of a new instance (a new
//! instance id, also after cloning a disk) gets new SSH host keys, the
//! hostname, the admin user's SSH keys and a root file system grown to the
//! disk. User-data scripts are never run.

mod apply;
mod imds;
mod meta;
mod nocloud;
mod sys;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const STATE: &str = "/var/lib/sediros/firstboot/instance-id";
const USAGE: &str =
    "usage: sediros-firstboot [--dry-run] [--seed <dir>] [--imds <addr:port>] [--wait <seconds>]

Sets up a SedirOS cloud image on the first boot of each new instance:
SSH host keys, hostname, the admin user's SSH keys, root file system size.
Metadata from a NoCloud seed (label cidata) or the instance metadata service
at 169.254.169.254. User-data scripts are never run.";

struct Args {
    dry_run: bool,
    seed: Option<PathBuf>,
    imds: String,
    wait: Duration,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        dry_run: false,
        seed: None,
        imds: imds::DEFAULT_ADDR.into(),
        wait: Duration::from_secs(20),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--dry-run" => a.dry_run = true,
            "--seed" => a.seed = Some(PathBuf::from(value()?)),
            "--imds" => a.imds = value()?,
            "--wait" => {
                a.wait = Duration::from_secs(value()?.parse().map_err(|_| "--wait: seconds")?)
            }
            "-h" | "--help" => return Err(USAGE.into()),
            "-V" | "--version" => {
                return Err(format!("sediros-firstboot {}", env!("CARGO_PKG_VERSION")))
            }
            _ => return Err(format!("unknown argument: {arg}\n\n{USAGE}")),
        }
    }
    Ok(a)
}

fn find_metadata(a: &Args) -> Option<meta::Metadata> {
    if let Some(dir) = &a.seed {
        return nocloud::read_dir(dir);
    }
    if let Some(dev) = nocloud::find() {
        return nocloud::read_device(&dev);
    }
    imds::Imds::connect(&a.imds, a.wait)?.metadata()
}

fn admin_user() -> String {
    fs::read_to_string("/etc/sediros/build-info")
        .ok()
        .and_then(|t| sys::kv(&t, "USER"))
        .unwrap_or_else(|| "sediros".into())
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            println!("{msg}");
            return ExitCode::SUCCESS;
        }
    };
    let Some(md) = find_metadata(&a) else {
        println!("no metadata source (NoCloud seed or metadata service): nothing to do");
        return ExitCode::SUCCESS;
    };
    let previous = fs::read_to_string(STATE).unwrap_or_default();
    if previous.trim() == md.instance_id {
        println!("instance {} is already set up", md.instance_id);
        return ExitCode::SUCCESS;
    }
    let user = admin_user();
    println!("new instance {} (from the {})", md.instance_id, md.source);
    let ignored = md.user_data.notice();
    if a.dry_run {
        println!("dry run, would set up:");
        println!("  SSH host keys: new");
        println!(
            "  hostname: {}",
            md.hostname.as_deref().unwrap_or("(unchanged)")
        );
        println!("  SSH keys for {user}: {}", md.ssh_keys.len());
        println!("  root file system: grow to the disk");
        if let Some(n) = &ignored {
            println!("  {n}");
        }
        return ExitCode::SUCCESS;
    }

    let mut steps: Vec<apply::Step> = vec![apply::regenerate_host_keys()];
    if let Some(h) = &md.hostname {
        steps.push(apply::set_hostname(h));
    }
    if !md.ssh_keys.is_empty() {
        steps.push(apply::install_keys(&user, &md.ssh_keys));
    }
    steps.push(apply::grow_root());
    if let Some(n) = &ignored {
        println!("  ! {n}");
        steps.push(apply::login_notice(std::slice::from_ref(n)));
    }

    let mut failed = 0;
    for s in &steps {
        match s {
            Ok(m) => println!("  ✓ {m}"),
            Err(e) => {
                failed += 1;
                println!("  ✗ {e}");
            }
        }
    }
    // Recorded even after a failed step: the host keys are new now, and a
    // second run would replace them again at every boot.
    if let Some(dir) = std::path::Path::new(STATE).parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(e) = fs::write(STATE, format!("{}\n", md.instance_id)) {
        println!("  ✗ {STATE}: {e}");
        failed += 1;
    }
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
