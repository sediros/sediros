use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "sedir",
    version,
    about = "Build SedirOS images and run them as fast, isolated QEMU VMs",
    after_help = "Getting started:\n  \
        sedir pull               download the latest released VM image\n  \
        sedir launch dev         start a VM named dev\n  \
        sedir shell dev          log in (SSH key is set up automatically)\n  \
        sedir transfer f dev:    copy a file into the VM (dev:path for the VM side)\n  \
        sedir forward dev 8080:80  open http://localhost:8080 to port 80 in the VM\n  \
        sedir stop dev\n\n\
        Guide: https://github.com/sediros/sediros#readme"
)]
pub struct Cli {
    /// SedirOS project directory (contains scripts/ and build/). Auto-detected from cwd.
    #[arg(long, global = true, env = "SEDIR_PROJECT_DIR")]
    pub project: Option<PathBuf>,

    /// State directory for VMs and cached base images.
    /// Default: $XDG_DATA_HOME/sedir or ~/.local/share/sedir
    #[arg(long, global = true, env = "SEDIR_HOME")]
    pub home: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Download a released base image from GitHub (no source checkout needed)
    Pull(PullArgs),
    /// Build the hardened rootfs and the qcow2 base image (source checkout)
    Build(BuildArgs),
    /// Boot a VM in the background (creates it on first use)
    Launch(LaunchArgs),
    /// Open an SSH session in a running VM, or run a command: sedir shell vm -- uname -a
    Shell(ShellArgs),
    /// Copy files between the host and a running VM: sedir transfer ./f dev:/tmp
    Transfer(TransferArgs),
    /// Forward a port on 127.0.0.1 to a VM: sedir forward dev 8080:80
    Forward(ForwardArgs),
    /// Take, list or delete snapshots of a stopped VM: sedir snapshot dev before-nginx
    Snapshot(SnapshotArgs),
    /// Return a stopped VM to a snapshot: sedir restore dev before-nginx
    Restore(RestoreArgs),
    /// List VMs and their state
    List,
    /// Stop a running VM (ACPI power-down, then kill on timeout)
    Stop(StopArgs),
    /// Stop VMs and delete their disks and state
    Purge(PurgeArgs),
}

#[derive(Args, Debug)]
pub struct PullArgs {
    /// Release to download, e.g. 13.1 (default: the latest published release)
    #[arg(long)]
    pub release: Option<String>,

    /// GitHub repository the releases come from
    #[arg(long, env = "SEDIR_REPO", default_value = "sediros/sediros")]
    pub repo: String,

    /// Download again even if this release is already present
    #[arg(long)]
    pub force: bool,

    /// Download from this base URL instead of GitHub (mirrors, tests)
    #[arg(long, hide = true)]
    pub url: Option<String>,
}

#[derive(Args, Debug)]
pub struct BuildArgs {
    /// SedirOS release, e.g. 13.1 (default: RELEASE in config/sedir.conf)
    #[arg(long, env = "SEDIR_RELEASE")]
    pub release: Option<String>,

    /// Debian suite override, only for development builds on sid
    #[arg(long, env = "SEDIR_SUITE")]
    pub suite: Option<String>,

    /// Image profile: vm (passwordless sudo, for throwaway VMs) or install
    /// (sudo password set at first console login)
    #[arg(long, env = "SEDIR_PROFILE", default_value = "vm",
          value_parser = ["vm", "install"])]
    pub profile: String,

    /// Remove the previous rootfs before building
    #[arg(long)]
    pub clean: bool,

    /// Only build the rootfs archive
    #[arg(long, conflicts_with = "image_only")]
    pub rootfs_only: bool,

    /// Only convert an existing rootfs archive into a qcow2 image
    #[arg(long)]
    pub image_only: bool,

    /// Virtual disk size of the base image (e.g. 4G, 2048M)
    #[arg(long, default_value = "4G")]
    pub size: String,
}

#[derive(Args, Debug)]
pub struct LaunchArgs {
    /// VM name ([a-z0-9-], max 32 chars)
    pub name: String,

    /// Number of vCPUs
    #[arg(long, default_value_t = 2)]
    pub cpus: u32,

    /// Memory in MiB
    #[arg(long, default_value_t = 1024)]
    pub memory: u32,

    /// Host port forwarded to guest SSH (default: first free from 2222)
    #[arg(long)]
    pub port: Option<u16>,

    /// Base image (.qcow2). Default: newest sedir-*.qcow2 in <project>/build,
    /// else the newest image from `sedir pull`
    #[arg(long)]
    pub image: Option<PathBuf>,

    /// Seconds to wait for SSH to come up
    #[arg(long, default_value_t = 90)]
    pub timeout: u64,

    /// Return immediately after QEMU starts, without waiting for SSH
    #[arg(long)]
    pub no_wait: bool,

    /// Forward 127.0.0.1:HOST to the VM's port GUEST (repeatable, e.g.
    /// --forward 8080:80). Kept with the VM; the VM's firewall is opened for it.
    #[arg(long = "forward", value_name = "HOST:GUEST")]
    pub forwards: Vec<String>,

    /// Attach an empty second disk of this size (e.g. 16G) to a new VM, as
    /// /dev/vdb (installer tests). Kept with the VM, deleted by purge.
    #[arg(long, value_name = "SIZE")]
    pub data_disk: Option<String>,
}

#[derive(Args, Debug)]
pub struct ShellArgs {
    /// VM name
    pub name: String,

    /// SSH private key. Default: the CLI key (<home>/id_ed25519), else <project>/id_sedir
    #[arg(short = 'i', long, env = "SEDIR_SSH_KEY")]
    pub identity: Option<PathBuf>,

    /// Guest user
    #[arg(short, long, env = "SEDIR_USER", default_value = "sedir")]
    pub user: String,

    /// Command to run instead of an interactive shell (after --)
    #[arg(last = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct TransferArgs {
    /// Sources, then the destination. The VM side is written <vm>:<path>
    /// (dev: is the home directory); the other side is a host path.
    #[arg(required = true, num_args = 2.., value_name = "PATH")]
    pub paths: Vec<String>,

    /// Copy directories recursively
    #[arg(short, long)]
    pub recursive: bool,

    /// SSH private key. Default: the CLI key (<home>/id_ed25519), else <project>/id_sedir
    #[arg(short = 'i', long, env = "SEDIR_SSH_KEY")]
    pub identity: Option<PathBuf>,

    /// Guest user
    #[arg(short, long, env = "SEDIR_USER", default_value = "sedir")]
    pub user: String,
}

#[derive(Args, Debug)]
pub struct ForwardArgs {
    /// VM name
    pub name: String,

    /// Forwards to add, HOST:GUEST (e.g. 8080:80). Without any: list them.
    #[arg(value_name = "HOST:GUEST")]
    pub add: Vec<String>,

    /// Remove the forward from this host port (repeatable)
    #[arg(long, value_name = "HOST")]
    pub remove: Vec<u16>,
}

#[derive(Args, Debug)]
pub struct SnapshotArgs {
    /// VM name
    pub name: String,

    /// Snapshot to take. Without one: list the VM's snapshots.
    pub snapshot: Option<String>,

    /// Delete this snapshot
    #[arg(long, value_name = "SNAPSHOT", conflicts_with = "snapshot")]
    pub delete: Option<String>,
}

#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// VM name
    pub name: String,

    /// Snapshot to return to
    pub snapshot: String,

    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct StopArgs {
    /// VM name
    pub name: String,

    /// Kill immediately instead of a graceful ACPI power-down
    #[arg(short, long)]
    pub force: bool,

    /// Seconds to wait for a graceful shutdown before killing
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,
}

#[derive(Args, Debug)]
pub struct PurgeArgs {
    /// Only purge this VM (default: all VMs)
    pub name: Option<String>,

    /// Also delete cached base images no longer used by any VM
    #[arg(long)]
    pub images: bool,

    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}
