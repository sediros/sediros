//! `sediros unlock`: send the disk encryption passphrase to a SedirOS server
//! that is waiting at boot (dropbear in the initramfs, set up by the
//! installer; key-only, forced command cryptroot-unlock).
//!
//! The passphrase goes to ssh's stdin, never into arguments or the
//! environment. cryptroot-unlock takes all of stdin as the passphrase, so it
//! is sent without a newline. The passphrase can be kept in the system's
//! keychain (macOS `security`, Linux `secret-tool`): no keychain library is
//! linked, and nothing is written to disk by sediros itself.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::cli::UnlockArgs;

const SERVICE: &str = "sediros-unlock";

/// The host part of user@host, for the keychain entry.
fn host_only(target: &str) -> &str {
    target.rsplit('@').next().unwrap_or(target)
}

fn valid_target(t: &str) -> bool {
    !t.is_empty()
        && !t.starts_with('-')
        && t.len() <= 255
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._@:[]".contains(c))
}

/// What one connection attempt ended with.
#[derive(Debug, PartialEq, Eq)]
enum Attempt {
    Unlocked,
    /// Not reachable yet (still booting, network not up): try again.
    NotYet,
    WrongPassphrase,
    /// Anything else (key refused, host key changed): stop and show it.
    Failed(String),
}

fn classify(ok: bool, output: &str) -> Attempt {
    if ok && output.contains("set up successfully") {
        return Attempt::Unlocked;
    }
    let o = output.to_ascii_lowercase();
    if o.contains("bad password") || o.contains("no key available") {
        return Attempt::WrongPassphrase;
    }
    let not_yet = [
        "connection refused",
        "timed out",
        "no route to host",
        "network is unreachable",
        "connection reset",
        "banner exchange",
        "connection closed by remote host",
        "could not resolve hostname",
    ];
    if not_yet.iter().any(|m| o.contains(m)) {
        return Attempt::NotYet;
    }
    let last = output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("ssh failed");
    Attempt::Failed(last.trim().to_string())
}

// --- keychain -----------------------------------------------------------------------

fn keychain_get(host: &str) -> Option<String> {
    let out = if cfg!(target_os = "macos") {
        Command::new("security")
            .args(["find-generic-password", "-s", SERVICE, "-a", host, "-w"])
            .stderr(Stdio::null())
            .output()
            .ok()?
    } else {
        Command::new("secret-tool")
            .args(["lookup", "service", SERVICE, "host", host])
            .stderr(Stdio::null())
            .output()
            .ok()?
    };
    let pass = String::from_utf8(out.stdout).ok()?;
    let pass = pass.strip_suffix('\n').unwrap_or(&pass).to_string();
    (out.status.success() && !pass.is_empty()).then_some(pass)
}

fn keychain_set(host: &str, pass: &str) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        // -w as the last option makes `security` read the password (twice)
        // from stdin, instead of taking it as an argument (visible in ps).
        let mut child = Command::new("security")
            .args([
                "add-generic-password",
                "-U",
                "-s",
                SERVICE,
                "-a",
                host,
                "-w",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("running security")?;
        child
            .stdin
            .take()
            .context("security stdin")?
            .write_all(format!("{pass}\n{pass}\n").as_bytes())?;
        child.wait()?
    } else {
        let mut child = Command::new("secret-tool")
            .args([
                "store",
                "--label",
                &format!("SedirOS disk passphrase for {host}"),
            ])
            .args(["service", SERVICE, "host", host])
            .stdin(Stdio::piped())
            .spawn()
            .context("running secret-tool (package libsecret-tools)")?;
        child
            .stdin
            .take()
            .context("secret-tool stdin")?
            .write_all(pass.as_bytes())?;
        child.wait()?
    };
    if !status.success() {
        bail!("could not save the passphrase in the keychain");
    }
    Ok(())
}

fn keychain_delete(host: &str) -> Result<bool> {
    let status = if cfg!(target_os = "macos") {
        Command::new("security")
            .args(["delete-generic-password", "-s", SERVICE, "-a", host])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("running security")?
    } else {
        Command::new("secret-tool")
            .args(["clear", "service", SERVICE, "host", host])
            .status()
            .context("running secret-tool (package libsecret-tools)")?
    };
    Ok(status.success())
}

/// Reads a line from the terminal without echo (stty, as ssh-keygen does).
fn ask_passphrase(host: &str) -> Result<String> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("no terminal to ask for the passphrase")?;
    write!(tty, "Disk passphrase for {host}: ")?;
    tty.flush()?;
    let stty = |arg: &str| {
        Command::new("stty")
            .arg(arg)
            .stdin(std::fs::File::open("/dev/tty")?)
            .status()
    };
    stty("-echo")?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    let read = loop {
        match tty.read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) if byte[0] == b'\n' => break Ok(()),
            Ok(_) => line.push(byte[0]),
            Err(e) => break Err(e),
        }
    };
    let _ = stty("echo");
    writeln!(tty)?;
    read?;
    let pass = String::from_utf8(line).context("passphrase is not UTF-8")?;
    let pass = pass.strip_suffix('\r').unwrap_or(&pass).to_string();
    if pass.is_empty() {
        bail!("no passphrase given");
    }
    Ok(pass)
}

fn attempt(args: &UnlockArgs, target: &str, pass: &str) -> Result<Attempt> {
    let mut cmd = Command::new("ssh");
    cmd.args(["-T", "-p", &args.port.to_string()])
        // The boot-time server has its own host key; ssh keeps it as
        // [host]:port. A changed key still stops the connection.
        .args(["-o", "StrictHostKeyChecking=accept-new"])
        .args(["-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=10"])
        .args([
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
        ]);
    if let Some(key) = &args.identity {
        cmd.arg("-i").arg(key).args(["-o", "IdentitiesOnly=yes"]);
    }
    let mut child = cmd
        .arg(target)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running ssh (is OpenSSH installed?)")?;
    // No newline: cryptroot-unlock reads all of stdin as the passphrase.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(pass.as_bytes());
    }
    let out = child.wait_with_output()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(classify(out.status.success(), &text))
}

pub fn run(args: &UnlockArgs) -> Result<()> {
    if !valid_target(&args.host) {
        bail!("invalid host '{}'", args.host);
    }
    let target = if args.host.contains('@') {
        args.host.clone()
    } else {
        format!("root@{}", args.host)
    };
    let host = host_only(&target).to_string();
    if args.forget {
        if keychain_delete(&host)? {
            println!("Passphrase for {host} removed from the keychain.");
        } else {
            println!("No passphrase for {host} in the keychain.");
        }
        return Ok(());
    }

    let (pass, from_keychain) = match keychain_get(&host) {
        Some(p) => (p, true),
        None => (ask_passphrase(&host)?, false),
    };
    if from_keychain {
        println!("Using the passphrase for {host} from the keychain.");
    }

    println!(
        "Waiting for {host}:{} (the server's boot-time SSH)...",
        args.port
    );
    let start = Instant::now();
    loop {
        match attempt(args, &target, &pass)? {
            Attempt::Unlocked => {
                println!("Unlocked: {host} continues to boot.");
                if args.save && !from_keychain {
                    keychain_set(&host, &pass)?;
                    println!("Passphrase saved in the keychain (sediros unlock --forget {host} removes it).");
                }
                return Ok(());
            }
            Attempt::WrongPassphrase if from_keychain => bail!(
                "the passphrase in the keychain was refused; remove it with \
                 sediros unlock --forget {host} and try again"
            ),
            Attempt::WrongPassphrase => {
                bail!("wrong passphrase (the server allows a few tries per boot)")
            }
            Attempt::Failed(why) => bail!("{why}"),
            Attempt::NotYet if start.elapsed() >= Duration::from_secs(args.wait) => {
                bail!("{host}:{} did not answer within {} s", args.port, args.wait)
            }
            Attempt::NotYet => std::thread::sleep(Duration::from_secs(5)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes() {
        assert_eq!(
            classify(
                true,
                "Please unlock disk sediros-root\ncryptsetup: sediros-root set up successfully\n"
            ),
            Attempt::Unlocked
        );
        assert_eq!(
            classify(
                false,
                "cryptsetup: ERROR: sediros-root: cryptsetup failed, bad password or options?"
            ),
            Attempt::WrongPassphrase
        );
        assert_eq!(
            classify(
                false,
                "ssh: connect to host x port 2222: Connection refused"
            ),
            Attempt::NotYet
        );
        assert_eq!(
            classify(false, "Connection timed out during banner exchange"),
            Attempt::NotYet
        );
        assert_eq!(
            classify(false, "root@x: Permission denied (publickey)."),
            Attempt::Failed("root@x: Permission denied (publickey).".into())
        );
        assert!(matches!(
            classify(
                false,
                "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nHost key verification failed."
            ),
            Attempt::Failed(_)
        ));
    }

    #[test]
    fn targets() {
        assert!(valid_target("server.example.org"));
        assert!(valid_target("admin@10.0.0.5"));
        assert!(!valid_target("-oProxyCommand=x"));
        assert!(!valid_target("a b"));
        assert_eq!(host_only("root@web1"), "web1");
    }
}
