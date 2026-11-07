# Sisura Linux

Sisura is a minimal, hardened Linux distribution based on Debian 13 (trixie). It is Debian underneath: the same packages, the same security updates, the same `apt`. On top of that it adds a set of security defaults, an installer with disk encryption, and `sisura`, a small CLI that boots Sisura VMs in seconds.

> **Status:** Sisura 13.1 is in testing. Release candidates are on the [download page](https://sisura-linux.github.io/sisura/); the final 13.1 follows once they hold up.

## What you get

- **Hardened by default:** kernel and network hardening (sysctl), AppArmor, a firewall that denies incoming traffic, key-only SSH with modern cryptography, no unnecessary SUID programs, automatic Debian security updates.
- **Checkable:** `sudo sisura-audit` tells you whether that hardening is still in place, and which file changed it if not.
- **Encrypted installs:** the installer sets up full disk encryption (LUKS2) and can let you unlock the disk over SSH at boot, for servers you cannot reach physically.
- **Plain Debian underneath:** Sisura adds a few packages to Debian; it does not replace it. Your Debian knowledge, documentation and packages apply.
- **Fast throwaway VMs:** `sisura launch` starts a Sisura VM on macOS (Apple Silicon) or Linux in a few seconds.

## Two ways to use Sisura

| I want to... | Use | Start here |
|---|---|---|
| run Sisura as the operating system of a server, PC or cloud VM | the installer ISO | [Installing to disk](#installing-to-disk) |
| try Sisura, or get fast throwaway Linux VMs on my Mac or Linux machine | the `sisura` CLI | [Trying Sisura in a VM](#trying-sisura-in-a-vm) |

The two are independent: an installed system does not need the CLI, and the CLI never installs anything to disk.

## Getting Sisura

**Download page: <https://sisura-linux.github.io/sisura/>**: the newest installer ISOs with their checksums, nothing else. Which one:

| Your machine | ISO |
|---|---|
| PC or server with Intel/AMD, or a VM on one (VirtualBox, VMware, Proxmox, Hyper-V, QEMU/KVM) | `sisura-13.1-install-amd64.iso` |
| Mac with Apple Silicon (VM in UTM, Parallels, VMware Fusion), or an ARM server | `sisura-13.1-install-arm64.iso` |

For the `sisura` CLI you download nothing by hand: see [Trying Sisura in a VM](#trying-sisura-in-a-vm).

<details>
<summary>All release files, and verifying them</summary>

Everything is published on the [GitHub releases page](https://github.com/sisura-linux/sisura/releases). Each release has, for `amd64` and `arm64`:

| File | What it is |
|---|---|
| `sisura-13.1-install-<arch>.iso` | live system and installer |
| `sisura-13.1-<arch>.qcow2` + kernel files | VM image, downloaded by `sisura pull` |
| `sisura-<version>-<platform>.tar.gz` | the `sisura` CLI for macOS and Linux |
| `sisura-*.deb` | Sisura's packages (also in the [apt repository](https://sisura-linux.github.io/sisura/apt/)) |
| `SHA256SUMS` | checksums of all files |

**Verify what you downloaded** before using it:

```bash
sha256sum -c SHA256SUMS --ignore-missing          # Linux
shasum -a 256 -c SHA256SUMS --ignore-missing      # macOS
```

To also check that a file was built by this repository's release workflow (needs the GitHub CLI):

```bash
gh attestation verify sisura-13.1-install-amd64.iso --repo sisura-linux/sisura
```

</details>

## Installing to disk

### Quick steps

1. **Download** the ISO for your machine from the [download page](https://sisura-linux.github.io/sisura/).
2. **Boot it:**
   - PC or server: write it to a USB stick ([how](#writing-the-usb-stick)) and boot from the stick in **UEFI** mode.
   - VM: attach it as a CD/DVD, with **UEFI** firmware and the guest type **Linux** ([details](#installing-in-a-virtual-machine)).
3. **Install:** in the live session that opens, run `sudo sisura-install install` and answer the questions ([what it asks](#the-installers-questions)).
4. **Power off and remove the ISO** or the USB stick. In a VM, clear the CD/DVD drive *while the VM is off*.
5. **Boot from the disk:** enter the disk encryption passphrase, then log in with your user and password.
6. **SSH:** if you gave an SSH key, log in with `ssh <user>@<address>`. If not, run `sudo sisura-ssh-enroll` on the console and follow it ([details](#logging-in)).

### Requirements

- A 64-bit PC or server (amd64) or ARM machine (arm64) that boots with **UEFI**. Legacy BIOS/CSM boot is not supported; the installer stops if the ISO was started that way.
- **Secure Boot** can stay on: Sisura boots with Debian's signed boot chain (shim signed by Microsoft, Debian-signed GRUB and kernel), which machines with Microsoft's UEFI keys accept. With Secure Boot on, the kernel runs in lockdown mode, and kernel modules that are not part of Debian (DKMS) need a key enrolled with `mokutil`.
- At least 8 GiB of disk space; the whole disk is used and erased. Installation has been tested with 2 GiB of RAM.

### Writing the USB stick

Everything on the stick is erased.
- Any system: a graphical tool such as [balenaEtcher](https://etcher.balena.io/).
- Linux: `lsblk` shows which `/dev/sdX` is the stick, then `sudo dd if=sisura-13.1-install-amd64.iso of=/dev/sdX bs=4M conv=fsync status=progress`.
- macOS: `diskutil list` to find the stick (`/dev/diskN`), `diskutil unmountDisk /dev/diskN`, then `sudo dd if=sisura-13.1-install-amd64.iso of=/dev/rdiskN bs=4m`.
- VPS: most panels have "mount ISO"; attach it and boot from it.

### The installer's questions

Disk, hostname, keyboard layout, admin user name, sudo password, your SSH public key (paste it, or type `gh:<github-user>` to fetch it from GitHub), whether the disk may be unlocked over SSH, the disk encryption passphrase and an optional GRUB password. Every answer is checked before anything is written; you confirm by typing the disk name, and the installation takes under a minute. For automated installs every question has a flag: `sisura-install install --help`.

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

### Installing in a virtual machine

For a hypervisor such as UTM, VirtualBox, VMware, Proxmox or virt-manager (for throwaway VMs, the `sisura` CLI is quicker: see [Trying Sisura in a VM](#trying-sisura-in-a-vm)):
- Choose **UEFI** firmware and **Linux / Debian 64-bit** as the guest type, not "Other".
- **Remove the ISO after installing, while the VM is off**, otherwise the VM boots the live session again. A prompt of `sisura@sisura` with "live session" in the login banner means you are not in the installed system; the banner also warns when the disk already holds a Sisura installation.

UTM on a Mac, step by step:
1. **Create a New Virtual Machine** → **Virtualize** → **Linux**. Boot ISO Image: **Browse** to `sisura-13.1-install-arm64.iso`. 2 GiB of memory and a 16 GiB disk are plenty.
2. Start the VM and install (`sudo sisura-install install`), then `sudo poweroff`.
3. With the VM off: right-click it → **Edit** → under **Drives** select the **USB Drive** that holds the ISO → **Clear** → **Save**. (Ejecting it from the toolbar while the VM runs lasts only until the next start.)
4. Start the VM: it asks for the disk encryption passphrase on the screen.

Known issues:
- UTM: with the guest type "Other" the screen stays at "Display output is not active". Choose "Linux".
- Installed from 13.1-rc7 in a VM with a virtio-gpu display (UTM's default): the screen shows "Display output is not active" while the disk passphrase is asked. Type it blind and press Enter; after `sudo apt update && sudo apt upgrade` the prompt appears on the screen. Installed from rc6 or earlier: the prompt goes to the serial console; reinstall from the current ISO.

## After installation

### Logging in

SSH accepts keys only, for members of the `sudo` group (the admin user) and of `sisura-ssh`. Passwords are never accepted over SSH; the admin password is for `sudo` and the local console. After 5 failed password attempts in 15 minutes the account is locked for 15 minutes.

```bash
ssh <admin user>@<address of the machine>     # port 22; find the address with: ip -4 -br addr
```

No SSH key given to the installer? Log in on the console and run

```bash
sudo sisura-ssh-enroll
```

It allows a password login over SSH for your user only, from local networks only, and prints the command to run on your computer:

```bash
ssh-copy-id <admin user>@<address of the machine>
```

As soon as the key arrives (or after 10 minutes, or on Ctrl-C) password logins are closed again; a reboot closes them too. `--anywhere` also allows other networks, for a VPS reached through the provider's console. Keys you already have on GitHub work without it: `curl -fsSL https://github.com/<github user>.keys >> ~/.ssh/authorized_keys`.

### Unlocking the disk over SSH

If you enabled it during installation (it needs SSH keys), a small SSH server runs **while the machine waits for the disk passphrase at boot**, only to ask for that passphrase:

```bash
ssh -p 2222 root@<address of the machine>
```

Port 2222 exists only during that wait. Once the system is running, the connection hangs (the firewall drops it without an answer); log in normally on port 22 instead.

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

- Debian security updates and updates of Sisura's own packages (`sisura-*`) are installed automatically every day. The system never reboots on its own; after a kernel update a "System restart required" notice appears at login. To allow automatic reboots on a server, put your settings in a file of your own, so package updates never have to ask about your changes:
  ```bash
  echo 'Unattended-Upgrade::Automatic-Reboot "true"; Unattended-Upgrade::Automatic-Reboot-Time "04:00";' \
    | sudo tee /etc/apt/apt.conf.d/60local-reboot
  ```
- Other updates: `sudo apt update && sudo apt upgrade`, as on Debian.
- Sisura's packages come from the Sisura apt repository (`https://sisura-linux.github.io/sisura/apt`), set up by the `sisura-archive-keyring` package:
  - The source is bound to Sisura's signing key (`Signed-By`): that key cannot sign anything else, and no other key can sign this repository.
  - An apt pin (`/etc/apt/preferences.d/sisura`) only accepts `sisura-*` packages from it. Even with a stolen signing key, the repository cannot replace Debian packages such as `openssh-server`.
  - Signing key fingerprint: `2E46 F4E2 4105 D45B A8A2  7376 0E93 53DF 8969 0935`

Systems installed from 13.1-rc1 or rc2 do not have the repository yet. Add it once:

```bash
curl -fsSLo /tmp/sisura-archive-keyring.deb \
  https://sisura-linux.github.io/sisura/apt/pool/main/s/sisura-archive-keyring/sisura-archive-keyring_0.1.0_all.deb
sudo apt install /tmp/sisura-archive-keyring.deb
sha256sum /usr/share/keyrings/sisura-archive-keyring.pgp
# must be 8acb4e38a8ab506b14dc02dc936339c03e3c9c8d28826621b65a925c13569112
sudo apt update && sudo apt upgrade
```

### Is the hardening still in place?

```bash
sudo sisura-audit            # all checks
sudo sisura-audit --quiet    # only warnings and failures
sudo sisura-audit --json     # for scripts and monitoring
```

It compares the running system with the hardening of the installed Sisura packages: kernel settings, SSH, firewall, AppArmor, setuid programs, accounts, sudo, PAM, kernel modules, automatic updates, the Sisura apt repository, boot parameters, disk encryption and Secure Boot. Each line is `✓` (in place), `!` (a warning, for example a setting an admin changed on purpose, or a reboot that is due), `✗` (hardening missing or broken) or `·` (information). Where it can, it names the file that changed a setting:

```
 ✗ ssh          SSH hardening weakened
                passwordauthentication: yes (Sisura: no), set in /etc/ssh/sshd_config.d/50-local.conf
```

Exit status: 0 no failures (warnings allowed), 1 failures, 2 the audit could not run.

### Which build am I running?

```bash
cat /etc/sisura/build-info
```

shows the release, the build id (for example `13.1`, `20261105.1523-2d85457`) and when the system was installed. Please include both when reporting a problem.

## Trying Sisura in a VM

The `sisura` CLI starts Sisura VMs with QEMU on macOS and Linux (amd64 or arm64). VMs use your machine's architecture. It is tested on Apple Silicon Macs and amd64/arm64 Linux; Intel Macs are untested.

**1. Install the CLI and QEMU.**

*macOS* (Homebrew; also installs QEMU):

```bash
brew tap sisura-linux/sisura https://github.com/sisura-linux/sisura
brew trust sisura-linux/sisura      # Homebrew 7+: allow formulae from this tap
brew install sisura
```

*Linux* (any distribution; the binary is static). Pick the file for your machine: `x86_64-unknown-linux-musl` for amd64, `aarch64-unknown-linux-musl` for arm64.

```bash
VERSION=0.6.0 RELEASE=13.1-rc8 TARGET=x86_64-unknown-linux-musl   # newest: see the download page
URL=https://github.com/sisura-linux/sisura/releases/download/v$RELEASE
curl -LO $URL/sisura-$VERSION-$TARGET.tar.gz -LO $URL/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
tar -xzf sisura-$VERSION-$TARGET.tar.gz
sudo install -m 0755 sisura-$VERSION-$TARGET/sisura /usr/local/bin/sisura
```

Then install QEMU and allow your user to use KVM (without it VMs run very slowly):

```bash
sudo apt install qemu-system-x86 qemu-utils openssh-client curl   # Debian, Ubuntu (arm64: qemu-system-arm)
sudo dnf install qemu-kvm qemu-img openssh-clients curl            # Fedora
sudo usermod -aG kvm $USER                                         # then log out and in again
```

Homebrew on Linux works too (`brew tap`, `brew trust` and `brew install sisura` as above).

*macOS without Homebrew:* download `sisura-<version>-aarch64-apple-darwin.tar.gz` (Apple Silicon) or `...-x86_64-apple-darwin.tar.gz` (Intel) the same way, verify it with `shasum -a 256 -c SHA256SUMS --ignore-missing`, put `sisura` on your `PATH`, and install QEMU (`brew install qemu` or MacPorts). A binary downloaded with a browser may be blocked by Gatekeeper; `xattr -d com.apple.quarantine sisura` allows it.

**2. Download the VM image** (once per release, a few hundred MB):

```bash
sisura pull                # latest release; or: sisura pull --release 13.1
```

Every file is checked against the release's `SHA256SUMS`.

**3. Use VMs:**

```bash
sisura launch dev          # start a VM (created on first use), ready in a few seconds
sisura shell dev           # SSH into it
sisura shell dev -- uname -a
sisura transfer ./app.tar.gz dev:      # copy into the VM's home (dev:/tmp for another dir)
sisura transfer dev:/var/log/syslog .  # and back; -r for directories
sisura forward dev 8080:80             # http://localhost:8080 -> port 80 in the VM
sisura snapshot dev before-nginx       # VM stopped: save its state (list: sisura snapshot dev)
sisura restore dev before-nginx        # back to it; later changes are lost
sisura list                # VMs, their state and build id
sisura stop dev
sisura purge dev           # delete the VM
```

- Each VM runs on its own copy-on-write disk on top of the read-only image, so `purge` and a new `launch` give you a fresh system.
- The VM's SSH port is reachable on `127.0.0.1` only. Images contain no SSH key: the CLI creates its own key pair in `~/.local/share/sisura/` and gives the public key to each VM when it starts.
- `sisura forward` (or `launch --forward 8080:80`) makes a service in the VM reachable from this machine. The port is opened on `127.0.0.1` only, kept with the VM and restored at every launch, and the VM's firewall admits it from this machine only. `sisura forward dev` lists the forwards, `--remove 8080` removes one. Installed systems do not need this: they have their own address on your network; open the port with `sudo ufw allow 80/tcp`.
- VMs have passwordless `sudo`; they are meant for testing and development, not for installing to disk.
- Everything (images, VMs, the key) lives in `~/.local/share/sisura/`; set `SISURA_HOME` to use another place.

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
