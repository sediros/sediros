//! The checks. Expected values are read from the files SedirOS's packages
//! install (99-sedir.conf, suid-allowlist, ...), so the audit follows the
//! hardening of the installed package versions instead of a copy in here.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::SystemTime;

use crate::report::Finding;
use crate::sys::{self, conf_lines, read, run, run_full, squash, Ctx, Kind};

const SYSCTL_CONF: &str = "/etc/sysctl.d/99-sedir.conf";
const SSHD_CONF: &str = "/etc/ssh/sshd_config.d/99-sedir.conf";
const SUID_ALLOWLIST: &str = "/etc/sedir/suid-allowlist";
const BLACKLIST: &str = "/etc/modprobe.d/sedir-blacklist.conf";
const GRUB_CFG: &str = "/etc/default/grub.d/90-sedir.cfg";
const APT_SOURCE: &str = "/etc/apt/sources.list.d/sedir.sources";
const APT_PIN: &str = "/etc/apt/preferences.d/sedir";
const KEYRING: &str = "/usr/share/keyrings/sedir-archive-keyring.pgp";
const REPO_PAGE: &str = "https://sediros.github.io/sediros/apt/";

pub fn all(ctx: &Ctx) -> Vec<Finding> {
    vec![
        packages(),
        services(ctx),
        sysctl(),
        ssh(),
        firewall(),
        netfilter(),
        listeners(),
        apparmor(),
        suid(),
        accounts(ctx),
        sudo(),
        pam(),
        modules(),
        updates(),
        apt_source(),
        kernel_cmdline(ctx),
        encryption(ctx),
        secure_boot(ctx),
        core_dumps(),
    ]
}

// --- packages ---------------------------------------------------------------------

fn packages() -> Finding {
    const ID: &str = "packages";
    let Some(out) = run(
        "dpkg-query",
        &[
            "-W",
            "-f=${db:Status-Abbrev} ${Package} ${Version}\\n",
            "sedir-*",
        ],
    ) else {
        return Finding::fail(ID, "no SedirOS packages installed");
    };
    let installed: Vec<(String, String)> = out
        .lines()
        .filter(|l| l.starts_with("ii "))
        .filter_map(|l| {
            let mut f = l.split_whitespace().skip(1);
            Some((f.next()?.to_string(), f.next()?.to_string()))
        })
        .collect();
    let names: Vec<&str> = installed.iter().map(|(n, _)| n.as_str()).collect();
    let missing: Vec<&str> = ["sedir-base-files", "sedir-hardening"]
        .into_iter()
        .filter(|p| !names.contains(p))
        .collect();
    if !missing.is_empty() {
        return Finding::fail(ID, format!("missing: {}", missing.join(", ")));
    }

    let mut args = vec!["--verify"];
    args.extend(names.iter().copied());
    let verify = run_full("dpkg", &args)
        .map(|(_, o, _)| o)
        .unwrap_or_default();
    let (conffiles, others) = parse_dpkg_verify(&verify);
    let list = installed
        .iter()
        .map(|(n, v)| format!("{n} {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    if !others.is_empty() {
        return Finding::fail(ID, "files of SedirOS packages changed or missing").with(
            others
                .into_iter()
                .chain(
                    conffiles
                        .into_iter()
                        .map(|c| format!("{c} (config, changed by an admin)")),
                )
                .collect(),
        );
    }
    if !conffiles.is_empty() {
        return Finding::warn(
            ID,
            format!(
                "{} SedirOS config file(s) changed by an admin",
                conffiles.len()
            ),
        )
        .with(conffiles);
    }
    Finding::pass(ID, format!("unmodified: {list}"))
}

/// `dpkg --verify`: "??5?????? c /etc/x" (c = conffile), "missing     /usr/y".
/// Returns (changed conffiles, other changed or missing files).
fn parse_dpkg_verify(out: &str) -> (Vec<String>, Vec<String>) {
    let mut conf = Vec::new();
    let mut other = Vec::new();
    for l in out.lines() {
        let Some(path) = l.split_whitespace().last().filter(|p| p.starts_with('/')) else {
            continue;
        };
        if l.contains(" c /") {
            conf.push(path.to_string());
        } else if l.starts_with("missing") {
            other.push(format!("{path} (missing)"));
        } else {
            other.push(format!("{path} (changed)"));
        }
    }
    (conf, other)
}

fn services(ctx: &Ctx) -> Finding {
    const ID: &str = "services";
    let mut units = vec![
        "sedir-ssh-hostkeys.service",
        "sedir-firstboot-ufw.service",
        "sedir-netfilter.service",
    ];
    if ctx.kind == Kind::Vm {
        units.extend(["sedir-guest-ssh-keys.service", "sedir-guest-ufw.service"]);
    }
    let bad: Vec<String> = units
        .iter()
        .filter_map(|u| {
            let state = run("systemctl", &["is-enabled", u]).unwrap_or_default();
            (state.trim() != "enabled").then(|| format!("{u}: {}", or_dash(state.trim())))
        })
        .collect();
    if bad.is_empty() {
        Finding::pass(ID, format!("{} SedirOS units enabled", units.len()))
    } else {
        Finding::fail(ID, "SedirOS units not enabled").with(bad)
    }
}

// --- sysctl ---------------------------------------------------------------------------

fn sysctl() -> Finding {
    const ID: &str = "sysctl";
    let Some(conf) = read(SYSCTL_CONF) else {
        return Finding::fail(ID, format!("{SYSCTL_CONF} missing"));
    };
    let wanted = parse_sysctl(&conf);
    let overrides = sysctl_overrides();
    let (mut ok, mut warn, mut fail) = (0, Vec::new(), Vec::new());
    for (key, want, optional) in &wanted {
        let path = format!("/proc/sys/{}", key.replace('.', "/"));
        let Some(got) = read(&path) else {
            if !optional {
                fail.push(format!("{key}: not supported by this kernel"));
            }
            continue;
        };
        let got = squash(&got);
        if got == *want {
            ok += 1;
        } else if let Some(file) = overrides.get(key.as_str()) {
            warn.push(format!("{key} = {got} (SedirOS: {want}), set in {file}"));
        } else {
            fail.push(format!("{key} = {got}, want {want} (changed at runtime?)"));
        }
    }
    if !fail.is_empty() {
        return Finding::fail(
            ID,
            format!("{} kernel setting(s) not as configured", fail.len()),
        )
        .with(fail.into_iter().chain(warn).collect());
    }
    if !warn.is_empty() {
        return Finding::warn(
            ID,
            format!("{} setting(s) overridden by an admin", warn.len()),
        )
        .with(warn);
    }
    Finding::pass(ID, format!("{ok} kernel settings as configured"))
}

/// (key, value, optional) from a sysctl.d file; "-key = v" is optional.
fn parse_sysctl(text: &str) -> Vec<(String, String, bool)> {
    conf_lines(text)
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let k = k.trim();
            let optional = k.starts_with('-');
            Some((k.trim_start_matches('-').to_string(), squash(v), optional))
        })
        .collect()
}

/// Keys set by sysctl.d files applied after 99-sedir.conf (systemd-sysctl
/// sorts all directories' files by name; a later file wins).
fn sysctl_overrides() -> BTreeMap<String, String> {
    let mut files: Vec<String> = [
        "/etc/sysctl.d",
        "/run/sysctl.d",
        "/usr/local/lib/sysctl.d",
        "/usr/lib/sysctl.d",
    ]
    .iter()
    .flat_map(|d| sys::dropins(d, ".conf"))
    .filter(|f| file_name(f).as_str() > "99-sedir.conf")
    .collect();
    files.sort_by_key(|f| file_name(f));
    let mut map = BTreeMap::new();
    for f in files {
        if let Some(t) = read(&f) {
            for (k, _, _) in parse_sysctl(&t) {
                map.insert(k, f.clone());
            }
        }
    }
    map
}

// --- SSH ----------------------------------------------------------------------------

/// Settings whose weakening opens the door; others only warn when an
/// admin's file explains the change.
const SSH_CRITICAL: &[&str] = &[
    "permitrootlogin",
    "passwordauthentication",
    "kbdinteractiveauthentication",
    "permitemptypasswords",
    "authenticationmethods",
    "pubkeyauthentication",
];

fn ssh() -> Finding {
    const ID: &str = "ssh";
    if !sys::exists("sshd") {
        return Finding::skip(ID, "openssh-server not installed");
    }
    let Some(conf) = read(SSHD_CONF) else {
        return Finding::fail(ID, format!("{SSHD_CONF} missing"));
    };
    let Some(effective) = run("sshd", &["-T"]) else {
        return Finding::fail(ID, "sshd -T failed: the SSH configuration has errors");
    };
    let eff = parse_sshd_t(&effective);
    let mut fail = Vec::new();
    let mut warn = Vec::new();
    for (key, want) in parse_sshd_conf(&conf) {
        let Some(got) = eff.get(&key) else { continue };
        let ok = match key.as_str() {
            // Several values, each must be present.
            "allowgroups" | "allowusers" | "denygroups" | "denyusers" | "hostkey" => want
                .split_whitespace()
                .all(|w| got.iter().any(|g| g.eq_ignore_ascii_case(w))),
            _ => got.len() == 1 && got[0].eq_ignore_ascii_case(&want),
        };
        if ok {
            continue;
        }
        let line = format!("{key}: {} (SedirOS: {want})", got.join(" "));
        match sshd_setter(&key) {
            Some(f) if !SSH_CRITICAL.contains(&key.as_str()) => {
                warn.push(format!("{line}, set in {f}"))
            }
            Some(f) => fail.push(format!("{line}, set in {f}")),
            None => fail.push(line),
        }
    }
    if !fail.is_empty() {
        return Finding::fail(ID, "SSH hardening weakened")
            .with(fail.into_iter().chain(warn).collect());
    }
    // sedir-ssh-enroll's temporary window; sshd -T (no Match context) does
    // not show it.
    if fs::read_dir("/run/sedir")
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("ssh-enroll"))
        })
        .unwrap_or(false)
    {
        warn.insert(
            0,
            "password login window of sedir-ssh-enroll is open (closes itself)".to_string(),
        );
    }
    if !warn.is_empty() {
        return Finding::warn(ID, warn[0].clone()).with(warn[1..].to_vec());
    }
    Finding::pass(ID, "key-only, no root login, no forwarding, modern ciphers")
}

/// Directives of an sshd_config file before any Match block, keys lowercase.
fn parse_sshd_conf(text: &str) -> Vec<(String, String)> {
    conf_lines(text)
        .take_while(|l| !l.to_ascii_lowercase().starts_with("match "))
        .filter_map(|l| {
            let (k, v) = l.split_once(char::is_whitespace)?;
            Some((k.to_ascii_lowercase(), squash(v)))
        })
        .collect()
}

/// `sshd -T` output: "key value" per line, keys may repeat.
fn parse_sshd_t(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in text.lines() {
        if let Some((k, v)) = l.split_once(' ') {
            m.entry(k.to_string())
                .or_default()
                .push(v.trim().to_string());
        }
    }
    m
}

/// The file that sets an sshd key first: sshd uses the first value it reads,
/// and sshd_config includes sshd_config.d/*.conf in name order at its top.
fn sshd_setter(key: &str) -> Option<String> {
    let mut files = sys::dropins("/etc/ssh/sshd_config.d", ".conf");
    files.push("/etc/ssh/sshd_config".to_string());
    files.into_iter().find(|f| {
        f != SSHD_CONF && read(f).is_some_and(|t| parse_sshd_conf(&t).iter().any(|(k, _)| k == key))
    })
}

// --- firewall -----------------------------------------------------------------------

fn firewall() -> Finding {
    const ID: &str = "firewall";
    let Some(out) = run("ufw", &["status", "verbose"]) else {
        return Finding::fail(ID, "ufw not available");
    };
    let fw = parse_ufw(&out);
    if !fw.active {
        return Finding::fail(ID, "ufw is inactive: incoming connections are not filtered");
    }
    if !matches!(fw.incoming.as_str(), "deny" | "reject") {
        return Finding::fail(
            ID,
            format!("default policy for incoming is '{}'", fw.incoming),
        );
    }
    let mut details = Vec::new();
    if !fw.open_to_all.is_empty() {
        details.push(format!("open to everyone: {}", fw.open_to_all.join(", ")));
    }
    if !fw.limited.is_empty() {
        details.push(format!("rate-limited: {}", fw.limited.join(", ")));
    }
    if !fw.restricted.is_empty() {
        details.push(format!(
            "open to some addresses: {}",
            fw.restricted.join(", ")
        ));
    }
    let summary = format!(
        "active, incoming {} by default, outgoing {}",
        fw.incoming, fw.outgoing
    );
    if !fw.limited.iter().any(|p| p.starts_with("22/tcp"))
        && fw.open_to_all.iter().any(|p| p.starts_with("22/tcp"))
    {
        return Finding::warn(ID, format!("{summary}; SSH is not rate-limited")).with(details);
    }
    Finding::pass(ID, summary).with(details)
}

#[derive(Debug, Default, PartialEq)]
struct Ufw {
    active: bool,
    incoming: String,
    outgoing: String,
    open_to_all: Vec<String>,
    limited: Vec<String>,
    restricted: Vec<String>,
}

fn parse_ufw(text: &str) -> Ufw {
    let mut u = Ufw {
        active: text.lines().any(|l| l.trim() == "Status: active"),
        ..Default::default()
    };
    if let Some(d) = text.lines().find_map(|l| l.strip_prefix("Default: ")) {
        // "deny (incoming), allow (outgoing), deny (routed)"
        for part in d.split(',') {
            let mut w = part.split_whitespace();
            let (Some(policy), Some(dir)) = (w.next(), w.next()) else {
                continue;
            };
            match dir {
                "(incoming)" => u.incoming = policy.to_string(),
                "(outgoing)" => u.outgoing = policy.to_string(),
                _ => {}
            }
        }
    }
    let rules = text.lines().skip_while(|l| !l.starts_with("--")).skip(1);
    for l in rules {
        let rule = l.split('#').next().unwrap_or("").trim();
        if rule.is_empty() {
            continue;
        }
        // "80/tcp   ALLOW IN   Anywhere" / "22/tcp (v6)  LIMIT IN  Anywhere (v6)"
        let cols: Vec<&str> = rule
            .split("  ")
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        if cols.len() < 3 || cols[0].ends_with("(v6)") {
            continue;
        }
        let (port, action, from) = (cols[0], cols[1], cols[2]);
        if action.starts_with("LIMIT") {
            u.limited.push(port.to_string());
        } else if action.starts_with("ALLOW") {
            if from.starts_with("Anywhere") {
                u.open_to_all.push(port.to_string());
            } else {
                u.restricted.push(format!("{port} from {from}"));
            }
        }
    }
    u
}

fn netfilter() -> Finding {
    const ID: &str = "netfilter";
    match run("nft", &["list", "table", "ip", "sedir"]) {
        Some(t) if t.contains("hdrlength") => {
            Finding::pass(ID, "IPv4 TCP packets with IP options dropped")
        }
        Some(_) => Finding::fail(ID, "table ip sedir exists but has no IP options rule"),
        None => Finding::fail(ID, "table ip sedir missing (sedir-netfilter.service)"),
    }
}

fn listeners() -> Finding {
    const ID: &str = "listeners";
    let Some(out) = run("ss", &["-H", "-tulnp"]) else {
        return Finding::skip(ID, "ss not available");
    };
    let exposed = parse_ss(&out);
    if exposed.is_empty() {
        return Finding::info(ID, "nothing listens on the network");
    }
    Finding::info(
        ID,
        format!("listening on the network: {}", exposed.join(", ")),
    )
}

/// Non-loopback listeners as "22/tcp sshd", sorted and unique.
fn parse_ss(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 5 {
                return None;
            }
            let (proto, local) = (f[0], f[4]);
            let (addr, port) = local.rsplit_once(':')?;
            let host = addr.trim_start_matches('[').trim_end_matches(']');
            if host.starts_with("127.")
                || host == "::1"
                || host.starts_with("::ffff:127.")
                || addr.ends_with("%lo")
            {
                return None;
            }
            let process = f
                .get(6)
                .and_then(|p| p.split('"').nth(1))
                .map(|n| format!(" {n}"))
                .unwrap_or_default();
            Some(format!("{port}/{proto}{process}"))
        })
        .collect();
    v.sort_by_key(|s| {
        s.split('/')
            .next()
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0)
    });
    v.dedup();
    v
}

// --- MAC, SUID ----------------------------------------------------------------------

fn apparmor() -> Finding {
    const ID: &str = "apparmor";
    let enabled = read("/sys/module/apparmor/parameters/enabled").is_some_and(|s| s.trim() == "Y");
    if !enabled {
        return Finding::fail(ID, "AppArmor is not enabled");
    }
    let profiles = read("/sys/kernel/security/apparmor/profiles").unwrap_or_default();
    let enforce = profiles
        .lines()
        .filter(|l| l.ends_with("(enforce)"))
        .count();
    let complain = profiles
        .lines()
        .filter(|l| l.ends_with("(complain)"))
        .count();
    if enforce == 0 {
        return Finding::fail(ID, "AppArmor enabled but no profile enforced");
    }
    Finding::pass(
        ID,
        format!("enabled, {enforce} profiles enforced, {complain} in complain mode"),
    )
}

fn suid() -> Finding {
    const ID: &str = "suid";
    let allowed: Vec<String> = read(SUID_ALLOWLIST)
        .map(|t| conf_lines(&t).map(str::to_string).collect())
        .unwrap_or_default();
    if allowed.is_empty() {
        return Finding::fail(ID, format!("{SUID_ALLOWLIST} missing or empty"));
    }
    let Ok(root_dev) = fs::metadata("/").map(|m| m.dev()) else {
        return Finding::skip(ID, "cannot read /");
    };
    let mut found = Vec::new();
    walk(Path::new("/"), root_dev, &mut found);
    let extra: Vec<String> = found.into_iter().filter(|f| !allowed.contains(f)).collect();
    if extra.is_empty() {
        Finding::pass(ID, "setuid/setgid only on the allowlisted programs")
    } else {
        Finding::fail(
            ID,
            format!("{} setuid/setgid file(s) not on the allowlist", extra.len()),
        )
        .with(extra)
    }
}

/// setuid/setgid regular files on the root file system (like find -xdev).
fn walk(dir: &Path, dev: u64, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        let Ok(m) = fs::symlink_metadata(&path) else {
            continue;
        };
        if m.dev() != dev {
            continue;
        }
        if m.is_dir() {
            walk(&path, dev, out);
        } else if m.is_file() && m.permissions().mode() & 0o6000 != 0 {
            out.push(path.to_string_lossy().into_owned());
        }
    }
}

// --- accounts, sudo, PAM --------------------------------------------------------------

fn accounts(ctx: &Ctx) -> Finding {
    const ID: &str = "accounts";
    let Some(shadow) = read("/etc/shadow") else {
        return Finding::skip(ID, "cannot read /etc/shadow");
    };
    let passwd = read("/etc/passwd").unwrap_or_default();
    let mut fail = Vec::new();
    for l in shadow.lines() {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 2 {
            continue;
        }
        if f[1].is_empty() {
            fail.push(format!("{}: empty password (login without password)", f[0]));
        } else if f[0] == "root" && !f[1].starts_with('!') && !f[1].starts_with('*') {
            fail.push("root has a password (SedirOS locks it; use sudo)".to_string());
        }
    }
    for l in passwd.lines() {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() > 2 && f[2] == "0" && f[0] != "root" {
            fail.push(format!("{}: user id 0 (a second root)", f[0]));
        }
    }
    if !fail.is_empty() {
        return Finding::fail(ID, "unsafe accounts").with(fail);
    }
    let nopasswd = nopasswd_rules();
    if !nopasswd.is_empty() {
        if ctx.kind == Kind::Vm {
            return Finding::pass(
                ID,
                "root locked, no empty passwords (VM image: passwordless sudo)",
            );
        }
        return Finding::warn(ID, "sudo without password allowed").with(nopasswd);
    }
    Finding::pass(ID, "root locked, no empty passwords, no second root")
}

fn nopasswd_rules() -> Vec<String> {
    let mut files = vec!["/etc/sudoers".to_string()];
    files.extend(sys::dropins("/etc/sudoers.d", ""));
    files
        .into_iter()
        .filter(|f| read(f).is_some_and(|t| conf_lines(&t).any(|l| l.contains("NOPASSWD"))))
        .map(|f| format!("NOPASSWD in {f}"))
        .collect()
}

fn sudo() -> Finding {
    const ID: &str = "sudo";
    let Some(policy) = read("/etc/sudoers.d/sedir") else {
        return Finding::fail(ID, "/etc/sudoers.d/sedir missing");
    };
    let missing: Vec<&str> = ["use_pty", "logfile=", "timestamp_timeout=5"]
        .into_iter()
        .filter(|d| !policy.contains(d))
        .collect();
    if !missing.is_empty() {
        return Finding::fail(ID, format!("policy incomplete: {}", missing.join(", ")));
    }
    match run_full("visudo", &["-c", "-q"]) {
        Some((true, _, _)) => {
            Finding::pass(ID, "pty, audit log /var/log/sudo.log, 5 min timestamp")
        }
        _ => Finding::fail(ID, "visudo -c reports errors in the sudoers files"),
    }
}

fn pam() -> Finding {
    const ID: &str = "pam";
    let auth = read("/etc/pam.d/common-auth").unwrap_or_default();
    let password = read("/etc/pam.d/common-password").unwrap_or_default();
    let order: Vec<&str> = conf_lines(&auth)
        .filter_map(|l| {
            if l.contains("pam_faillock.so") && l.contains("preauth") {
                Some("preauth")
            } else if l.contains("pam_faillock.so") && l.contains("authfail") {
                Some("authfail")
            } else if l.contains("pam_unix.so") {
                Some("unix")
            } else {
                None
            }
        })
        .collect();
    let mut bad = Vec::new();
    if order != ["preauth", "unix", "authfail"] {
        bad.push(format!(
            "common-auth: pam_faillock must wrap pam_unix (found: {})",
            order.join(", ")
        ));
    }
    if !conf_lines(&password).any(|l| l.starts_with("password") && l.contains("pam_pwquality.so")) {
        bad.push("common-password: pam_pwquality missing".to_string());
    }
    if bad.is_empty() {
        Finding::pass(ID, "lockout after failed logins, password quality rules")
    } else {
        Finding::fail(ID, "login protection incomplete").with(bad)
    }
}

// --- kernel modules -------------------------------------------------------------------

fn modules() -> Finding {
    const ID: &str = "modules";
    let Some(conf) = read(BLACKLIST) else {
        return Finding::fail(ID, format!("{BLACKLIST} missing"));
    };
    let blocked: Vec<&str> = conf_lines(&conf)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 3 && f[0] == "install").then(|| f[1])
        })
        .collect();
    let loaded: Vec<String> = read("/proc/modules")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(|m| m.replace('_', "-")))
        .collect();
    let mut bad = Vec::new();
    for m in &blocked {
        if loaded.iter().any(|l| l == &m.replace('_', "-")) {
            bad.push(format!("{m}: loaded"));
            continue;
        }
        let out = run_full("modprobe", &["-n", "-v", m])
            .map(|(_, o, e)| o + &e)
            .unwrap_or_default();
        if !out.contains("/bin/false") && !out.contains("/bin/true") && !out.contains("not found") {
            bad.push(format!("{m}: loadable"));
        }
    }
    if bad.is_empty() {
        Finding::pass(
            ID,
            format!("{} unneeded kernel modules blocked", blocked.len()),
        )
    } else {
        Finding::fail(ID, "blocked kernel modules can be loaded").with(bad)
    }
}

// --- updates ----------------------------------------------------------------------------

fn updates() -> Finding {
    const ID: &str = "updates";
    let cfg = run("apt-config", &["dump"]).unwrap_or_default();
    let mut fail = Vec::new();
    let mut warn = Vec::new();
    if !cfg.contains("APT::Periodic::Unattended-Upgrade \"1\";") {
        fail.push("automatic upgrades disabled (APT::Periodic::Unattended-Upgrade)".to_string());
    }
    if !cfg.contains("\"origin=Debian,label=Debian-Security\";") {
        fail.push("Debian security updates are not installed automatically".to_string());
    }
    if !cfg.contains("\"origin=SedirOS,site=sediros.github.io\";") {
        warn.push("updates of SedirOS's packages are not installed automatically".to_string());
    }
    for t in ["apt-daily.timer", "apt-daily-upgrade.timer"] {
        if run("systemctl", &["is-enabled", t])
            .map(|s| s.trim().to_string())
            .as_deref()
            != Some("enabled")
        {
            fail.push(format!("{t} is not enabled"));
        }
    }
    let mut summary = "Debian security and SedirOS updates installed automatically".to_string();
    if let Some(days) = age_days("/var/lib/apt/periodic/unattended-upgrades-stamp") {
        if days > 3 {
            warn.push(format!("last automatic update run {days} days ago"));
        } else {
            summary.push_str(&format!(
                ", last run {}",
                if days == 0 {
                    "today".into()
                } else {
                    format!("{days} day(s) ago")
                }
            ));
        }
    }
    if Path::new("/run/reboot-required").exists() {
        let pkgs = read("/run/reboot-required.pkgs")
            .map(|t| squash(&t))
            .unwrap_or_default();
        warn.push(
            format!(
                "reboot required to finish updates {}",
                if pkgs.is_empty() {
                    String::new()
                } else {
                    format!("({pkgs})")
                }
            )
            .trim()
            .to_string(),
        );
    }
    if !fail.is_empty() {
        return Finding::fail(ID, "automatic security updates not working")
            .with(fail.into_iter().chain(warn).collect());
    }
    if !warn.is_empty() {
        return Finding::warn(ID, warn[0].clone()).with(warn[1..].to_vec());
    }
    Finding::pass(ID, summary)
}

fn age_days(path: &str) -> Option<u64> {
    let m = fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(m).ok()?.as_secs() / 86400)
}

fn apt_source() -> Finding {
    const ID: &str = "apt-source";
    let Some(src) = read(APT_SOURCE) else {
        return Finding::warn(
            ID,
            "SedirOS apt repository not set up: SedirOS packages get no updates",
        )
        .with(vec![format!("set it up once: {REPO_PAGE}")]);
    };
    let mut bad = Vec::new();
    if !conf_lines(&src).any(|l| l == format!("Signed-By: {KEYRING}"))
        || !Path::new(KEYRING).is_file()
    {
        bad.push(format!("not bound to {KEYRING} (Signed-By)"));
    }
    if !conf_lines(&src).any(|l| l.starts_with("URIs: https://")) {
        bad.push("not fetched over https".to_string());
    }
    let pin = read(APT_PIN).unwrap_or_default();
    if !pin_ok(&pin) {
        bad.push(format!(
            "{APT_PIN}: pin to sedir-* packages missing or changed"
        ));
    }
    if bad.is_empty() {
        Finding::pass(
            ID,
            "SedirOS repository signed by its key, limited to sedir-* packages",
        )
    } else {
        Finding::fail(ID, "SedirOS repository not protected as shipped").with(bad)
    }
}

/// sedir-* allowed (priority > 0) and everything else refused (< 0) from the repository.
fn pin_ok(text: &str) -> bool {
    let mut pkg = "";
    let mut prio = BTreeMap::new();
    for l in conf_lines(text) {
        if let Some(p) = l.strip_prefix("Package:") {
            pkg = p.trim();
        } else if let Some(p) = l.strip_prefix("Pin-Priority:") {
            prio.insert(pkg.to_string(), p.trim().parse::<i32>().unwrap_or(0));
        }
    }
    prio.get("sedir-*").is_some_and(|p| *p > 0) && prio.get("*").is_some_and(|p| *p < 0)
}

// --- boot -------------------------------------------------------------------------------

fn kernel_cmdline(ctx: &Ctx) -> Finding {
    const ID: &str = "kernel";
    let cmdline = read("/proc/cmdline").unwrap_or_default();
    let want: Vec<String> = read(GRUB_CFG)
        .and_then(|t| {
            conf_lines(&t).find_map(|l| {
                l.strip_prefix("SEDIR_KERNEL_CMDLINE=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .map(|v| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if want.is_empty() {
        return Finding::skip(ID, "no SedirOS boot settings on this system");
    }
    let have: Vec<&str> = cmdline.split_whitespace().collect();
    let missing: Vec<String> = want
        .into_iter()
        // vsyscall= only exists on x86; SedirOS drops it elsewhere.
        .filter(|w| std::env::consts::ARCH == "x86_64" || !w.starts_with("vsyscall="))
        .filter(|w| !have.contains(&w.as_str()))
        .collect();
    if missing.is_empty() {
        return Finding::pass(ID, "hardening boot parameters active");
    }
    let hint = if ctx.kind == Kind::Installed {
        "missing; run sudo update-grub and reboot"
    } else {
        "missing from the kernel command line"
    };
    Finding::warn(ID, format!("{} boot parameter(s) {hint}", missing.len())).with(missing)
}

fn encryption(ctx: &Ctx) -> Finding {
    const ID: &str = "encryption";
    let src = run("findmnt", &["-n", "-o", "SOURCE", "/"]).unwrap_or_default();
    let src = src.trim();
    let types = run("lsblk", &["-n", "-s", "-o", "TYPE", src]).unwrap_or_default();
    if types.lines().any(|t| t.trim() == "crypt") {
        return Finding::pass(ID, "root file system on LUKS");
    }
    match ctx.kind {
        Kind::Installed => Finding::warn(ID, "root file system is not encrypted"),
        _ => Finding::info(
            ID,
            "root file system not encrypted (VM image or live system)",
        ),
    }
}

fn secure_boot(ctx: &Ctx) -> Finding {
    const ID: &str = "secure-boot";
    if !Path::new("/sys/firmware/efi").exists() {
        return Finding::info(ID, "not booted with UEFI (BIOS or direct kernel boot)");
    }
    let sb = fs::read("/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c")
        .ok()
        .and_then(|v| v.last().copied());
    let lockdown = read("/sys/kernel/security/lockdown")
        .and_then(|l| {
            l.split('[')
                .nth(1)
                .and_then(|r| r.split(']').next())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".into());
    match sb {
        Some(1) => Finding::pass(ID, format!("enabled, kernel lockdown: {lockdown}")),
        _ if ctx.kind == Kind::Installed => Finding::info(
            ID,
            "Secure Boot is off in the firmware (SedirOS supports it)",
        ),
        _ => Finding::info(ID, "Secure Boot is off"),
    }
}

fn core_dumps() -> Finding {
    const ID: &str = "core-dumps";
    let limits = read("/proc/self/limits").unwrap_or_default();
    let hard = limits
        .lines()
        .find(|l| l.starts_with("Max core file size"))
        .and_then(|l| l.split_whitespace().nth(5))
        .unwrap_or("?")
        .to_string();
    if hard == "0" {
        Finding::pass(ID, "disabled for login sessions (hard limit 0)")
    } else {
        Finding::warn(ID, format!("core dumps allowed (hard limit {hard})"))
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpkg_verify() {
        let (c, o) = parse_dpkg_verify(
            "??5?????? c /etc/ssh/sshd_config.d/99-sedir.conf\n??5??????   /usr/sbin/sedir-suid-strip\nmissing     /usr/lib/x\n",
        );
        assert_eq!(c, ["/etc/ssh/sshd_config.d/99-sedir.conf"]);
        assert_eq!(
            o,
            [
                "/usr/sbin/sedir-suid-strip (changed)",
                "/usr/lib/x (missing)"
            ]
        );
    }

    #[test]
    fn sysctl_file() {
        let v = parse_sysctl("# x\nkernel.kptr_restrict = 2\n-net.x.y=1 2  3\n");
        assert_eq!(v[0], ("kernel.kptr_restrict".into(), "2".into(), false));
        assert_eq!(v[1], ("net.x.y".into(), "1 2 3".into(), true));
    }

    #[test]
    fn sshd_files() {
        let c = parse_sshd_conf(
            "PermitRootLogin no\nAllowGroups sudo sedir-ssh\nMatch User x\nPermitRootLogin yes\n",
        );
        assert_eq!(
            c,
            [
                ("permitrootlogin".into(), "no".into()),
                ("allowgroups".into(), "sudo sedir-ssh".into())
            ]
        );
        let t = parse_sshd_t("permitrootlogin no\nallowgroups sudo\nallowgroups sedir-ssh\n");
        assert_eq!(t["allowgroups"], ["sudo", "sedir-ssh"]);
    }

    #[test]
    fn ufw_status() {
        let u = parse_ufw(
            "Status: active\nLogging: on (low)\nDefault: deny (incoming), allow (outgoing), deny (routed)\nNew profiles: skip\n\n\
             To                         Action      From\n--                         ------      ----\n\
             22/tcp                     ALLOW IN    10.0.2.2                   # sedir-launch\n\
             22/tcp                     LIMIT IN    Anywhere                   # sedir-ssh\n\
             80/tcp                     ALLOW IN    Anywhere\n\
             22/tcp (v6)                LIMIT IN    Anywhere (v6)              # sedir-ssh\n",
        );
        assert!(u.active);
        assert_eq!(
            (u.incoming.as_str(), u.outgoing.as_str()),
            ("deny", "allow")
        );
        assert_eq!(u.limited, ["22/tcp"]);
        assert_eq!(u.open_to_all, ["80/tcp"]);
        assert_eq!(u.restricted, ["22/tcp from 10.0.2.2"]);
        assert!(!parse_ufw("Status: inactive\n").active);
    }

    #[test]
    fn ss_listeners() {
        let v = parse_ss(
            "tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))\n\
             tcp LISTEN 0 128 [::]:22 [::]:* users:((\"sshd\",pid=1,fd=4))\n\
             tcp LISTEN 0 5 127.0.0.1:631 0.0.0.0:* users:((\"cupsd\",pid=2,fd=5))\n\
             udp UNCONN 0 0 10.0.2.15%enp0s2:68 0.0.0.0:* users:((\"systemd-network\",pid=3,fd=6))\n\
             tcp LISTEN 0 511 *:80 *:* users:((\"nginx\",pid=4,fd=6))\n",
        );
        assert_eq!(v, ["22/tcp sshd", "68/udp systemd-network", "80/tcp nginx"]);
    }

    #[test]
    fn apt_pin() {
        assert!(pin_ok("Package: sedir-*\nPin: origin \"x\"\nPin-Priority: 500\n\nPackage: *\nPin: origin \"x\"\nPin-Priority: -1\n"));
        assert!(!pin_ok("Package: *\nPin-Priority: 500\n"));
    }
}
