//! Findings and their output: a table for people, JSON for tools.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Hardening in place.
    Pass,
    /// Informational, nothing to fix.
    Info,
    /// Deliberate change or something to look at (e.g. a reboot is due).
    Warn,
    /// Hardening missing or broken.
    Fail,
    /// Could not be checked here (tool missing, not applicable).
    Skip,
}

impl Status {
    fn mark(self) -> &'static str {
        match self {
            Status::Pass => "✓",
            Status::Info => "·",
            Status::Warn => "!",
            Status::Fail => "✗",
            Status::Skip => "-",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Info => "info",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub id: &'static str,
    pub status: Status,
    pub summary: String,
    pub details: Vec<String>,
}

impl Finding {
    pub fn new(id: &'static str, status: Status, summary: impl Into<String>) -> Self {
        Self {
            id,
            status,
            summary: summary.into(),
            details: Vec::new(),
        }
    }
    pub fn pass(id: &'static str, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Pass, summary)
    }
    pub fn info(id: &'static str, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Info, summary)
    }
    pub fn warn(id: &'static str, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Warn, summary)
    }
    pub fn fail(id: &'static str, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Fail, summary)
    }
    pub fn skip(id: &'static str, summary: impl Into<String>) -> Self {
        Self::new(id, Status::Skip, summary)
    }
    pub fn with(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

pub struct Header {
    pub host: String,
    pub system: String,
    pub time: String,
}

pub fn count(findings: &[Finding], s: Status) -> usize {
    findings.iter().filter(|f| f.status == s).count()
}

pub fn text(h: &Header, findings: &[Finding], quiet: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "SedirOS audit: {} ({}), {}\n",
        h.host, h.system, h.time
    );
    for f in findings {
        if quiet && matches!(f.status, Status::Pass | Status::Info | Status::Skip) {
            continue;
        }
        let _ = writeln!(out, " {} {:<12} {}", f.status.mark(), f.id, f.summary);
        for d in &f.details {
            let _ = writeln!(out, "   {:<12} {d}", "");
        }
    }
    let _ = writeln!(
        out,
        "\n{} passed, {} warnings, {} failed{}",
        count(findings, Status::Pass),
        count(findings, Status::Warn),
        count(findings, Status::Fail),
        match count(findings, Status::Skip) {
            0 => String::new(),
            n => format!(", {n} skipped"),
        }
    );
    out
}

pub fn json(h: &Header, findings: &[Finding]) -> String {
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"host\":{},\"system\":{},\"time\":{},",
        q(&h.host),
        q(&h.system),
        q(&h.time)
    );
    let _ = write!(
        out,
        "\"summary\":{{\"pass\":{},\"warn\":{},\"fail\":{},\"skip\":{}}},\"findings\":[",
        count(findings, Status::Pass),
        count(findings, Status::Warn),
        count(findings, Status::Fail),
        count(findings, Status::Skip)
    );
    for (i, f) in findings.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let details: Vec<String> = f.details.iter().map(|d| q(d)).collect();
        let _ = write!(
            out,
            "{{\"id\":{},\"status\":{},\"summary\":{},\"details\":[{}]}}",
            q(f.id),
            q(f.status.name()),
            q(&f.summary),
            details.join(",")
        );
    }
    out.push_str("]}\n");
    out
}

/// JSON string literal.
fn q(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes() {
        assert_eq!(q("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
    }

    #[test]
    fn json_shape() {
        let h = Header {
            host: "h".into(),
            system: "s".into(),
            time: "t".into(),
        };
        let f = vec![Finding::fail("ssh", "x").with(vec!["d".into()])];
        let j = json(&h, &f);
        assert!(j.contains("\"fail\":1"));
        assert!(j.contains(
            "{\"id\":\"ssh\",\"status\":\"fail\",\"summary\":\"x\",\"details\":[\"d\"]}"
        ));
    }
}
