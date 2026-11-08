//! Snapshots of a stopped VM: qcow2 internal snapshots of its disks.
//!
//! The VM's disk is a copy-on-write overlay, so a snapshot only keeps the
//! blocks changed since; taking one is cheap. The VM must be stopped: a
//! snapshot of a running VM could catch the guest's filesystem mid-write.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::qemu::Platform;
use super::state::VmState;
use crate::cli::{RestoreArgs, SnapshotArgs};
use crate::paths::{validate_vm_name, Paths};
use crate::purge::confirm;

pub fn snapshot_cmd(paths: &Paths, args: &SnapshotArgs) -> Result<()> {
    let vm = load(paths, &args.name)?;
    let img = qemu_img(&vm)?;

    if let Some(snap) = &args.delete {
        stopped(&vm, "delete a snapshot")?;
        require(&img, &vm, snap)?;
        for disk in disks(&vm) {
            if list(&img, &disk)?.iter().any(|s| s.name == *snap) {
                run(&img, &["snapshot", "-d", snap], &disk)?;
            }
        }
        println!("Deleted snapshot '{snap}' of '{}'.", vm.name);
        return Ok(());
    }

    let Some(snap) = &args.snapshot else {
        return print_list(&img, &vm);
    };
    validate_snapshot_name(snap)?;
    stopped(&vm, "take a snapshot")?;
    // qemu-img would happily create a second snapshot with the same name.
    if list(&img, &vm.overlay())?.iter().any(|s| s.name == *snap) {
        bail!(
            "VM '{}' already has a snapshot '{snap}' (delete it first: sediros snapshot {} --delete {snap})",
            vm.name,
            vm.name
        );
    }
    for disk in disks(&vm) {
        run(&img, &["snapshot", "-c", snap], &disk)?;
    }
    println!(
        "Snapshot '{snap}' of '{}' taken. Back to it later with: sediros restore {} {snap}",
        vm.name, vm.name
    );
    Ok(())
}

pub fn restore_cmd(paths: &Paths, args: &RestoreArgs) -> Result<()> {
    let vm = load(paths, &args.name)?;
    let img = qemu_img(&vm)?;
    stopped(&vm, "restore a snapshot")?;
    require(&img, &vm, &args.snapshot)?;
    if !args.yes {
        println!(
            "Restore '{}' to snapshot '{}'? Everything changed in the VM since then is lost.",
            vm.name, args.snapshot
        );
        if !confirm("Proceed? [y/N] ")? {
            println!("Aborted.");
            return Ok(());
        }
    }
    for disk in disks(&vm) {
        // A data disk attached later may not have this snapshot.
        if list(&img, &disk)?.iter().any(|s| s.name == args.snapshot) {
            run(&img, &["snapshot", "-a", &args.snapshot], &disk)?;
        }
    }
    println!(
        "Restored '{}' to '{}'. Start it with: sediros launch {}",
        vm.name, args.snapshot, vm.name
    );
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct Snap {
    name: String,
    date: String,
}

fn load(paths: &Paths, name: &str) -> Result<VmState> {
    validate_vm_name(name)?;
    VmState::load(&paths.vm_dir(name)).with_context(|| format!("VM '{name}' not found"))
}

fn qemu_img(vm: &VmState) -> Result<PathBuf> {
    Ok(Platform::detect(&vm.arch)?.qemu_img())
}

fn stopped(vm: &VmState, what: &str) -> Result<()> {
    if vm.is_running() {
        bail!(
            "VM '{0}' is running; stop it to {what} (sediros stop {0})",
            vm.name
        );
    }
    Ok(())
}

fn require(img: &Path, vm: &VmState, snap: &str) -> Result<()> {
    if !list(img, &vm.overlay())?.iter().any(|s| s.name == snap) {
        bail!(
            "VM '{}' has no snapshot '{snap}' (list them: sediros snapshot {})",
            vm.name,
            vm.name
        );
    }
    Ok(())
}

/// The root disk, and the data disk if the VM has one.
fn disks(vm: &VmState) -> Vec<PathBuf> {
    let mut d = vec![vm.overlay()];
    if vm.data_disk().is_file() {
        d.push(vm.data_disk());
    }
    d
}

fn print_list(img: &Path, vm: &VmState) -> Result<()> {
    let snaps = list(img, &vm.overlay())?;
    if snaps.is_empty() {
        println!(
            "VM '{}' has no snapshots. Take one (VM stopped): sediros snapshot {} <name>",
            vm.name, vm.name
        );
        return Ok(());
    }
    println!("{:<32} TAKEN", "SNAPSHOT");
    for s in snaps {
        println!("{:<32} {}", s.name, s.date);
    }
    Ok(())
}

fn list(img: &Path, disk: &Path) -> Result<Vec<Snap>> {
    let out = Command::new(img)
        .args(["snapshot", "-l", "-U"])
        .arg(disk)
        .output()
        .context("running qemu-img")?;
    if !out.status.success() {
        bail!(
            "qemu-img snapshot -l failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_list(&String::from_utf8_lossy(&out.stdout)))
}

/// `qemu-img snapshot -l`: two header lines, then
/// `<id> <tag> <vm size> <unit> <date> <time> <vm clock> <icount>`.
fn parse_list(text: &str) -> Vec<Snap> {
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 6).then(|| Snap {
                name: f[1].to_string(),
                date: format!("{} {}", f[4], f[5]),
            })
        })
        .collect()
}

fn run(img: &Path, args: &[&str], disk: &Path) -> Result<()> {
    let out = Command::new(img)
        .args(args)
        .arg(disk)
        .output()
        .context("running qemu-img")?;
    if !out.status.success() {
        bail!(
            "qemu-img {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Letters, digits, '.', '_', '-'; not only digits, which qemu-img would
/// also take as a snapshot ID.
fn validate_snapshot_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.chars().all(|c| c.is_ascii_digit())
        && !name.starts_with('-');
    if !ok {
        bail!("invalid snapshot name '{name}': use letters, digits, '.', '_', '-' (max 64, not only digits)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qemu_img_list() {
        let text = "Snapshot list:\n\
            ID      TAG               VM_SIZE                DATE        VM_CLOCK     ICOUNT\n\
            1       before-nginx          0 B 2026-11-07 12:32:39  0000:00:00.000          0\n\
            2       second                0 B 2026-11-07 12:40:01  0000:00:00.000          0\n";
        let s = parse_list(text);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "before-nginx");
        assert_eq!(s[1].date, "2026-11-07 12:40:01");
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn snapshot_names() {
        for ok in ["before-nginx", "v1.2", "a_b", "x1"] {
            assert!(validate_snapshot_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "12", "-x", "a b", "a/b", "ä", &"x".repeat(65)] {
            assert!(validate_snapshot_name(bad).is_err(), "{bad}");
        }
    }
}
