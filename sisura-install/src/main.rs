//! sisura-install: install Sisura Linux from the live ISO to a disk.

mod cmd;
mod disk;
mod install;
mod secret;
mod setup;

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
    Install(Box<InstallArgs>),
}

#[derive(clap::Args, Debug)]
struct InstallArgs {
    /// Target disk, e.g. vdb, /dev/nvme0n1 (ALL DATA ON IT IS DESTROYED).
    /// Asked interactively when omitted.
    #[arg(long)]
    disk: Option<String>,

    /// Root filesystem to copy (default: the live ISO's squashfs)
    #[arg(long, default_value = LIVE_SQUASHFS)]
    source: PathBuf,

    /// Mount point used during the installation
    #[arg(long, default_value = "/mnt/sisura-target")]
    target: PathBuf,

    /// Hostname of the installed system [default: sisura]
    #[arg(long)]
    hostname: Option<String>,

    /// Admin user name [default: the image's user, sisura]
    #[arg(long)]
    user: Option<String>,

    /// Admin (sudo) password from this file. Without it, and without a
    /// prompt, the first console login has to set one.
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,

    /// authorized_keys for the admin user (SSH is key-only)
    #[arg(long, value_name = "FILE")]
    ssh_key_file: Option<PathBuf>,

    /// Fetch the admin's SSH keys from https://github.com/<USER>.keys
    #[arg(long, value_name = "USER")]
    github_user: Option<String>,

    /// Keyboard layout[:variant] for the console and the LUKS prompt,
    /// e.g. us, tr, tr:f, de [default: us]
    #[arg(long, value_name = "LAYOUT")]
    keyboard: Option<String>,

    /// Read the LUKS passphrase from this file instead of prompting
    /// (automated installs/tests; delete the file afterwards)
    #[arg(long, value_name = "FILE")]
    passphrase_file: Option<PathBuf>,

    /// Do not offer unlocking the disk over SSH at boot (dropbear in the
    /// initramfs; only possible with SSH keys)
    #[arg(long)]
    no_ssh_unlock: bool,

    /// Port of the SSH server in the initramfs (disk unlock only)
    #[arg(long, default_value_t = 2222, value_name = "PORT")]
    unlock_port: u16,

    /// Protect GRUB entry editing with the password in this file
    #[arg(long, value_name = "FILE")]
    grub_password_file: Option<PathBuf>,

    /// Non-interactive: no questions, no confirmation; missing values use
    /// their defaults (no password, no SSH key, US keyboard, no GRUB password)
    #[arg(long)]
    yes: bool,

    /// Check everything and show the plan, change nothing
    #[arg(long)]
    dry_run: bool,

    /// Install although the running system was not booted via UEFI (VMs
    /// started with a direct kernel boot). The target machine must boot UEFI.
    #[arg(long, hide = true)]
    skip_uefi_check: bool,
}

fn list_disks() -> Result<()> {
    let disks = disk::list()?;
    if disks.is_empty() {
        println!("No disks found.");
    }
    for d in disks {
        println!("{}{}", d.describe(), disk_note(&d));
    }
    Ok(())
}

fn disk_note(d: &disk::Disk) -> String {
    match disk::in_use(d) {
        Ok(r) if r.is_empty() && d.size >= disk::MIN_SIZE => String::new(),
        Ok(r) if r.is_empty() => "  [too small]".to_string(),
        Ok(_) => "  [in use]".to_string(),
        Err(e) => format!("  [{e}]"),
    }
}

/// Ask until `parse` accepts the answer; empty input means `default`.
fn ask<T>(question: &str, default: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    loop {
        let prompt = if default.is_empty() {
            format!("{question}: ")
        } else {
            format!("{question} [{default}]: ")
        };
        let answer = secret::prompt_line(&prompt)?;
        let answer = if answer.is_empty() {
            default
        } else {
            answer.as_str()
        };
        match parse(answer) {
            Ok(v) => return Ok(v),
            Err(e) => eprintln!("  {e:#}"),
        }
    }
}

fn ask_disk() -> Result<disk::Disk> {
    println!("\nDisks:");
    let candidates: Vec<disk::Disk> = disk::list()?
        .into_iter()
        .inspect(|d| println!("  {}{}", d.describe(), disk_note(d)))
        .filter(|d| disk_note(d).is_empty())
        .collect();
    if candidates.is_empty() {
        bail!("no usable disk (needs at least 8 GiB, not in use)");
    }
    let default = if candidates.len() == 1 {
        candidates[0].name.clone()
    } else {
        String::new()
    };
    ask("Install to disk", &default, |a| {
        let d = disk::find(a)?;
        disk::check_target(&d)?;
        Ok(d)
    })
}

fn ask_ssh_keys() -> Result<Vec<String>> {
    println!(
        "\nSSH access is key-only. Paste public key lines (end with an empty line),\n\
         or type gh:<github-user> to fetch your keys, or just Enter to skip."
    );
    loop {
        let first = secret::prompt_line("SSH key: ")?;
        if first.is_empty() {
            println!("  no SSH key: log in on the console and add one to ~/.ssh/authorized_keys");
            return Ok(Vec::new());
        }
        let result = if let Some(user) = first.strip_prefix("gh:") {
            setup::github_keys(user.trim())
        } else {
            let mut text = first;
            loop {
                let more = secret::prompt_line("")?;
                if more.is_empty() {
                    break;
                }
                text.push('\n');
                text.push_str(&more);
            }
            setup::parse_keys(&text)
        };
        match result {
            Ok(keys) if !keys.is_empty() => {
                println!("  {} key(s) accepted", keys.len());
                return Ok(keys);
            }
            Ok(_) => eprintln!("  no key found, try again"),
            Err(e) => eprintln!("  {e:#}"),
        }
    }
}

fn ask_password(user: &str) -> Result<secret::Secret> {
    loop {
        let pw = secret::prompt_new(
            &format!("Password for '{user}' (sudo, console)"),
            setup::MIN_PASSWORD,
        )?;
        match setup::check_password(user, &pw) {
            Ok(()) => return Ok(pw),
            Err(e) => eprintln!("  weak password: {e:#}"),
        }
    }
}

fn install(args: InstallArgs) -> Result<()> {
    let interactive = !args.yes;
    if interactive {
        println!("Sisura Linux installer. Nothing is written until you confirm at the end.");
    }

    let target_disk = match &args.disk {
        Some(d) => disk::find(d)?,
        None if interactive => ask_disk()?,
        None => bail!("--disk is required with --yes"),
    };

    let hostname = match &args.hostname {
        Some(h) => h.clone(),
        None if interactive => ask("Hostname", "sisura", |h| {
            if install::valid_hostname(h) {
                Ok(h.to_string())
            } else {
                bail!("a-z, 0-9 and '-', at most 63 characters")
            }
        })?,
        None => "sisura".to_string(),
    };
    if !install::valid_hostname(&hostname) {
        bail!("invalid hostname '{hostname}' (a-z, 0-9, '-', max 63)");
    }

    // Disk and source checks before anything secret is asked.
    let mut opts = Options {
        disk: target_disk,
        source: args.source.clone(),
        target: args.target.clone(),
        hostname,
        passphrase: secret::Secret::empty(),
        skip_uefi_check: args.skip_uefi_check,
        setup: setup::Setup {
            account: setup::Account {
                user: String::new(),
                password: None,
                ssh_keys: Vec::new(),
            },
            keyboard: setup::Keyboard::parse("us")?,
            grub_password: None,
            ssh_unlock_port: None,
        },
    };
    install::preflight(&opts)?;

    let s = &mut opts.setup;
    s.keyboard = match &args.keyboard {
        Some(k) => setup::Keyboard::parse(k)?,
        None if interactive => ask(
            "Keyboard layout (us, tr, tr:f, de, fr, gb, ...)",
            "us",
            setup::Keyboard::parse,
        )?,
        None => setup::Keyboard::parse("us")?,
    };
    if interactive {
        setup::apply_live_keyboard(&s.keyboard);
    }

    s.account.user = match &args.user {
        Some(u) => u.clone(),
        None if interactive => ask("Admin user name", "sisura", |u| {
            if setup::valid_username(u) {
                Ok(u.to_string())
            } else {
                bail!("lowercase letters, digits, '-', '_', max 32, not a system name")
            }
        })?,
        None => "sisura".to_string(),
    };
    if !setup::valid_username(&s.account.user) {
        bail!("invalid user name '{}'", s.account.user);
    }

    s.account.password = match &args.password_file {
        Some(p) => {
            let pw = secret::Secret::from_file(p)?;
            setup::check_password(&s.account.user, &pw)
                .map_err(|e| anyhow::anyhow!("password in {}: {e:#}", p.display()))?;
            Some(pw)
        }
        None if interactive => Some(ask_password(&s.account.user)?),
        None => None,
    };

    s.account.ssh_keys = match (&args.ssh_key_file, &args.github_user) {
        (Some(f), _) => {
            let text = std::fs::read_to_string(f)?;
            setup::parse_keys(&text)?
        }
        (None, Some(gh)) => setup::github_keys(gh)?,
        (None, None) if interactive => ask_ssh_keys()?,
        (None, None) => Vec::new(),
    };

    opts.setup.ssh_unlock_port = if opts.setup.account.ssh_keys.is_empty() || args.no_ssh_unlock {
        None
    } else if interactive {
        let port = args.unlock_port;
        let want = ask(
            &format!(
                "Allow unlocking the disk over SSH at boot (port {port}, your keys only)? (y/n)"
            ),
            "y",
            |a| match a {
                "y" | "Y" | "yes" => Ok(true),
                "n" | "N" | "no" => Ok(false),
                _ => bail!("y or n"),
            },
        )?;
        want.then_some(port)
    } else {
        Some(args.unlock_port)
    };
    if opts.setup.ssh_unlock_port == Some(22) {
        bail!("--unlock-port 22 would clash with the system's SSH host key; use another port");
    }

    opts.passphrase = match &args.passphrase_file {
        Some(p) => secret::Secret::from_file(p)?,
        None if interactive => {
            println!(
                "\nDisk encryption passphrase: asked at every boot, keyboard layout '{}'.\n\
                 It cannot be recovered - without it the data is lost.",
                opts.setup.keyboard.describe()
            );
            secret::prompt_new("LUKS passphrase", MIN_PASSPHRASE)?
        }
        None => bail!("--passphrase-file is required with --yes"),
    };
    if opts.passphrase.bytes().len() < MIN_PASSPHRASE {
        bail!("LUKS passphrase must be at least {MIN_PASSPHRASE} characters");
    }

    opts.setup.grub_password = match &args.grub_password_file {
        Some(p) => Some(secret::Secret::from_file(p)?),
        None if interactive => {
            let want = ask(
                "Protect boot entry editing with a GRUB password? (y/n)",
                "y",
                |a| match a {
                    "y" | "Y" | "yes" => Ok(true),
                    "n" | "N" | "no" => Ok(false),
                    _ => bail!("y or n"),
                },
            )?;
            if want {
                Some(secret::prompt_new("GRUB password", setup::MIN_PASSWORD)?)
            } else {
                None
            }
        }
        None => None,
    };
    if let Some(pw) = &opts.setup.grub_password {
        if pw.bytes().len() < setup::MIN_PASSWORD {
            bail!(
                "GRUB password must be at least {} characters",
                setup::MIN_PASSWORD
            );
        }
    }

    let s = &opts.setup;
    println!("\nInstall plan:");
    println!("  disk      {}", opts.disk.describe());
    println!("            1: 512 MiB ESP  2: 1 GiB /boot  3: rest, LUKS2 -> ext4 /");
    println!(
        "  source    {} (Sisura {})",
        opts.source.display(),
        install::build_label(&opts.source)
    );
    println!("  hostname  {}", opts.hostname);
    println!("  user      {} (sudo)", s.account.user);
    println!(
        "  password  {}",
        if s.account.password.is_some() {
            "set"
        } else {
            "not set: first console login must set it"
        }
    );
    println!("  ssh keys  {}", s.account.ssh_keys.len());
    println!("  keyboard  {}", s.keyboard.describe());
    println!(
        "  unlock    {}",
        match s.ssh_unlock_port {
            Some(p) => format!("console or SSH (port {p})"),
            None => "console only".to_string(),
        }
    );
    println!(
        "  grub      {}",
        if s.grub_password.is_some() {
            "password protected"
        } else {
            "no password"
        }
    );
    if args.dry_run {
        println!("\nDry run: nothing was changed.");
        return Ok(());
    }

    if interactive {
        println!(
            "\nALL DATA ON {} WILL BE DESTROYED.",
            opts.disk.path().display()
        );
        // Enter alone has no default here (unlike every other question):
        // ask again instead of throwing all answers away. Anything else
        // that is not the disk name aborts.
        loop {
            let answer = secret::prompt_line(&format!(
                "Type '{}' to erase it and install, or 'no' to abort: ",
                opts.disk.name
            ))?;
            if answer == opts.disk.name {
                break;
            }
            if !answer.is_empty() {
                bail!("aborted, nothing was changed");
            }
            eprintln!("  Enter alone does not confirm: type the disk name");
        }
    }

    install::run_install(&opts)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Disks => list_disks(),
        Command::Install(args) => install(*args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sisura-install: {e:#}");
            ExitCode::FAILURE
        }
    }
}
