//! The installation itself: partition, encrypt, copy, configure, boot loader.
//!
//! Layout (GPT, UEFI):
//!   1  512 MiB  ESP    FAT32  /boot/efi
//!   2    1 GiB  boot   ext4   /boot       (unencrypted: kernel + initramfs,
//!                                         which asks for the LUKS passphrase)
//!   3     rest  root   LUKS2 -> ext4 /

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cmd::{self, chroot, chroot_filtered, run, run_with_stdin};
use crate::disk::{self, Disk};
use crate::secret::Secret;
use crate::setup::{self, Setup};

/// Name of the unlocked root volume (/dev/mapper/<name>), also in crypttab.
pub const CRYPT_NAME: &str = "sisura-root";
/// Labels are distinct from the VM images' root (LABEL=sisura-root): a disk
/// carrying both must never make the initramfs pick the wrong one.
const LUKS_LABEL: &str = "sisura-crypt";
const ROOT_FS_LABEL: &str = "sisura-system";
/// Where live-boot mounts the read-only squashfs (the pristine system image).
pub const LIVE_SQUASHFS: &str = "/run/live/rootfs/filesystem.squashfs";
/// Live-session-only packages, removed from the installed system.
const LIVE_PACKAGES: &[&str] = &[
    "sisura-live",
    "sisura-install",
    "live-boot",
    "live-boot-initramfs-tools",
];

pub struct Options {
    pub disk: Disk,
    pub source: PathBuf,
    pub target: PathBuf,
    pub hostname: String,
    pub passphrase: Secret,
    pub setup: Setup,
    /// Install even though the running system did not boot via UEFI.
    pub skip_uefi_check: bool,
}

/// Undo mounts and close LUKS on every exit path (success, error, panic=abort
/// excluded). Steps register what they set up; Drop tears down in reverse.
struct Teardown {
    target: PathBuf,
    mounted: bool,
    luks_open: bool,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        if self.mounted {
            println!("==> Unmounting {}", self.target.display());
            let t = self.target.to_string_lossy().into_owned();
            cmd::try_run("sync", &[]);
            cmd::try_run("umount", &["-R", &t]);
        }
        if self.luks_open {
            cmd::try_run("cryptsetup", &["close", CRYPT_NAME]);
        }
    }
}

fn step(msg: &str) {
    println!("\n==> {msg}");
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("non UTF-8 path {}", p.display()))
}

fn blkid(dev: &Path, tag: &str) -> Result<String> {
    let value = cmd::output("blkid", &["-s", tag, "-o", "value", path_str(dev)?])?;
    if value.is_empty() {
        bail!("blkid: no {tag} on {}", dev.display());
    }
    Ok(value)
}

fn grub_target() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => Ok("arm64-efi"),
        "x86_64" => Ok("x86_64-efi"),
        other => bail!("unsupported architecture {other}"),
    }
}

/// Wait for udev to create partition nodes after re-partitioning.
fn wait_for(paths: &[PathBuf]) -> Result<()> {
    cmd::try_run("udevadm", &["settle", "--timeout=10"]);
    for _ in 0..50 {
        if paths.iter().all(|p| p.exists()) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("partition device nodes did not appear: {paths:?}");
}

pub fn preflight(opts: &Options) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("must run as root");
    }
    // Only UEFI installs are supported (GPT + ESP, GRUB for EFI). A live ISO
    // started in legacy BIOS/CSM mode would produce a disk that never boots
    // on that machine.
    if !opts.skip_uefi_check && !Path::new("/sys/firmware/efi").is_dir() {
        bail!(
            "this system was not booted in UEFI mode; Sisura installs for UEFI only.\n  \
             Enable UEFI (disable legacy/CSM boot) in the firmware settings and boot the ISO again."
        );
    }
    for tool in [
        "sfdisk",
        "wipefs",
        "mkfs.vfat",
        "mkfs.ext4",
        "cryptsetup",
        "blkid",
        "tar",
        "udevadm",
    ] {
        if cmd::output("sh", &["-c", &format!("command -v {tool}")]).is_err() {
            bail!("required tool '{tool}' not found");
        }
    }
    let src = &opts.source;
    if !src.join("usr/lib/os-release").is_file() || !src.join("boot").is_dir() {
        bail!(
            "{} does not look like a Sisura root filesystem (usr/lib/os-release, boot/)",
            src.display()
        );
    }
    if fs::read_dir(src.join("boot"))?
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with("vmlinuz-"))
    {
        bail!("no kernel in {}/boot", src.display());
    }
    if Path::new("/dev/mapper").join(CRYPT_NAME).exists() {
        bail!("/dev/mapper/{CRYPT_NAME} already exists (close it first)");
    }
    if opts.target.exists() && fs::read_dir(&opts.target)?.next().is_some() {
        bail!("target directory {} is not empty", opts.target.display());
    }
    disk::check_target(&opts.disk)
}

pub fn run_install(opts: &Options) -> Result<()> {
    let disk = &opts.disk;
    let dev = disk.path();
    let (esp, boot, root) = (disk.partition(1), disk.partition(2), disk.partition(3));
    let mapper = PathBuf::from("/dev/mapper").join(CRYPT_NAME);
    let target = &opts.target;
    let t = path_str(target)?;

    let mut teardown = Teardown {
        target: target.clone(),
        mounted: false,
        luks_open: false,
    };

    step(&format!("Partitioning {}", dev.display()));
    run("wipefs", &["--all", "--force", path_str(&dev)?])?;
    run_with_stdin(
        "sfdisk",
        &[
            "--wipe",
            "always",
            "--wipe-partitions",
            "always",
            path_str(&dev)?,
        ],
        b"label: gpt\n\
          size=512MiB, type=U, name=\"sisura-esp\"\n\
          size=1GiB, type=L, name=\"sisura-boot\"\n\
          type=L, name=\"sisura-root\"\n",
    )?;
    wait_for(&[esp.clone(), boot.clone(), root.clone()])?;

    step("Creating file systems");
    run(
        "mkfs.vfat",
        &["-F", "32", "-n", "SISURA-ESP", path_str(&esp)?],
    )?;
    run(
        "mkfs.ext4",
        &["-q", "-F", "-L", "sisura-boot", path_str(&boot)?],
    )?;

    step("Encrypting the root partition (LUKS2, argon2id)");
    // 512 MiB argon2 memory: unlockable on small machines too (the default
    // scales with the RAM of the machine that formats the volume).
    run_with_stdin(
        "cryptsetup",
        &[
            "luksFormat",
            "--batch-mode",
            "--type",
            "luks2",
            "--pbkdf",
            "argon2id",
            "--pbkdf-memory",
            "524288",
            "--iter-time",
            "2000",
            "--label",
            LUKS_LABEL,
            "--key-file",
            "-",
            path_str(&root)?,
        ],
        opts.passphrase.bytes(),
    )?;
    run_with_stdin(
        "cryptsetup",
        &[
            "open",
            "--allow-discards",
            "--key-file",
            "-",
            path_str(&root)?,
            CRYPT_NAME,
        ],
        opts.passphrase.bytes(),
    )?;
    teardown.luks_open = true;
    run(
        "mkfs.ext4",
        &["-q", "-F", "-L", ROOT_FS_LABEL, path_str(&mapper)?],
    )?;

    step(&format!("Mounting at {t}"));
    fs::create_dir_all(target)?;
    // Private self-bind first: "/" is a shared mount under systemd, so mounts
    // made below the target would otherwise propagate to any other bind of
    // "/" and keep the LUKS volume busy after 'umount -R'.
    run("mount", &["--bind", t, t])?;
    teardown.mounted = true;
    run("mount", &["--make-rprivate", t])?;
    run("mount", &[path_str(&mapper)?, t])?;
    fs::create_dir_all(target.join("boot"))?;
    run("mount", &[path_str(&boot)?, &format!("{t}/boot")])?;
    fs::create_dir_all(target.join("boot/efi"))?;
    run(
        "mount",
        &[
            "-o",
            "umask=0077",
            path_str(&esp)?,
            &format!("{t}/boot/efi"),
        ],
    )?;

    step(&format!(
        "Copying the system from {}",
        opts.source.display()
    ));
    cmd::copy_tree(
        &opts.source,
        target,
        // Empty in the squashfs; matter when --source is a running system.
        &[
            "./proc/*",
            "./sys/*",
            "./dev/*",
            "./run/*",
            "./tmp/*",
            "./mnt/*",
            "./media/*",
            // The ESP mount point itself too: tar would try to set POSIX
            // ACLs on it, which vfat rejects ("Operation not supported").
            "./boot/efi",
            "./boot/initrd.img-*",
            "./etc/sudoers.d/90-sisura-live",
        ],
    )?;

    step("Writing fstab, crypttab, hostname");
    let luks_uuid = blkid(&root, "UUID")?;
    let boot_uuid = blkid(&boot, "UUID")?;
    let esp_uuid = blkid(&esp, "UUID")?;
    write_fstab(target, &boot_uuid, &esp_uuid)?;
    fs::write(
        target.join("etc/crypttab"),
        format!(
            "# Written by sisura-install\n\
             # <name>     <device>                                   <key>  <options>\n\
             {CRYPT_NAME}  UUID={luks_uuid}  none   luks,discard\n"
        ),
    )?;
    fs::create_dir_all(target.join("etc/cryptsetup-initramfs"))?;
    fs::write(
        target.join("etc/cryptsetup-initramfs/conf-hook"),
        "# Written by sisura-install: root is on LUKS, always include cryptsetup.\nCRYPTSETUP=y\n",
    )?;
    write_hostname(target, &opts.hostname)?;
    stamp_install(target)?;
    setup::write_keyboard(target, &opts.setup.keyboard)?;

    step("Preparing the chroot");
    for (src, dst) in [("/dev", "dev"), ("/sys", "sys")] {
        run("mount", &["--rbind", src, &format!("{t}/{dst}")])?;
        run("mount", &["--make-rslave", &format!("{t}/{dst}")])?;
    }
    run("mount", &["-t", "proc", "proc", &format!("{t}/proc")])?;
    // A private /run: maintainer scripts must not see the live system's
    // systemd (they would start/stop its services). udev data is needed by
    // grub-probe and the cryptsetup initramfs hook.
    run(
        "mount",
        &[
            "-t",
            "tmpfs",
            "-o",
            "mode=0755,nosuid,nodev",
            "tmpfs",
            &format!("{t}/run"),
        ],
    )?;
    fs::create_dir_all(target.join("run/udev"))?;
    run("mount", &["--bind", "/run/udev", &format!("{t}/run/udev")])?;
    // No service starts/stops from maintainer scripts while installing.
    let policy = target.join("usr/sbin/policy-rc.d");
    fs::write(&policy, "#!/bin/sh\nexit 101\n")?;
    run("chmod", &["0755", path_str(&policy)?])?;

    let result = configure_chroot(target, &opts.setup);
    let _ = fs::remove_file(&policy);
    let unlock_fingerprint = result?;

    step("Done");
    drop(teardown);
    println!(
        "\nSisura is installed on {}. Remove the installation medium (ISO, USB stick)\n\
         and reboot; otherwise the installer starts again instead of Sisura.",
        dev.display()
    );
    if opts.setup.account.ssh_keys.is_empty() {
        println!(
            "\nNo SSH key was given, and SSH accepts keys only. After the reboot, log in\n\
             on this console and run\n  sudo sisura-ssh-enroll\n\
             then follow its instructions (ssh-copy-id from your computer)."
        );
    }
    if let (Some(fp), Some(port)) = (unlock_fingerprint, opts.setup.ssh_unlock_port) {
        println!(
            "\nUnlock the disk over SSH at boot:\n  ssh -p {port} root@<address of this machine>\n\
             Check the host key on first connect: {fp} (ED25519)"
        );
    }
    Ok(())
}

/// Returns the initramfs SSH host key fingerprint when SSH unlock is enabled.
fn configure_chroot(target: &Path, setup: &Setup) -> Result<Option<String>> {
    step("Removing live-session packages");
    let installed: Vec<&str> = LIVE_PACKAGES
        .iter()
        .copied()
        .filter(|p| {
            cmd::output(
                "chroot",
                &[
                    target.to_str().unwrap_or("/"),
                    "dpkg-query",
                    "-W",
                    "-f=${Status}",
                    p,
                ],
            )
            .map(|s| s.ends_with(" installed"))
            .unwrap_or(false)
        })
        .collect();
    if !installed.is_empty() {
        let mut args = vec!["--purge"];
        args.extend(&installed);
        chroot(target, "dpkg", &args)?;
    }

    step(&format!(
        "Configuring the admin account '{}'",
        setup.account.user
    ));
    setup::configure_account(target, &setup.account)?;

    let fingerprint = match setup.ssh_unlock_port {
        Some(port) if !setup.account.ssh_keys.is_empty() => {
            step(&format!(
                "Enabling SSH unlock in the initramfs (port {port})"
            ));
            Some(setup::enable_ssh_unlock(
                target,
                &setup.account.ssh_keys,
                port,
            )?)
        }
        _ => {
            step("Removing dropbear (no SSH unlock)");
            setup::remove_ssh_unlock(target)?;
            None
        }
    };

    // Plain `dpkg --purge` (above) leaves triggers it activated pending,
    // e.g. shim-signed's; apt would run them at its end. Without this the
    // installed system starts with packages that `dpkg --audit` reports.
    // Both can build an initramfs, whose keymap step is noisy (cmd::NOISE).
    chroot_filtered(target, "dpkg", &["--configure", "--pending"])?;

    step("Building the initramfs (cryptsetup, keyboard layout)");
    chroot_filtered(target, "update-initramfs", &["-c", "-k", "all"])?;

    if let Some(tty) = serial_console() {
        step(&format!(
            "Serial console {tty} for GRUB, the kernel and the LUKS prompt"
        ));
        write_serial_console(target, &tty)?;
    }

    step("Installing GRUB (UEFI)");
    let grub = grub_target()?;
    let efivars = Path::new("/sys/firmware/efi/efivars")
        .read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    let mut args = vec![
        format!("--target={grub}"),
        "--efi-directory=/boot/efi".to_string(),
        "--bootloader-id=sisura".to_string(),
        // shim (signed by Microsoft) + Debian's signed GRUB: boots with
        // Secure Boot on, and without it.
        "--uefi-secure-boot".to_string(),
    ];
    if !efivars {
        // Not booted via UEFI (or no efivars): no NVRAM boot entry possible.
        args.push("--no-nvram".to_string());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    chroot(target, "grub-install", &refs)?;
    // Also the fallback path EFI/BOOT/BOOT*.EFI: boots even when the firmware
    // forgets (or never had) the NVRAM entry, e.g. VMs and moved disks.
    chroot(
        target,
        "grub-install",
        &[
            &format!("--target={grub}"),
            "--efi-directory=/boot/efi",
            "--removable",
            "--uefi-secure-boot",
            "--no-nvram",
        ],
    )?;
    match &setup.grub_password {
        Some(pw) => {
            step("Setting the GRUB password");
            setup::set_grub_password(target, pw)?;
        }
        None => chroot(target, "update-grub", &[])?,
    }
    Ok(fingerprint)
}

/// Serial console the installer runs on (ttyS0, ttyAMA0, ...), if any:
/// like the Debian installer, a system installed over a serial console
/// (VPS, server, VM) keeps using it.
fn serial_console() -> Option<String> {
    console()
        .filter(|(_, serial)| *serial)
        .map(|(name, _)| name)
}

/// The real terminal the installer was started from: (device name, is it a
/// serial port), e.g. ("ttyS0", true) or ("tty1", false).
///
/// sudo with `use_pty` (Sisura's policy) runs the installer on a new
/// pseudo-terminal, so this walks up the parent processes to the first
/// one attached to a real terminal: a serial port or a virtual console.
pub fn console() -> Option<(String, bool)> {
    let mut pid = std::process::id();
    for _ in 0..16 {
        for fd in 0..3 {
            let Ok(link) = fs::read_link(format!("/proc/{pid}/fd/{fd}")) else {
                continue;
            };
            let Some(name) = link.strip_prefix("/dev").ok().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(serial) = console_kind(name) {
                return Some((name.to_string(), serial));
            }
        }
        let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        pid = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))?
            .trim()
            .parse()
            .ok()?;
        if pid <= 1 {
            return None;
        }
    }
    None
}

/// Some(true): serial port, Some(false): virtual console, None: anything
/// else (pseudo-terminal, pipe, file).
fn console_kind(dev: &str) -> Option<bool> {
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if let Some(unit) = dev
        .strip_prefix("ttyS")
        .or_else(|| dev.strip_prefix("ttyAMA"))
    {
        return digits(unit).then_some(true);
    }
    dev.strip_prefix("tty").filter(|n| digits(n)).map(|_| false)
}

fn write_serial_console(target: &Path, tty: &str) -> Result<()> {
    let mut cfg = format!(
        "# Written by sisura-install: installed over the serial console {tty}.\n\
         # Kernel messages go to the screen (tty0) and {tty}; the LUKS prompt\n\
         # and the boot log go to {tty}, the last console listed.\n\
         GRUB_CMDLINE_LINUX=\"${{GRUB_CMDLINE_LINUX:+$GRUB_CMDLINE_LINUX }}console=tty0 console={tty},115200\"\n"
    );
    if let Some(unit) = tty.strip_prefix("ttyS") {
        // PC serial port: GRUB's own menu on it as well (UEFI consoles on
        // arm64 already include the serial port).
        cfg.push_str(&format!(
            "GRUB_TERMINAL=\"console serial\"\n\
             GRUB_SERIAL_COMMAND=\"serial --unit={unit} --speed=115200\"\n"
        ));
    }
    let dir = target.join("etc/default/grub.d");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("80-sisura-console.cfg"), cfg)?;
    Ok(())
}

fn write_fstab(target: &Path, boot_uuid: &str, esp_uuid: &str) -> Result<()> {
    let path = target.join("etc/fstab");
    let old = fs::read_to_string(&path).unwrap_or_default();
    // Keep the image's tmpfs lines, replace every disk mount.
    let kept: Vec<&str> = old
        .lines()
        .filter(|l| l.split_whitespace().next() == Some("tmpfs"))
        .collect();
    let mut fstab = format!(
        "# Written by sisura-install\n\
         # <device>                 <mount>    <type> <options>                               <dump> <pass>\n\
         /dev/mapper/{CRYPT_NAME}    /          ext4   defaults,noatime,errors=remount-ro       0 1\n\
         UUID={boot_uuid}  /boot  ext4   defaults,noatime,nodev,nosuid,noexec     0 2\n\
         UUID={esp_uuid}                             /boot/efi  vfat   umask=0077,shortname=winnt              0 1\n"
    );
    for l in kept {
        fstab.push_str(l);
        fstab.push('\n');
    }
    fs::write(path, fstab)?;
    Ok(())
}

/// "13.1 build 20261105.1432-47888b3" of a root filesystem, from its
/// /etc/sisura/build-info.
pub fn build_label(root: &Path) -> String {
    let info = fs::read_to_string(root.join("etc/sisura/build-info")).unwrap_or_default();
    let get = |k: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or("?")
            .to_string()
    };
    format!("{} build {}", get("RELEASE"), get("BUILD_ID"))
}

/// Record when this system was installed, next to the image's build id.
fn stamp_install(target: &Path) -> Result<()> {
    let path = target.join("etc/sisura/build-info");
    let mut info = fs::read_to_string(&path).unwrap_or_default();
    let now = cmd::output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])?;
    info.push_str(&format!("INSTALLED={now}\n"));
    fs::write(path, info)?;
    Ok(())
}

pub fn valid_hostname(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 63
        && !h.starts_with('-')
        && !h.ends_with('-')
        && h.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn write_hostname(target: &Path, hostname: &str) -> Result<()> {
    fs::write(target.join("etc/hostname"), format!("{hostname}\n"))?;
    let hosts = target.join("etc/hosts");
    let old = fs::read_to_string(&hosts).unwrap_or_default();
    let new: Vec<String> = old
        .lines()
        .map(|l| {
            if l.starts_with("127.0.1.1") {
                format!("127.0.1.1   {hostname}")
            } else {
                l.to_string()
            }
        })
        .collect();
    fs::write(hosts, new.join("\n") + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::console_kind;

    #[test]
    fn console_kinds() {
        assert_eq!(console_kind("ttyS0"), Some(true));
        assert_eq!(console_kind("ttyAMA0"), Some(true));
        assert_eq!(console_kind("tty1"), Some(false));
        assert_eq!(console_kind("pts/0"), None);
        assert_eq!(console_kind("ttyS"), None);
        assert_eq!(console_kind("tty"), None);
        assert_eq!(console_kind("null"), None);
    }
}
