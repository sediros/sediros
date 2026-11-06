# Sisura Linux

A minimal, hardened Linux distribution based on Debian, plus the `sisura` CLI, which boots its images in QEMU within seconds for testing.

## Quick start

Requirements:
- **macOS (Apple Silicon):** Docker, `brew install qemu`, Rust
- **Linux:** `debootstrap zstd e2fsprogs qemu-utils qemu-system squashfs-tools xorriso mtools grub-*-bin`, Rust, `sudo`

```bash
make build      # .deb packages + rootfs + qcow2 image + sisura binary
make test       # boots a throwaway VM, runs the hardening checks, deletes it
make launch     # starts the 'dev' VM
make shell      # SSH into the 'dev' VM
make iso        # live/installer ISO (install profile, no development key)
make iso-run    # boots the ISO in QEMU with a serial console
make help       # all targets
```

Settings live in `config/sisura.conf` and can be overridden on the command line: `make build RELEASE=13.2`.

## sisura CLI

```bash
sisura build [--release 13.1] [--clean] [--rootfs-only|--image-only]
sisura launch <vm> [--cpus N] [--memory MiB] [--port P] [--data-disk 16G]
sisura shell  <vm> [-- command]
sisura list
sisura stop   <vm> [--force]
sisura purge  [<vm>] [--images] [--yes]
```

- VMs are kept under `~/.local/share/sisura/` (override with `SISURA_HOME`). Each VM runs on its own copy-on-write disk on top of a read-only base image.
- Acceleration is picked automatically: HVF on macOS, KVM on Linux, TCG (slow) otherwise.
- SSH is only reachable on `127.0.0.1`. Each VM has its own `known_hosts` file.
- Images contain no SSH key. The CLI creates its own key pair (`~/.local/share/sisura/id_ed25519`) and passes the public key to each VM at boot as a systemd credential; `sisura-guest` adds it to the admin user's `authorized_keys`.
- `--data-disk` attaches an empty second disk to a new VM (installer tests).

## Repository layout

| Path | Contents |
|---|---|
| `scripts/build_rootfs.sh` | debootstrap minbase + hardening → `build/*.tar.zst` |
| `scripts/build_image.sh` | rootfs → `build/*.qcow2` (ext4, `LABEL=sisura-root`) |
| `scripts/build_iso.sh` | rootfs → squashfs + live-boot + GRUB → `build/*.iso` |
| `packages/sisura-base-files/` | `os-release` (`ID=sisura`, `ID_LIKE=debian`); Debian's copy is kept with `dpkg-divert` |
| `packages/sisura-hardening/` | Hardening package: sysctl, sshd, nftables, automatic security updates, sudo/PAM, modprobe, SUID removal, first-boot units |
| `packages/sisura-boot-hardening/` | GRUB: kernel hardening parameters and an optional GRUB password (`sisura-grub-password`); inert without GRUB |
| `packages/sisura-guest/` | VM images: adds the SSH key passed by `sisura launch` at boot |
| `packages/sisura-live/` | Live ISO session: console autologin, passwordless sudo; only with `boot=live` and only in RAM |
| `packages/sisura-install/` | Installer package (ISO only; removed from the installed system) |
| `scripts/build_packages.sh` | `packages/*` → `build/debs/*.deb` (fills in placeholders such as `@SISURA_RELEASE@`) |
| `scripts/build_installer.sh` | Builds `sisura-install` for Linux (Docker with `debian:trixie` on macOS) |
| `scripts/lib/release.sh` | Release → Debian suite mapping (13 → trixie) |
| `config/iso/grub.cfg` | ISO boot menu template |
| `sisura-cli/` | Rust CLI |
| `sisura-install/` | Rust installer: ISO to disk (UEFI, GPT, LUKS2) |
| `tests/` | Smoke test and in-guest checks |

## Profiles

| | `PROFILE=vm` (default) | `PROFILE=install` |
|---|---|---|
| Used for | `sisura launch`, `make test`, CI | Systems installed to disk, the ISO |
| SSH | Key only, passed by `sisura launch` at boot (none in the image) | Key only, set by the installer (none in the image) |
| User password | Locked | Empty and expired: set at the first console login |
| sudo | Passwordless | Asks for the password |
| Artifact name | `sisura-13.1-arm64.*` | `sisura-13.1-install-arm64.*` |

On installed systems the account, password and SSH keys come from the installer (see below). `make test` covers the `vm` profile only.

## Release numbers

Sisura follows Debian's major version: `<debian major>.<point>`.

| Release | Base | Meaning |
|---|---|---|
| `13.1` | Debian 13 (trixie) | First Sisura 13 release |
| `13.2` | Debian 13 (trixie) | Same base, rebuilt image/ISO (security updates + hardening changes) |
| `14.1` | Debian 14 (forky) | Move to the next Debian base |

- Security support lasts as long as Debian's.
- The release is set in one place: `config/sisura.conf` → `RELEASE`. The suite is derived from it. Development builds on `sid`: `make build SUITE=sid`.
- Artifact names: `build/sisura-<release>-<arch>.{qcow2,iso,vmlinuz,...}`.
- Every build also gets a build id, `<UTC yyyymmdd.HHMM>-<git commit>`, with `-dirty` when the tree had uncommitted changes, e.g. `20261105.1432-47888b3`. It is stored in `/etc/sisura/build-info` (with the build date, profile and, on installed systems, the install date) and in `build/*.build-info`, shown in the first line of `build/*.manifest`, in the ISO boot menu, in the installer's plan and in `sisura list`. Report bugs with release and build id.
- A release that gets published is tagged in git (`git tag v13.1`); release builds should come from a clean tree (no `-dirty`).
- Packages have their own versions (SemVer), e.g. `sisura-hardening 0.3.0`, CLI `0.1.0`. `sisura-base-files` carries the distribution release.

## Versions and releases

Three numbers, each with its own rule:

| Number | Example | Changes when |
|---|---|---|
| Release | `13.1` | A new ISO/image is published. Major = Debian major (13 trixie, 14 forky). Point (`13.1` → `13.2`): a Debian point release, a critical security fix in a package on the ISO, or an important installer/hardening fix. Development builds do not change it. |
| Package / tool version (SemVer) | `sisura-hardening 0.3.0` | Any file of the package changes: patch for fixes, minor for new features, major (minor before 1.0) for changes that can break existing systems. Without a new version, apt never delivers the change. |
| Build id | `20261105.1505-f69b925` | Every build, automatically. |

`make lint` runs `scripts/check_versions.sh`, which fails when a package changed since the last release tag without a new version.

Releasing:
1. Update `RELEASE` in `config/sisura.conf` and `CHANGELOG.md`; check that changed packages have new versions (`make lint`).
2. Commit, so the build id has no `-dirty` suffix.
3. `make build && make test && make iso`.
4. `git tag -a v13.2 -m "Sisura 13.2"`.

## Hardening summary

sysctl (63 keys on trixie / kernel 6.12), AppArmor (default profiles), UFW (incoming denied, rate-limited 22/tcp), removal of SUID/SGID bits outside an allowlist (re-applied after every `dpkg` run), key-only SSH with modern cryptography, an nftables rule dropping IPv4 TCP packets with IP options, automatic installation of Debian security updates (unattended-upgrades), a sudo policy (pty, `/var/log/sudo.log`, 5-minute timestamp), a 15-minute account lock after 5 failed authentications in 15 minutes (`pam_faillock`; with key-only SSH this affects sudo and the console only), passwords of at least 12 characters with a dictionary check (`pwquality`), a locked root password, unused kernel modules blocked and core dumps disabled. `make test` verifies all of this on a running VM.

## Installing to disk

The ISO logs in automatically on the console. Start the installer with:

```bash
sudo sisura-install install
```

The installer asks for the disk, hostname, keyboard layout, admin user name, sudo password, SSH key (paste it, or `gh:<github-user>`), whether the disk may be unlocked over SSH, the LUKS passphrase and a GRUB password. Every answer is validated before the disk is touched:
- Passwords are checked against the installed system's `pwquality` rules (`pwscore`).
- SSH keys are checked with `ssh-keygen`; a pasted private key is rejected.

Nothing is written until you confirm by typing the disk name.

| Partition | Size | Contents |
|---|---|---|
| 1 | 512 MiB | ESP (FAT32), GRUB EFI |
| 2 | 1 GiB | `/boot` (ext4, unencrypted: kernel and initramfs) |
| 3 | rest | LUKS2 (argon2id, label `sisura-crypt`) → ext4 `/` (`sisura-system`) |

- **Keyboard layout:** applied to the live console, the installed system and the initramfs, so the LUKS passphrase is typed with the same layout at install time and at boot.
- **Refused disks:** mounted, used as swap, holding the live medium, or smaller than 8 GiB.
- **Unlocking over SSH:** with SSH keys, the disk can also be unlocked remotely at boot: `ssh -p 2222 root@<host>` reaches a small SSH server in the initramfs (dropbear). It accepts only the installer's keys, no passwords, no forwarding, and runs nothing but `cryptroot-unlock`. Every installation gets its own host key; the installer prints its fingerprint to check on first connect. The port uses a different host key than the system's SSH, hence not 22. The network is configured with DHCP; it is taken down again before the system starts. Without SSH unlock, dropbear is removed from the installed system. The live ISO never starts it.
- **Unattended install:** `--disk --hostname --user --keyboard --password-file --ssh-key-file|--github-user --passphrase-file --grub-password-file [--no-ssh-unlock] [--unlock-port N] --yes`. See `sisura-install install --help`.
- **Trying it in QEMU:** `SISURA_ISO_DISK=build/test-disk.qcow2 make iso-run`. After installing, boot the disk with `SISURA_ISO_DISK=build/test-disk.qcow2 scripts/run_iso.sh --no-iso build/sisura-13.1-install-arm64.iso`.
- **Not yet available:** a static IP address for SSH unlock (only DHCP for now).

## GRUB password (installed systems)

```bash
sudo sisura-grub-password set       # set a password (at least 12 characters)
sudo sisura-grub-password status
sudo sisura-grub-password disable
```

With a password set:
- The default entry boots without it, so unattended reboots do not hang.
- Editing an entry (`e`, e.g. `init=/bin/bash`), the GRUB shell (`c`), recovery entries and "UEFI Firmware Settings" require it.
- Only a PBKDF2 hash is stored (`/etc/sisura/grub-password`, mode 0600); `grub.cfg` becomes 0600 as well.

A GRUB password does not stop booting from other media. With a remote console (IPMI, VPS panel), also use a firmware password, Secure Boot and disk encryption (LUKS).

## Known limitations

- Unprivileged user namespaces are disabled: rootless podman and the Chromium sandbox do not work.
- `ptrace_scope=2`: regular users cannot attach gdb or strace to a process.
- `PROFILE=vm` (the default) has passwordless sudo; it is meant for VMs started by `sisura launch`, not for installing to disk. Systems to install are built with `PROFILE=install`.
- unattended-upgrades only installs from the Debian-Security archive and never reboots. After a kernel update a "System restart required" notice appears at login. For automatic reboots on a server, set `Automatic-Reboot "true"` in `/etc/apt/apt.conf.d/52sisura-unattended-upgrades`.
- unattended-upgrades pulls in python3: about 30-40 MB of disk and about 20 MB of RAM for its shutdown helper.
- On macOS, Unix socket paths are limited to 104 characters: keep `SISURA_HOME` short.

## Packages and updates

The hardening settings ship in the `sisura-hardening` package. Once the Sisura apt repository is published, installed systems will receive new versions through `apt upgrade` (the repository does not exist yet). To change a setting:

1. Edit the file under `packages/sisura-hardening/`.
2. Bump `Version` in `DEBIAN/control`.
3. Run `make build && make test`.

dpkg never overwrites `/etc` files changed by the admin; they are marked as conffiles. Files owned by other packages (`/etc/login.defs`, `/etc/default/ufw`) are only set when the image is built and are not updated through `apt`.

## License

Sisura's own code (build scripts, packages, `sisura-cli`, `sisura-install`) is licensed under the [Apache License 2.0](LICENSE), Copyright 2026 The Sisura Authors. The images and ISOs also contain Debian packages, each under its own license; see `/usr/share/doc/<package>/copyright` on a Sisura system.
