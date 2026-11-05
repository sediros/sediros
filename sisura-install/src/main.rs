//! sisura-install: install Sisura Linux from the live ISO to a disk.

mod cmd;
mod disk;
mod install;
mod secret;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use install::{Options, LIVE_SQUASHFS};

/// Minimum LUKS passphrase length.
const MIN_PASSPHRASE: usize = 12;

#[derive(Parser, Debug)]
#[command(
    name = "sisura-install",
    version,
    about = "Install Sisura Linux to a disk (UEFI, GPT, LUKS2-encrypted root)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List disks that can be install targets
    Disks,
    /// Erase a disk and install Sisura on it
    Install(InstallArgs),
}

#[derive(clap::Args, Debug)]
struct InstallArgs {
    /// Target disk, e.g. vdb, /dev/nvme0n1 (ALL DATA ON IT IS DESTROYED)
    #[arg(long)]
    disk: String,

    /// Root filesystem to copy (default: the live ISO's squashfs)
    #[arg(long, default_value = LIVE_SQUASHFS)]
    source: PathBuf,

    /// Mount point used during the installation
    #[arg(long, default_value = "/mnt/sisura-target")]
    target: PathBuf,

    /// Hostname of the installed system
    #[arg(long, default_value = "sisura")]
    hostname: String,

    /// Read the LUKS passphrase from this file instead of prompting
    /// (automated installs/tests; delete the file afterwards)
    #[arg(long, value_name = "FILE")]
    passphrase_file: Option<PathBuf>,

    /// Do not ask for confirmation before erasing the disk
    #[arg(long)]
    yes: bool,

    /// Check everything and show the plan, change nothing
    #[arg(long)]
    dry_run: bool,
}

fn list_disks() -> Result<()> {
    let disks = disk::list()?;
    if disks.is_empty() {
        println!("No disks found.");
    }
    for d in disks {
        let note = match disk::in_use(&d) {
            Ok(r) if r.is_empty() && d.size >= disk::MIN_SIZE => String::new(),
            Ok(r) if r.is_empty() => "  [too small]".to_string(),
            Ok(_) => "  [in use]".to_string(),
            Err(e) => format!("  [{e}]"),
        };
        println!("{}{note}", d.describe());
    }
    Ok(())
}

fn install(args: InstallArgs) -> Result<()> {
    if !install::valid_hostname(&args.hostname) {
        bail!(
            "invalid hostname '{}' (a-z, 0-9, '-', max 63)",
            args.hostname
        );
    }
    let target_disk = disk::find(&args.disk)?;

    // Validate before asking for anything secret.
    let mut opts = Options {
        disk: target_disk,
        source: args.source,
        target: args.target,
        hostname: args.hostname,
        passphrase: secret::Secret::empty(),
    };
    install::preflight(&opts)?;

    println!("Install plan:");
    println!("  disk      {}", opts.disk.describe());
    println!("            1: 512 MiB ESP (FAT32)  2: 1 GiB /boot (ext4)  3: rest, LUKS2 -> ext4 /");
    println!("  source    {}", opts.source.display());
    println!("  hostname  {}", opts.hostname);
    if args.dry_run {
        println!("\nDry run: nothing was changed.");
        return Ok(());
    }

    if !args.yes {
        println!(
            "\nALL DATA ON {} WILL BE DESTROYED.",
            opts.disk.path().display()
        );
        let answer = secret::prompt_line(&format!(
            "Type the disk name ({}) to continue: ",
            opts.disk.name
        ))?;
        if answer != opts.disk.name {
            bail!("aborted, nothing was changed");
        }
    }

    opts.passphrase = match &args.passphrase_file {
        Some(p) => secret::Secret::from_file(p)?,
        None => {
            println!(
                "\nDisk encryption passphrase: asked at every boot (console or SSH unlock).\n\
                 Keyboard layout at boot is US unless configured otherwise."
            );
            secret::prompt_new("LUKS passphrase", MIN_PASSPHRASE)?
        }
    };
    if opts.passphrase.bytes().len() < MIN_PASSPHRASE {
        bail!("LUKS passphrase must be at least {MIN_PASSPHRASE} characters");
    }

    install::run_install(&opts)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Disks => list_disks(),
        Command::Install(args) => install(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sisura-install: {e:#}");
            ExitCode::FAILURE
        }
    }
}
