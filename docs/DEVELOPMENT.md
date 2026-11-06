# Developing Sisura

How Sisura is built, tested and released. For using Sisura, see the [README](../README.md).

## How it fits together

Sisura is an overlay on Debian, not a fork: images are built from Debian's packages with `debootstrap`, and Sisura adds its own packages (`packages/`) on top. Installed systems get Debian's security updates straight from Debian; Sisura only maintains its own packages.

```
packages/*  ──build_packages.sh──▶ build/debs/*.deb ─┐
                                                     ├─ build_rootfs.sh ─▶ rootfs (.tar.zst) ─┬─ build_image.sh ─▶ qcow2 (vm profile)
Debian mirror ── debootstrap + apt ──────────────────┘                                        └─ build_iso.sh ───▶ ISO (install profile)
sisura-install/ ─build_installer.sh─▶ build/bin/sisura-install-<arch> ─▶ sisura-install .deb (ISO only)
sisura-cli/ ──cargo──▶ sisura (runs the qcow2 images in QEMU)
```

## Requirements

- **macOS (Apple Silicon):** Docker, `brew install qemu`, Rust. The build scripts re-run themselves in Docker; images are arm64.
- **Linux:** `debootstrap zstd e2fsprogs qemu-utils qemu-system squashfs-tools xorriso mtools grub-*-bin`, Rust, `sudo`. Images match the host architecture (amd64 or arm64); cross builds are not supported.

Building the rootfs and the installer downloads packages and crates, so Docker needs network access.

## Building

```bash
make build      # .deb packages + rootfs + qcow2 image + sisura binary
make test       # boots a throwaway VM and runs the checks in tests/
make iso        # installer + install-profile rootfs + live/installer ISO
make lint       # bash -n, shellcheck (if installed), rustfmt, clippy, package versions
make help       # all targets
```

Settings are in `config/sisura.conf` and can be overridden on the command line, e.g. `make build RELEASE=13.2`.

| Target | Output |
|---|---|
| `make build` | `build/sisura-<release>-<arch>.{qcow2,vmlinuz,initrd.img,cmdline,manifest,build-info}` |
| `make iso` | `build/sisura-<release>-install-<arch>.iso` |
| `make installer` | `build/bin/sisura-install-<arch>` |
| `make install` | installs the `sisura` CLI into `~/.cargo/bin` |
| `make launch` / `shell` / `stop` | VM `dev` (or `VM=name`) |
| `make iso-run` | boots the ISO in QEMU on a serial console (quit: Ctrl-A X) |

## Repository layout

| Path | Contents |
|---|---|
| `scripts/build_rootfs.sh` | debootstrap minbase + packages + hardening → `build/*.tar.zst` |
| `scripts/build_image.sh` | rootfs → `build/*.qcow2` (ext4, `LABEL=sisura-root`) |
| `scripts/build_iso.sh` | rootfs → squashfs + live-boot + GRUB → `build/*.iso` |
| `scripts/build_packages.sh` | `packages/*` → `build/debs/*.deb`; fills in `@SISURA_RELEASE@`, `@SISURA_CODENAME@`, `@SISURA_ARCH@`; adds compiled programs listed in `packages/<pkg>/.sisura-binaries` |
| `scripts/build_installer.sh` | builds `sisura-install` for Linux (Docker with `debian:trixie` on macOS) |
| `scripts/check_versions.sh` | fails when a package changed since the last release tag without a new version |
| `scripts/lib/release.sh` | release → Debian suite mapping, build id |
| `scripts/run_iso.sh` | boots an ISO (or an installed disk) in QEMU |
| `packages/sisura-base-files/` | `os-release` (`ID=sisura`, `ID_LIKE=debian`, Debian's file kept with `dpkg-divert`), login banner |
| `packages/sisura-hardening/` | sysctl, sshd, nftables, unattended-upgrades, sudo/PAM, modprobe, SUID removal, first-boot units |
| `packages/sisura-boot-hardening/` | kernel parameters for GRUB, `sisura-grub-password`; inert without GRUB |
| `packages/sisura-guest/` | VM images: adds the SSH key passed by `sisura launch` |
| `packages/sisura-live/` | live ISO session: autologin, passwordless sudo, only with `boot=live`, only in RAM |
| `packages/sisura-install/` | installer package (ISO only, removed from installed systems) |
| `sisura-cli/` | Rust CLI (`sisura`) |
| `sisura-install/` | Rust installer |
| `config/` | build settings, ISO boot menu template |
| `tests/` | smoke test (`smoke.sh`) and in-guest checks (`guest_checks.sh`) |

## Profiles

One build script produces two kinds of root file systems:

| | `PROFILE=vm` (default) | `PROFILE=install` |
|---|---|---|
| Used for | `sisura launch`, `make test`, CI, published VM image | the ISO and the systems it installs |
| SSH key | none in the image; `sisura launch` passes one at boot | none in the image; set by the installer |
| Admin password | locked | empty and expired (the installer sets it) |
| sudo | passwordless | asks for the password |
| Extra packages | `sisura-guest` | `sisura-live`, `sisura-install`, cryptsetup, GRUB EFI, dropbear-initramfs (disabled), ... |
| Artifact name | `sisura-13.1-arm64.*` | `sisura-13.1-install-arm64.*` |

How a VM gets its key: the CLI keeps a key pair in `SISURA_HOME` (`~/.local/share/sisura/id_ed25519`) and adds `systemd.set_credential_binary=sisura.ssh.authorized_keys:<base64>` to the kernel command line; `sisura-guest` copies it into the admin user's `authorized_keys`. A public key is not secret, so the readable kernel command line is fine.

How the ISO avoids an SSH server: `dropbear-initramfs` is installed for the installer, but its initramfs files are moved away with `dpkg-divert` and its host keys deleted (`disable_initramfs_ssh` in `build_rootfs.sh`). The installer restores them with new per-machine keys only when SSH unlock is chosen.

## Testing

`make test` boots the current image in a throwaway VM (isolated `SISURA_HOME`) and checks boot time, that the VM runs the build in `build/`, and in the guest: systemd state, every sysctl value, AppArmor, UFW, listening ports, SUID files, effective sshd settings, Sisura packages (`dpkg --verify`), the SSH key, nftables, unattended-upgrades, sudo/PAM, kernel module blacklist, core dumps and memory use. It needs the `vm` profile.

### Testing the installer

Without building an ISO, in a VM with an empty second disk:

```bash
sisura launch inst --memory 2048 --data-disk 16G
sisura shell inst
# inside: install the installer's tools, build or copy sisura-install, then
sudo mount --bind / /mnt/src && sudo mount --make-private /mnt/src
sudo sisura-install install --disk vdb --source /mnt/src --skip-uefi-check ...
```

`--skip-uefi-check` is needed because these VMs boot the kernel directly, without UEFI firmware. The installed disk is then booted with UEFI firmware (`scripts/run_iso.sh --no-iso` with `SISURA_ISO_DISK`).

With the ISO, end to end:

```bash
make iso
SISURA_ISO_DISK=build/test-disk.qcow2 make iso-run       # install in the live session
SISURA_ISO_DISK=build/test-disk.qcow2 scripts/run_iso.sh --no-iso build/sisura-13.1-install-arm64.iso
```

## Versions and releases

| Number | Example | Changes when |
|---|---|---|
| Release | `13.1` | A new ISO/image is published. Major = Debian major (13 trixie, 14 forky). Point (`13.1` → `13.2`): a Debian point release, a critical security fix in a package on the ISO, or an important installer/hardening fix. |
| Package / tool version (SemVer) | `sisura-hardening 0.3.0` | Any file of the package changes: patch for fixes, minor for features, major (minor before 1.0) for changes that can break existing systems. Without a new version apt never delivers the change. |
| Build id | `20261105.1505-f69b925` | Every build: `<UTC yyyymmdd.HHMM>-<git commit>`, `-dirty` if the tree had uncommitted changes. |

- The release is set in one place, `RELEASE` in `config/sisura.conf`; the Debian suite is derived from it. Development builds on sid: `make build SUITE=sid`.
- The build id is written to `/etc/sisura/build-info` and `build/*.build-info`, the first line of `build/*.manifest`, the ISO boot menu, the installer's plan and `sisura list`.
- `make lint` runs `scripts/check_versions.sh`: every package and Rust tool that changed since the last `v*` tag needs a new version; `sisura-base-files` needs a new `RELEASE`.

Releasing:
1. Update `RELEASE` in `config/sisura.conf` and `CHANGELOG.md`; `make lint`.
2. Commit, so the build id has no `-dirty` suffix.
3. `make build && make test && make iso`, and an install from the ISO.
4. `git tag -a v13.2 -m "Sisura 13.2" && git push origin v13.2`.

## Changing a hardening setting

1. Edit the file under `packages/sisura-hardening/`.
2. Bump `Version` in its `DEBIAN/control`.
3. `make build && make test`.

Files under `/etc` in Sisura packages are conffiles: dpkg never overwrites an admin's changes. Files owned by Debian packages (`/etc/login.defs`, `/etc/default/ufw`) are only set at image build time and never touched by package updates.

## Continuous integration

`.github/workflows/ci.yml` runs on every push and pull request: `make lint`, then a build for amd64 (with KVM and the smoke test) and arm64 (build only), uploading the images, ISOs and manifests as artifacts for 7 days.

## Conventions

- Code comments, documentation and commit messages are in English.
- Shell scripts pass `shellcheck`; Rust code passes `cargo fmt` and `cargo clippy -D warnings`.
- Secrets (passwords, passphrases) are never passed in command-line arguments; the installer feeds them on stdin.
