//! The `integrity` check: report changes to files that should not change.
//!
//! Two layers:
//!   - watched files (WATCH, plus every login user's authorized_keys): their
//!     SHA-256, type, mode and owner, compared with a reference taken
//!     earlier (/var/lib/sediros/audit/integrity.ref);
//!   - package files: `dpkg --verify` against Debian's own checksums. It
//!     takes several seconds, so it runs in the daily run and in
//!     `sediros-audit integrity`; a plain `sediros-audit` shows the last
//!     daily result.
//!
//! Not a full-disk scanner: most of /var, /home and /tmp change all the
//! time. A file is watched if it should not change and is worth something
//! to an attacker. A root attacker can rewrite the reference too; the
//! reference's own hash is printed so it can be noted elsewhere.
//!
//! Package updates legitimately change watched files (initrd, grub.cfg):
//! the dpkg hook (`--after-dpkg`) takes a new reference after every apt
//! run and keeps what changed in integrity-last-update, so such changes
//! are accepted but stay visible.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::record::STATE_DIR;
use crate::report::Finding;
use crate::sys::{self, run_full};

const ID: &str = "integrity";

/// Watched paths. `dir/**`: every file below dir; `*` in the last
/// component: matching names in that directory.
const WATCH: &[&str] = &[
    // accounts and privilege
    "/etc/passwd",
    "/etc/shadow",
    "/etc/group",
    "/etc/gshadow",
    "/etc/sudoers",
    "/etc/sudoers.d/*",
    "/etc/pam.d/*",
    "/etc/security/**",
    "/etc/login.defs",
    "/etc/nsswitch.conf",
    // SSH (authorized_keys: see login_users)
    "/etc/ssh/sshd_config",
    "/etc/ssh/sshd_config.d/*",
    "/etc/ssh/ssh_host_*_key.pub",
    // what runs at boot, on a schedule or at login
    "/etc/systemd/system/**",
    "/etc/systemd/user/**",
    "/etc/crontab",
    "/etc/cron.d/*",
    "/etc/cron.hourly/*",
    "/etc/cron.daily/*",
    "/etc/cron.weekly/*",
    "/etc/cron.monthly/*",
    "/var/spool/cron/crontabs/*",
    "/etc/rc.local",
    "/etc/init.d/*",
    "/etc/profile",
    "/etc/profile.d/*",
    "/etc/bash.bashrc",
    "/etc/environment",
    "/etc/udev/rules.d/*",
    // loader and kernel
    "/etc/ld.so.preload",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d/*",
    "/etc/modprobe.d/*",
    "/etc/modules-load.d/*",
    "/etc/sysctl.conf",
    "/etc/sysctl.d/*",
    // boot and disks
    "/etc/default/grub",
    "/etc/default/grub.d/*",
    "/boot/grub/grub.cfg",
    "/boot/initrd.img-*",
    "/etc/initramfs-tools/**",
    "/etc/fstab",
    "/etc/crypttab",
    // network and firewall
    "/etc/hosts",
    "/etc/nftables.conf",
    "/etc/default/ufw",
    "/etc/ufw/*.rules",
    // package sources and their keys
    "/etc/apt/sources.list",
    "/etc/apt/sources.list.d/*",
    "/etc/apt/keyrings/*",
    "/etc/apt/trusted.gpg.d/*",
    "/etc/apt/preferences.d/*",
    "/etc/apt/apt.conf.d/*",
    // programs outside packages
    "/usr/local/bin/**",
    "/usr/local/sbin/**",
    "/usr/local/lib/**",
    // SedirOS itself, audit.conf included: turning reports off is a change
    "/etc/sediros/**",
];

/// Every program loads the libraries listed here: a classic way to hide.
const LD_PRELOAD: &str = "/etc/ld.so.preload";
const DPKG_CFG_DIR: &str = "/etc/dpkg/dpkg.cfg.d";
/// Details shown by a full `sediros-audit`; `sediros-audit integrity` shows all.
const SHOWN: usize = 15;

fn reference_path() -> PathBuf {
    Path::new(STATE_DIR).join("integrity.ref")
}
fn packages_path() -> PathBuf {
    Path::new(STATE_DIR).join("integrity-packages")
}
fn last_update_path() -> PathBuf {
    Path::new(STATE_DIR).join("integrity-last-update")
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `sediros-audit`: watched files; package files from the last daily run.
    Full,
    /// `sediros-audit --record`: everything; takes the first reference.
    Record,
    /// `sediros-audit integrity`: everything, every detail.
    Single,
}

/// One watched file as recorded: type (f file, l symlink, o other), mode,
/// owner, and the SHA-256 (file), the link target (symlink) or "-".
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: char,
    mode: u32,
    uid: u32,
    gid: u32,
    hash: String,
}

type Snapshot = BTreeMap<String, Entry>;

// --- the check ------------------------------------------------------------------

pub fn check(mode: Mode) -> Finding {
    let (now, unusual) = snapshot();
    let reference = load_reference();

    let mut fail = Vec::new();
    let mut warn = Vec::new();
    let mut info = Vec::new();

    if now.contains_key(LD_PRELOAD) {
        fail.push(format!(
            "{LD_PRELOAD} exists: every program loads the libraries listed in it"
        ));
    }
    for p in now.keys().filter(|p| p.ends_with("/.ssh/authorized_keys2")) {
        warn.push(format!(
            "{p} exists: a second, rarely used key file sshd also reads"
        ));
    }
    for p in unusual {
        warn.push(format!("{p:?}: unusual file name, not checked"));
    }

    match package_files(mode) {
        PackageFiles::Problems(lines, when) => {
            for l in lines {
                fail.push(format!("{l} (package file, {when})"));
            }
        }
        PackageFiles::Ok(when) => info.push(format!("package files: unchanged ({when})")),
        PackageFiles::Unknown => info.push("package files: checked by the daily run".into()),
    }

    let summary_ref;
    match &reference {
        Some((taken, old)) => {
            let changes = diff(old, &now);
            summary_ref = format!("{} watched files, reference {taken}", now.len());
            warn.extend(changes);
        }
        None if mode == Mode::Record => match save_reference(&now) {
            Ok(()) => summary_ref = format!("reference taken: {} watched files", now.len()),
            Err(e) => return Finding::fail(ID, format!("cannot write the reference: {e}")),
        },
        None => {
            summary_ref = "no reference yet: the daily run takes it, or now: \
                           sudo sediros-audit integrity --accept"
                .into();
        }
    }

    if let Some((when, files)) = last_update() {
        info.push(format!(
            "{} watched file(s) changed by the package update of {when} (accepted)",
            files.len()
        ));
        if mode == Mode::Single {
            info.extend(files.into_iter().map(|f| format!("  {f}")));
        }
    }
    if mode == Mode::Single {
        if let Some(h) = reference_hash() {
            info.push(format!(
                "reference sha256 {h} (note it elsewhere: a changed hash you did not expect means the reference was rewritten)"
            ));
        }
    }

    let (finding, mut details) = if !fail.is_empty() {
        let changes = match warn.len() {
            0 => String::new(),
            n => format!(", {n} change(s)"),
        };
        (
            Finding::fail(
                ID,
                format!("{} problem(s){changes}; {summary_ref}", fail.len()),
            ),
            [fail, warn].concat(),
        )
    } else if !warn.is_empty() {
        (
            Finding::warn(ID, format!("{} change(s); {summary_ref}", warn.len())),
            warn,
        )
    } else if reference.is_none() && mode != Mode::Record {
        (Finding::info(ID, summary_ref), Vec::new())
    } else {
        (Finding::pass(ID, summary_ref), Vec::new())
    };
    if mode != Mode::Single && details.len() > SHOWN {
        let more = details.len() - SHOWN;
        details.truncate(SHOWN);
        details.push(format!("... and {more} more: sudo sediros-audit integrity"));
    }
    details.extend(info);
    finding.with(details)
}

/// `sediros-audit integrity --accept`: take the current state as the new
/// reference. Returns the lines to print.
pub fn accept() -> Result<Vec<String>, String> {
    let (now, _) = snapshot();
    let mut out = match load_reference() {
        Some((_, old)) => {
            let changes = diff(&old, &now);
            if changes.is_empty() {
                vec!["No changes since the reference.".to_string()]
            } else {
                let mut v = vec![format!("Accepted {} change(s):", changes.len())];
                v.extend(changes.into_iter().map(|c| format!("  {c}")));
                v
            }
        }
        None => vec![format!("First reference: {} watched files.", now.len())],
    };
    save_reference(&now)?;
    let _ = fs::remove_file(last_update_path());
    if let Some(h) = reference_hash() {
        out.push(format!("Reference sha256 {h}"));
    }
    Ok(out)
}

/// `sediros-audit integrity --after-dpkg`, from the apt hook: package
/// updates change watched files; take a new reference, keep what changed.
pub fn after_dpkg() {
    let Some((_, old)) = load_reference() else {
        return;
    };
    let (now, _) = snapshot();
    let changes = diff(&old, &now);
    if changes.is_empty() {
        return;
    }
    let text = format!("{}\n{}\n", sys::now_utc(), changes.join("\n"));
    let _ = crate::record::write_atomic(&last_update_path(), &text, 0o600);
    let _ = save_reference(&now);
}

fn last_update() -> Option<(String, Vec<String>)> {
    let text = sys::read(last_update_path())?;
    let mut lines = text.lines();
    let when = lines.next()?.to_string();
    Some((when, lines.map(str::to_string).collect()))
}

// --- watched files --------------------------------------------------------------

/// The current state of every watched file, and names that cannot be stored
/// in the reference (tabs, newlines, backslashes).
fn snapshot() -> (Snapshot, Vec<String>) {
    let mut paths: Vec<PathBuf> = WATCH.iter().flat_map(|p| expand(p)).collect();
    for home in login_homes() {
        for name in ["authorized_keys", "authorized_keys2"] {
            let p = home.join(".ssh").join(name);
            if p.symlink_metadata().is_ok() {
                paths.push(p);
            }
        }
    }
    paths.sort();
    paths.dedup();

    let mut snap = Snapshot::new();
    let mut unusual = Vec::new();
    let mut to_hash = Vec::new();
    for p in paths {
        let Some(s) = p.to_str().map(str::to_string) else {
            unusual.push(p.display().to_string());
            continue;
        };
        if s.contains(['\t', '\n', '\\']) {
            unusual.push(s);
            continue;
        }
        let Ok(md) = p.symlink_metadata() else {
            continue;
        };
        let ft = md.file_type();
        let (kind, hash) = if ft.is_symlink() {
            let t = fs::read_link(&p)
                .map(|t| t.display().to_string())
                .unwrap_or_default();
            ('l', t)
        } else if ft.is_file() {
            to_hash.push(s.clone());
            ('f', String::new())
        } else {
            ('o', "-".to_string())
        };
        snap.insert(
            s,
            Entry {
                kind,
                mode: md.permissions().mode() & 0o7777,
                uid: md.uid(),
                gid: md.gid(),
                hash,
            },
        );
    }
    for (path, hash) in sha256(&to_hash) {
        if let Some(e) = snap.get_mut(&path) {
            e.hash = hash;
        }
    }
    (snap, unusual)
}

/// SHA-256 of files with coreutils' sha256sum, many per call. A file that
/// cannot be read keeps an empty hash, which differs from any recorded one.
fn sha256(files: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for chunk in files.chunks(200) {
        let mut args: Vec<&str> = vec!["--"];
        args.extend(chunk.iter().map(String::as_str));
        let Some((_, stdout, _)) = run_full("sha256sum", &args) else {
            continue;
        };
        for l in stdout.lines() {
            if let Some((h, p)) = l.split_once("  ") {
                out.push((p.to_string(), h.to_string()));
            }
        }
    }
    out
}

/// Home directories of root and of every account that can log in.
fn login_homes() -> Vec<PathBuf> {
    let Some(text) = sys::read("/etc/passwd") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            let (home, shell) = (*f.get(5)?, *f.get(6)?);
            let login = !shell.ends_with("/nologin") && !shell.ends_with("/false");
            (login && home.starts_with('/')).then(|| PathBuf::from(home))
        })
        .collect()
}

fn expand(pattern: &str) -> Vec<PathBuf> {
    if let Some(dir) = pattern.strip_suffix("/**") {
        let mut out = Vec::new();
        walk(Path::new(dir), &mut out);
        return out;
    }
    let path = Path::new(pattern);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if !name.contains('*') {
        return if path.symlink_metadata().is_ok() {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        };
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
        .filter(|e| wildcard(name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect()
}

/// Every non-directory below `dir`; symlinked directories are not followed.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&e.path(), out),
            Ok(_) => out.push(e.path()),
            Err(_) => {}
        }
    }
}

/// Shell-style match with `*` only (any run of characters, '/' included,
/// as dpkg's path-exclude does).
fn wildcard(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == s;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !s.starts_with(first) || s.len() < first.len() + last.len() || !s.ends_with(last) {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

fn diff(old: &Snapshot, now: &Snapshot) -> Vec<String> {
    let mut out = Vec::new();
    for (p, n) in now {
        let Some(o) = old.get(p) else {
            out.push(format!("{p} (new)"));
            continue;
        };
        let mut what = Vec::new();
        if o.kind != n.kind {
            what.push("type".to_string());
        } else if o.hash != n.hash {
            what.push(if n.kind == 'l' {
                format!("link {} -> {}", o.hash, n.hash)
            } else {
                "content".to_string()
            });
        }
        if o.mode != n.mode {
            what.push(format!("mode {:04o} -> {:04o}", o.mode, n.mode));
        }
        if (o.uid, o.gid) != (n.uid, n.gid) {
            what.push(format!("owner {}:{} -> {}:{}", o.uid, o.gid, n.uid, n.gid));
        }
        if !what.is_empty() {
            out.push(format!("{p} ({})", what.join(", ")));
        }
    }
    for p in old.keys().filter(|p| !now.contains_key(*p)) {
        out.push(format!("{p} (removed)"));
    }
    out
}

// --- the reference --------------------------------------------------------------

fn serialize(snap: &Snapshot, taken: &str) -> String {
    let mut out = format!("# sediros-audit integrity reference {taken}\n");
    for (p, e) in snap {
        out.push_str(&format!(
            "{}\t{:o}\t{}\t{}\t{}\t{p}\n",
            e.kind, e.mode, e.uid, e.gid, e.hash
        ));
    }
    out
}

fn parse(text: &str) -> Option<(String, Snapshot)> {
    let mut lines = text.lines();
    let taken = lines
        .next()?
        .strip_prefix("# sediros-audit integrity reference ")?
        .to_string();
    let mut snap = Snapshot::new();
    for l in lines {
        let f: Vec<&str> = l.splitn(6, '\t').collect();
        let [kind, mode, uid, gid, hash, path] = f[..] else {
            return None;
        };
        snap.insert(
            path.to_string(),
            Entry {
                kind: kind.chars().next()?,
                mode: u32::from_str_radix(mode, 8).ok()?,
                uid: uid.parse().ok()?,
                gid: gid.parse().ok()?,
                hash: hash.to_string(),
            },
        );
    }
    Some((taken, snap))
}

fn load_reference() -> Option<(String, Snapshot)> {
    parse(&sys::read(reference_path())?)
}

/// root only: it lists hashes of /etc/shadow and of private files.
fn save_reference(snap: &Snapshot) -> Result<(), String> {
    fs::create_dir_all(STATE_DIR).map_err(|e| format!("{STATE_DIR}: {e}"))?;
    crate::record::write_atomic(&reference_path(), &serialize(snap, &sys::now_utc()), 0o600)
}

fn reference_hash() -> Option<String> {
    let path = reference_path().display().to_string();
    sha256(&[path]).into_iter().next().map(|(_, h)| h)
}

// --- package files --------------------------------------------------------------

enum PackageFiles {
    Ok(String),
    Problems(Vec<String>, String),
    Unknown,
}

/// `dpkg --verify` over every package: files that differ from Debian's
/// checksums, or are missing. Changed config files (conffiles) are an
/// admin's business; the important ones are watched files. Skipped: files
/// dpkg was told not to install (path-exclude: docs, translations) and
/// /var, which holds variable data by definition (apt's lists and caches
/// are emptied in images); crontabs there are watched files.
fn package_files(mode: Mode) -> PackageFiles {
    if mode == Mode::Full {
        let Some(text) = sys::read(packages_path()) else {
            return PackageFiles::Unknown;
        };
        let mut lines = text.lines();
        let when = format!("daily run {}", lines.next().unwrap_or("?"));
        let problems: Vec<String> = lines.map(str::to_string).collect();
        return if problems.is_empty() {
            PackageFiles::Ok(when)
        } else {
            PackageFiles::Problems(problems, when)
        };
    }
    let Some((_, out, _)) = run_full("dpkg", &["--verify"]) else {
        return PackageFiles::Unknown;
    };
    let problems = parse_verify(&out, &excludes());
    let when = sys::now_utc();
    if mode == Mode::Record {
        let text: String = std::iter::once(when.as_str())
            .chain(problems.iter().map(String::as_str))
            .map(|l| format!("{l}\n"))
            .collect();
        let _ = crate::record::write_atomic(&packages_path(), &text, 0o640);
    }
    if problems.is_empty() {
        PackageFiles::Ok(format!("checked now, {when}"))
    } else {
        PackageFiles::Problems(problems, "checked now".into())
    }
}

/// path-exclude / path-include rules from /etc/dpkg/dpkg.cfg.d, in order.
fn excludes() -> Vec<(bool, String)> {
    sys::dropins(DPKG_CFG_DIR, "")
        .iter()
        .filter_map(sys::read)
        .flat_map(|t| {
            t.lines()
                .filter_map(|l| {
                    let l = l.trim();
                    if let Some(p) = l.strip_prefix("path-exclude") {
                        Some((true, p.trim_start_matches([' ', '=']).trim().to_string()))
                    } else {
                        l.strip_prefix("path-include")
                            .map(|p| (false, p.trim_start_matches([' ', '=']).trim().to_string()))
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// dpkg applies the last matching rule.
fn excluded(rules: &[(bool, String)], path: &str) -> bool {
    rules
        .iter()
        .rev()
        .find(|(_, p)| wildcard(p, path))
        .is_some_and(|(ex, _)| *ex)
}

fn parse_verify(out: &str, rules: &[(bool, String)]) -> Vec<String> {
    out.lines()
        .filter_map(|l| {
            let path = l.split_whitespace().last().filter(|p| p.starts_with('/'))?;
            if l.contains(" c /") || path.starts_with("/var/") || excluded(rules, path) {
                return None;
            }
            Some(if l.starts_with("missing") {
                format!("{path} missing")
            } else {
                format!("{path} changed")
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: char, mode: u32, hash: &str) -> Entry {
        Entry {
            kind,
            mode,
            uid: 0,
            gid: 0,
            hash: hash.into(),
        }
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("ssh_host_*_key.pub", "ssh_host_ed25519_key.pub"));
        assert!(!wildcard("ssh_host_*_key.pub", "ssh_host_ed25519_key"));
        assert!(wildcard("*.rules", "user.rules"));
        assert!(!wildcard("*.rules", "user.rules.20261115"));
        assert!(wildcard("/usr/share/doc/*", "/usr/share/doc/a/b/c"));
        assert!(wildcard("initrd.img-*", "initrd.img-6.12.117+deb13-arm64"));
        assert!(!wildcard("a*b*c", "acb"));
        assert!(wildcard("exact", "exact"));
    }

    #[test]
    fn diffs() {
        let mut old = Snapshot::new();
        old.insert("/etc/a".into(), e('f', 0o644, "h1"));
        old.insert("/etc/b".into(), e('f', 0o644, "h2"));
        old.insert("/etc/l".into(), e('l', 0o777, "/x"));
        let mut now = old.clone();
        now.get_mut("/etc/a").unwrap().hash = "h9".into();
        now.get_mut("/etc/a").unwrap().mode = 0o666;
        now.remove("/etc/b");
        now.get_mut("/etc/l").unwrap().hash = "/y".into();
        now.insert("/etc/c".into(), e('f', 0o600, "h3"));
        let d = diff(&old, &now);
        assert_eq!(
            d,
            [
                "/etc/a (content, mode 0644 -> 0666)",
                "/etc/c (new)",
                "/etc/l (link /x -> /y)",
                "/etc/b (removed)"
            ]
        );
        assert!(diff(&old, &old).is_empty());
    }

    #[test]
    fn reference_round_trip() {
        let mut snap = Snapshot::new();
        snap.insert("/etc/a b".into(), e('f', 0o4755, "abc"));
        snap.insert("/etc/l".into(), e('l', 0o777, "../x y"));
        let text = serialize(&snap, "2026-11-15 10:00 UTC");
        let (taken, back) = parse(&text).unwrap();
        assert_eq!(taken, "2026-11-15 10:00 UTC");
        assert_eq!(back, snap);
        assert!(parse("garbage").is_none());
        assert!(parse("# sediros-audit integrity reference x\nbad line").is_none());
    }

    #[test]
    fn dpkg_verify_filtering() {
        let rules = vec![
            (true, "/usr/share/doc/*".to_string()),
            (false, "/usr/share/doc/*/copyright".to_string()),
            (true, "/usr/share/locale/*".to_string()),
        ];
        let out = "missing     /usr/share/doc/x/changelog.gz\n\
                   missing     /usr/share/doc/x/copyright\n\
                   missing     /usr/share/locale/de/LC_MESSAGES/x.mo\n\
                   ??5?????? c /etc/login.defs\n\
                   missing     /var/lib/apt/lists/partial\n\
                   ??5??????   /usr/bin/sudo\n";
        assert_eq!(
            parse_verify(out, &rules),
            [
                "/usr/share/doc/x/copyright missing",
                "/usr/bin/sudo changed"
            ]
        );
    }
}
