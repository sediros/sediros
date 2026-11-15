//! sediros-audit: check that a SedirOS system's hardening is still in place.
//!
//! Exit status: 0 nothing failed (warnings allowed), 1 something failed,
//! 2 the audit could not run.

mod checks;
mod integrity;
mod record;
mod report;
mod sys;

use std::process::ExitCode;

use report::{Header, Status};

const USAGE: &str = "\
Usage: sediros-audit [--json] [--quiet] [--record]
       sediros-audit integrity [--accept] [--json]

Checks that this SedirOS system's hardening is still in place: kernel
settings, SSH, firewall, AppArmor, setuid programs, accounts, sudo, PAM,
kernel modules, automatic updates, boot settings and disk encryption.
Expected values come from the installed SedirOS packages.

  --json    machine-readable output
  -q, --quiet   only warnings and failures
  --record  the daily run (sediros-audit.timer): keep the result in
            /var/lib/sediros/audit, write Prometheus metrics, report when
            the hardening got worse (/etc/sediros/audit.conf)

  integrity           only the integrity check, with every changed file:
                      watched files (accounts, SSH keys and settings, sudo,
                      PAM, cron, systemd units, boot, apt sources and keys,
                      /usr/local) against a reference, and package files
                      against Debian's checksums (dpkg --verify)
  integrity --accept  take the current state as the new reference, after
                      changes you made on purpose
  -h, --help    this help
  -V, --version version

Exit status: 0 no failures (warnings allowed), 1 failures, 2 error.
Run as root: sudo sediros-audit";

fn main() -> ExitCode {
    let mut json = false;
    let mut quiet = false;
    let mut record = false;
    let mut only_integrity = false;
    let mut accept = false;
    let mut after_dpkg = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--json" => json = true,
            "-q" | "--quiet" => quiet = true,
            "--record" => record = true,
            "integrity" => only_integrity = true,
            "--accept" => accept = true,
            // The apt hook (99-zz-sediros-integrity), not for people.
            "--after-dpkg" => after_dpkg = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("sediros-audit {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("sediros-audit: unknown argument '{other}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    if sys::euid() != Some(0) {
        eprintln!(
            "sediros-audit: needs root to read the system's configuration: sudo sediros-audit"
        );
        return ExitCode::from(2);
    }
    if (accept && !only_integrity) || (only_integrity && record) {
        eprintln!("sediros-audit: --accept goes with integrity, --record without it\n\n{USAGE}");
        return ExitCode::from(2);
    }
    if after_dpkg {
        integrity::after_dpkg();
        return ExitCode::SUCCESS;
    }
    if accept {
        return match integrity::accept() {
            Ok(lines) => {
                for l in lines {
                    println!("{l}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("sediros-audit: {e}");
                ExitCode::from(2)
            }
        };
    }

    let ctx = sys::Ctx::load();
    let findings = if only_integrity {
        vec![integrity::check(integrity::Mode::Single)]
    } else {
        let mut f = checks::all(&ctx);
        f.push(integrity::check(if record {
            integrity::Mode::Record
        } else {
            integrity::Mode::Full
        }));
        f
    };
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

    if record {
        for line in record::record(&header, &findings) {
            println!("{line}");
        }
    } else if json {
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
