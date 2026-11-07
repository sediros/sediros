//! sisura-audit: check that a Sisura system's hardening is still in place.
//!
//! Exit status: 0 nothing failed (warnings allowed), 1 something failed,
//! 2 the audit could not run.

mod checks;
mod report;
mod sys;

use std::process::ExitCode;

use report::{Header, Status};

const USAGE: &str = "\
Usage: sisura-audit [--json] [--quiet]

Checks that this Sisura system's hardening is still in place: kernel
settings, SSH, firewall, AppArmor, setuid programs, accounts, sudo, PAM,
kernel modules, automatic updates, boot settings and disk encryption.
Expected values come from the installed Sisura packages.

  --json    machine-readable output
  -q, --quiet   only warnings and failures
  -h, --help    this help
  -V, --version version

Exit status: 0 no failures (warnings allowed), 1 failures, 2 error.
Run as root: sudo sisura-audit";

fn main() -> ExitCode {
    let mut json = false;
    let mut quiet = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--json" => json = true,
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("sisura-audit {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("sisura-audit: unknown argument '{other}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    if sys::euid() != Some(0) {
        eprintln!("sisura-audit: needs root to read the system's configuration: sudo sisura-audit");
        return ExitCode::from(2);
    }

    let ctx = sys::Ctx::load();
    let findings = checks::all(&ctx);
    let os = sys::read("/etc/os-release")
        .map(|t| sys::parse_kv(&t))
        .unwrap_or_default();
    let mut system = os
        .get("PRETTY_NAME")
        .cloned()
        .unwrap_or_else(|| "unknown system".into());
    if let Some(b) = ctx.build.get("BUILD_ID") {
        system.push_str(&format!(", build {b}"));
    }
    system.push_str(match ctx.kind {
        sys::Kind::Vm => ", VM image",
        sys::Kind::Installed => ", installed",
        sys::Kind::Other => "",
    });
    let header = Header {
        host: sys::read("/etc/hostname")
            .map(|h| h.trim().to_string())
            .unwrap_or_default(),
        system,
        time: sys::now_utc(),
    };

    if json {
        print!("{}", report::json(&header, &findings));
    } else {
        print!("{}", report::text(&header, &findings, quiet));
    }
    if report::count(&findings, Status::Fail) > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
