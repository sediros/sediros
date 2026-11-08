//! The changes made for a new instance.

use crate::sys;
use std::fs;
use std::os::unix::fs::{chown, PermissionsExt};
use std::path::Path;

pub type Step = Result<String, String>;

/// /etc/hosts with `127.0.1.1 <name>` (Debian's line for the own hostname).
pub fn hosts_with(text: &str, name: &str) -> String {
    let line = format!("127.0.1.1\t{name}");
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.split_whitespace().next() == Some("127.0.1.1") {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        let at = out
            .iter()
            .position(|l| l.starts_with("127.0.0.1"))
            .map_or(out.len(), |i| i + 1);
        out.insert(at, line);
    }
    out.join("\n") + "\n"
}

/// authorized_keys with the keys that are not in it yet; (text, added).
pub fn merge_keys(existing: &str, keys: &[String]) -> (String, usize) {
    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    let blob = |k: &str| k.split_whitespace().nth(1).map(str::to_string);
    let have: Vec<String> = existing.lines().filter_map(blob).collect();
    let mut added = 0;
    for k in keys {
        if blob(k).is_some_and(|b| !have.contains(&b) && !out.contains(&b)) {
            out.push_str(k);
            out.push('\n');
            added += 1;
        }
    }
    (out, added)
}

pub fn set_hostname(name: &str) -> Step {
    fs::write("/etc/hostname", format!("{name}\n")).map_err(|e| format!("/etc/hostname: {e}"))?;
    let hosts = fs::read_to_string("/etc/hosts").unwrap_or_default();
    fs::write("/etc/hosts", hosts_with(&hosts, name)).map_err(|e| format!("/etc/hosts: {e}"))?;
    if !sys::ok("hostname", &[name]) {
        return Err("hostname command failed".into());
    }
    Ok(format!("hostname {name}"))
}

pub fn install_keys(user: &str, keys: &[String]) -> Step {
    let entry = sys::output("getent", &["passwd", user]).ok_or(format!("no user '{user}'"))?;
    let f: Vec<&str> = entry.trim().split(':').collect();
    let (uid, gid, home) = match f.as_slice() {
        [_, _, uid, gid, _, home, ..] => (uid.parse().ok(), gid.parse().ok(), Path::new(home)),
        _ => return Err(format!("unexpected passwd entry for '{user}'")),
    };
    // ssh-keygen checks each key fully; one bad key does not stop the others.
    let valid: Vec<String> = keys
        .iter()
        .filter(|k| sys::with_input("ssh-keygen", &["-l", "-f", "-"], k).is_ok())
        .cloned()
        .collect();
    let dir = home.join(".ssh");
    let file = dir.join("authorized_keys");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (text, added) = merge_keys(&fs::read_to_string(&file).unwrap_or_default(), &valid);
    fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    chown(&dir, uid, gid).map_err(|e| e.to_string())?;
    chown(&file, uid, gid).map_err(|e| e.to_string())?;
    let rejected = keys.len() - valid.len();
    Ok(format!(
        "{added} SSH key(s) added for {user}{}",
        if rejected > 0 {
            format!(", {rejected} invalid key(s) skipped")
        } else {
            String::new()
        }
    ))
}

/// New SSH host keys: every instance made from the same image would
/// otherwise share them (and anyone with the image could impersonate it).
pub fn regenerate_host_keys() -> Step {
    for e in fs::read_dir("/etc/ssh")
        .map_err(|e| e.to_string())?
        .flatten()
    {
        if e.file_name().to_string_lossy().starts_with("ssh_host_") {
            fs::remove_file(e.path()).map_err(|err| format!("{}: {err}", e.path().display()))?;
        }
    }
    if !sys::ok("ssh-keygen", &["-A"]) {
        return Err("ssh-keygen -A failed".into());
    }
    let fp = sys::output(
        "ssh-keygen",
        &["-l", "-f", "/etc/ssh/ssh_host_ed25519_key.pub"],
    )
    .unwrap_or_default();
    Ok(format!(
        "new SSH host keys: {}",
        fp.split_whitespace().nth(1).unwrap_or("?")
    ))
}

fn sysfs_num(path: impl AsRef<Path>) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Grows the root partition and its ext4 file system to the end of the disk
/// (cloud disks are usually larger than the image).
pub fn grow_root() -> Step {
    let src = sys::output("findmnt", &["-no", "SOURCE", "/"]).ok_or("findmnt failed")?;
    let src = src.trim();
    let part = src
        .strip_prefix("/dev/")
        .filter(|p| !p.starts_with("mapper/") && !p.starts_with("dm-"));
    let Some(part) = part else {
        return Ok(format!("root on {src}: not grown (device mapper)"));
    };
    let sys_part = Path::new("/sys/class/block").join(part);
    let Some(nr) = sysfs_num(sys_part.join("partition")) else {
        return Ok(format!("root on {src}: not a partition, not grown"));
    };
    let disk_dir = fs::canonicalize(sys_part.join("..")).map_err(|e| e.to_string())?;
    let disk = disk_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("no parent disk")?
        .to_string();
    let (start, size, total) = (
        sysfs_num(sys_part.join("start")).ok_or("no partition start")?,
        sysfs_num(sys_part.join("size")).ok_or("no partition size")?,
        sysfs_num(disk_dir.join("size")).ok_or("no disk size")?,
    );
    // Only the last partition can grow into the free space at the end.
    let later = fs::read_dir(&disk_dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| sysfs_num(e.path().join("start")))
        .any(|s| s > start);
    let free_mib = total.saturating_sub(start + size) / 2048;
    if later || free_mib < 64 {
        return Ok(format!(
            "root /dev/{part}: no free space after it, not grown"
        ));
    }
    let dev = format!("/dev/{disk}");
    // A bigger disk leaves GPT's backup header in the middle: move it to the end.
    sys::ok("sfdisk", &["--relocate", "gpt-bak-std", &dev]);
    let nr = nr.to_string();
    sys::with_input(
        "sfdisk",
        &["--no-reread", "--no-tell-kernel", "-N", &nr, &dev],
        ", +\n",
    )
    .map_err(|e| format!("sfdisk: {e}"))?;
    if !sys::ok("partx", &["--update", "--nr", &nr, &dev]) {
        return Err("partx --update failed (reboot to use the new size)".into());
    }
    sys::with_input("resize2fs", &[src], "").map_err(|e| format!("resize2fs: {e}"))?;
    Ok(format!("root /dev/{part} grown by {free_mib} MiB"))
}

/// A notice shown at login until the admin deletes it.
pub fn login_notice(lines: &[String]) -> Step {
    let dir = Path::new("/etc/motd.d");
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = dir.join("50-sediros-firstboot");
    let mut text = String::from("sediros-firstboot:\n");
    for l in lines {
        text.push_str(&format!("  {l}\n"));
    }
    text.push_str(&format!(
        "  (delete {} to hide this notice)\n",
        file.display()
    ));
    fs::write(&file, text).map_err(|e| e.to_string())?;
    Ok(format!("login notice in {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_line() {
        let debian = "127.0.0.1\tlocalhost\n127.0.1.1\tsediros\n\n::1\tlocalhost ip6-localhost\n";
        let new = hosts_with(debian, "web1");
        assert!(new.contains("127.0.1.1\tweb1\n") && !new.contains("sediros"));
        assert!(new.contains("::1\tlocalhost"));
        let none = hosts_with("127.0.0.1 localhost\n::1 localhost\n", "web1");
        assert_eq!(
            none,
            "127.0.0.1 localhost\n127.0.1.1\tweb1\n::1 localhost\n"
        );
    }

    #[test]
    fn keys_are_added_once() {
        let a = "ssh-ed25519 AAAAone a@x".to_string();
        let b = "ssh-ed25519 AAAAtwo b@y".to_string();
        let (text, n) = merge_keys(
            "ssh-ed25519 AAAAone old-comment",
            &[a.clone(), b.clone(), b.clone()],
        );
        assert_eq!(n, 1);
        assert_eq!(
            text,
            "ssh-ed25519 AAAAone old-comment\nssh-ed25519 AAAAtwo b@y\n"
        );
        assert_eq!(merge_keys("", &[a]).1, 1);
    }
}
