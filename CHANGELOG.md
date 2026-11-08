# Changelog

Notable changes per SedirOS release. A release is `<major>.<minor>` (SedirOS 1 = Debian 13) and is tagged `v<release>` in git; see "Versions and support" in the README. Package and tool versions are listed when they change.

## [1.1] - unreleased

### Login greeting (`sediros-base-files` 1.1)
- At every login (console and SSH) SedirOS greets with its logo (the cedar in a shield, as text), the release, the hostname and build, and the hardening state; until the daily audit (#2) records a result it points to `sudo sediros-audit`. Off with `chmod -x /etc/update-motd.d/20-sediros`.

### Cloud images (`sediros-firstboot` 0.1.0)
- `sediros-firstboot` sets up a cloud or VPS instance on its first boot, from a NoCloud seed (label `cidata`) or the EC2-style metadata service at 169.254.169.254 (Vultr, Hetzner, AWS, OpenStack): new SSH host keys, hostname, the admin user's SSH keys, root file system grown to the disk. A new instance id (a cloned disk) runs it again; a known one changes nothing.
- User-data scripts are never run. From `#cloud-config` only `hostname` and `ssh_authorized_keys` are used; anything else is reported at login (`/etc/motd.d/50-sediros-firstboot`).
- Built and published as a package; no image contains it yet (cloud images follow). No Rust dependencies, like `sediros-audit`.

## [1.0] - 2026-11-08

First release, based on Debian 13 (trixie).

Named after the cedar (*sedir* in Turkish): a hard, long-lived wood that resists decay. Release candidates up to 13.1-rc11 were published under the name Sisura, 13.1-rc12 and rc13 as SedirOS 13.1; SedirOS then switched to its own release numbers (SedirOS 1 = Debian 13), so the minor number is no longer mistaken for Debian's point release. `sediros-base-files` carries the epoch `1:` so that systems installed from a release candidate move to 1.0.

### Distribution
- Minimal Debian trixie image (debootstrap minbase) as a qcow2 VM image and a live/installer ISO (amd64, arm64).
- `os-release` identifies the system as SedirOS (`ID=sediros`, `ID_LIKE=debian`); Debian's file is kept via `dpkg-divert`.
- `/etc/sediros/build-info` records release, build id, build date and profile.

### Hardening (`sediros-hardening` 0.3.0, `sediros-boot-hardening` 0.2.1)
- sysctl hardening (63 keys), AppArmor, UFW with incoming traffic denied and SSH rate-limited.
- Key-only SSH with modern ciphers, `AllowGroups sudo sediros-ssh`.
- nftables rule dropping IPv4 TCP packets with IP options (mitigation for CVE-2026-98432-type handshake bugs).
- Automatic installation of Debian security updates (unattended-upgrades), login notice when a reboot is required.
- systemd-ssh-generator masked: no SSH on AF_VSOCK (reachable from the hypervisor, outside the firewall) or on a local AF_UNIX socket; it also printed an error at every boot in VMs without vsock.
- sudo policy (pty, audit log, 5-minute timestamp), `pam_faillock` (5 failures in 15 minutes lock the account for 15 minutes), `pwquality` (12 characters, dictionary check).
- SUID/SGID removal outside an allowlist, re-applied after every dpkg run; kernel module blacklist; core dumps disabled.
- Kernel hardening parameters on every GRUB entry; optional GRUB password (`sediros-grub-password`) that protects entry editing while the default entry still boots unattended.

### Updates (`sediros-archive-keyring` 0.1.0, `sediros-hardening` 0.5.4)
- SedirOS apt repository (`https://sediros.github.io/sediros/apt`), built from the packages of each published release and signed in CI; SedirOS's own packages now receive fixes through apt, not only with a new image.
- `sediros-archive-keyring`: the repository's signing key and source, bound to each other with `Signed-By`, and an apt pin that only accepts `sediros-*` packages from the repository (matched by host name), so even a stolen signing key cannot replace Debian packages.
- unattended-upgrades also installs updates of SedirOS's packages.
- `sudo sediros-ssh-enroll`: adds the first SSH key with `ssh-copy-id` through a short password login window (calling user only, local networks only unless `--anywhere`, closed when the key arrives, after 10 minutes or on reboot). In the live ISO session it refuses and says to install or boot the installed disk. It shows `ssh-copy-id -i ~/.ssh/id_ed25519.pub ...` (without `-i`, ssh-copy-id may send another key) and how to compare the received key's fingerprint. A user without an SSH key is reminded of it after logging in on the console (not before login, and only about their own keys).
- `make apt-test` updates a VM through a test repository: signature accepted, unattended update installed, foreign package refused by the pin, repository signed with another key rejected, hardening intact afterwards.

### Audit (`sediros-audit` 0.2.0)
- `sudo sediros-audit` checks that the hardening is still in place on a running system (kernel settings, SSH, firewall, AppArmor, setuid programs, accounts, sudo, PAM, kernel modules, automatic updates, apt repository, boot parameters, disk encryption, Secure Boot). Expected values come from the installed SedirOS packages; changes are reported with the file that made them where possible. `--json`, `--quiet`; exit status 1 on failures.
- In every image; the smoke test runs it next to its own checks.
- Warns while a `sediros-ssh-enroll` window is open.

### Installer (`sediros-install` 0.3.4, `sediros-live` 0.2.2)
- Live session with console autologin and the SedirOS login banner; nothing written to disk. It warns when a disk already holds a SedirOS installation (the ISO was booted again instead of the installed system), and the installer's last message says what to do without an SSH key.
- Installs to disk with GPT, an EFI system partition, an unencrypted `/boot` and a LUKS2 (argon2id) root.
- Asks for disk, hostname, keyboard layout, admin user, sudo password, SSH keys (pasted or from GitHub) and GRUB password; validates every answer before touching the disk. All answers are also available as flags.
- Keyboard layout applies to the LUKS prompt at boot. The layout chosen in the wizard also applies to the live console right away (it did not when the installer ran under sudo), and the dozens of harmless `ckbcomp` warnings it caused while building the initramfs are no longer shown. Copying the system no longer prints hundreds of harmless "Operation not supported" warnings: the live squashfs has no extended attributes to copy, and the EFI partition's mount point (vfat) takes no ACLs. Neither does removing the SSH-unlock files print `dpkg-divert` warnings any more.
- Installed over a serial console (VPS, server, VM): the installed system keeps using it for GRUB, kernel messages and the LUKS prompt.
- Installed from the screen: the LUKS prompt stays on the screen (`console=tty0`), also on arm64 VMs and servers whose firmware names a serial console (ACPI SPCR). VMs with a virtio-gpu display (UTM, QEMU) show it too: the driver is now in the initramfs (`sediros-boot-hardening` 0.2.1); before, the screen stayed at "Display output is not active" until the disk was unlocked.
- Optional disk unlock over SSH at boot (dropbear in the initramfs, port 2222, key-only, restricted to `cryptroot-unlock`, per-machine host key).
- Secure Boot: the ISO and installed systems boot with Debian's signed chain (Microsoft-signed `shim`, Debian-signed GRUB and kernel); tested with firmware that enforces Secure Boot, for the live ISO, the installation and the installed system.

### Tools (`sediros` CLI 0.6.3)
- `sediros pull` without `--release`, before a final release exists, takes the most recently published pre-release; it took the first one GitHub lists, which is ordered by tag and put 13.1-rc13 before 1.0-rc1. Pulled 13.1 images count as older than 1.x.
- `sediros pull` says so when GitHub's API limit for requests without a login (60 per hour per IP address) is reached, instead of reporting that no release exists.
- A failed `sediros pull` removes its partial download (earlier versions left the files of every failed attempt in the download directory); leftovers older than an hour are removed by the next pull.
- In a source checkout without a build of its own, `sediros launch` uses the image from `sediros pull` instead of failing.
- `sediros build/launch/shell/list/stop/purge`: QEMU VMs with HVF (macOS) or KVM (Linux), copy-on-write disks, SSH on localhost only.
- `launch --data-disk` attaches an empty second disk; `list` shows each VM's build id; `sediros` without a command shows the help with a getting-started section.
- `sediros pull` downloads a released VM image from GitHub and verifies it against the release's SHA256SUMS; no source checkout needed.
- Images contain no SSH key: the CLI passes its own key to each VM at boot.
- `sediros transfer` copies files and directories (`-r`) between the host and a running VM over the same SSH connection as `shell`; the VM side is written `<vm>:<path>`.
- `sediros forward dev 8080:80` (or `launch --forward`) forwards a port on 127.0.0.1 to a VM, live or at launch; kept with the VM, shown by `list`, and the VM's firewall is opened for this machine only.
- `sediros snapshot` / `sediros restore`: snapshots of a stopped VM (qcow2 internal snapshots of its disks, only changed blocks are kept); list with `sediros snapshot <vm>`, delete with `--delete`.
- `make test` boots a throwaway VM and verifies the hardening, packages and build id; `make apt-test` checks updates through the apt repository; `make iso-test` and `make install-test` check the live ISO, an installation and the installed system with Secure Boot enforced.

### Known limitations
- SSH disk unlock uses DHCP only (no static IP yet).
