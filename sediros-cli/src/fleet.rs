//! `sediros fleet <hosts-file>`: the audit state of many SedirOS servers in
//! one table, over SSH.
//!
//! By default it reads each server's last daily audit
//! (/var/lib/sediros/audit/last.json, readable by the sudo group): no sudo,
//! no load on the servers. --live runs the audit now instead, which needs
//! passwordless sudo for sediros-audit on the servers.

use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::cli::FleetArgs;

/// A daily result older than this means the timer is not running.
const STALE: Duration = Duration::from_secs(2 * 86400);

#[derive(Debug, Default, PartialEq, Eq)]
struct Audit {
    host: String,
    time: String,
    pass: u32,
    warn: u32,
    fail: u32,
    /// Checks with status warn or fail, e.g. "firewall!" / "ssh✗".
    problems: Vec<String>,
}

#[derive(Debug)]
enum Row {
    Ok(Audit),
    NoData(String),
    Unreachable(String),
}

/// Hosts file: one ssh destination per line (host, user@host, an ssh config
/// alias or ssh://user@host:port); '#' starts a comment.
fn parse_hosts(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn valid_dest(d: &str) -> bool {
    !d.starts_with('-')
        && d.len() <= 255
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._@:/[]".contains(c))
}

// --- the few fields of sediros-audit's JSON that the table needs ------------------------

fn str_field(json: &str, key: &str) -> Option<String> {
    let (_, rest) = json.split_once(&format!("\"{key}\":\""))?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

fn num_field(json: &str, key: &str) -> Option<u32> {
    let (_, rest) = json.split_once(&format!("\"{key}\":"))?;
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn parse_audit(json: &str) -> Option<Audit> {
    let (_, summary) = json.split_once("\"summary\":{")?;
    let summary = summary.split('}').next()?;
    let mut a = Audit {
        host: str_field(json, "host")?,
        time: str_field(json, "time").unwrap_or_default(),
        pass: num_field(summary, "pass")?,
        warn: num_field(summary, "warn")?,
        fail: num_field(summary, "fail")?,
        problems: Vec::new(),
    };
    let findings = json.split_once("\"findings\":[")?.1;
    for f in findings.split("{\"id\":").skip(1) {
        let f = format!("{{\"id\":{f}");
        let (Some(id), Some(status)) = (str_field(&f, "id"), str_field(&f, "status")) else {
            continue;
        };
        match status.as_str() {
            "fail" => a.problems.push(format!("{id}✗")),
            "warn" => a.problems.push(format!("{id}!")),
            _ => {}
        }
    }
    Some(a)
}

/// "2026-11-08 11:55 UTC" -> seconds since the epoch.
fn parse_utc(t: &str) -> Option<u64> {
    let mut p = t.split(['-', ' ', ':']);
    let mut n = || -> Option<i64> { p.next()?.parse().ok() };
    let (y, m, d, hh, mm) = (n()?, n()?, n()?, n()?, n()?);
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + hh * 3600 + mm * 60).ok()
}

fn check(dest: &str, live: bool, config: Option<&std::path::Path>) -> Row {
    let remote = if live {
        "sudo -n sediros-audit --json"
    } else {
        "cat /var/lib/sediros/audit/last.json"
    };
    let mut cmd = Command::new("ssh");
    if let Some(c) = config {
        cmd.arg("-F").arg(c);
    }
    let out = cmd
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(dest)
        .arg(remote)
        .stdin(Stdio::null())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return Row::Unreachable(format!("ssh: {e}")),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // sediros-audit exits 1 when a check fails; its JSON is still complete.
    if let Some(a) = parse_audit(&stdout) {
        return Row::Ok(a);
    }
    let why = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    match out.status.code() {
        Some(255) => Row::Unreachable(why),
        _ if live && why.contains("password is required") => Row::NoData(
            "sudo asks for a password: --live needs passwordless sudo for sediros-audit".into(),
        ),
        _ if !live => Row::NoData(
            "no daily audit result (sediros-audit 0.3.0 or newer, and its timer)".into(),
        ),
        _ => Row::NoData(if why.is_empty() {
            "no audit output".into()
        } else {
            why
        }),
    }
}

pub fn run(args: &FleetArgs) -> Result<()> {
    let text = std::fs::read_to_string(&args.hosts)
        .with_context(|| format!("reading {}", args.hosts.display()))?;
    let hosts = parse_hosts(&text);
    if hosts.is_empty() {
        bail!("no hosts in {}", args.hosts.display());
    }
    if let Some(bad) = hosts.iter().find(|h| !valid_dest(h)) {
        bail!("invalid host '{bad}' in {}", args.hosts.display());
    }

    // Parallel, a few at a time: ssh does the waiting.
    let jobs = args.jobs.max(1);
    let mut rows: Vec<(String, Row)> = Vec::new();
    for chunk in hosts.chunks(jobs) {
        let handles: Vec<_> = chunk
            .iter()
            .map(|h| {
                let (h, live, config) = (h.clone(), args.live, args.ssh_config.clone());
                std::thread::spawn(move || {
                    let row = check(&h, live, config.as_deref());
                    (h, row)
                })
            })
            .collect();
        for handle in handles {
            rows.push(handle.join().expect("fleet worker"));
        }
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let width = rows.iter().map(|(h, _)| h.len()).max().unwrap_or(4).max(4);
    println!(
        "{:<width$}  {:<11}  {:>4} {:>4} {:>4}  {:<20}  PROBLEMS",
        "HOST", "RESULT", "PASS", "WARN", "FAIL", "CHECKED"
    );
    let mut bad = 0;
    for (host, row) in &rows {
        match row {
            Row::Ok(a) => {
                let stale =
                    parse_utc(&a.time).is_some_and(|t| now.saturating_sub(t) > STALE.as_secs());
                let result = if a.fail > 0 {
                    "FAIL"
                } else if stale {
                    "stale"
                } else if a.warn > 0 {
                    "warnings"
                } else {
                    "ok"
                };
                if a.fail > 0 || stale {
                    bad += 1;
                }
                let mut problems = a.problems.join(" ");
                if stale {
                    problems = format!("daily audit not running? {problems}");
                }
                println!(
                    "{:<width$}  {:<11}  {:>4} {:>4} {:>4}  {:<20}  {}",
                    host, result, a.pass, a.warn, a.fail, a.time, problems
                );
            }
            Row::NoData(why) => {
                bad += 1;
                println!(
                    "{:<width$}  {:<11}  {:>4} {:>4} {:>4}  {:<20}  {why}",
                    host, "no data", "-", "-", "-", "-"
                );
            }
            Row::Unreachable(why) => {
                bad += 1;
                println!(
                    "{:<width$}  {:<11}  {:>4} {:>4} {:>4}  {:<20}  {why}",
                    host, "unreachable", "-", "-", "-", "-"
                );
            }
        }
    }
    println!(
        "\n{} host(s), {} need attention{}",
        rows.len(),
        bad,
        if args.live {
            ""
        } else {
            " (results of the last daily audit; --live runs it now)"
        }
    );
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"host":"web1","system":"SedirOS 1.1","time":"2026-11-08 11:55 UTC","summary":{"pass":14,"warn":1,"fail":1,"skip":0},"findings":[{"id":"ssh","status":"pass","summary":"ok","details":[]},{"id":"firewall","status":"fail","summary":"ufw \"inactive\"","details":["x"]},{"id":"packages","status":"warn","summary":"1 changed","details":[]}]}"#;

    #[test]
    fn audit_json() {
        let a = parse_audit(JSON).unwrap();
        assert_eq!(
            (a.host.as_str(), a.pass, a.warn, a.fail),
            ("web1", 14, 1, 1)
        );
        assert_eq!(a.problems, vec!["firewall✗", "packages!"]);
        assert_eq!(a.time, "2026-11-08 11:55 UTC");
        assert!(parse_audit("cat: No such file or directory").is_none());
    }

    #[test]
    fn hosts_file() {
        let h = parse_hosts("# servers\nweb1\nadmin@db1  # primary\n\nssh://ops@10.0.0.5:2200\n");
        assert_eq!(h, vec!["web1", "admin@db1", "ssh://ops@10.0.0.5:2200"]);
        assert!(!valid_dest("-oProxyCommand=evil"));
        assert!(!valid_dest("web1;rm"));
    }

    #[test]
    fn utc_times() {
        assert_eq!(parse_utc("1970-01-01 00:00 UTC"), Some(0));
        assert_eq!(parse_utc("2026-11-08 11:55 UTC"), Some(1794138900));
        assert_eq!(parse_utc("garbage"), None);
    }
}
