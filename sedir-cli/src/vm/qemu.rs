//! QEMU process management: platform/accelerator detection, launch, QMP.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::image::BaseImage;
use super::state::VmState;
use crate::paths::which;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accel {
    Hvf,
    Kvm,
    Tcg,
}

impl Accel {
    fn as_str(self) -> &'static str {
        match self {
            Accel::Hvf => "hvf",
            Accel::Kvm => "kvm",
            Accel::Tcg => "tcg",
        }
    }
}

pub struct Platform {
    pub binary: PathBuf,
    pub machine: &'static str,
    pub accel: Accel,
    pub console: &'static str,
    /// Why no hardware acceleration is used (when accel is TCG).
    pub tcg_reason: Option<String>,
}

impl Platform {
    pub fn detect(image_arch: &str) -> Result<Self> {
        let (binary, machine, console) = match image_arch {
            "arm64" => ("qemu-system-aarch64", "virt", "ttyAMA0"),
            "amd64" => ("qemu-system-x86_64", "q35", "ttyS0"),
            other => bail!("unsupported image architecture: {other}"),
        };
        let binary = which(binary)
            .with_context(|| format!("{binary} not found in PATH (brew/apt install qemu)"))?;

        let native = matches!(
            (image_arch, std::env::consts::ARCH),
            ("arm64", "aarch64") | ("amd64", "x86_64")
        );
        let (accel, tcg_reason) = if !native {
            (
                Accel::Tcg,
                Some(format!(
                    "{image_arch} image on a {} host",
                    std::env::consts::ARCH
                )),
            )
        } else if cfg!(target_os = "macos") {
            (Accel::Hvf, None)
        } else {
            match kvm_usable() {
                Ok(()) => (Accel::Kvm, None),
                Err(reason) => (Accel::Tcg, Some(reason)),
            }
        };
        Ok(Self {
            binary,
            machine,
            accel,
            console,
            tcg_reason,
        })
    }

    /// qemu-img shipped next to qemu-system-* (avoids e.g. an Android SDK copy earlier in PATH).
    pub fn qemu_img(&self) -> PathBuf {
        self.binary
            .parent()
            .map(|d| d.join("qemu-img"))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("qemu-img"))
    }
}

fn kvm_usable() -> std::result::Result<(), String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .map(|_| ())
        .map_err(|e| format!("cannot open /dev/kvm: {e}"))
}

/// QEMU splits -drive/-qmp options on ',', so paths must not contain one.
fn qemu_path(p: &Path) -> Result<String> {
    let s = p
        .to_str()
        .with_context(|| format!("non UTF-8 path {}", p.display()))?;
    if s.contains(',') {
        bail!("path contains ',' which QEMU cannot parse: {s}");
    }
    Ok(s.to_string())
}

pub fn create_overlay(platform: &Platform, base: &BaseImage, overlay: &Path) -> Result<()> {
    let mut cmd = Command::new(platform.qemu_img());
    cmd.args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(base.disk())
        .arg(overlay);
    qemu_img(cmd)
}

/// Empty, sparse qcow2 disk (size as understood by qemu-img, e.g. 16G).
pub fn create_blank(platform: &Platform, path: &Path, size: &str) -> Result<()> {
    if size.is_empty() || !size.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("invalid disk size '{size}' (e.g. 16G)");
    }
    let mut cmd = Command::new(platform.qemu_img());
    cmd.args(["create", "-q", "-f", "qcow2"])
        .arg(path)
        .arg(size);
    qemu_img(cmd)
}

fn qemu_img(mut cmd: Command) -> Result<()> {
    let out = cmd.output().context("running qemu-img")?;
    if !out.status.success() {
        bail!(
            "qemu-img create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Standard base64 (RFC 4648, with padding).
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn kernel_cmdline(platform: &Platform, image: &BaseImage, authorized_key: &str) -> String {
    let mut parts = vec![
        "root=LABEL=sedir-root".to_string(),
        "rw".to_string(),
        format!("console={}", platform.console),
    ];
    for p in image.cmdline().split_whitespace() {
        // vsyscall= is x86-only.
        if image.arch != "amd64" && p.starts_with("vsyscall=") {
            continue;
        }
        parts.push(p.to_string());
    }
    // SSH public key for the admin user, read by sedir-guest. A public key
    // is not secret, so the world-readable kernel command line is fine.
    parts.push(format!(
        "systemd.set_credential_binary=sedir.ssh.authorized_keys:{}",
        base64(format!("{}\n", authorized_key.trim()).as_bytes())
    ));
    parts.join(" ")
}

/// Start QEMU daemonized. Returns once QEMU has forked into the background.
pub fn spawn(
    platform: &Platform,
    vm: &VmState,
    image: &BaseImage,
    authorized_key: &str,
) -> Result<()> {
    let cpu = if platform.accel == Accel::Tcg {
        "max"
    } else {
        "host"
    };

    let mut cmd = Command::new(&platform.binary);
    cmd.arg("-name")
        .arg(format!("sedir-{}", vm.name))
        .args([
            "-machine",
            platform.machine,
            "-accel",
            platform.accel.as_str(),
        ])
        .args(["-cpu", cpu])
        .arg("-smp")
        .arg(vm.cpus.to_string())
        .arg("-m")
        .arg(format!("{}M", vm.memory_mib))
        .args(["-nodefaults", "-display", "none", "-monitor", "none"])
        // Direct kernel boot: no firmware/bootloader, fastest path to userspace.
        .arg("-kernel")
        .arg(qemu_path(&image.kernel())?)
        .arg("-initrd")
        .arg(qemu_path(&image.initrd())?)
        .arg("-append")
        .arg(kernel_cmdline(platform, image, authorized_key))
        .arg("-drive")
        .arg(format!(
            "if=none,id=root,format=qcow2,discard=unmap,file={}",
            qemu_path(&vm.overlay())?
        ))
        .args(["-device", "virtio-blk-pci,drive=root"]);
    if vm.data_disk().is_file() {
        cmd.arg("-drive")
            .arg(format!(
                "if=none,id=data,format=qcow2,discard=unmap,file={}",
                qemu_path(&vm.data_disk())?
            ))
            .args(["-device", "virtio-blk-pci,drive=data,serial=sedir-data"]);
    }
    cmd
        // User-mode networking: no root, no host bridge; SSH bound to loopback only.
        .arg("-netdev")
        .arg(netdev(vm))
        .args(["-device", "virtio-net-pci,netdev=net0"])
        // Entropy for first-boot SSH host key generation.
        .args(["-device", "virtio-rng-pci"])
        .arg("-serial")
        .arg(format!("file:{}", qemu_path(&vm.serial_log())?))
        .arg("-qmp")
        .arg(format!(
            "unix:{},server=on,wait=off",
            qemu_path(&vm.qmp_socket())?
        ))
        .arg("-pidfile")
        .arg(qemu_path(&vm.pidfile())?)
        .arg("-daemonize")
        .stdin(Stdio::null());

    let out = cmd
        .output()
        .with_context(|| format!("starting {}", platform.binary.display()))?;
    if !out.status.success() {
        bail!(
            "QEMU failed to start: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// User-mode network: SSH and the VM's port forwards, bound to loopback only.
fn netdev(vm: &VmState) -> String {
    let mut s = format!("user,id=net0,hostfwd=tcp:127.0.0.1:{}-:22", vm.ssh_port);
    for f in &vm.forwards {
        s.push_str(&format!(",hostfwd=tcp:127.0.0.1:{}-:{}", f.host, f.guest));
    }
    s
}

/// Add or remove a port forward of a running VM (QEMU monitor commands
/// hostfwd_add / hostfwd_remove).
pub fn qmp_hostfwd(socket: &Path, add: bool, host: u16, guest: u16) -> Result<()> {
    let line = if add {
        format!("hostfwd_add net0 tcp:127.0.0.1:{host}-:{guest}")
    } else {
        format!("hostfwd_remove net0 tcp:127.0.0.1:{host}")
    };
    let (mut writer, mut reader) = qmp_connect(socket)?;
    qmp_command(&mut writer, &mut reader, "qmp_capabilities")?;
    writeln!(
        writer,
        "{{\"execute\":\"human-monitor-command\",\"arguments\":{{\"command-line\":\"{line}\"}}}}"
    )?;
    let reply = qmp_reply(&mut reader, "human-monitor-command")?;
    // HMP answers in text inside an otherwise successful reply: nothing for
    // a successful add, "... removed" for a successful remove, an error
    // message otherwise.
    let text = hmp_text(&reply);
    let ok = if add {
        text.is_empty()
    } else {
        text.ends_with("removed")
    };
    if !ok {
        bail!("QEMU refused '{line}': {text}");
    }
    Ok(())
}

/// The text of a human-monitor-command reply: {"return": "<text>"}.
fn hmp_text(reply: &str) -> String {
    reply
        .split_once("\"return\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .and_then(|(_, rest)| rest.rsplit_once('"'))
        .map(|(text, _)| text.replace("\\r", "").replace("\\n", " "))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn qmp_connect(socket: &Path) -> Result<(UnixStream, BufReader<UnixStream>)> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to QMP socket {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut greeting = String::new();
    reader.read_line(&mut greeting)?;
    if !greeting.contains("QMP") {
        bail!("unexpected QMP greeting: {}", greeting.trim());
    }
    Ok((writer, reader))
}

/// Ask the guest to power off via ACPI / GPIO power button.
pub fn qmp_powerdown(socket: &Path) -> Result<()> {
    let (mut writer, mut reader) = qmp_connect(socket)?;
    qmp_command(&mut writer, &mut reader, "qmp_capabilities")?;
    qmp_command(&mut writer, &mut reader, "system_powerdown")?;
    Ok(())
}

fn qmp_command(
    writer: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
    command: &str,
) -> Result<()> {
    writeln!(writer, "{{\"execute\":\"{command}\"}}")?;
    qmp_reply(reader, command).map(|_| ())
}

/// The reply line to the last command; asynchronous events are skipped.
fn qmp_reply(reader: &mut BufReader<UnixStream>, command: &str) -> Result<String> {
    for _ in 0..16 {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("QMP connection closed during '{command}'");
        }
        if line.contains("\"return\"") {
            return Ok(line);
        }
        if line.contains("\"error\"") {
            bail!("QMP '{command}' failed: {}", line.trim());
        }
    }
    bail!("no QMP reply for '{command}'")
}

#[cfg(test)]
mod tests {
    use super::{base64, hmp_text};

    #[test]
    fn hmp_reply_text() {
        assert_eq!(hmp_text("{\"return\": \"\"}\r\n"), "");
        assert_eq!(
            hmp_text("{\"return\": \"host forwarding rule for tcp:127.0.0.1:1 removed\\r\\n\"}"),
            "host forwarding rule for tcp:127.0.0.1:1 removed"
        );
        assert_eq!(
            hmp_text("{\"return\": \"Could not set up host forwarding rule 'x'\\r\\n\"}"),
            "Could not set up host forwarding rule 'x'"
        );
    }

    #[test]
    fn base64_rfc4648_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }
}
