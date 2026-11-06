#!/usr/bin/env bash
# =============================================================================
#  Sisura Linux - minimal hardened Debian RootFS builder
#
#  Usage : sudo ./scripts/build_rootfs.sh [--release 13.1] [--suite sid]
#                                         [--clean] [--no-tar] [-h]
#  Host  : Debian/Ubuntu (or any Linux) with debootstrap, zstd, coreutils.
#          Must run as root. Will NOT run on macOS - use a Linux VM/container.
#
#  Output (build/):
#    rootfs/                               unpacked root filesystem
#    sisura-rootfs-<release>-<arch>.tar.zst  packed rootfs (+ .sha256)
#    sisura-<release>-<arch>.vmlinuz/.initrd.img  for QEMU direct kernel boot
#    sisura-<release>-<arch>.cmdline              hardening kernel parameters
#    sisura-<release>-<arch>.manifest             installed package list
#    sisura-<release>-<arch>.build-info           release, build id, profile
#                                                 (also /etc/sisura/build-info)
#
#  Environment overrides:
#    SISURA_RELEASE         Sisura release, e.g. 13.1 (default: config/sisura.conf)
#    SISURA_BUILD_ID        Build id (default: <UTC time>-<git sha>[-dirty])
#    SISURA_SUITE           Debian suite override: sid/unstable only
#                           (default: derived from the release, 13 -> trixie)
#    SISURA_DEBS_DIR        Sisura .deb packages      (default: <repo>/build/debs)
#    SISURA_ARCH            Target arch, == host arch (default: host)
#    SISURA_MIRROR          Debian mirror             (default: deb.debian.org)
#    SISURA_BUILD_DIR       Output directory          (default: <repo>/build)
#    SISURA_HOSTNAME        Hostname                  (default: sisura)
#    SISURA_USER            Admin user                (default: sisura)
#    SISURA_SSH_PUBKEY      Path to a public key for SISURA_USER
#    SISURA_DEBUG_PASSWORD  Console/sudo password for SISURA_USER (dev only!)
#    SISURA_PROFILE         vm      = password locked, passwordless sudo
#                                     (throwaway VMs, sisura launch, CI)
#                           install = empty + expired password: the first
#                                     console login must set one; sudo asks
#                                     for it. SSH stays key-only.
#                           (default: vm)
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly SCRIPT_DIR PROJECT_ROOT

# ----------------------------------------------------------------------------
# Configuration
# ----------------------------------------------------------------------------
# shellcheck source=scripts/lib/release.sh
source "${SCRIPT_DIR}/lib/release.sh"
RELEASE=""  # set by sisura_resolve_release (main)
BUILD_ID="" # set by sisura_build_id (main)
SUITE=""
ARCH="${SISURA_ARCH:-}"
MIRROR="${SISURA_MIRROR:-http://deb.debian.org/debian}"
SECURITY_MIRROR="http://security.debian.org/debian-security"
BUILD_DIR="${SISURA_BUILD_DIR:-${PROJECT_ROOT}/build}"
TARGET_HOSTNAME="${SISURA_HOSTNAME:-sisura}"
ADMIN_USER="${SISURA_USER:-sisura}"
SSH_PUBKEY_FILE="${SISURA_SSH_PUBKEY:-}"
DEBUG_PASSWORD="${SISURA_DEBUG_PASSWORD:-}"
PROFILE="${SISURA_PROFILE:-vm}"
DEBS_DIR="${SISURA_DEBS_DIR:-${PROJECT_ROOT}/build/debs}"
DOCKER_IMAGE="${SISURA_DOCKER_IMAGE:-debian:bookworm}"

DO_CLEAN=0
DO_TAR=1

# Packages on top of debootstrap --variant=minbase. Keep this list short:
# every entry costs RAM, disk and attack surface.
BASE_PACKAGES=(
    # init / core
    systemd systemd-sysv systemd-resolved systemd-timesyncd
    libpam-systemd dbus udev kmod procps netbase
    # kernel + initramfs (zstd compressed). live-boot only activates with
    # 'boot=live' on the cmdline, so the same initrd boots disk and ISO.
    initramfs-tools zstd live-boot
    # fsck.ext4 for the root filesystem at boot
    e2fsprogs
    # security
    apparmor apparmor-profiles ufw
    # remote access (used by `sisura shell`)
    openssh-server sudo ca-certificates
    # networking
    iproute2
)

# Sisura's own packages (scripts/build_packages.sh -> build/debs). They carry
# the hardening (sysctl, sshd, modprobe, SUID strip, first-boot units) so that
# installed systems receive fixes through apt, not only fresh images.
SISURA_PACKAGES=(sisura-base-files sisura-hardening sisura-boot-hardening)

# install profile: the live ISO / installer and the system it installs.
# grub-efi-* only installs files here; sisura-install runs grub-install.
# console-setup: keyboard layout for the LUKS passphrase prompt (initramfs).
# libpwquality-tools (pwscore): installer checks passwords with the target's
# rules before touching the disk. curl: SSH keys from github.com/<user>.keys.
INSTALL_PROFILE_PACKAGES=(
    cryptsetup cryptsetup-initramfs dosfstools fdisk
    console-setup keyboard-configuration
    libpwquality-tools curl
    dropbear-initramfs
)

# dropbear-initramfs files moved out of initramfs-tools' reach in the image
# (see disable_initramfs_ssh); sisura-install moves them back when SSH unlock
# is enabled. Keep in sync with sisura-install/src/setup.rs.
DROPBEAR_INITRAMFS_FILES=(hooks/dropbear scripts/init-premount/dropbear scripts/init-bottom/dropbear)
DROPBEAR_DIVERT_DIR=/usr/share/sisura/initramfs-dropbear
INSTALL_PROFILE_SISURA_PACKAGES=(sisura-live sisura-install)
# vm profile: SSH keys arrive at boot from `sisura launch` (systemd credential).
VM_PROFILE_SISURA_PACKAGES=(sisura-guest)

# Kernel parameters for the bootloader / QEMU -append (consumed by stage 2/3).
# The hardening parameters live in sisura-boot-hardening (GRUB config of
# installed systems); VM images get the same value from that file.
KERNEL_GRUB_CFG="${PROJECT_ROOT}/packages/sisura-boot-hardening/etc/default/grub.d/90-sisura.cfg"
KERNEL_CMDLINE_HARDENING="$(sed -n 's/^SISURA_KERNEL_CMDLINE="\(.*\)"$/\1/p' "$KERNEL_GRUB_CFG" 2>/dev/null) quiet loglevel=3"

# ----------------------------------------------------------------------------
# Logging helpers
# ----------------------------------------------------------------------------
if [[ -t 1 ]]; then
    C_BLUE=$'\e[1;34m'; C_YEL=$'\e[1;33m'; C_RED=$'\e[1;31m'; C_GRN=$'\e[1;32m'; C_OFF=$'\e[0m'
else
    C_BLUE=""; C_YEL=""; C_RED=""; C_GRN=""; C_OFF=""
fi
log()  { printf '%s==>%s %s\n' "$C_BLUE" "$C_OFF" "$*"; }
ok()   { printf '%s  ✓%s %s\n' "$C_GRN" "$C_OFF" "$*"; }
warn() { printf '%s  !%s %s\n' "$C_YEL" "$C_OFF" "$*" >&2; }
die()  { printf '%s  ✗%s %s\n' "$C_RED" "$C_OFF" "$*" >&2; exit 1; }

usage() {
    awk 'NR>2 && /^# =====/ {exit} NR>2 {sub(/^#/, ""); sub(/^ /, ""); print}' "${BASH_SOURCE[0]}"
    exit "${1:-0}"
}

# ----------------------------------------------------------------------------
# Argument parsing
# ----------------------------------------------------------------------------
parse_args() {
    while (($#)); do
        case "$1" in
            # Read by sisura_resolve_release (scripts/lib/release.sh).
            --release) [[ $# -ge 2 ]] || die "--release needs a value"; export SISURA_RELEASE="$2"; shift 2 ;;
            --suite)   [[ $# -ge 2 ]] || die "--suite needs a value"; export SISURA_SUITE="$2"; shift 2 ;;
            --clean)   DO_CLEAN=1; shift ;;
            --no-tar)  DO_TAR=0; shift ;;
            -h|--help) usage 0 ;;
            *)         warn "Unknown argument: $1"; usage 1 ;;
        esac
    done
}

# ----------------------------------------------------------------------------
# Chroot helpers
# ----------------------------------------------------------------------------
ROOTFS=""          # set in init_paths
ARTIFACT_PREFIX="" # set in init_paths
MOUNTED=0

chroot_run() {
    chroot "$ROOTFS" /usr/bin/env -i \
        HOME=/root \
        TERM="${TERM:-dumb}" \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        LANG=C.UTF-8 LC_ALL=C.UTF-8 \
        DEBIAN_FRONTEND=noninteractive \
        DEBCONF_NONINTERACTIVE_SEEN=true \
        "$@"
}

# write_file <mode> <path-inside-rootfs>  (content from stdin)
write_file() {
    local mode="$1" dest="${ROOTFS}$2"
    mkdir -p "$(dirname "$dest")"
    cat > "$dest"
    chmod "$mode" "$dest"
}

mount_chroot() {
    log "Mounting pseudo filesystems into chroot"
    mount -t proc  proc  "$ROOTFS/proc"
    mount -t sysfs sysfs "$ROOTFS/sys"
    mount --bind /dev     "$ROOTFS/dev"
    mount --bind /dev/pts "$ROOTFS/dev/pts"
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs "$ROOTFS/run"
    MOUNTED=1
}

umount_chroot() {
    [[ -n "$ROOTFS" && -d "$ROOTFS" ]] || return 0
    local m
    for m in run dev/pts dev sys proc; do
        if mountpoint -q "$ROOTFS/$m"; then
            umount "$ROOTFS/$m" 2>/dev/null || umount -l "$ROOTFS/$m" \
                || warn "Could not unmount $ROOTFS/$m"
        fi
    done
    MOUNTED=0
}

on_exit() {
    local rc=$?
    if ((MOUNTED)); then umount_chroot; fi
    if ((rc != 0)) && [[ -n "$ROOTFS" ]]; then
        warn "Build failed (exit $rc). Partial rootfs left at: ${ROOTFS:-<unset>}"
        warn "Re-run with --clean to start from scratch."
    fi
}
on_error() { warn "Command failed at line $1: $2"; }
trap on_exit EXIT
trap 'on_error "$LINENO" "$BASH_COMMAND"' ERR

# ----------------------------------------------------------------------------
# Non-Linux host (macOS): re-run inside a privileged Debian container.
# The rootfs is built on the container's own filesystem (/build), only the
# artifacts are copied back to <project>/build.
# ----------------------------------------------------------------------------
reexec_in_docker() {
    command -v docker >/dev/null 2>&1 \
        || die "Non-Linux host: Docker is required (or run on a Linux machine)."

    local pubkey_in=""
    if [[ -n "$SSH_PUBKEY_FILE" ]]; then
        [[ -r "$SSH_PUBKEY_FILE" ]] || die "SISURA_SSH_PUBKEY not readable: $SSH_PUBKEY_FILE"
        local abs
        abs="$(cd "$(dirname "$SSH_PUBKEY_FILE")" && pwd)/$(basename "$SSH_PUBKEY_FILE")"
        [[ "$abs" == "$PROJECT_ROOT"/* ]] \
            || die "On macOS SISURA_SSH_PUBKEY must be inside the project dir (bind-mounted)."
        pubkey_in="/sisura/${abs#"$PROJECT_ROOT"/}"
    fi

    log "Non-Linux host, re-running inside a privileged ${DOCKER_IMAGE} container"
    # '-e VAR' without a value forwards the host value only if it is set.
    exec docker run --rm --privileged \
        -v "${PROJECT_ROOT}:/sisura" -w /sisura \
        -e SISURA_RELEASE="$RELEASE" \
        -e SISURA_BUILD_ID="$BUILD_ID" \
        -e SISURA_SUITE="$SUITE" \
        -e SISURA_MIRROR \
        -e SISURA_HOSTNAME \
        -e SISURA_USER \
        -e SISURA_DEBUG_PASSWORD \
        -e SISURA_PROFILE \
        -e SISURA_SSH_PUBKEY="$pubkey_in" \
        "$DOCKER_IMAGE" bash -c '
            set -e
            apt-get update -qq
            DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
                debootstrap zstd ca-certificates >/dev/null
            SISURA_BUILD_DIR=/build ./scripts/build_rootfs.sh "$@"
            mkdir -p /sisura/build
            for f in /build/sisura-*; do
                [ -f "$f" ] && cp -f "$f" /sisura/build/
            done
            echo "==> Artifacts copied to <project>/build"' _ "$@"
}

# ----------------------------------------------------------------------------
# Pre-flight
# ----------------------------------------------------------------------------
host_arch() {
    case "$(uname -m)" in
        x86_64)  echo amd64 ;;
        aarch64) echo arm64 ;;
        *)       die "Unsupported host architecture: $(uname -m)" ;;
    esac
}

preflight() {
    log "Pre-flight checks"
    [[ "$(uname -s)" == "Linux" ]] || die "Linux host required (debootstrap/chroot). Use a Debian VM or a privileged container."
    [[ $EUID -eq 0 ]]              || die "Must run as root (sudo $0)."

    local cmd missing=()
    for cmd in debootstrap chroot mount umount mountpoint findmnt tar zstd sha256sum; do
        command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
    done
    ((${#missing[@]} == 0)) || die "Missing host tools: ${missing[*]} (apt install debootstrap zstd)"

    local harch; harch="$(host_arch)"
    ARCH="${ARCH:-$harch}"
    [[ "$ARCH" == "$harch" ]] || die "Cross-arch builds ($ARCH on $harch) are not supported yet."

    case "$SUITE" in
        bookworm|trixie|forky|sid|unstable) ;;
        *) die "Unsupported suite: $SUITE" ;;
    esac

    case "$PROFILE" in
        install) SISURA_PACKAGES+=("${INSTALL_PROFILE_SISURA_PACKAGES[@]}") ;;
        vm)      SISURA_PACKAGES+=("${VM_PROFILE_SISURA_PACKAGES[@]}") ;;
    esac
    local pkg
    for pkg in "${SISURA_PACKAGES[@]}"; do
        [[ -n "$(sisura_deb "$pkg")" ]] \
            || die "Missing ${pkg} package for release ${RELEASE} in ${DEBS_DIR} - run make deb"
    done
    if [[ -n "$SSH_PUBKEY_FILE" ]]; then
        [[ -r "$SSH_PUBKEY_FILE" ]] || die "SISURA_SSH_PUBKEY not readable: $SSH_PUBKEY_FILE"
        grep -qE '^(ssh-ed25519|ecdsa-sha2-|ssh-rsa|sk-) ' "$SSH_PUBKEY_FILE" \
            || die "SISURA_SSH_PUBKEY does not look like an OpenSSH public key"
    fi
    case "$PROFILE" in
        vm)
            # No key in the image: `sisura launch` passes one at boot.
            [[ -z "$SSH_PUBKEY_FILE" ]] \
                || warn "SISURA_SSH_PUBKEY set: that key is baked into the image (do not publish it)."
            ;;
        install)
            [[ -z "$SSH_PUBKEY_FILE" ]] \
                || warn "install profile with SISURA_SSH_PUBKEY: that key is baked into the image."
            ;;
        *) die "Invalid SISURA_PROFILE: $PROFILE (vm|install)" ;;
    esac
    [[ "$ADMIN_USER" =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] || die "Invalid SISURA_USER: $ADMIN_USER"
    ok "Host OK (arch=$ARCH, release=$RELEASE, build=$BUILD_ID, suite=$SUITE, profile=$PROFILE)"
}

# .deb to install for a Sisura package: sisura-base-files must carry exactly
# this release (it writes os-release), the others: newest version built
# (Architecture: all, or the image architecture for compiled programs).
sisura_deb() {
    if [[ "$1" == sisura-base-files ]]; then
        find "$DEBS_DIR" -maxdepth 1 -name "${1}_${RELEASE}_all.deb"
    else
        find "$DEBS_DIR" -maxdepth 1 \( -name "${1}_*_all.deb" -o -name "${1}_*_${ARCH}.deb" \) \
            | sort -V | tail -n1
    fi
}

# Artifact name tag: the vm profile is the default and stays untagged.
profile_tag() {
    [[ "$PROFILE" == vm ]] || printf -- '-%s' "$PROFILE"
}

init_paths() {
    mkdir -p "$BUILD_DIR"
    BUILD_DIR="$(cd "$BUILD_DIR" && pwd)"
    ROOTFS="${BUILD_DIR}/rootfs"
    # Per release/arch names: several builds can coexist in build/ without
    # pairing an image with another build's kernel.
    ARTIFACT_PREFIX="${BUILD_DIR}/sisura-${RELEASE}$(profile_tag)-${ARCH}"
    # Guard rails for every rm -rf below.
    [[ "$BUILD_DIR" != "/" && "$ROOTFS" == "$BUILD_DIR"/rootfs ]] \
        || die "Refusing to operate on suspicious path: $ROOTFS"
}

prepare_rootfs_dir() {
    if [[ -e "$ROOTFS" ]]; then
        if ((DO_CLEAN)); then
            log "Removing previous rootfs ($ROOTFS)"
            umount_chroot
            if findmnt -rn -o TARGET | grep -q "^${ROOTFS}/"; then
                die "Stale mounts still present under $ROOTFS, aborting rm."
            fi
            rm -rf --one-file-system "$ROOTFS"
        else
            die "$ROOTFS already exists. Use --clean to rebuild."
        fi
    fi
    mkdir -p "$ROOTFS"
}

# ----------------------------------------------------------------------------
# 1) Bootstrap
# ----------------------------------------------------------------------------
bootstrap() {
    log "debootstrap --variant=minbase $SUITE ($ARCH) from $MIRROR"
    debootstrap \
        --variant=minbase \
        --arch="$ARCH" \
        --components=main \
        --merged-usr \
        "$SUITE" "$ROOTFS" "$MIRROR"
    ok "Base system bootstrapped"
}

# ----------------------------------------------------------------------------
# 2) APT / dpkg: no recommends, no docs, no locales
# ----------------------------------------------------------------------------
configure_apt() {
    log "Configuring APT and dpkg (debloat)"

    write_file 0644 /etc/apt/apt.conf.d/90-sisura <<'EOF'
// Sisura: keep the system minimal.
APT::Install-Recommends "false";
APT::Install-Suggests "false";
APT::AutoRemove::RecommendsImportant "false";
APT::AutoRemove::SuggestsImportant "false";
Acquire::Languages "none";
Acquire::GzipIndexes "true";
Acquire::CompressionTypes::Order:: "gz";
Dpkg::Use-Pty "0";
EOF

    write_file 0644 /etc/dpkg/dpkg.cfg.d/01-sisura-nodoc <<'EOF'
# Sisura: do not install documentation, man pages or translations.
path-exclude=/usr/share/doc/*
path-include=/usr/share/doc/*/copyright
path-exclude=/usr/share/man/*
path-exclude=/usr/share/info/*
path-exclude=/usr/share/groff/*
path-exclude=/usr/share/lintian/*
path-exclude=/usr/share/linda/*
path-exclude=/usr/share/locale/*
path-include=/usr/share/locale/locale.alias
EOF

    # deb822 sources. Use Signed-By only if we can find the keyring file.
    local signed_by="" kr
    for kr in /usr/share/keyrings/debian-archive-keyring.pgp \
              /usr/share/keyrings/debian-archive-keyring.gpg; do
        if [[ -f "${ROOTFS}${kr}" ]]; then signed_by="Signed-By: ${kr}"; break; fi
    done

    rm -f "$ROOTFS/etc/apt/sources.list"
    if [[ "$SUITE" == "sid" || "$SUITE" == "unstable" ]]; then
        write_file 0644 /etc/apt/sources.list.d/debian.sources <<EOF
Types: deb
URIs: ${MIRROR}
Suites: ${SUITE}
Components: main
${signed_by}
EOF
    else
        write_file 0644 /etc/apt/sources.list.d/debian.sources <<EOF
Types: deb
URIs: ${MIRROR}
Suites: ${SUITE} ${SUITE}-updates
Components: main
${signed_by}

Types: deb
URIs: ${SECURITY_MIRROR}
Suites: ${SUITE}-security
Components: main
${signed_by}
EOF
    fi

    # Never start services inside the chroot.
    write_file 0755 /usr/sbin/policy-rc.d <<'EOF'
#!/bin/sh
exit 101
EOF

    # Network inside chroot during the build.
    rm -f "$ROOTFS/etc/resolv.conf"
    cp -L /etc/resolv.conf "$ROOTFS/etc/resolv.conf"

    ok "APT configured"
}

# ----------------------------------------------------------------------------
# 3) Packages
# ----------------------------------------------------------------------------
install_packages() {
    log "Installing base packages"

    write_file 0644 /etc/initramfs-tools/conf.d/sisura.conf <<'EOF'
# Sisura: small, fast initramfs.
COMPRESS=zstd
EOF

    chroot_run apt-get update
    chroot_run apt-get -y dist-upgrade
    local extra=()
    [[ "$PROFILE" == install ]] && extra=("${INSTALL_PROFILE_PACKAGES[@]}" "grub-efi-${ARCH}")
    chroot_run apt-get -y install "${BASE_PACKAGES[@]}" "linux-image-${ARCH}" "${extra[@]+"${extra[@]}"}"
    chroot_run apt-get -y autoremove --purge
    ok "Packages installed"
}

# install profile: dropbear-initramfs ships in the image for sisura-install,
# but must not be active there. Left alone it would configure the network and
# start an SSH server in every live ISO initrd, with host keys generated at
# build time and shared by every installed system. Its initramfs files are
# diverted away and its host keys deleted; the installer restores the files
# and creates per-machine keys when the user enables SSH unlock.
disable_initramfs_ssh() {
    [[ "$PROFILE" == install ]] || return 0
    log "Disabling dropbear in the image's initramfs"
    local f
    for f in "${DROPBEAR_INITRAMFS_FILES[@]}"; do
        mkdir -p "${ROOTFS}${DROPBEAR_DIVERT_DIR}/$(dirname "$f")"
        chroot_run dpkg-divert --local --rename \
            --divert "${DROPBEAR_DIVERT_DIR}/$f" --add "/usr/share/initramfs-tools/$f" >/dev/null
    done
    rm -f "$ROOTFS"/etc/dropbear/initramfs/dropbear_*_host_key*
    chroot_run update-initramfs -u -k all
    ok "dropbear diverted, host keys removed"
}

# Sisura's own .debs, installed with apt so their Depends are resolved. Their
# postinst enables the first-boot units and strips SUID bits.
install_sisura_packages() {
    log "Installing Sisura packages (${SISURA_PACKAGES[*]})"

    local stage="/var/cache/sisura-debs" pkg deb debs=()
    mkdir -p "${ROOTFS}${stage}"
    for pkg in "${SISURA_PACKAGES[@]}"; do
        deb="$(sisura_deb "$pkg")"
        cp "$deb" "${ROOTFS}${stage}/"
        debs+=("${stage}/$(basename "$deb")")
    done
    chroot_run apt-get -y install "${debs[@]}"
    rm -rf "${ROOTFS:?}${stage}"

    chroot_run dpkg --verify "${SISURA_PACKAGES[@]}" || die "dpkg --verify failed for Sisura packages"
    # shellcheck disable=SC2016  # ${Package} is a dpkg-query format, not shell
    ok "Sisura packages installed: $(chroot_run dpkg-query -W -f='${Package} ${Version}  ' "${SISURA_PACKAGES[@]}")"
}

# ----------------------------------------------------------------------------
# 4) Base system configuration
# ----------------------------------------------------------------------------
configure_system() {
    log "Configuring hostname, locale, time, fstab, network"

    echo "$TARGET_HOSTNAME" > "$ROOTFS/etc/hostname"
    write_file 0644 /etc/hosts <<EOF
127.0.0.1   localhost
127.0.1.1   ${TARGET_HOSTNAME}
::1         localhost ip6-localhost ip6-loopback
ff02::1     ip6-allnodes
ff02::2     ip6-allrouters
EOF

    write_file 0644 /etc/default/locale <<'EOF'
LANG=C.UTF-8
EOF

    if [[ -e "$ROOTFS/usr/share/zoneinfo/Etc/UTC" ]]; then
        ln -sf /usr/share/zoneinfo/Etc/UTC "$ROOTFS/etc/localtime"
        echo "Etc/UTC" > "$ROOTFS/etc/timezone"
    fi

    # Root fs label is the contract with stage 2 (qcow2 builder: mkfs.ext4 -L sisura-root).
    # /tmp is not noexec on purpose: debconf/apt-extracttemplates execute from /tmp.
    write_file 0644 /etc/fstab <<'EOF'
# <device>          <mount>    <type>  <options>                                    <dump> <pass>
LABEL=sisura-root   /          ext4    defaults,noatime,errors=remount-ro           0      1
tmpfs               /tmp       tmpfs   defaults,nosuid,nodev,size=25%,mode=1777     0      0
tmpfs               /dev/shm   tmpfs   defaults,nosuid,nodev,noexec                 0      0
EOF

    # systemd-networkd: DHCP on any wired NIC (virtio: enp0s*, ens*).
    write_file 0644 /etc/systemd/network/20-sisura-wired.network <<'EOF'
[Match]
Name=en* eth*

[Network]
DHCP=yes
IPv6AcceptRA=yes
IPv6PrivacyExtensions=yes
LLMNR=no
MulticastDNS=no

[DHCPv4]
UseDomains=yes
EOF

    write_file 0644 /etc/systemd/resolved.conf.d/99-sisura.conf <<'EOF'
[Resolve]
LLMNR=no
MulticastDNS=no
# Not allow-downgrade: behind resolvers that strip RRSIGs (QEMU slirp, many
# home routers and VPS resolvers) it fails signed zones with "no-signature"
# and breaks apt / unattended-upgrades. Same as Debian's default.
DNSSEC=no
EOF

    # Low-RAM journald.
    write_file 0644 /etc/systemd/journald.conf.d/99-sisura.conf <<'EOF'
[Journal]
Storage=persistent
Compress=yes
SystemMaxUse=64M
RuntimeMaxUse=32M
MaxRetentionSec=2week
ForwardToSyslog=no
EOF

    ok "Base system configured"
}

# ----------------------------------------------------------------------------
# 5) SSH: config comes from sisura-hardening; host keys are per machine
# ----------------------------------------------------------------------------
configure_ssh() {
    log "Preparing OpenSSH"
    # Keys generated by the openssh-server postinst would be shared by every
    # VM booted from this image. sisura-ssh-hostkeys.service creates a fresh
    # one on first boot. Validate the effective config first, while the
    # build-time keys still exist (sshd -t needs a host key).
    mkdir -p "$ROOTFS/run/sshd"   # privilege separation dir, checked by sshd -t
    chroot_run sshd -t || die "sshd configuration test failed"
    rm -f "$ROOTFS"/etc/ssh/ssh_host_*
    ok "sshd config valid; host keys removed (regenerated on first boot)"
}

# ----------------------------------------------------------------------------
# 6) Users, sudo, login defaults
# ----------------------------------------------------------------------------
set_login_def() {
    local key="$1" value="$2" file="$ROOTFS/etc/login.defs"
    if grep -qE "^[#[:space:]]*${key}[[:space:]]" "$file"; then
        sed -i -E "s|^[#[:space:]]*${key}[[:space:]].*|${key}\t\t${value}|" "$file"
    else
        printf '%s\t\t%s\n' "$key" "$value" >> "$file"
    fi
}

configure_users() {
    log "Configuring users and sudo"

    # /etc/login.defs belongs to the 'login' package; editing another
    # package's conffile is image-time only (not part of sisura-hardening).
    set_login_def UMASK 027
    set_login_def HOME_MODE 0700
    set_login_def PASS_MAX_DAYS 365
    set_login_def PASS_MIN_DAYS 1
    set_login_def PASS_WARN_AGE 14
    set_login_def ENCRYPT_METHOD YESCRYPT

    # Root: no password login at all.
    chroot_run passwd -l root >/dev/null

    # Member of 'sudo' => allowed by sshd (AllowGroups sudo sisura-ssh).
    if ! chroot_run id "$ADMIN_USER" >/dev/null 2>&1; then
        chroot_run useradd --create-home --shell /bin/bash \
            --user-group --groups sudo "$ADMIN_USER"
    fi

    if [[ -n "$DEBUG_PASSWORD" ]]; then
        warn "SISURA_DEBUG_PASSWORD set - development image, do not ship!"
        # Passed via stdin, never via argv (not visible in ps). -e: the hash
        # is computed here, so pwquality (enforce_for_root) cannot block it.
        local hash
        hash="$(printf '%s' "$DEBUG_PASSWORD" | chroot_run openssl passwd -6 -stdin)"
        printf '%s:%s\n' "$ADMIN_USER" "$hash" | chroot_run chpasswd -e
    elif [[ "$PROFILE" == install ]]; then
        # Empty + expired: the first console login (pam_unix nullok) goes
        # straight to a forced password change checked by pwquality. Until
        # then sudo works only from that console session; SSH never accepts
        # passwords and has no key yet.
        chroot_run passwd -d "$ADMIN_USER" >/dev/null
        chroot_run chage -d 0 "$ADMIN_USER"
    else
        # Locked password; with UsePAM=yes, pubkey login still works.
        chroot_run passwd -l "$ADMIN_USER" >/dev/null
    fi

    if [[ -n "$SSH_PUBKEY_FILE" ]]; then
        local home="/home/${ADMIN_USER}"
        install -d -m 0700 "${ROOTFS}${home}/.ssh"
        install -m 0600 "$SSH_PUBKEY_FILE" "${ROOTFS}${home}/.ssh/authorized_keys"
        chroot_run chown -R "${ADMIN_USER}:${ADMIN_USER}" "${home}/.ssh"
        ok "Installed SSH public key for ${ADMIN_USER}"
    fi

    # sudo policy (Defaults) ships in sisura-hardening: /etc/sudoers.d/sisura.
    # The vm profile only adds the NOPASSWD rule; install keeps "%sudo ALL".
    if [[ "$PROFILE" == vm ]]; then
        local sudoers="/etc/sudoers.d/90-sisura-vm"
        printf '# Sisura vm profile: throwaway VMs, no password exists.\n%s ALL=(ALL:ALL) NOPASSWD: ALL\n' \
            "$ADMIN_USER" | write_file 0440 "$sudoers"
    fi
    chroot_run visudo -c >/dev/null || die "Invalid sudoers configuration"

    # Provenance of this image; sisura-install adds INSTALLED= on disk installs.
    {
        printf 'RELEASE=%s\n' "$RELEASE"
        printf 'BUILD_ID=%s\n' "$BUILD_ID"
        printf 'BUILD_DATE=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'SUITE=%s\nARCH=%s\nPROFILE=%s\nUSER=%s\n' "$SUITE" "$ARCH" "$PROFILE" "$ADMIN_USER"
    } | write_file 0644 /etc/sisura/build-info

    ok "Users configured (admin=${ADMIN_USER}, profile=${PROFILE})"
}

# ----------------------------------------------------------------------------
# 7) Kernel command line (sysctl/modprobe/limits come from sisura-hardening)
# ----------------------------------------------------------------------------
write_kernel_cmdline() {
    [[ "$KERNEL_CMDLINE_HARDENING" == apparmor=1* ]] \
        || die "SISURA_KERNEL_CMDLINE not found in $KERNEL_GRUB_CFG"
    echo "$KERNEL_CMDLINE_HARDENING" > "${ARTIFACT_PREFIX}.cmdline"
    ok "Kernel cmdline: ${ARTIFACT_PREFIX}.cmdline"
}

# ----------------------------------------------------------------------------
# 8) AppArmor + UFW defaults (image-time: these are other packages' conffiles)
# ----------------------------------------------------------------------------
harden_lsm_firewall() {
    log "Configuring AppArmor and UFW"

    # AppArmor: Debian kernels enable it by default; we also pin it on the
    # kernel cmdline. Default + apparmor-profiles stay as shipped.
    chroot_run systemctl enable apparmor.service

    # UFW applies IPT_SYSCTL on every start: point it at our sysctl file,
    # otherwise /etc/ufw/sysctl.conf would silently override values.
    # The rules themselves are applied by sisura-firstboot-ufw.service.
    sed -i -E \
        -e 's|^IPV6=.*|IPV6=yes|' \
        -e 's|^DEFAULT_INPUT_POLICY=.*|DEFAULT_INPUT_POLICY="DROP"|' \
        -e 's|^DEFAULT_OUTPUT_POLICY=.*|DEFAULT_OUTPUT_POLICY="ACCEPT"|' \
        -e 's|^DEFAULT_FORWARD_POLICY=.*|DEFAULT_FORWARD_POLICY="DROP"|' \
        -e 's|^IPT_SYSCTL=.*|IPT_SYSCTL=/etc/sysctl.d/99-sisura.conf|' \
        "$ROOTFS/etc/default/ufw"

    ok "AppArmor + UFW configured"
}

# ----------------------------------------------------------------------------
# 9) SUID/SGID: final pass + report (the APT hook keeps it applied later)
# ----------------------------------------------------------------------------
report_suid() {
    log "setuid/setgid binaries after strip"
    chroot_run /usr/sbin/sisura-suid-strip | sed 's/^/    /'
}

# ----------------------------------------------------------------------------
# 10) Services
# ----------------------------------------------------------------------------
configure_services() {
    log "Enabling / disabling services"

    chroot_run systemctl set-default multi-user.target
    chroot_run systemctl enable \
        systemd-networkd.service \
        systemd-resolved.service \
        systemd-timesyncd.service \
        ssh.service \
        ufw.service
    # sisura-ssh-hostkeys / sisura-firstboot-ufw: enabled by sisura-hardening postinst.

    # Not needed in a minimal VM, or wasteful on RAM/CPU. apt-daily(-upgrade)
    # stay enabled: they drive unattended security upgrades.
    local unit
    for unit in e2scrub_all.timer e2scrub_reap.service fstrim.timer; do
        chroot_run systemctl disable "$unit" >/dev/null 2>&1 || true
    done

    # Hardening: no Ctrl-Alt-Del reboot, no debug shell on tty9.
    chroot_run systemctl mask ctrl-alt-del.target debug-shell.service

    ok "Services configured"
}

# ----------------------------------------------------------------------------
# 11) Cleanup: caches, docs, identity
# ----------------------------------------------------------------------------
cleanup_rootfs() {
    log "Cleaning caches, docs and per-machine identity"

    chroot_run apt-get clean
    chroot_run /bin/bash -c '
        set -e
        rm -rf /var/lib/apt/lists/* /var/cache/apt/*.bin /var/cache/debconf/*-old
        # Files installed by debootstrap before the dpkg path-exclude was active.
        find /usr/share/doc -mindepth 1 ! -name copyright ! -type d -delete 2>/dev/null || true
        find /usr/share/doc -mindepth 1 -type d -empty -delete 2>/dev/null || true
        rm -rf /usr/share/man/* /usr/share/info/* /usr/share/groff/* /usr/share/lintian/*
        find /usr/share/locale -mindepth 1 -maxdepth 1 ! -name locale.alias -exec rm -rf {} + 2>/dev/null || true
        # Logs / temp / history
        find /var/log -type f -exec truncate -s 0 {} +
        rm -rf /tmp/* /var/tmp/* /root/.bash_history /root/.cache
    '

    # Identity must be regenerated per VM.
    : > "$ROOTFS/etc/machine-id"
    rm -f "$ROOTFS/var/lib/dbus/machine-id"
    rm -f "$ROOTFS"/etc/ssh/ssh_host_*
    rm -f "$ROOTFS/var/lib/systemd/random-seed"

    # Build-only artifacts.
    rm -f "$ROOTFS/usr/sbin/policy-rc.d"
    rm -f "$ROOTFS/etc/resolv.conf"
    ln -s ../run/systemd/resolve/stub-resolv.conf "$ROOTFS/etc/resolv.conf"

    {
        printf '# Sisura %s build %s (%s, %s)\n' "$RELEASE" "$BUILD_ID" "$PROFILE" "$ARCH"
        # shellcheck disable=SC2016  # dpkg-query format, not shell
        chroot_run dpkg-query -W -f='${Package}\t${Version}\n'
    } > "${ARTIFACT_PREFIX}.manifest"
    ok "Cleanup done ($(wc -l < "${ARTIFACT_PREFIX}.manifest") packages)"
}

# ----------------------------------------------------------------------------
# 12) Export artifacts
# ----------------------------------------------------------------------------
export_artifacts() {
    log "Exporting kernel, initrd and rootfs archive"

    local vmlinuz initrd
    vmlinuz="$(find "$ROOTFS/boot" -maxdepth 1 -name 'vmlinuz-*' | sort -V | tail -n1)"
    initrd="$(find "$ROOTFS/boot" -maxdepth 1 -name 'initrd.img-*' | sort -V | tail -n1)"
    [[ -n "$vmlinuz" && -n "$initrd" ]] || die "Kernel or initrd not found in $ROOTFS/boot"
    install -m 0644 "$vmlinuz" "${ARTIFACT_PREFIX}.vmlinuz"
    install -m 0644 "$initrd"  "${ARTIFACT_PREFIX}.initrd.img"
    install -m 0644 "$ROOTFS/etc/sisura/build-info" "${ARTIFACT_PREFIX}.build-info"

    if ((DO_TAR)); then
        local out
        out="${BUILD_DIR}/sisura-rootfs-${RELEASE}$(profile_tag)-${ARCH}.tar.zst"
        # --xattrs keeps file capabilities (security.capability), e.g. ping.
        tar --numeric-owner --xattrs --xattrs-include='*' --acls \
            -C "$ROOTFS" -cpf - . | zstd -T0 -19 -q -f -o "$out"
        (cd "$BUILD_DIR" && sha256sum "$(basename "$out")" > "$(basename "$out").sha256")
        ok "Archive: $out ($(du -h "$out" | cut -f1))"
    fi
}

summary() {
    printf '\n%sSisura rootfs ready%s\n' "$C_GRN" "$C_OFF"
    printf '  rootfs      : %s (%s)\n' "$ROOTFS" "$(du -sh --one-file-system "$ROOTFS" | cut -f1)"
    printf '  kernel      : %s.vmlinuz\n' "$ARTIFACT_PREFIX"
    printf '  initrd      : %s.initrd.img\n' "$ARTIFACT_PREFIX"
    printf '  cmdline     : %s.cmdline\n' "$ARTIFACT_PREFIX"
    printf '  manifest    : %s.manifest\n' "$ARTIFACT_PREFIX"
    printf '  build       : %s %s (%s.build-info)\n' "$RELEASE" "$BUILD_ID" "$ARTIFACT_PREFIX"
    printf '  admin user  : %s\n' "$ADMIN_USER"
}

# ----------------------------------------------------------------------------
main() {
    parse_args "$@"
    sisura_resolve_release || die "Release configuration error"
    BUILD_ID="$(sisura_build_id)" || die "Build id error"
    if [[ "$(uname -s)" != "Linux" ]]; then
        reexec_in_docker "$@"
    fi
    preflight
    init_paths
    prepare_rootfs_dir

    bootstrap
    configure_apt
    mount_chroot
    install_packages
    disable_initramfs_ssh
    install_sisura_packages
    configure_system
    configure_ssh
    configure_users
    write_kernel_cmdline
    harden_lsm_firewall
    configure_services
    report_suid
    cleanup_rootfs
    umount_chroot

    export_artifacts
    summary
}

main "$@"
