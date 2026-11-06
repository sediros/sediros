# Sisura Linux

Sisura is a minimal, hardened Linux distribution based on Debian 13 (trixie). It is Debian underneath: the same packages, the same security updates, the same `apt`. On top of that it adds a set of security defaults, an installer with disk encryption, and `sisura`, a small CLI that boots Sisura VMs in seconds.

> **Status:** Sisura 13.1 is in development and has not been released yet. There are no downloads yet; to try it, build it from source (see [Development](docs/DEVELOPMENT.md)).

## What you get

- **Hardened by default:** kernel and network hardening (sysctl), AppArmor, a firewall that denies incoming traffic, key-only SSH with modern cryptography, no unnecessary SUID programs, automatic Debian security updates.
- **Encrypted installs:** the installer sets up full disk encryption (LUKS2) and can let you unlock the disk over SSH at boot, for servers you cannot reach physically.
- **Plain Debian underneath:** Sisura adds a few packages to Debian; it does not replace it. Your Debian knowledge, documentation and packages apply.
- **Fast throwaway VMs:** `sisura launch` starts a Sisura VM on macOS (Apple Silicon) or Linux in a few seconds.

## Getting Sisura

Releases will be published on the [GitHub releases page](https://github.com/sisura-linux/sisura/releases): an installer ISO and a VM image for amd64 and arm64, with `SHA256SUMS` to verify them. Until the first release, build the ISO yourself: see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

Requirements for installing:
- A 64-bit PC (amd64) or ARM machine (arm64) that boots with **UEFI**. Legacy BIOS/CSM boot is not supported; the installer stops if the ISO was started that way.
- **Secure Boot must be disabled** for now.
- At least 8 GiB of disk space (the whole disk is used and erased). Installation has been tested with 2 GiB of RAM.

## Installing to disk

1. Boot the ISO. The live session logs in automatically; nothing is written to disk.
2. Start the installer:
   ```bash
   sudo sisura-install install
   ```
3. Answer the questions: disk, hostname, keyboard layout, admin user name, sudo password, your SSH public key (paste it, or type `gh:<github-user>` to fetch it from GitHub), whether the disk may be unlocked over SSH, the disk encryption passphrase and an optional GRUB password. Every answer is checked before anything is written.
4. Confirm by typing the disk name. The installation takes under a minute.
5. Remove the ISO and reboot. Enter the disk encryption passphrase at the prompt (or over SSH, see below) and log in with your SSH key.

What the installer creates:

| Partition | Size | Contents |
|---|---|---|
| 1 | 512 MiB | EFI system partition (boot loader) |
| 2 | 1 GiB | `/boot`, unencrypted: kernel and initramfs |
| 3 | rest | encrypted (LUKS2) root file system |

Good to know:
- **The passphrase cannot be recovered.** Without it, the data on the disk is lost.
- **Keyboard layout:** the layout you choose is also used at the passphrase prompt during boot, so type the passphrase with the same layout during installation.
- The installer refuses disks that are mounted, used as swap, hold the live medium, or are smaller than 8 GiB.
- For automated installs every question has a flag: `sisura-install install --help`.

## After installation

### Logging in

SSH accepts keys only, for members of the `sudo` group (the admin user) and of `sisura-ssh`. Passwords are never accepted over SSH; the admin password is for `sudo` and the local console. After 5 failed password attempts in 15 minutes the account is locked for 15 minutes.

### Unlocking the disk over SSH

If you enabled it during installation, a small SSH server runs before the system starts, only to ask for the disk passphrase:

```bash
ssh -p 2222 root@<address of the machine>
```

- It accepts only the SSH keys given to the installer, no passwords, and runs nothing but the unlock prompt.
- Compare the host key fingerprint with the one the installer printed on your first connection.
- The network is configured with DHCP; a static address is not supported yet.

### Protecting the boot menu

```bash
sudo sisura-grub-password set       # set a password (at least 12 characters)
sudo sisura-grub-password status
sudo sisura-grub-password disable
```

With a GRUB password the default entry still boots unattended, but editing boot entries (for example `init=/bin/bash`), the GRUB shell, recovery entries and firmware settings require the password. It does not stop booting from other media: with a remote console (IPMI, VPS panel) also use a firmware password and keep the disk encrypted.

### Updates

- Debian security updates are installed automatically every day. The system never reboots on its own; after a kernel update a "System restart required" notice appears at login. To allow automatic reboots on a server, set `Unattended-Upgrade::Automatic-Reboot "true";` in `/etc/apt/apt.conf.d/52sisura-unattended-upgrades`.
- Other updates: `sudo apt update && sudo apt upgrade`, as on Debian.
- Updates of Sisura's own packages will come from a Sisura apt repository, which does not exist yet; until then they arrive with a new release.

### Which build am I running?

```bash
cat /etc/sisura/build-info
```

shows the release, the build id (for example `13.1`, `20261105.1523-2d85457`) and when the system was installed. Please include both when reporting a problem.

## Trying Sisura in a VM

The `sisura` CLI starts Sisura VMs with QEMU (HVF on macOS, KVM on Linux):

```bash
sisura launch dev          # start a VM (created on first use)
sisura shell dev           # SSH into it
sisura shell dev -- uname -a
sisura list                # VMs, their state and build id
sisura stop dev
sisura purge dev           # delete the VM
```

- Each VM runs on its own copy-on-write disk on top of a read-only base image, and its SSH port is reachable on `127.0.0.1` only.
- Images contain no SSH key: the CLI creates its own key pair (in `~/.local/share/sisura/`) and gives the public key to each VM when it starts.
- VMs have passwordless `sudo`; they are meant for testing, not for installing to disk.

> For now the CLI needs a Sisura source checkout with a built image. Prebuilt CLI binaries, a Homebrew formula and `sisura pull` (downloading released images) are planned.

## Security defaults and their side effects

| Setting | Effect you may notice |
|---|---|
| Unprivileged user namespaces disabled | Rootless podman and the Chromium sandbox do not work |
| `kernel.yama.ptrace_scope=2` | Regular users cannot attach `gdb` or `strace` to running processes |
| Firewall denies incoming traffic | Only SSH (22/tcp, rate-limited) is open; open other ports with `ufw` |
| SUID bits removed outside an allowlist | Some rarely used tools that need SUID stop working (see `/etc/sisura/suid-allowlist`) |
| unattended-upgrades | About 30-40 MB of disk and 20 MB of RAM for Python |

Files under `/etc` that you change are kept: package updates never overwrite them.

## More

- [CHANGELOG.md](CHANGELOG.md): what changed in each release
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): building Sisura, tests, repository layout, release process

## License

Sisura's own code (build scripts, packages, `sisura-cli`, `sisura-install`) is licensed under the [Apache License 2.0](LICENSE), Copyright 2026 The Sisura Authors (see [NOTICE](NOTICE)). The images and ISOs also contain Debian packages, each under its own license; see `/usr/share/doc/<package>/copyright` on a Sisura system.
