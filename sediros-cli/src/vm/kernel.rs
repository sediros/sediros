//! The kernel a VM boots.
//!
//! VMs boot directly (-kernel/-initrd, no firmware or boot loader), first
//! with the kernel of their base image. Kernel updates are installed inside
//! the VM by unattended-upgrades like on any SedirOS system, but QEMU would
//! keep booting the image's kernel, whose modules may even be removed from
//! the VM later. So `sediros stop` copies the newest kernel and initrd
//! installed in the VM to <vm-dir>/kernels/<version>/ and the next launch
//! boots them.
//!
//! Snapshots record the kernel they were taken with (<vm-dir>/snapshot-kernels):
//! restoring one boots that kernel again, matching the modules on its disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::image::BaseImage;
use super::state::VmState;
use crate::paths::{create_private_dir, remove_dir_within, Paths};
use crate::ssh;

/// Upper bound for a fetched kernel or initrd. Debian's are 10-70 MB; the
/// files come from the VM, so their size is not trusted.
const MAX_FILE: u64 = 256 << 20;

/// Prints "<booted>\n<newest with an initrd>\n<busy|idle>\n". "busy" while
/// dpkg runs: update-initramfs runs inside it. (Not `pgrep -f`: it would
/// match this script's own shell and unattended-upgrades' idle shutdown
/// helper.) An initrd is written to a temporary file and renamed, so one
/// that exists is complete.
const PROBE: &str = r#"uname -r
for f in /boot/vmlinuz-*; do v="${f#/boot/vmlinuz-}"; [ -e "/boot/initrd.img-$v" ] && echo "$v"; done | sort -V | tail -n 1
if pgrep -x dpkg >/dev/null || pgrep -x apt-get >/dev/null || pgrep -x apt >/dev/null; then echo busy; else echo idle; fi"#;

/// Kernel and initrd to boot: the VM's own when it has one, else the image's.
pub fn boot_files(vm: &VmState, image: &BaseImage) -> (PathBuf, PathBuf) {
    if let Some(v) = &vm.kernel {
        let dir = kernels_dir(vm).join(v);
        let (k, i) = (dir.join("vmlinuz"), dir.join("initrd.img"));
        if k.is_file() && i.is_file() {
            return (k, i);
        }
        eprintln!(
            "warning: kernel {v} of '{}' is missing, booting the image's kernel",
            vm.name
        );
    }
    (image.kernel(), image.initrd())
}

pub struct Probe {
    pub booted: String,
    pub newest: Option<String>,
    pub busy: bool,
}

/// What the running VM booted and what it has installed.
pub fn probe(paths: &Paths, vm: &VmState) -> Result<Probe> {
    let out = ssh::capture_in_vm(paths, vm, PROBE)?
        .context("could not read the kernels installed in the VM")?;
    parse_probe(&out)
}

fn parse_probe(out: &str) -> Result<Probe> {
    let mut lines = out.lines().map(str::trim);
    let booted = lines.next().unwrap_or_default().to_string();
    let newest = lines.next().unwrap_or_default();
    let busy = lines.next() == Some("busy");
    validate_version(&booted)?;
    let newest = if newest.is_empty() {
        None
    } else {
        validate_version(newest)?;
        Some(newest.to_string())
    };
    Ok(Probe {
        booted,
        newest,
        busy,
    })
}

/// Called by `sediros stop` while the VM still runs: take the newest kernel
/// installed in the VM if it is not the one booted. Returns its version
/// when the next launch boots another kernel than this one.
pub fn sync(paths: &Paths, vm: &mut VmState) -> Result<Option<String>> {
    let p = probe(paths, vm)?;
    let Some(newest) = p.newest else {
        return Ok(None);
    };
    if newest == p.booted {
        return Ok(None);
    }
    let dir = kernels_dir(vm).join(&newest);
    if !(dir.join("vmlinuz").is_file() && dir.join("initrd.img").is_file()) {
        if p.busy {
            bail!("an update is running in the VM; kernel {newest} is taken at the next stop");
        }
        fetch(paths, vm, &newest, &dir)?;
    }
    vm.kernel = Some(newest.clone());
    vm.save()?;
    prune(vm)?;
    Ok(Some(newest))
}

fn fetch(paths: &Paths, vm: &VmState, version: &str, dir: &Path) -> Result<()> {
    let kernels = kernels_dir(vm);
    create_private_dir(&kernels)?;
    let tmp = kernels.join(format!(".fetch-{version}"));
    if tmp.exists() {
        remove_dir_within(&kernels, &tmp)?;
    }
    create_private_dir(&tmp)?;
    for (src, dst) in [
        (format!("/boot/vmlinuz-{version}"), "vmlinuz"),
        (format!("/boot/initrd.img-{version}"), "initrd.img"),
    ] {
        let out = tmp.join(dst);
        // One byte more than allowed, so an oversized file is noticed.
        let cmd = format!("sudo -n head -c {} '{src}'", MAX_FILE + 1);
        let ok = ssh::fetch_from_vm(paths, vm, &cmd, &out)?;
        let len = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        if !ok || len == 0 || len > MAX_FILE {
            remove_dir_within(&kernels, &tmp)?;
            bail!("could not copy {src} from the VM ({len} bytes)");
        }
    }
    fs::rename(&tmp, dir).with_context(|| format!("moving {} into place", dir.display()))?;
    Ok(())
}

/// Delete fetched kernels that neither the VM nor one of its snapshots uses.
fn prune(vm: &VmState) -> Result<()> {
    let kernels = kernels_dir(vm);
    let mut keep: Vec<String> = snapshot_kernels(vm)?.into_values().flatten().collect();
    keep.extend(vm.kernel.clone());
    let Ok(entries) = fs::read_dir(&kernels) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.contains(&name) {
            remove_dir_within(&kernels, &entry.path())?;
        }
    }
    Ok(())
}

fn kernels_dir(vm: &VmState) -> PathBuf {
    vm.dir.join("kernels")
}

fn snapshot_kernels_file(vm: &VmState) -> PathBuf {
    vm.dir.join("snapshot-kernels")
}

/// Snapshot name -> kernel it was taken with (None: the image's).
fn snapshot_kernels(vm: &VmState) -> Result<BTreeMap<String, Option<String>>> {
    let text = match fs::read_to_string(snapshot_kernels_file(vm)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    Ok(text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(s, k)| {
            let k = k.trim();
            (s.trim().to_string(), (!k.is_empty()).then(|| k.to_string()))
        })
        .collect())
}

fn write_snapshot_kernels(vm: &VmState, map: &BTreeMap<String, Option<String>>) -> Result<()> {
    let body: String = map
        .iter()
        .map(|(s, k)| format!("{s}={}\n", k.as_deref().unwrap_or("")))
        .collect();
    let path = snapshot_kernels_file(vm);
    let tmp = vm.dir.join(".snapshot-kernels.tmp");
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// A snapshot was taken: remember the kernel the VM boots now.
pub fn snapshot_taken(vm: &VmState, snap: &str) -> Result<()> {
    let mut map = snapshot_kernels(vm)?;
    map.insert(snap.to_string(), vm.kernel.clone());
    write_snapshot_kernels(vm, &map)
}

/// A snapshot was deleted: forget its kernel, delete it if nothing uses it.
pub fn snapshot_deleted(vm: &VmState, snap: &str) -> Result<()> {
    let mut map = snapshot_kernels(vm)?;
    if map.remove(snap).is_some() {
        write_snapshot_kernels(vm, &map)?;
        prune(vm)?;
    }
    Ok(())
}

/// A snapshot was restored: boot the kernel it was taken with. Snapshots
/// from before kernels were recorded used the image's.
pub fn snapshot_restored(vm: &mut VmState, snap: &str) -> Result<()> {
    let kernel = snapshot_kernels(vm)?.get(snap).cloned().flatten();
    if vm.kernel != kernel {
        vm.kernel = kernel;
        vm.save()?;
    }
    Ok(())
}

/// Debian kernel versions: "6.12.117+deb13-arm64". Also keeps them safe as
/// a directory name and inside single quotes.
fn validate_version(v: &str) -> Result<()> {
    let ok = !v.is_empty()
        && v.len() <= 100
        && !v.starts_with('.')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_' | '~'));
    if !ok {
        bail!("unexpected kernel version from the VM: {v:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe() {
        let p = parse_probe("6.12.115+deb13.1-arm64\n6.12.117+deb13-arm64\nidle\n").unwrap();
        assert_eq!(p.booted, "6.12.115+deb13.1-arm64");
        assert_eq!(p.newest.as_deref(), Some("6.12.117+deb13-arm64"));
        assert!(!p.busy);
        let p = parse_probe("6.12.117+deb13-arm64\n\nbusy\n").unwrap();
        assert_eq!(p.newest, None);
        assert!(p.busy);
        assert!(parse_probe("6.1\n../../etc\nidle\n").is_err());
        assert!(parse_probe("6.1\nx'; rm -rf /\nidle\n").is_err());
        assert!(parse_probe("").is_err());
    }
}
