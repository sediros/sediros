//! `--record`: the daily run (sediros-audit.timer). Keeps the result for
//! the login greeting and for tools, writes Prometheus metrics, and reports
//! when the hardening got worse since the previous run.
//!
//! State in /var/lib/sediros/audit:
//!   last.json       the full result (same as --json)
//!   last-summary    one line for the login greeting
//!   last-status     `<check> <status>` per line, compared with the next run
//! Settings in /etc/sediros/audit.conf (KEY=value).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::report::{self, Finding, Header, Status};
use crate::sys;

pub const STATE_DIR: &str = "/var/lib/sediros/audit";
const CONF: &str = "/etc/sediros/audit.conf";
const NODE_EXPORTER_DIR: &str = "/var/lib/prometheus/node-exporter";

/// How bad a status is, for "got worse": info and skip are not problems.
fn rank(s: &str) -> u8 {
    match s {
        "warn" => 1,
        "fail" => 2,
        _ => 0,
    }
}

/// Checks that got worse: (id, before, now, summary). A check that is new
/// counts as worse only when it is a problem.
pub fn worse(before: &BTreeMap<String, String>, findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .filter_map(|f| {
            let now = f.status.name();
            let was = before.get(f.id).map(String::as_str).unwrap_or("pass");
            (rank(now) > rank(was)).then(|| format!("{}: {was} -> {now}: {}", f.id, f.summary))
        })
        .collect()
}

pub fn parse_status(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect()
}

pub fn summary_line(findings: &[Finding], time: &str) -> String {
    format!(
        "{} passed, {} warnings, {} failed · {time}",
        report::count(findings, Status::Pass),
        report::count(findings, Status::Warn),
        report::count(findings, Status::Fail),
    )
}

/// node_exporter textfile collector format.
pub fn prometheus(findings: &[Finding], epoch: u64) -> String {
    let mut out = String::from(
        "# HELP sediros_audit_check Result of one sediros-audit check: 1 for its current status.\n\
         # TYPE sediros_audit_check gauge\n",
    );
    for f in findings {
        out.push_str(&format!(
            "sediros_audit_check{{check=\"{}\",status=\"{}\"}} 1\n",
            f.id,
            f.status.name()
        ));
    }
    out.push_str(
        "# HELP sediros_audit_findings Number of sediros-audit checks per status.\n\
         # TYPE sediros_audit_findings gauge\n",
    );
    for s in [
        Status::Pass,
        Status::Info,
        Status::Warn,
        Status::Fail,
        Status::Skip,
    ] {
        out.push_str(&format!(
            "sediros_audit_findings{{status=\"{}\"}} {}\n",
            s.name(),
            report::count(findings, s)
        ));
    }
    out.push_str(&format!(
        "# HELP sediros_audit_last_run_timestamp_seconds When sediros-audit last ran.\n\
         # TYPE sediros_audit_last_run_timestamp_seconds gauge\n\
         sediros_audit_last_run_timestamp_seconds {epoch}\n"
    ));
    out
}

/// Writes through a temporary file, so readers never see half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn conf() -> BTreeMap<String, String> {
    sys::read(CONF)
        .map(|t| sys::parse_kv(&t))
        .unwrap_or_default()
}

/// JSON string for a webhook body.
fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn notify(conf: &BTreeMap<String, String>, subject: &str, body: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(to) = conf.get("MAILTO").filter(|v| !v.is_empty()) {
        if !sys::exists("sendmail") {
            errors.push(
                "MAILTO is set, but there is no sendmail (install a mail transfer agent)".into(),
            );
        } else {
            let mail = format!("To: {to}\nSubject: {subject}\n\n{body}\n");
            if let Err(e) = sys::run_input("sendmail", &["-t"], &mail) {
                errors.push(format!("sendmail: {e}"));
            }
        }
    }
    if let Some(url) = conf.get("WEBHOOK").filter(|v| !v.is_empty()) {
        let text = format!("{subject}\n{body}");
        let (ctype, payload) = match conf.get("WEBHOOK_FORMAT").map(String::as_str) {
            Some("slack") => (
                "application/json",
                format!("{{\"text\":{}}}", json_str(&text)),
            ),
            _ => ("text/plain; charset=utf-8", text),
        };
        if !sys::exists("curl") {
            errors.push("WEBHOOK is set, but curl is not installed".into());
        } else {
            let header = format!("Content-Type: {ctype}");
            let args = [
                "-fsS",
                "--max-time",
                "30",
                "-H",
                &header,
                "--data-binary",
                "@-",
                url.as_str(),
            ];
            if let Err(e) = sys::run_input("curl", &args, &payload) {
                errors.push(format!("webhook: {e}"));
            }
        }
    }
    errors
}

/// The daily run. Returns the lines to print (for the journal).
pub fn record(header: &Header, findings: &[Finding]) -> Vec<String> {
    let mut log = Vec::new();
    let dir = Path::new(STATE_DIR);
    if let Err(e) = fs::create_dir_all(dir) {
        return vec![format!("✗ {STATE_DIR}: {e}")];
    }
    let before = sys::read(dir.join("last-status")).map(|t| parse_status(&t));
    let status: String = findings
        .iter()
        .map(|f| format!("{} {}\n", f.id, f.status.name()))
        .collect();
    let results = [
        ("last.json", report::json(header, findings)),
        ("last-summary", summary_line(findings, &header.time) + "\n"),
        ("last-status", status),
    ];
    for (name, text) in results {
        if let Err(e) = write_atomic(&dir.join(name), &text) {
            log.push(format!("✗ {e}"));
        }
    }
    log.push(summary_line(findings, &header.time));

    let conf = conf();
    let prom_dir = conf
        .get("PROMETHEUS_DIR")
        .cloned()
        .unwrap_or_else(|| NODE_EXPORTER_DIR.into());
    if Path::new(&prom_dir).is_dir() {
        let path = Path::new(&prom_dir).join("sediros_audit.prom");
        match write_atomic(&path, &prometheus(findings, sys::epoch())) {
            Ok(()) => log.push(format!("metrics: {}", path.display())),
            Err(e) => log.push(format!("✗ metrics: {e}")),
        }
    }

    // First run: a report only if something is already wrong.
    let changes = match &before {
        Some(b) => worse(b, findings),
        None => worse(&BTreeMap::new(), findings),
    };
    if !changes.is_empty() {
        let subject = format!("SedirOS audit on {}: hardening got worse", header.host);
        let body = format!(
            "{}\n\n{}\n\nDetails: sudo sediros-audit\n",
            changes.join("\n"),
            summary_line(findings, &header.time)
        );
        log.push(subject.clone());
        log.extend(changes.iter().map(|c| format!("  {c}")));
        for e in notify(&conf, &subject, &body) {
            log.push(format!("✗ {e}"));
        }
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: &'static str, s: Status) -> Finding {
        Finding::new(id, s, "x")
    }

    #[test]
    fn only_worse_is_reported() {
        let before = parse_status("ssh pass\nfirewall warn\nsuid fail\nupdates pass\n");
        let now = vec![
            f("ssh", Status::Fail),      // worse
            f("firewall", Status::Pass), // better
            f("suid", Status::Fail),     // unchanged
            f("updates", Status::Info),  // not a problem
            f("kernel", Status::Warn),   // new and a problem
        ];
        let w = worse(&before, &now);
        assert_eq!(
            w,
            vec![
                "ssh: pass -> fail: x".to_string(),
                "kernel: pass -> warn: x".to_string()
            ]
        );
    }

    #[test]
    fn first_run_reports_only_problems() {
        let now = vec![f("ssh", Status::Pass), f("suid", Status::Fail)];
        assert_eq!(worse(&BTreeMap::new(), &now).len(), 1);
    }

    #[test]
    fn metrics_and_summary() {
        let now = vec![f("ssh", Status::Pass), f("suid", Status::Warn)];
        let p = prometheus(&now, 1700000000);
        assert!(p.contains("sediros_audit_check{check=\"suid\",status=\"warn\"} 1\n"));
        assert!(p.contains("sediros_audit_findings{status=\"pass\"} 1\n"));
        assert!(p.contains("sediros_audit_last_run_timestamp_seconds 1700000000\n"));
        assert_eq!(
            summary_line(&now, "T"),
            "1 passed, 1 warnings, 0 failed · T"
        );
    }

    #[test]
    fn webhook_json() {
        assert_eq!(json_str("a\"b\nc"), "\"a\\\"b\\nc\"");
    }
}
