//! External commands. Every command is echoed before it runs; secrets are
//! only ever passed on stdin, never in argv (visible in ps / logs).

use std::io::{BufRead, BufReader, Write};
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

/// stderr lines of Debian tools that are known to be harmless. ckbcomp
/// (console-setup's initramfs hook, setupcon) prints one per key of the
/// keyboard layout that has no console equivalent, dozens for e.g. "tr".
const NOISE: &[&str] = &["Unknown X keysym"];

/// Like `run`, but stderr lines matching `NOISE` are counted instead of shown.
pub fn run_filtered(prog: &str, args: &[&str]) -> Result<()> {
    println!("  $ {}", render(prog, args));
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {prog}"))?;
    let stderr = child.stderr.take().context("stderr")?;
    let hidden = filter_lines(BufReader::new(stderr), &mut std::io::stderr());
    let status = child.wait()?;
    if hidden > 0 {
        println!("  ({hidden} harmless keymap warnings of ckbcomp not shown)");
    }
    if !status.success() {
        bail!("{} failed ({status})", render(prog, args));
    }
    Ok(())
}

/// Copy lines from `input` to `out`, except noise; returns how many were dropped.
fn filter_lines(input: impl BufRead, out: &mut impl Write) -> usize {
    let mut hidden = 0;
    for line in input.lines().map_while(Result::ok) {
        if NOISE.iter().any(|n| line.contains(n)) {
            hidden += 1;
        } else {
            let _ = writeln!(out, "{line}");
        }
    }
    hidden
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

/// Run a check that reads `input` on stdin (secrets, keys). Ok(stdout) on
/// success, Err(stderr + stdout) when the command rejects the input.
pub fn check_with_stdin(
    prog: &str,
    args: &[&str],
    input: &[u8],
) -> Result<std::result::Result<String, String>> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {prog}"))?;
    // A rejected input may close stdin early: ignore EPIPE, judge by exit code.
    let _ = child.stdin.take().context("stdin")?.write_all(input);
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(Ok(stdout))
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Ok(Err(format!("{stderr} {stdout}").trim().to_string()))
    }
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

/// `chroot` with the known harmless stderr noise filtered (see `NOISE`).
pub fn chroot_filtered(target: &Path, prog: &str, args: &[&str]) -> Result<()> {
    let root = target.to_str().context("non UTF-8 target path")?;
    let mut full = vec![root, prog];
    full.extend_from_slice(args);
    run_filtered("chroot", &full)
}

/// `tar -c src | tar -x dst`, preserving owners, modes, ACLs and xattrs
/// (file capabilities such as ping's cap_net_raw live in xattrs).
pub fn copy_tree(src: &Path, dst: &Path, excludes: &[&str]) -> Result<()> {
    let xattrs = has_xattr_support(src);
    let src = src.to_str().context("non UTF-8 source path")?;
    let dst = dst.to_str().context("non UTF-8 target path")?;
    let common = [
        "--numeric-owner",
        "--xattrs",
        "--xattrs-include=*",
        "--acls",
    ];

    let mut create: Vec<String> = vec!["-C".into(), src.into()];
    if xattrs {
        create.extend(common.iter().map(|s| s.to_string()));
    } else {
        // A squashfs built from files without any xattr has no xattr table,
        // and the kernel then answers every listxattr with EOPNOTSUPP: tar
        // would print one warning per file although there is nothing to copy.
        println!("  (source has no extended attributes: none to copy)");
        create.push("--numeric-owner".into());
    }
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

/// Whether the file system at `path` supports listing extended attributes
/// (false only for EOPNOTSUPP; other errors leave tar to report them).
#[cfg(target_os = "linux")]
fn has_xattr_support(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: c is a valid NUL-terminated path; a NULL buffer of size 0
    // only asks for the size of the list.
    let r = unsafe { libc::listxattr(c.as_ptr(), std::ptr::null_mut(), 0) };
    !(r < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EOPNOTSUPP))
}

#[cfg(not(target_os = "linux"))]
fn has_xattr_support(_path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::filter_lines;

    #[test]
    fn drops_only_keymap_noise() {
        let input = "WARNING: Unknown X keysym \"none\"\nreal error\nWARNING: Unknown X keysym \"dead_x\"\n";
        let mut out = Vec::new();
        assert_eq!(filter_lines(input.as_bytes(), &mut out), 2);
        assert_eq!(String::from_utf8(out).unwrap(), "real error\n");
    }
}
