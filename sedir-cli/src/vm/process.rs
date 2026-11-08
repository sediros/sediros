use std::io;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::Result;

pub fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // Signal 0: existence/permission check only.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Alive *and* still a QEMU process (guards against PID reuse).
pub fn is_qemu_alive(pid: i32) -> bool {
    alive(pid)
        && Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("qemu"))
            .unwrap_or(false)
}

pub fn signal(pid: i32, sig: i32) -> Result<()> {
    if unsafe { libc::kill(pid, sig) } != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(err.into());
        }
    }
    Ok(())
}

/// Wait until the process is gone. Returns false on timeout.
pub fn wait_exit(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(200));
    }
    true
}
