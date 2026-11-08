//! Reading the system: files, commands, SedirOS's build info.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

/// What kind of SedirOS this is; some expectations differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// VM image (`sediros launch`): passwordless sudo, no LUKS, direct kernel boot.
    Vm,
    /// Installed to disk by sediros-install.
    Installed,
    /// Live ISO session or unknown.
    Other,
}

pub struct Ctx {
    pub kind: Kind,
    pub build: BTreeMap<String, String>,
}

impl Ctx {
    pub fn load() -> Self {
        let build = read("/etc/sediros/build-info")
            .map(|t| parse_kv(&t))
            .unwrap_or_default();
        let kind = if build.contains_key("INSTALLED") {
            Kind::Installed
        } else if build.get("PROFILE").map(String::as_str) == Some("vm") {
            Kind::Vm
        } else {
            Kind::Other
        };
        Self { kind, build }
    }
}

pub fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// stdout of a command that succeeded (C locale, no stdin).
pub fn run(prog: &str, args: &[&str]) -> Option<String> {
    let (ok, out, _) = run_full(prog, args)?;
    ok.then_some(out)
}

/// (success, stdout, stderr); None if the program could not be started.
pub fn run_full(prog: &str, args: &[&str]) -> Option<(bool, String, String)> {
    let out = Command::new(prog)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

pub fn exists(prog: &str) -> bool {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
        .iter()
        .any(|d| Path::new(d).join(prog).is_file())
}

/// KEY=value lines (build-info, os-release); quotes around values removed.
pub fn parse_kv(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

/// Lines without comments and blank lines.
pub fn conf_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
}

/// Collapse runs of whitespace (sysctl values like "4096 87380 6291456").
pub fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The effective user id, from /proc (no libc dependency).
pub fn euid() -> Option<u32> {
    read("/proc/self/status")?
        .lines()
        .find(|l| l.starts_with("Uid:"))?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}

/// Seconds since the epoch.
pub fn epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Runs a program with `input` on stdin; Err carries its stderr.
pub fn run_input(prog: &str, args: &[&str], input: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = Command::new(prog)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn now_utc() -> String {
    run("date", &["-u", "+%Y-%m-%d %H:%M UTC"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Files of a drop-in directory, sorted by name.
pub fn dropins(dir: &str, suffix: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.to_string_lossy().ends_with(suffix))
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_and_conf() {
        let m = parse_kv("NAME=\"SedirOS\"\nBUILD_ID=1-abc\nnoise\n");
        assert_eq!(m["NAME"], "SedirOS");
        assert_eq!(m["BUILD_ID"], "1-abc");
        let lines: Vec<&str> = conf_lines("# c\n\n a = 1 # x\nb=2\n").collect();
        assert_eq!(lines, ["a = 1", "b=2"]);
        assert_eq!(squash(" 4096\t87380  6291456 "), "4096 87380 6291456");
    }
}
