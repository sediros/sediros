//! Passphrases: read without echo from the terminal, or from a file
//! (automated tests), and wiped from memory when dropped.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{bail, Context, Result};

pub struct Secret(Vec<u8>);

impl Secret {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Placeholder until the real passphrase is read.
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let mut data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        while matches!(data.last(), Some(b'\n' | b'\r')) {
            data.pop();
        }
        Ok(Self(data))
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for b in self.0.iter_mut() {
            // Volatile write: not optimised away as a dead store.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

/// Terminal echo off for as long as the guard lives.
struct NoEcho {
    fd: i32,
    saved: libc::termios,
}

impl NoEcho {
    fn new(tty: &File) -> Result<Self> {
        let fd = tty.as_raw_fd();
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            bail!("tcgetattr failed: not a terminal");
        }
        let mut raw = saved;
        raw.c_lflag &= !libc::ECHO;
        raw.c_lflag |= libc::ECHONL;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            bail!("tcsetattr failed");
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

fn read_hidden(tty: &File, prompt: &str) -> Result<Secret> {
    let mut out = tty.try_clone()?;
    write!(out, "{prompt}")?;
    out.flush()?;
    let _guard = NoEcho::new(tty)?;
    let mut line = Vec::new();
    BufReader::new(tty.try_clone()?).read_until(b'\n', &mut line)?;
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Ok(Secret(line))
}

/// Ask twice on /dev/tty until both entries match and are long enough.
pub fn prompt_new(what: &str, min_len: usize) -> Result<Secret> {
    let tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("no terminal for the passphrase prompt (use --passphrase-file)")?;
    loop {
        let first = read_hidden(&tty, &format!("{what}: "))?;
        if first.bytes().len() < min_len {
            eprintln!("  at least {min_len} characters, try again");
            continue;
        }
        let second = read_hidden(&tty, &format!("{what} (again): "))?;
        if first.bytes() != second.bytes() {
            eprintln!("  entries do not match, try again");
            continue;
        }
        return Ok(first);
    }
}

/// Read one line from /dev/tty (visible), for confirmations.
pub fn prompt_line(prompt: &str) -> Result<String> {
    let tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("no terminal for confirmation (use --yes)")?;
    let mut out = tty.try_clone()?;
    write!(out, "{prompt}")?;
    out.flush()?;
    let mut line = String::new();
    BufReader::new(tty).read_line(&mut line)?;
    Ok(line.trim().to_string())
}
