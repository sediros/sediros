//! External commands. Every command is echoed before it runs; secrets are
//! only ever passed on stdin, never in argv (visible in ps / logs).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

fn render(prog: &str, args: &[&str]) -> String {
    let mut s = prog.to_string();
    for a in args {
        s.push(' ');
        if a.is_empty() || a.contains(char::is_whitespace) {
            s.push_str(&format!("'{a}'"));
        } else {
            s.push_str(a);
        }
    }
    s
}

/// Run a command, fail on non-zero exit.
pub fn run(prog: &str, args: &[&str]) -> Result<()> {
    println!("  $ {}", render(prog, args));
    let status = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("starting {prog}"))?;
    if !status.success() {
        bail!("{} failed ({status})", render(prog, args));
    }
    Ok(())
}

/// Run a command with `input` on stdin (secrets). The input is not echoed.
pub fn run_with_stdin(prog: &str, args: &[&str], input: &[u8]) -> Result<()> {
    println!("  $ {}   (stdin: <secret>)", render(prog, args));
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {prog}"))?;
    child
        .stdin
        .take()
        .context("stdin")?
        .write_all(input)
        .with_context(|| format!("writing stdin of {prog}"))?;
    let status = child.wait()?;
    if !status.success() {
        bail!("{} failed ({status})", render(prog, args));
    }
    Ok(())
}

/// Capture stdout of a read-only query.
pub fn output(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("starting {prog}"))?;
    if !out.status.success() {
        bail!(
            "{} failed: {}",
            render(prog, args),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Best effort (cleanup paths): errors are reported, not returned.
pub fn try_run(prog: &str, args: &[&str]) {
    let ok = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("  (cleanup) {} failed", render(prog, args));
    }
}

/// Run `prog args` inside the target root.
pub fn chroot(target: &Path, prog: &str, args: &[&str]) -> Result<()> {
    let root = target.to_str().context("non UTF-8 target path")?;
    let mut full = vec![root, prog];
    full.extend_from_slice(args);
    run("chroot", &full)
}

/// `tar -c src | tar -x dst`, preserving owners, modes, ACLs and xattrs
/// (file capabilities such as ping's cap_net_raw live in xattrs).
pub fn copy_tree(src: &Path, dst: &Path, excludes: &[&str]) -> Result<()> {
    let src = src.to_str().context("non UTF-8 source path")?;
    let dst = dst.to_str().context("non UTF-8 target path")?;
    let common = [
        "--numeric-owner",
        "--xattrs",
        "--xattrs-include=*",
        "--acls",
    ];

    let mut create: Vec<String> = vec!["-C".into(), src.into()];
    create.extend(common.iter().map(|s| s.to_string()));
    create.extend(excludes.iter().map(|e| format!("--exclude={e}")));
    create.extend(["-cpf".into(), "-".into(), ".".into()]);
    let mut extract: Vec<String> = vec!["-C".into(), dst.into()];
    extract.extend(common.iter().map(|s| s.to_string()));
    extract.extend(["-xpf".into(), "-".into()]);

    println!("  $ tar {} | tar {}", create.join(" "), extract.join(" "));
    let mut reader = Command::new("tar")
        .args(&create)
        .stdout(Stdio::piped())
        .spawn()
        .context("starting tar (create)")?;
    let pipe = reader.stdout.take().context("tar stdout")?;
    let writer = Command::new("tar")
        .args(&extract)
        .stdin(pipe)
        .status()
        .context("starting tar (extract)")?;
    let read_status = reader.wait()?;
    if !read_status.success() || !writer.success() {
        bail!("copying {src} -> {dst} failed (create: {read_status}, extract: {writer})");
    }
    Ok(())
}
