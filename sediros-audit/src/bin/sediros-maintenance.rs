//! sediros-maintenance: reboot in a maintenance window when an update needs it.
//!
//! Security updates install themselves, but a fixed kernel (or libc,
//! systemd, OpenSSL in long-running services) only takes effect after a
//! reboot. /etc/sediros/maintenance.conf chooses what happens when one is
//! required (/run/reboot-required):
//!   off   never reboot (default); the login notice and the audit say so
//!   ask   reboot in the window once an admin approved it
//!   auto  reboot in the window, with a notification before and after
//! A window is one hour, in the server's local time.
//!
//! In every mode: no reboot while someone is logged in (moved to the next
//! window), `skip` skips one window, and an encrypted root refuses unless
//! the disk can be unlocked over SSH at boot (`sediros unlock`), since the
//! server would otherwise wait for its passphrase, unreachable.
//!
//! Notifications use the daily audit's channels (MAILTO, WEBHOOK in
//! /etc/sediros/audit.conf), once per window and kind.
//!
//! sediros-maintenance.timer runs `run` every 15 minutes.

#[allow(dead_code)]
#[path = "../record.rs"]
mod record;
#[allow(dead_code)]
#[path = "../report.rs"]
mod report;
#[allow(dead_code)]
#[path = "../sys.rs"]
mod sys;

use std::fs;
use std::path::Path;
use std::process::ExitCode;

const CONF: &str = "/etc/sediros/maintenance.conf";
const STATE: &str = "/var/lib/sediros/maintenance";
const REBOOT_REQUIRED: &str = "/run/reboot-required";
const REBOOT_PKGS: &str = "/run/reboot-required.pkgs";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
const DROPBEAR_KEYS: &str = "/etc/dropbear/initramfs/authorized_keys";
const DROPBEAR_HOOK: &str = "/usr/share/initramfs-tools/hooks/dropbear";
const WINDOW_MINUTES: u32 = 60;

const USAGE: &str = "\
Usage: sediros-maintenance [status]
       sudo sediros-maintenance approve   (mode ask: reboot in the next window)
       sudo sediros-maintenance skip      (do not reboot in the next window)
       sudo sediros-maintenance run       (sediros-maintenance.timer)

Reboots in a maintenance window when an update needs it. Settings in
/etc/sediros/maintenance.conf:
  MODE=off|ask|auto    off: never (default); ask: after approval; auto: always
  WINDOW=Sun 04:00     day(s) and time, one hour, server's local time:
                       \"Sun 04:00\", \"Mon,Thu 03:30\", \"daily 04:00\"
No reboot while someone is logged in. An encrypted root disk needs SSH
unlock at boot (set up by the installer): unlock it from your machine
with `sediros unlock <server>` during the window.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Off,
    Ask,
    Auto,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Ask => "ask",
            Mode::Auto => "auto",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Window {
    /// ISO weekdays, 1 = Monday.
    days: Vec<u32>,
    minute_of_day: u32,
    text: String,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("status");
    if matches!(cmd, "-h" | "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if matches!(cmd, "-V" | "--version") {
        println!("sediros-maintenance {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.len() > 1 || !matches!(cmd, "status" | "approve" | "skip" | "run") {
        eprintln!("sediros-maintenance: unknown arguments\n\n{USAGE}");
        return ExitCode::from(2);
    }
    if cmd != "status" && sys::euid() != Some(0) {
        eprintln!("sediros-maintenance: needs root: sudo sediros-maintenance {cmd}");
        return ExitCode::from(2);
    }
    let (mode, window) = match load_conf() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sediros-maintenance: {CONF}: {e}");
            return ExitCode::from(2);
        }
    };
    let result = match cmd {
        "approve" => approve(mode, &window),
        "skip" => skip(&window),
        "run" => {
            run(mode, &window);
            Ok(())
        }
        _ => {
            status(mode, &window);
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sediros-maintenance: {e}");
            ExitCode::from(1)
        }
    }
}

// --- settings -------------------------------------------------------------------

fn load_conf() -> Result<(Mode, Window), String> {
    let kv = sys::read(CONF).map(|t| parse_conf(&t)).unwrap_or_default();
    let mode = match kv.get("MODE").map(String::as_str).unwrap_or("off") {
        "off" | "" => Mode::Off,
        "ask" => Mode::Ask,
        "auto" => Mode::Auto,
        other => return Err(format!("MODE={other}: use off, ask or auto")),
    };
    let window = parse_window(kv.get("WINDOW").map(String::as_str).unwrap_or("Sun 04:00"))?;
    Ok((mode, window))
}

/// KEY=value lines; # starts a comment.
fn parse_conf(text: &str) -> std::collections::BTreeMap<String, String> {
    sys::conf_lines(text)
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

fn parse_window(s: &str) -> Result<Window, String> {
    let bad =
        || format!("WINDOW={s}: use e.g. \"Sun 04:00\", \"Mon,Thu 03:30\" or \"daily 04:00\"");
    let (days, time) = s.trim().split_once(' ').ok_or_else(bad)?;
    let days: Vec<u32> = if matches!(days, "daily" | "*") {
        (1..=7).collect()
    } else {
        days.split(',')
            .map(|d| {
                ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(d.trim()))
                    .map(|i| i as u32 + 1)
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(bad)?
    };
    let (h, m) = time.trim().split_once(':').ok_or_else(bad)?;
    let (h, m): (u32, u32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 || days.is_empty() {
        return Err(bad());
    }
    Ok(Window {
        days,
        minute_of_day: h * 60 + m,
        text: s.trim().to_string(),
    })
}

// --- time -----------------------------------------------------------------------

/// Local time now: (ISO weekday, minute of the day, date "YYYY-MM-DD").
fn now_local() -> Option<(u32, u32, String)> {
    let out = sys::run("date", &["+%u %H %M %F"])?;
    let f: Vec<&str> = out.split_whitespace().collect();
    let [wd, h, m, date] = f[..] else {
        return None;
    };
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    Some((wd.parse().ok()?, h * 60 + m, date.to_string()))
}

/// The window we are in now, as "YYYY-MM-DD HH:MM" (its start), if any.
/// A window that started before midnight continues into the next day.
fn current_window(w: &Window, weekday: u32, minute: u32, date: &str) -> Option<String> {
    let start = w.minute_of_day;
    if w.days.contains(&weekday) && minute >= start && minute < start + WINDOW_MINUTES {
        return Some(format!("{date} {}", hhmm(start)));
    }
    let yesterday = if weekday == 1 { 7 } else { weekday - 1 };
    let end = start + WINDOW_MINUTES;
    if end > 24 * 60 && w.days.contains(&yesterday) && minute < end - 24 * 60 {
        let d = sys::run("date", &["-d", &format!("{date} -1 day"), "+%F"])?;
        return Some(format!("{} {}", d.trim(), hhmm(start)));
    }
    None
}

/// Start of the next window that has not begun yet, "YYYY-MM-DD HH:MM".
fn next_window(w: &Window, weekday: u32, minute: u32, date: &str) -> Option<String> {
    let offset = next_window_offset(w, weekday, minute)?;
    let d = sys::run("date", &["-d", &format!("{date} +{offset} day"), "+%a %F"])?;
    Some(format!("{} {}", d.trim(), hhmm(w.minute_of_day)))
}

/// Days from today to the next window start that is still ahead.
fn next_window_offset(w: &Window, weekday: u32, minute: u32) -> Option<u32> {
    (0..=7).find(|&off| {
        let wd = (weekday - 1 + off) % 7 + 1;
        w.days.contains(&wd) && (off > 0 || w.minute_of_day > minute)
    })
}

fn hhmm(m: u32) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}

// --- state ----------------------------------------------------------------------

fn state(name: &str) -> std::path::PathBuf {
    Path::new(STATE).join(name)
}

fn write_state(name: &str, text: &str) -> Result<(), String> {
    fs::create_dir_all(STATE).map_err(|e| format!("{STATE}: {e}"))?;
    let path = state(name);
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn boot_id() -> String {
    sys::read(BOOT_ID).unwrap_or_default().trim().to_string()
}

/// Approved during this boot (an approval does not outlive the reboot it was for).
fn approved() -> bool {
    sys::read(state("approved")).is_some_and(|t| t.lines().next() == Some(boot_id().as_str()))
}

fn reboot_required() -> Option<(String, String)> {
    let md = fs::metadata(REBOOT_REQUIRED).ok()?;
    let since = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| sys::run("date", &["-d", &format!("@{}", d.as_secs()), "+%F %H:%M"]))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let pkgs = sys::read(REBOOT_PKGS)
        .map(|t| {
            let mut v: Vec<&str> = t.split_whitespace().collect();
            v.sort();
            v.dedup();
            v.join(" ")
        })
        .unwrap_or_default();
    Some((since, pkgs))
}

/// None: the root file system is not encrypted. Some(true): it is, and can
/// be unlocked over SSH at boot.
fn encrypted_unlockable() -> Option<bool> {
    let src = sys::run("findmnt", &["-n", "-o", "SOURCE", "/"])?;
    let types = sys::run("lsblk", &["-n", "-s", "-o", "TYPE", src.trim()]).unwrap_or_default();
    if !types.lines().any(|t| t.trim() == "crypt") {
        return None;
    }
    let keys = sys::read(DROPBEAR_KEYS).is_some_and(|k| k.lines().any(|l| !l.trim().is_empty()));
    Some(keys && Path::new(DROPBEAR_HOOK).is_file())
}

fn sessions() -> Vec<String> {
    sys::run("who", &[])
        .map(|o| {
            o.lines()
                .filter_map(|l| {
                    let mut f = l.split_whitespace();
                    Some(format!("{} on {}", f.next()?, f.next()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn host() -> String {
    sys::read("/etc/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_else(|| "server".into())
}

/// Once per window and kind: through the audit's channels, and the journal.
fn notify_once(window: &str, kind: &str, subject: &str, body: &str) {
    let key = format!("{window} {kind}");
    let sent = sys::read(state("notified")).unwrap_or_default();
    println!("{subject}: {body}");
    if sent.lines().any(|l| l == key) {
        return;
    }
    let conf = record::conf();
    for e in record::notify(&conf, &format!("{subject} on {}", host()), body) {
        eprintln!("notification: {e}");
    }
    // Only this window's keys are kept.
    let keep: String = sent
        .lines()
        .filter(|l| l.starts_with(window))
        .chain(std::iter::once(key.as_str()))
        .map(|l| format!("{l}\n"))
        .collect();
    let _ = write_state("notified", &keep);
}

// --- commands -------------------------------------------------------------------

fn status(mode: Mode, w: &Window) {
    let mode_text = match mode {
        Mode::Off => "off: never reboots by itself",
        Mode::Ask => "ask: reboots in the window after `sudo sediros-maintenance approve`",
        Mode::Auto => "auto: reboots in the window when an update needs it",
    };
    println!("Mode:     {mode_text} ({CONF})");
    let next = now_local()
        .and_then(|(wd, m, d)| next_window(w, wd, m, &d))
        .unwrap_or_else(|| "?".into());
    println!("Window:   {}, one hour, local time (next: {next})", w.text);
    match reboot_required() {
        None => println!("Reboot:   not required"),
        Some((since, pkgs)) => {
            println!(
                "Reboot:   required since {since}{}",
                if pkgs.is_empty() {
                    String::new()
                } else {
                    format!(" ({pkgs})")
                }
            );
            if mode == Mode::Ask {
                println!(
                    "Approved: {}",
                    if approved() {
                        "yes, reboots in the next window"
                    } else {
                        "no: sudo sediros-maintenance approve"
                    }
                );
            }
        }
    }
    if sys::read(state("skip")).is_some() {
        println!("Skip:     the next window is skipped");
    }
    match encrypted_unlockable() {
        None => println!("Disk:     not encrypted"),
        Some(true) => println!("Disk:     encrypted, SSH unlock set up: during the window run `sediros unlock {}` from your machine", host()),
        Some(false) if mode != Mode::Off => println!("Disk:     encrypted, no SSH unlock: {} refuses to reboot (the server would wait for its passphrase)", mode.name()),
        Some(false) => println!("Disk:     encrypted, no SSH unlock"),
    }
}

fn approve(mode: Mode, w: &Window) -> Result<(), String> {
    if mode != Mode::Ask {
        return Err(format!(
            "MODE={} in {CONF}; approve is for MODE=ask",
            mode.name()
        ));
    }
    if reboot_required().is_none() {
        return Err("no reboot is required".into());
    }
    write_state("approved", &format!("{}\n{}\n", boot_id(), sys::now_utc()))?;
    let next = now_local()
        .and_then(|(wd, m, d)| current_window(w, wd, m, &d).or_else(|| next_window(w, wd, m, &d)));
    println!(
        "Approved: reboots in the window {}",
        next.unwrap_or_else(|| w.text.clone())
    );
    Ok(())
}

fn skip(w: &Window) -> Result<(), String> {
    let (wd, m, d) = now_local().ok_or("cannot read the time")?;
    // The window we are in, else the next one.
    let target = current_window(w, wd, m, &d)
        .or_else(|| {
            next_window(w, wd, m, &d)
                .map(|n| n.split_once(' ').map(|(_, r)| r.to_string()).unwrap_or(n))
        })
        .ok_or("no window ahead")?;
    write_state("skip", &format!("{target}\n"))?;
    println!("Skipping the window {target}");
    Ok(())
}

fn run(mode: Mode, w: &Window) {
    after_reboot();
    let Some((since, pkgs)) = reboot_required() else {
        let _ = fs::remove_file(state("approved"));
        return;
    };
    if mode == Mode::Off {
        return;
    }
    let Some((wd, m, d)) = now_local() else {
        return;
    };
    let Some(window) = current_window(w, wd, m, &d) else {
        return;
    };
    let what = if pkgs.is_empty() {
        format!("required since {since}")
    } else {
        format!("required since {since} by {pkgs}")
    };

    if let Some(skip) = sys::read(state("skip")) {
        if skip.trim() == window {
            println!("Window {window} skipped (sediros-maintenance skip)");
            return;
        }
        // A skip for a window that has passed is done.
        if skip.trim() < window.as_str() {
            let _ = fs::remove_file(state("skip"));
        }
    }
    if encrypted_unlockable() == Some(false) {
        notify_once(&window, "locked", "Maintenance reboot not possible", &format!(
            "A reboot is {what}, but the root disk is encrypted and cannot be unlocked over SSH: the server would wait for its passphrase. Reboot by hand when you can enter it."
        ));
        return;
    }
    if mode == Mode::Ask && !approved() {
        notify_once(&window, "ask", "Maintenance reboot waiting for approval", &format!(
            "A reboot is {what}. Approve it for a maintenance window ({}): sudo sediros-maintenance approve", w.text
        ));
        return;
    }
    let who = sessions();
    if !who.is_empty() {
        notify_once(
            &window,
            "busy",
            "Maintenance reboot moved to the next window",
            &format!(
                "A reboot is {what}, but someone is logged in: {}.",
                who.join(", ")
            ),
        );
        return;
    }
    let unlock = if encrypted_unlockable() == Some(true) {
        format!(
            " The disk is encrypted: unlock it with `sediros unlock {}`.",
            host()
        )
    } else {
        String::new()
    };
    notify_once(
        &window,
        "reboot",
        "Maintenance reboot now",
        &format!("Rebooting: a reboot is {what}.{unlock}"),
    );
    let _ = write_state(
        "rebooted",
        &format!("{}\n{}\n{pkgs}\n", boot_id(), sys::now_utc()),
    );
    let _ = fs::remove_file(state("approved"));
    if sys::run("systemctl", &["reboot"]).is_none() {
        eprintln!("systemctl reboot failed");
        let _ = fs::remove_file(state("rebooted"));
    }
}

/// After a maintenance reboot: report that the server is back.
fn after_reboot() {
    let Some(text) = sys::read(state("rebooted")) else {
        return;
    };
    let mut lines = text.lines();
    let (Some(old_boot), Some(when)) = (lines.next(), lines.next()) else {
        let _ = fs::remove_file(state("rebooted"));
        return;
    };
    if old_boot == boot_id() {
        return;
    }
    let kernel = sys::run("uname", &["-r"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let still = if reboot_required().is_some() {
        " A reboot is still required."
    } else {
        ""
    };
    let conf = record::conf();
    let body = format!("Back up after the maintenance reboot of {when}, kernel {kernel}.{still}");
    println!("{body}");
    for e in record::notify(
        &conf,
        &format!("Maintenance reboot done on {}", host()),
        &body,
    ) {
        eprintln!("notification: {e}");
    }
    let _ = fs::remove_file(state("rebooted"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows() {
        let w = parse_window("Sun 04:00").unwrap();
        assert_eq!((w.days.clone(), w.minute_of_day), (vec![7], 240));
        assert_eq!(parse_window("Mon,thu 03:30").unwrap().days, [1, 4]);
        assert_eq!(parse_window("daily 23:30").unwrap().days.len(), 7);
        for bad in [
            "",
            "Sun",
            "Sun 4",
            "Sun 24:00",
            "Sun 04:60",
            "Son 04:00",
            "04:00",
        ] {
            assert!(parse_window(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn in_window() {
        let w = parse_window("Sun 04:00").unwrap();
        assert_eq!(
            current_window(&w, 7, 240, "2026-11-22").as_deref(),
            Some("2026-11-22 04:00")
        );
        assert_eq!(
            current_window(&w, 7, 299, "2026-11-22").as_deref(),
            Some("2026-11-22 04:00")
        );
        assert_eq!(current_window(&w, 7, 300, "2026-11-22"), None);
        assert_eq!(current_window(&w, 6, 240, "2026-11-21"), None);
        assert_eq!(current_window(&w, 7, 239, "2026-11-22"), None);
    }

    #[test]
    fn next_windows() {
        let w = parse_window("Sun 04:00").unwrap();
        assert_eq!(next_window_offset(&w, 7, 100), Some(0)); // Sunday before 04:00
        assert_eq!(next_window_offset(&w, 7, 240), Some(7)); // in it: the next one
        assert_eq!(next_window_offset(&w, 1, 0), Some(6)); // Monday
        let d = parse_window("daily 04:00").unwrap();
        assert_eq!(next_window_offset(&d, 3, 300), Some(1));
    }

    #[test]
    fn conf_comments() {
        let kv = parse_conf("# MODE=auto\nMODE=ask # comment\nWINDOW=\"Sun 04:00\"\n");
        assert_eq!(kv.get("MODE").map(String::as_str), Some("ask"));
        assert_eq!(kv.get("WINDOW").map(String::as_str), Some("Sun 04:00"));
    }
}
