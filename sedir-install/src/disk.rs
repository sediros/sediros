//! Block device discovery and safety checks (from /sys and /proc, no lsblk).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Smallest disk we install to: ESP 512 MiB + /boot 1 GiB + root.
pub const MIN_SIZE: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Disk {
    /// Kernel name, e.g. "vdb", "nvme0n1".
    pub name: String,
    pub size: u64,
    pub model: String,
    pub removable: bool,
    pub read_only: bool,
}

impl Disk {
    pub fn path(&self) -> PathBuf {
        PathBuf::from("/dev").join(&self.name)
    }

    /// Path of partition `n`: vdb -> vdb1, nvme0n1 -> nvme0n1p1.
    pub fn partition(&self, n: u32) -> PathBuf {
        let sep = if self.name.ends_with(|c: char| c.is_ascii_digit()) {
            "p"
        } else {
            ""
        };
        PathBuf::from(format!("/dev/{}{sep}{n}", self.name))
    }

    pub fn describe(&self) -> String {
        let mut s = format!("/dev/{}  {}", self.name, human_size(self.size));
        if !self.model.is_empty() {
            s.push_str(&format!("  {}", self.model));
        }
        if self.removable {
            s.push_str("  (removable)");
        }
        if self.read_only {
            s.push_str("  (read-only)");
        }
        s
    }
}

pub fn human_size(bytes: u64) -> String {
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gib >= 1024.0 {
        format!("{:.1} TiB", gib / 1024.0)
    } else {
        format!("{gib:.1} GiB")
    }
}

fn read_trim(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Whole disks that could be install targets (no partitions, loop, ram,
/// zram, optical or device-mapper devices).
pub fn list() -> Result<Vec<Disk>> {
    let mut disks = Vec::new();
    for entry in fs::read_dir("/sys/class/block").context("reading /sys/class/block")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let sys = entry.path();
        if sys.join("partition").exists()
            || ["loop", "ram", "zram", "sr", "dm-", "md", "fd"]
                .iter()
                .any(|p| name.starts_with(p))
        {
            continue;
        }
        let sectors: u64 = read_trim(&sys.join("size")).parse().unwrap_or(0);
        if sectors == 0 {
            continue;
        }
        disks.push(Disk {
            size: sectors * 512, // /sys reports 512-byte units regardless of the device
            model: read_trim(&sys.join("device/model")),
            removable: read_trim(&sys.join("removable")) == "1",
            read_only: read_trim(&sys.join("ro")) == "1",
            name,
        });
    }
    disks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(disks)
}

pub fn find(spec: &str) -> Result<Disk> {
    let name = spec.trim_start_matches("/dev/");
    let disks = list()?;
    disks
        .into_iter()
        .find(|d| d.name == name)
        .with_context(|| format!("{spec} is not a whole disk (see: sedir-install disks)"))
}

/// The disk, its partitions and everything stacked on them (dm-crypt, LVM...).
fn device_family(disk: &str) -> BTreeSet<String> {
    let mut family = BTreeSet::new();
    let mut todo = vec![disk.to_string()];
    let base = PathBuf::from("/sys/class/block").join(disk);
    if let Ok(entries) = fs::read_dir(&base) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(disk) && e.path().join("partition").exists() {
                todo.push(n);
            }
        }
    }
    while let Some(dev) = todo.pop() {
        if !family.insert(dev.clone()) {
            continue;
        }
        let holders = PathBuf::from("/sys/class/block").join(&dev).join("holders");
        if let Ok(entries) = fs::read_dir(holders) {
            for e in entries.flatten() {
                todo.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    family
}

/// Kernel device name behind a mount source or swap path, if it is a block
/// device: /dev/mapper/x -> dm-0, /dev/disk/by-uuid/... -> vda1.
fn kernel_name(source: &str) -> Option<String> {
    if !source.starts_with("/dev/") {
        return None;
    }
    let real = fs::canonicalize(source).ok()?;
    Some(real.file_name()?.to_string_lossy().into_owned())
}

/// Why the disk must not be touched (mounted, swap, or the live medium).
pub fn in_use(disk: &Disk) -> Result<Vec<String>> {
    let family = device_family(&disk.name);
    let mut reasons = Vec::new();

    // mountinfo: "... - <fstype> <source> <options>"
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").context("reading mountinfo")?;
    for line in mountinfo.lines() {
        let mut fields = line.split_whitespace();
        let mountpoint = fields.nth(4).unwrap_or("?").to_string();
        let source = line
            .split(" - ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().nth(1));
        if let Some(name) = source.and_then(kernel_name) {
            if family.contains(&name) {
                reasons.push(format!("/dev/{name} is mounted on {mountpoint}"));
            }
        }
    }

    if let Ok(swaps) = fs::read_to_string("/proc/swaps") {
        for line in swaps.lines().skip(1) {
            if let Some(name) = line.split_whitespace().next().and_then(kernel_name) {
                if family.contains(&name) {
                    reasons.push(format!("/dev/{name} is used as swap"));
                }
            }
        }
    }

    // Partitions held open by device-mapper (e.g. an unlocked LUKS volume).
    for dev in &family {
        if dev.starts_with("dm-") {
            let name = read_trim(&PathBuf::from("/sys/class/block").join(dev).join("dm/name"));
            reasons.push(format!(
                "device-mapper volume '{name}' ({dev}) is open on it"
            ));
        }
    }
    Ok(reasons)
}

pub fn check_target(disk: &Disk) -> Result<()> {
    if disk.read_only {
        bail!("/dev/{} is read-only", disk.name);
    }
    let reasons = in_use(disk)?;
    if !reasons.is_empty() {
        bail!(
            "/dev/{} is in use, refusing to erase it:\n  - {}",
            disk.name,
            reasons.join("\n  - ")
        );
    }
    if disk.size < MIN_SIZE {
        bail!(
            "/dev/{} is {}, at least {} required",
            disk.name,
            human_size(disk.size),
            human_size(MIN_SIZE)
        );
    }
    Ok(())
}
