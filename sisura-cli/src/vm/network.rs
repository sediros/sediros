use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

pub const FIRST_SSH_PORT: u16 = 2222;
const PORT_RANGE: u16 = 100;

/// Pick the host port forwarded to guest :22.
pub fn pick_port(requested: Option<u16>, taken: &[u16]) -> Result<u16> {
    if let Some(port) = requested {
        if taken.contains(&port) {
            bail!("port {port} is already assigned to another Sisura VM");
        }
        if !port_free(port) {
            bail!("port {port} is in use on 127.0.0.1");
        }
        return Ok(port);
    }
    (FIRST_SSH_PORT..FIRST_SSH_PORT + PORT_RANGE)
        .find(|p| !taken.contains(p) && port_free(*p))
        .with_context(|| {
            format!(
                "no free SSH port in {FIRST_SSH_PORT}-{}",
                FIRST_SSH_PORT + PORT_RANGE - 1
            )
        })
}

/// Free on 127.0.0.1 and not taken on any address: macOS lets a program
/// bind 127.0.0.1:<port> while another one listens on *:<port>, and the
/// loopback listener then silently takes over localhost:<port>.
pub fn port_free(port: u16) -> bool {
    let in_use = |addr: &str| {
        matches!(TcpListener::bind((addr, port)),
                 Err(e) if e.kind() == std::io::ErrorKind::AddrInUse)
    };
    TcpListener::bind(("127.0.0.1", port)).is_ok() && !in_use("0.0.0.0") && !in_use("::")
}

/// QEMU's user-mode hostfwd accepts TCP connections even before the guest's
/// sshd listens, so a plain connect() is not enough: wait for the "SSH-" banner.
pub fn wait_for_ssh(
    port: u16,
    timeout: Duration,
    mut still_running: impl FnMut() -> bool,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if ssh_banner(port) {
            return Ok(());
        }
        if !still_running() {
            bail!("QEMU exited during boot");
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out after {}s waiting for SSH on 127.0.0.1:{port}",
                timeout.as_secs()
            );
        }
        sleep(Duration::from_millis(500));
    }
}

fn ssh_banner(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).is_ok() && &buf == b"SSH-"
}
