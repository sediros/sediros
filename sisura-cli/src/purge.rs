//! `sisura purge`: stop VMs, delete overlays/state and unused cached images.

use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::cli::PurgeArgs;
use crate::paths::{remove_dir_within, validate_vm_name, Paths};
use crate::vm::{self, VmState};

pub fn run(paths: &Paths, args: &PurgeArgs) -> Result<()> {
    let targets: Vec<VmState> = match &args.name {
        Some(name) => {
            validate_vm_name(name)?;
            vec![VmState::load(&paths.vm_dir(name))
                .with_context(|| format!("VM '{name}' not found"))?]
        }
        None => vm::load_all(&paths.vms_dir())?,
    };

    if targets.is_empty() && !args.images {
        println!("Nothing to purge.");
        return Ok(());
    }

    println!("This will permanently delete:");
    for vm in &targets {
        let state = if vm.is_running() {
            "running, will be stopped"
        } else {
            "stopped"
        };
        println!("  VM {:<20} ({state})  {}", vm.name, vm.dir.display());
    }
    if args.images {
        println!(
            "  cached base images not used by remaining VMs  {}",
            paths.images_dir().display()
        );
    }
    if !args.yes && !confirm("Proceed? [y/N] ")? {
        println!("Aborted.");
        return Ok(());
    }

    for vm in &targets {
        if vm.is_running() {
            vm::stop(vm, Duration::from_secs(10), false)?;
        }
        remove_dir_within(&paths.vms_dir(), &vm.dir)?;
        println!("  ✓ deleted VM {}", vm.name);
    }

    if args.images {
        purge_unused_images(paths)?;
    }
    Ok(())
}

fn purge_unused_images(paths: &Paths) -> Result<()> {
    let images_dir = paths.images_dir();
    let entries = match fs::read_dir(&images_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let in_use: HashSet<String> = vm::load_all(&paths.vms_dir())?
        .into_iter()
        .map(|vm| vm.image_key)
        .collect();

    for entry in entries {
        let entry = entry?;
        let key = entry.file_name().to_string_lossy().into_owned();
        if in_use.contains(&key) {
            println!("  - kept image {key} (still used)");
            continue;
        }
        remove_dir_within(&images_dir, &entry.path())?;
        println!("  ✓ deleted image {key}");
    }
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("refusing to purge without confirmation on a non-interactive stdin; pass --yes");
    }
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "evet" | "e"))
}
