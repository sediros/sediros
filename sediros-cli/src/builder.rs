//! `sediros build`: thin orchestration over scripts/build_rootfs.sh and
//! scripts/build_image.sh. On macOS the scripts re-run themselves in Docker.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::BuildArgs;
use crate::paths::Paths;

/// Environment forwarded through sudo on Linux.
const FORWARDED_ENV: &str =
    "SEDIROS_RELEASE,SEDIROS_SUITE,SEDIROS_ARCH,SEDIROS_MIRROR,SEDIROS_BUILD_DIR,\
SEDIROS_HOSTNAME,SEDIROS_USER,SEDIROS_SSH_PUBKEY,SEDIROS_DEBUG_PASSWORD,SEDIROS_PROFILE,\
SEDIROS_IMAGE_SIZE,SEDIROS_DOCKER_IMAGE";

pub fn run(paths: &Paths, args: &BuildArgs) -> Result<()> {
    let project = paths.project()?;
    let arch = host_debian_arch()?;
    let release = match &args.release {
        Some(r) => r.clone(),
        None => default_release(project)?,
    };

    if !args.image_only {
        let mut cmd = script(project, "scripts/build_rootfs.sh");
        cmd.args(["--release", &release]);
        if let Some(suite) = &args.suite {
            cmd.args(["--suite", suite]);
        }
        cmd.env("SEDIROS_PROFILE", &args.profile);
        if args.clean {
            cmd.arg("--clean");
        }
        execute(cmd, "rootfs build")?;
    }

    if !args.rootfs_only {
        // Relative path: valid both on the host and inside the Docker re-exec.
        let rootfs = format!(
            "build/sediros-rootfs-{release}{}-{arch}.tar.zst",
            profile_tag(&args.profile)
        );
        if !project.join(&rootfs).is_file() {
            bail!("{rootfs} not found; run `sediros build` without --image-only first");
        }
        let mut cmd = script(project, "scripts/build_image.sh");
        cmd.args(["--rootfs", &rootfs, "--size", &args.size]);
        execute(cmd, "image build")?;
    }

    println!("\nBuild complete. Next: sediros launch <name>");
    Ok(())
}

/// `RELEASE ?= 1.0` from config/sediros.conf, the same default the Makefile uses.
fn default_release(project: &Path) -> Result<String> {
    let conf = project.join("config/sediros.conf");
    let text =
        std::fs::read_to_string(&conf).with_context(|| format!("reading {}", conf.display()))?;
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("RELEASE"))
        .filter_map(|rest| rest.trim_start().strip_prefix("?="))
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
        .with_context(|| format!("no `RELEASE ?= ...` in {}", conf.display()))
}

/// Artifact name tag, same rule as build_rootfs.sh: vm is untagged.
fn profile_tag(profile: &str) -> String {
    if profile == "vm" {
        String::new()
    } else {
        format!("-{profile}")
    }
}

fn script(project: &Path, rel: &str) -> Command {
    let path = project.join(rel);
    let needs_sudo = cfg!(target_os = "linux") && unsafe { libc::geteuid() } != 0;
    let mut cmd = if needs_sudo {
        let mut c = Command::new("sudo");
        c.arg(format!("--preserve-env={FORWARDED_ENV}")).arg(path);
        c
    } else {
        Command::new(path)
    };
    cmd.current_dir(project);
    cmd
}

fn execute(mut cmd: Command, what: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("starting {what}"))?;
    if !status.success() {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

/// Debian architecture name of the host (images are built natively).
pub fn host_debian_arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        other => bail!("unsupported host architecture: {other}"),
    }
}
