# Changelog

Notable changes per Sisura release. A release is `<debian major>.<point>` and is tagged `v<release>` in git; see "Release numbers" in the README. Package and tool versions are listed when they change.

## [13.1] - unreleased

First release, based on Debian 13 (trixie).

### Distribution
- Minimal Debian trixie image (debootstrap minbase) as a qcow2 VM image and a live/installer ISO (amd64, arm64).
- `os-release` identifies the system as Sisura Linux (`ID=sisura`, `ID_LIKE=debian`); Debian's file is kept via `dpkg-divert`.
- `/etc/sisura/build-info` records release, build id, build date and profile.

### Hardening (`sisura-hardening` 0.3.0, `sisura-boot-hardening` 0.1.0)
- sysctl hardening (63 keys), AppArmor, UFW with incoming traffic denied and SSH rate-limited.
- Key-only SSH with modern ciphers, `AllowGroups sudo sisura-ssh`.
- nftables rule dropping IPv4 TCP packets with IP options (mitigation for CVE-2026-98432-type handshake bugs).
- Automatic installation of Debian security updates (unattended-upgrades), login notice when a reboot is required.
- sudo policy (pty, audit log, 5-minute timestamp), `pam_faillock` (5 failures in 15 minutes lock the account for 15 minutes), `pwquality` (12 characters, dictionary check).
- SUID/SGID removal outside an allowlist, re-applied after every dpkg run; kernel module blacklist; core dumps disabled.
- Kernel hardening parameters on every GRUB entry; optional GRUB password (`sisura-grub-password`) that protects entry editing while the default entry still boots unattended.

### Updates (`sisura-archive-keyring` 0.1.0, `sisura-hardening` 0.4.0)
- Sisura apt repository (`https://sisura-linux.github.io/sisura/apt`), built from the packages of each published release and signed in CI; Sisura's own packages now receive fixes through apt, not only with a new image.
- `sisura-archive-keyring`: the repository's signing key and source, bound to each other with `Signed-By`, and an apt pin that only accepts `sisura-*` packages from the repository (matched by host name), so even a stolen signing key cannot replace Debian packages.
- unattended-upgrades also installs updates of Sisura's packages.
- `make apt-test` updates a VM through a test repository: signature accepted, unattended update installed, foreign package refused by the pin, repository signed with another key rejected, hardening intact afterwards.

### Installer (`sisura-install` 0.3.0, `sisura-live` 0.2.1)
- Live session with console autologin and the Sisura login banner; nothing written to disk.
- Installs to disk with GPT, an EFI system partition, an unencrypted `/boot` and a LUKS2 (argon2id) root.
- Asks for disk, hostname, keyboard layout, admin user, sudo password, SSH keys (pasted or from GitHub) and GRUB password; validates every answer before touching the disk. All answers are also available as flags.
- Keyboard layout applies to the LUKS prompt at boot.
- Installed over a serial console (VPS, server, VM): the installed system keeps using it for GRUB, kernel messages and the LUKS prompt.
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
