# Developing SedirOS

How SedirOS is built, tested and released. For using SedirOS, see the [README](../README.md).

## How it fits together

SedirOS is an overlay on Debian, not a fork: images are built from Debian's packages with `debootstrap`, and SedirOS adds its own packages (`packages/`) on top. Installed systems get Debian's security updates straight from Debian; SedirOS only maintains its own packages.

```
packages/*  ──build_packages.sh──▶ build/debs/*.deb ─┐
                                                     ├─ build_rootfs.sh ─▶ rootfs (.tar.zst) ─┬─ build_image.sh ─▶ qcow2 (vm profile)
Debian mirror ── debootstrap + apt ──────────────────┘                                        └─ build_iso.sh ───▶ ISO (install profile)
sediros-install/ ─build_installer.sh─▶ build/bin/sediros-install-<arch> ─▶ sediros-install .deb (ISO only)
sediros-audit/ ───build_installer.sh─▶ build/bin/sediros-audit-<arch> ───▶ sediros-audit .deb (every image)
sediros-cli/ ──cargo──▶ sediros (runs the qcow2 images in QEMU)
```

## Requirements

- **macOS (Apple Silicon):** Docker, `brew install qemu`, Rust. The build scripts re-run themselves in Docker; images are arm64.
- **Linux:** `debootstrap zstd e2fsprogs qemu-utils qemu-system squashfs-tools xorriso mtools dosfstools grub-common`, `grub-pc-bin` (amd64), Rust, `sudo`; for `make iso-test` also `ovmf` (amd64) or `qemu-efi-aarch64` (arm64). Images match the host architecture (amd64 or arm64); cross builds are not supported.

Building the rootfs and the installer downloads packages and crates, so Docker needs network access.

## Building

```bash
make build      # .deb packages + rootfs + qcow2 image + sediros binary
make test       # boots a throwaway VM and runs the checks in tests/
make iso        # installer + install-profile rootfs + live/installer ISO
make lint       # bash -n, shellcheck (if installed), rustfmt, clippy, package versions
make help       # all targets
```

Settings are in `config/sediros.conf` and can be overridden on the command line, e.g. `make build RELEASE=1.2`.

| Target | Output |
|---|---|
| `make build` | `build/sediros-<release>-<arch>.{qcow2,vmlinuz,initrd.img,cmdline,manifest,build-info}` |
| `make iso` | `build/sediros-<release>-install-<arch>.iso` |
| `make installer` | `build/bin/sediros-install-<arch>` |
| `make audit` | `build/bin/sediros-audit-<arch>` |
| `make install` | installs the `sediros` CLI into `~/.cargo/bin` |
| `make launch` / `shell` / `stop` | VM `dev` (or `VM=name`) |
| `make iso-run` | boots the ISO in QEMU on a serial console (quit: Ctrl-A X) |
| `make iso-test` | boots the ISO with Secure Boot enforced and checks it from the live session |
| `make install-test` | installs from the ISO to an empty disk and boots it, both with Secure Boot enforced |
| `make apt-test` | updates a throwaway VM through a test apt repository (signature, pin, unattended upgrade) |
| `make firstboot-test` | installs `sediros-firstboot` in a throwaway VM and sets it up as two new instances: from a NoCloud seed and from a fake EC2-style metadata service on the host (`tests/firstboot.sh`) |

## Repository layout

| Path | Contents |
|---|---|
| `scripts/build_rootfs.sh` | debootstrap minbase + packages + hardening → `build/*.tar.zst` |
| `scripts/build_image.sh` | rootfs → `build/*.qcow2` (ext4, `LABEL=sediros-root`) |
| `scripts/build_iso.sh` | rootfs → squashfs + live-boot + shim/signed GRUB (UEFI) + GRUB i386-pc (BIOS, amd64) → `build/*.iso` |
| `scripts/build_packages.sh` | `packages/*` → `build/debs/*.deb`; fills in `@SEDIROS_RELEASE@`, `@SEDIROS_CODENAME@`, `@SEDIROS_ARCH@`; adds compiled programs listed in `packages/<pkg>/.sediros-binaries` |
| `scripts/build_installer.sh [crate]` | builds `sediros-install` or `sediros-audit` for Linux (Docker with `debian:trixie` on macOS) |
| `scripts/build_apt_repo.sh` | `.deb` files → signed apt repository (`pool/`, `dists/`, `index.html`); used by `apt-repo.yml` and `make apt-test` |
| `scripts/build_download_page.sh` | newest release → download page (`https://sediros.github.io/sediros/`, ISO links and checksums); used by `apt-repo.yml` |
| `scripts/render_terminal_svg.py` | terminal session (text, `$ ` lines are commands) → SVG for the README (`docs/images/*.svg`); regenerate them from real output when the output changes |
| `scripts/update_formula.sh` | Homebrew formula from a published release's `SHA256SUMS`: downloads from the SourceForge mirror first, GitHub as fallback; refuses until the mirror has the files |
| `scripts/check_versions.sh` | fails when a package changed since the last release tag without a new version |
| `scripts/lib/release.sh` | release → Debian suite mapping, build id |
| `scripts/run_iso.sh` | boots an ISO (or an installed disk) in QEMU |
| `packages/sediros-base-files/` | `os-release` (`ID=sediros`, `ID_LIKE=debian`, Debian's file kept with `dpkg-divert`), login banner |
| `packages/sediros-hardening/` | sysctl, sshd, nftables, unattended-upgrades, sudo/PAM, modprobe, SUID removal, first-boot units |
| `packages/sediros-boot-hardening/` | kernel parameters for GRUB, `sediros-grub-password`; inert without GRUB |
| `packages/sediros-guest/` | VM images: adds the SSH key passed by `sediros launch` |
| `packages/sediros-live/` | live ISO session: autologin, passwordless sudo, only with `boot=live`, only in RAM |
| `packages/sediros-install/` | installer package (ISO only, removed from installed systems) |
| `packages/sediros-audit/` | `sediros-audit` (every image) |
| `packages/sediros-archive-keyring/` | apt repository key, source and pin |
| `sediros-cli/` | Rust CLI (`sediros`) |
| `sediros-install/` | Rust installer |
| `sediros-audit/` | Rust hardening check, no dependencies beyond std |
| `config/` | build settings, ISO boot menu template |
| `tests/` | smoke test (`smoke.sh`), in-guest checks (`guest_checks.sh`), apt repository test (`apt_repo.sh`), Secure Boot tests |

## Profiles

One build script produces two kinds of root file systems:

| | `PROFILE=vm` (default) | `PROFILE=install` |
|---|---|---|
| Used for | `sediros launch`, `make test`, CI, published VM image | the ISO and the systems it installs |
| SSH key | none in the image; `sediros launch` passes one at boot | none in the image; set by the installer |
| Admin password | locked | empty and expired (the installer sets it) |
| sudo | passwordless | asks for the password |
| Extra packages | `sediros-guest` | `sediros-live`, `sediros-install`, cryptsetup, GRUB EFI with `shim-signed`, dropbear-initramfs (disabled), ... |
| Artifact name | `sediros-1.1-arm64.*` | `sediros-1.1-install-arm64.*` |

How a VM gets its key: the CLI keeps a key pair in `SEDIROS_HOME` (`~/.local/share/sediros/id_ed25519`) and adds `systemd.set_credential_binary=sediros.ssh.authorized_keys:<base64>` to the kernel command line; `sediros-guest` copies it into the admin user's `authorized_keys`. A public key is not secret, so the readable kernel command line is fine.

How the ISO avoids an SSH server: `dropbear-initramfs` is installed for the installer, but its initramfs files are moved away with `dpkg-divert` and its host keys deleted (`disable_initramfs_ssh` in `build_rootfs.sh`). The installer restores them with new per-machine keys only when SSH unlock is chosen.

## Secure Boot

SedirOS uses Debian's signed boot chain and signs nothing itself: UEFI firmware (Microsoft keys) → `shim-signed` (signed by Microsoft) → `grub-efi-*-signed` (signed by Debian) → Debian's signed kernel and modules. The install profile installs `shim-signed` and the signed GRUB; `build_iso.sh` copies shim and `gcd*.efi` from that rootfs into the ISO's EFI image, and `sediros-install` runs `grub-install --uefi-secure-boot`. Debian's package updates keep shim and GRUB current on installed systems.

Consequences:
- Signed GRUB cannot load modules: GRUB config (ISO `grub.cfg`, `/etc/grub.d` scripts) may only use commands built into it.
- With Secure Boot on, the kernel runs in lockdown (integrity) mode; out-of-tree modules (DKMS) need a key enrolled with `mokutil`.

`make iso-test` boots the ISO with firmware that enforces Secure Boot (Microsoft keys enrolled) and checks the `SecureBoot` EFI variable in the live session. `make install-test` runs `sediros-install --yes` (LUKS, admin and GRUB password) in that VM, then boots the installed disk with the same EFI variables, unlocks it on the serial console, logs in and checks Secure Boot and `dpkg --audit`. CI runs both on amd64 with KVM. On macOS they run under TCG (Debian's AAVMF hangs under HVF): copy `AAVMF_CODE.secboot.fd` and `AAVMF_VARS.ms.fd` from Debian's `qemu-efi-aarch64` into a directory and set `SEDIROS_FIRMWARE_DIR`.

## Testing

`make test` boots the current image in a throwaway VM (isolated `SEDIROS_HOME`) and checks boot time, that the VM runs the build in `build/`, and in the guest: systemd state, every sysctl value, AppArmor, UFW, listening ports, SUID files, effective sshd settings, SedirOS packages (`dpkg --verify`), the SSH key, nftables, unattended-upgrades, sudo/PAM, kernel module blacklist, core dumps and memory use. It needs the `vm` profile.

### Testing the installer

Without building an ISO, in a VM with an empty second disk:

```bash
sediros launch inst --memory 2048 --data-disk 16G
sediros shell inst
# inside: install the installer's tools, build or copy sediros-install, then
sudo mount --bind / /mnt/src && sudo mount --make-private /mnt/src
sudo sediros-install install --disk vdb --source /mnt/src --skip-uefi-check ...
```

`--skip-uefi-check` is needed because these VMs boot the kernel directly, without UEFI firmware. The installed disk is then booted with UEFI firmware (`scripts/run_iso.sh --no-iso` with `SEDIROS_ISO_DISK`).

With the ISO, end to end:

```bash
make iso
SEDIROS_ISO_DISK=build/test-disk.qcow2 make iso-run       # install in the live session
SEDIROS_ISO_DISK=build/test-disk.qcow2 scripts/run_iso.sh --no-iso build/sediros-1.1-install-arm64.iso
```

## Versions and releases

| Number | Example | Changes when |
|---|---|---|
| Release | `1.0` | A new ISO/image is published. SedirOS's own numbering: the major number selects the Debian base (1 = Debian 13 trixie, 2 = Debian 14 forky), the minor number (`1.0` → `1.1`) counts releases on it: a Debian point release, a critical security fix in a package on the ISO, or an important installer/hardening fix. The minor number is not Debian's point release. |
| Package / tool version (SemVer) | `sediros-hardening 0.3.0` | Any file of the package changes: patch for fixes, minor for features, major (minor before 1.0) for changes that can break existing systems. Without a new version apt never delivers the change. |
| Build id | `20261105.1505-f69b925` | Every build: `<UTC yyyymmdd.HHMM>-<git commit>`, `-dirty` if the tree had uncommitted changes. |

- The release is set in one place, `RELEASE` in `config/sediros.conf`; the Debian release and suite are derived from its major number (`scripts/lib/release.sh`). `sediros-base-files` carries the epoch `1:` (`1:1.0`) because the release candidates before 1.0 were numbered `13.1-rcN`: without it apt would see 1.0 as older. `make apt-test` checks that a 13.1 system moves to the current version. Development builds on sid: `make build SUITE=sid`.
- The build id is written to `/etc/sediros/build-info` and `build/*.build-info`, the first line of `build/*.manifest`, the ISO boot menu, the installer's plan and `sediros list`.
- CI skips documentation-only pushes (`*.md`, `docs/`, `LICENSE`, `NOTICE`): they change nothing the build or tests could catch.
- `make lint` runs `scripts/check_versions.sh`: every package and Rust tool that changed since the last `v*` tag needs a new version; `sediros-base-files` needs a new `RELEASE`.

Releasing:
1. Update `RELEASE` in `config/sediros.conf` and the `CHANGELOG.md` section `## [<release>]`; `make lint`.
2. Commit and push, so the build id has no `-dirty` suffix.
3. Optional dry run: run the `release` workflow manually (Actions → release → Run workflow). It builds everything and keeps the files as workflow artifacts, without creating a release.
   To test the published flow (downloads, Homebrew, `sediros pull`) before the final release, tag a pre-release instead: `v1.2-rc1`. It is built the same way, published as a GitHub "Pre-release", and `sediros pull` uses it while no final release exists.
   A pre-release may be tagged without waiting for CI: the `release` workflow runs the same tests (smoke, apt, Secure Boot install) on what it publishes, about 10 minutes instead of 20. If it fails, that rc number stays unused (tags are immutable) and the next one follows. For the final release, wait for a green CI on the commit first.
4. `git tag -a v1.2 -m "SedirOS 1.2" && git push origin v1.2`. The `release` workflow checks that the tag matches `RELEASE` and the changelog, builds and smoke-tests the images (amd64, arm64), builds the CLI for macOS (arm64, x86_64) and Linux (musl, amd64, arm64), writes `SHA256SUMS`, attests the build provenance and creates a **draft** release with the changelog section as notes.
5. Review the draft on GitHub (files, notes) and publish it. Publishing starts the `apt-repo` workflow, which puts the release's packages into the apt repository (see below) and points the download page at the new release.
6. Update the Homebrew formula from the published release and commit it:
   ```bash
   scripts/update_formula.sh v1.2
   git add Formula/sediros.rb && git commit -m "Formula: sediros for v1.2" && git push
   ```

## apt repository

Installed systems get updates of SedirOS's packages from `https://sediros.github.io/sediros/apt` (GitHub Pages, suite = Debian codename, component `main`).

- Every release carries its `.deb` files. `.github/workflows/apt-repo.yml` runs when a release is published (or manually): it downloads the packages of all published releases, oldest first, and keeps a file once it is there, so a published package file never changes. `scripts/build_apt_repo.sh` then writes `pool/` and `dists/`, signs `InRelease` and `Release.gpg`, and checks the signature with the key in `sediros-archive-keyring`; a signing key that does not match what systems trust is never published. The repository has no state of its own and can always be rebuilt from the releases.
- `sediros-archive-keyring` installs the key, `sources.list.d/sediros.sources` (`Signed-By` that key) and `preferences.d/sediros`, which only accepts `sediros-*` from the repository's host. `sediros-hardening` lets unattended-upgrades install from it (`origin=SedirOS,site=sediros.github.io`). The host name appears in all three files; change them together.
- `make apt-test` (also in CI on amd64) boots a VM, serves it a repository signed with a throwaway key (`tests/apt_fixture.sh`) and checks the signature, the unattended update, the pin and a repository signed with another key.

Signing key (Ed25519, fingerprint `D17BC3DCE5196167A7A0F3A46D4E0DA8534FFDAD`, expires 2029-11-07):
- The private key is the repository secret `APT_SIGNING_KEY`; an offline copy and a revocation certificate are kept by the maintainer, never in the repository.
- Rotation (before it expires, or if it leaks): create a new key, ship both public keys in a new `sediros-archive-keyring` version, publish a release, switch `APT_SIGNING_KEY` to the new key in a later release, then drop the old public key. Systems that never install the intermediate version need the keyring package once by hand, as in the README.

## Changing a hardening setting

1. Edit the file under `packages/sediros-hardening/`.
2. Bump `Version` in its `DEBIAN/control`.
3. `make build && make test`.

`sediros-audit` reads its expected values from those files (`99-sediros.conf`, `sshd_config.d/99-sediros.conf`, `suid-allowlist`, `sediros-blacklist.conf`, `90-sediros.cfg`), so a changed value needs no change in the audit. A new kind of setting needs a check in `sediros-audit/src/checks.rs`, and `tests/guest_checks.sh` should check it independently.

Files under `/etc` in SedirOS packages are conffiles: dpkg never overwrites an admin's changes. Files owned by Debian packages (`/etc/login.defs`, `/etc/default/ufw`) are only set at image build time and never touched by package updates.

## Continuous integration

- `.github/workflows/ci.yml` runs on every push and pull request: `make lint`, then a build for amd64 (with KVM, the smoke test and the apt repository test) and arm64 (build only), uploading the images, ISOs and manifests as artifacts for 7 days.
- `.github/workflows/release.yml` runs on `v*` tags (and manually, as a dry run); see "Releasing" above.
- `.github/workflows/apt-repo.yml` runs when a release is published; see "apt repository" above.

## Distribution

| What | Where | How users get it |
|---|---|---|
| ISO, VM image, kernel files | GitHub release | download, or `sediros pull` for the VM image |
| `sediros` CLI | GitHub release (`sediros-<version>-<target>.tar.gz`) | Homebrew (`Formula/sediros.rb`, tap of this repository) or manual download |
| Checksums, provenance | `SHA256SUMS` in the release, GitHub attestations | `sha256sum -c`, `gh attestation verify` |

`sediros pull` downloads `sediros-<release>-<arch>.{qcow2,vmlinuz,initrd.img,cmdline,build-info}` and `SHA256SUMS` with `curl` into `SEDIROS_HOME/downloads/<release>/`, and verifies each file. Outside a source checkout, `sediros launch` uses the newest pulled release.

## Conventions

- Code comments, documentation and commit messages are in English.
- Shell scripts pass `shellcheck`; Rust code passes `cargo fmt` and `cargo clippy -D warnings`.
- Secrets (passwords, passphrases) are never passed in command-line arguments; the installer feeds them on stdin.
