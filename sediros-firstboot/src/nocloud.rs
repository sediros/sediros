//! NoCloud seed: a small disk or ISO with the label `cidata`/`CIDATA` holding
//! `meta-data` and `user-data` (Proxmox, libvirt, local tests).

use crate::meta::{self, Metadata};
use crate::sys;
use std::fs;
use std::path::{Path, PathBuf};

/// The seed device, if one is attached.
pub fn find() -> Option<PathBuf> {
    ["cidata", "CIDATA"]
        .iter()
        .map(|l| Path::new("/dev/disk/by-label").join(l))
        .find(|p| p.exists())
}

/// Metadata from a seed directory (already mounted, or a test fixture).
pub fn read_dir(dir: &Path) -> Option<Metadata> {
    let mut md = Metadata {
        source: "NoCloud seed",
        ..Default::default()
    };
    meta::parse_nocloud_meta(&fs::read_to_string(dir.join("meta-data")).ok()?, &mut md);
    if md.instance_id.is_empty() {
        return None;
    }
    if let Ok(ud) = fs::read_to_string(dir.join("user-data")) {
        meta::parse_user_data(&ud, &mut md);
    }
    Some(md)
}

/// Mounts the seed read-only, reads it, unmounts it.
pub fn read_device(dev: &Path) -> Option<Metadata> {
    let mnt = Path::new("/run/sediros-firstboot/seed");
    fs::create_dir_all(mnt).ok()?;
    let dev = dev.to_str()?;
    let mnt_s = mnt.to_str()?;
    if !sys::ok("mount", &["-o", "ro,nodev,nosuid,noexec", dev, mnt_s]) {
        return None;
    }
    let md = read_dir(mnt);
    sys::ok("umount", &[mnt_s]);
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_seed_directory() {
        let dir = std::env::temp_dir().join(format!("sediros-seed-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("meta-data"),
            "instance-id: iid-1\nlocal-hostname: lab\n",
        )
        .unwrap();
        fs::write(dir.join("user-data"), "#cloud-config\npackages: [nginx]\n").unwrap();
        let md = read_dir(&dir).unwrap();
        assert_eq!(
            (md.instance_id.as_str(), md.hostname.as_deref()),
            ("iid-1", Some("lab"))
        );
        assert!(md.user_data.notice().unwrap().contains("packages"));
        fs::write(dir.join("meta-data"), "local-hostname: lab\n").unwrap();
        assert!(read_dir(&dir).is_none(), "no instance-id, no seed");
        fs::remove_dir_all(&dir).unwrap();
    }
}
