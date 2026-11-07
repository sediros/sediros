# Changelog

Notable changes per Sisura release. A release is `<debian major>.<point>` and is tagged `v<release>` in git; see "Release numbers" in the README. Package and tool versions are listed when they change.

## [13.1] - unreleased

First release, based on Debian 13 (trixie).

### Distribution
- Minimal Debian trixie image (debootstrap minbase) as a qcow2 VM image and a live/installer ISO (amd64, arm64).
- `os-release` identifies the system as Sisura Linux (`ID=sisura`, `ID_LIKE=debian`); Debian's file is kept via `dpkg-divert`.
- `/etc/sisura/build-info` records release, build id, build date and profile.

### Hardening (`sisura-hardening` 0.3.0, `sisura-boot-hardening` 0.2.0)
- sysctl hardening (63 keys), AppArmor, UFW with incoming traffic denied and SSH rate-limited.
- Key-only SSH with modern ciphers, `AllowGroups sudo sisura-ssh`.
- nftables rule dropping IPv4 TCP packets with IP options (mitigation for CVE-2026-98432-type handshake bugs).
- Automatic installation of Debian security updates (unattended-upgrades), login notice when a reboot is required.
- sudo policy (pty, audit log, 5-minute timestamp), `pam_faillock` (5 failures in 15 minutes lock the account for 15 minutes), `pwquality` (12 characters, dictionary check).
- SUID/SGID removal outside an allowlist, re-applied after every dpkg run; kernel module blacklist; core dumps disabled.
- Kernel hardening parameters on every GRUB entry; optional GRUB password (`sisura-grub-password`) that protects entry editing while the default entry still boots unattended.

### Updates (`sisura-archive-keyring` 0.1.0, `sisura-hardening` 0.5.1)
- Sisura apt repository (`https://sisura-linux.github.io/sisura/apt`), built from the packages of each published release and signed in CI; Sisura's own packages now receive fixes through apt, not only with a new image.
- `sisura-archive-keyring`: the repository's signing key and source, bound to each other with `Signed-By`, and an apt pin that only accepts `sisura-*` packages from the repository (matched by host name), so even a stolen signing key cannot replace Debian packages.
- unattended-upgrades also installs updates of Sisura's packages.
- `sudo sisura-ssh-enroll`: adds the first SSH key with `ssh-copy-id` through a short password login window (calling user only, local networks only unless `--anywhere`, closed when the key arrives, after 10 minutes or on reboot). In the live ISO session it refuses and says to install or boot the installed disk.
- `make apt-test` updates a VM through a test repository: signature accepted, unattended update installed, foreign package refused by the pin, repository signed with another key rejected, hardening intact afterwards.

### Audit (`sisura-audit` 0.2.0)
- `sudo sisura-audit` checks that the hardening is still in place on a running system (kernel settings, SSH, firewall, AppArmor, setuid programs, accounts, sudo, PAM, kernel modules, automatic updates, apt repository, boot parameters, disk encryption, Secure Boot). Expected values come from the installed Sisura packages; changes are reported with the file that made them where possible. `--json`, `--quiet`; exit status 1 on failures.
- In every image; the smoke test runs it next to its own checks.
- Warns while a `sisura-ssh-enroll` window is open.

### Installer (`sisura-install` 0.3.3, `sisura-live` 0.2.2)
- Live session with console autologin and the Sisura login banner; nothing written to disk. It warns when a disk already holds a Sisura installation (the ISO was booted again instead of the installed system), and the installer's last message says what to do without an SSH key.
- Installs to disk with GPT, an EFI system partition, an unencrypted `/boot` and a LUKS2 (argon2id) root.
- Asks for disk, hostname, keyboard layout, admin user, sudo password, SSH keys (pasted or from GitHub) and GRUB password; validates every answer before touching the disk. All answers are also available as flags.
- Keyboard layout applies to the LUKS prompt at boot. The layout chosen in the wizard also applies to the live console right away (it did not when the installer ran under sudo), and the dozens of harmless `ckbcomp` warnings it caused while building the initramfs are no longer shown. Copying the system no longer prints hundreds of harmless "Operation not supported" warnings: the live squashfs has no extended attributes to copy, and the EFI partition's mount point (vfat) takes no ACLs.
- Installed over a serial console (VPS, server, VM): the installed system keeps using it for GRUB, kernel messages and the LUKS prompt.
- Installed from the screen: the LUKS prompt stays on the screen (`console=tty0`), also on arm64 VMs and servers whose firmware names a serial console (ACPI SPCR). VMs with a virtio-gpu display (UTM, QEMU) show it too: the driver is now in the initramfs (`sisura-boot-hardening` 0.2.0); before, the screen stayed at "Display output is not active" until the disk was unlocked.
- Optional disk unlock over SSH at boot (dropbear in the initramfs, port 2222, key-only, restricted to `cryptroot-unlock`, per-machine host key).
- Secure Boot: the ISO and installed systems boot with Debian's signed chain (Microsoft-signed `shim`, Debian-signed GRUB and kernel); tested with firmware that enforces Secure Boot, for the live ISO, the installation and the installed system.

### Tools (`sisura` CLI 0.6.0)
- `sisura build/launch/shell/list/stop/purge`: QEMU VMs with HVF (macOS) or KVM (Linux), copy-on-write disks, SSH on localhost only.
- `launch --data-disk` attaches an empty second disk; `list` shows each VM's build id; `sisura` without a command shows the help with a getting-started section.
- `sisura pull` downloads a released VM image from GitHub and verifies it against the release's SHA256SUMS; no source checkout needed.
- Images contain no SSH key: the CLI passes its own key to each VM at boot.
- `sisura transfer` copies files and directories (`-r`) between the host and a running VM over the same SSH connection as `shell`; the VM side is written `<vm>:<path>`.
- `sisura forward dev 8080:80` (or `launch --forward`) forwards a port on 127.0.0.1 to a VM, live or at launch; kept with the VM, shown by `list`, and the VM's firewall is opened for this machine only.
- `sisura snapshot` / `sisura restore`: snapshots of a stopped VM (qcow2 internal snapshots of its disks, only changed blocks are kept); list with `sisura snapshot <vm>`, delete with `--delete`.
- `make test` boots a throwaway VM and verifies the hardening, packages and build id; `make apt-test` checks updates through the apt repository; `make iso-test` and `make install-test` check the live ISO, an installation and the installed system with Secure Boot enforced.

### Known limitations
- SSH disk unlock uses DHCP only (no static IP yet).
