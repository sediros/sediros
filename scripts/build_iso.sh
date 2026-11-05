#!/usr/bin/env bash
# =============================================================================
#  Sisura Linux - RootFS archive -> live ISO
#
#  Usage : ./scripts/build_iso.sh [--rootfs <tar.zst>]
#  Host  : Linux (root), or macOS with Docker (re-runs itself in a container).
#
#  Input : build/sisura-rootfs-<release>-<arch>.tar.zst  (must contain live-boot)
#          build/sisura-<release>-<arch>.cmdline          (optional)
#  Output: build/sisura-<release>-<arch>.iso (+ .sha256)
#          amd64: hybrid BIOS + UEFI, bootable from CD or USB
#          arm64: UEFI
#
#  Same rootfs as the qcow2 image: one hardening source, no live-build config
#  to keep in sync. Root is squashfs + tmpfs overlay (live-boot).
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly SCRIPT_DIR PROJECT_ROOT

BUILD_DIR="${SISURA_BUILD_DIR:-${PROJECT_ROOT}/build}"
GRUB_TEMPLATE="${PROJECT_ROOT}/config/iso/grub.cfg"
DOCKER_IMAGE="${SISURA_DOCKER_IMAGE:-debian:bookworm}"
ROOTFS_TAR=""

log() { printf '==> %s\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
die() { printf '  ✗ %s\n' "$*" >&2; exit 1; }

usage() {
    awk 'NR>2 && /^# =====/ {exit} NR>2 {sub(/^#/, ""); sub(/^ /, ""); print}' "${BASH_SOURCE[0]}"
    exit "${1:-0}"
}

ORIG_ARGS=("$@")
while (($#)); do
    case "$1" in
        --rootfs)  [[ $# -ge 2 ]] || die "--rootfs needs a value"; ROOTFS_TAR="$2"; shift 2 ;;
        -h|--help) usage 0 ;;
        *)         printf 'Unknown argument: %s\n' "$1" >&2; usage 1 ;;
    esac
done

# ----------------------------------------------------------------------------
# Non-Linux host: re-exec inside a throwaway Debian container.
# ----------------------------------------------------------------------------
if [[ "$(uname -s)" != "Linux" ]]; then
    command -v docker >/dev/null 2>&1 || die "Non-Linux host and no docker found."
    [[ "$BUILD_DIR" == "$PROJECT_ROOT"/* ]] \
        || die "On macOS SISURA_BUILD_DIR must be inside the project (bind-mounted)."
    log "Non-Linux host, re-running inside ${DOCKER_IMAGE}"
    exec docker run --rm \
        -v "${PROJECT_ROOT}:/sisura" -w /sisura \
        -e SISURA_BUILD_DIR="/sisura/${BUILD_DIR#"$PROJECT_ROOT"/}" \
        "$DOCKER_IMAGE" bash -c '
            set -e
            case "$(dpkg --print-architecture)" in
                amd64) grub="grub-pc-bin grub-efi-amd64-bin" ;;
                arm64) grub="grub-efi-arm64-bin" ;;
            esac
            apt-get update -qq
            DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
                grub-common $grub xorriso mtools squashfs-tools zstd >/dev/null
            exec ./scripts/build_iso.sh "$@"' _ "${ORIG_ARGS[@]+"${ORIG_ARGS[@]}"}"
fi

# ----------------------------------------------------------------------------
# Linux path
# ----------------------------------------------------------------------------
[[ $EUID -eq 0 ]] || die "Must run as root (file ownership inside the squashfs)."
for cmd in mksquashfs grub-mkrescue xorriso mformat zstd tar; do
    command -v "$cmd" >/dev/null 2>&1 \
        || die "Missing tool: $cmd (apt install grub-common grub-efi-*-bin xorriso mtools squashfs-tools zstd)"
done
[[ -r "$GRUB_TEMPLATE" ]] || die "Missing $GRUB_TEMPLATE"

if [[ -z "$ROOTFS_TAR" ]]; then
    ROOTFS_TAR="$(find "$BUILD_DIR" -maxdepth 1 -name 'sisura-rootfs-*.tar.zst' -print | sort -V | tail -n1)"
fi
[[ -n "$ROOTFS_TAR" && -r "$ROOTFS_TAR" ]] \
    || die "RootFS archive not found in $BUILD_DIR - run build_rootfs.sh first."

if [[ -r "${ROOTFS_TAR}.sha256" ]]; then
    (cd "$(dirname "$ROOTFS_TAR")" && sha256sum -c --quiet "$(basename "$ROOTFS_TAR").sha256") \
        || die "Checksum mismatch: $ROOTFS_TAR"
fi

name="$(basename "$ROOTFS_TAR" .tar.zst)"       # sisura-rootfs-13.1-arm64
name="sisura-${name#sisura-rootfs-}"             # sisura-13.1-arm64
arch="${name##*-}"
case "$arch" in
    amd64) console="console=tty0 console=ttyS0,115200"; grub_dirs=(i386-pc x86_64-efi) ;;
    arm64) console="console=tty0 console=ttyAMA0";      grub_dirs=(arm64-efi) ;;
    *)     die "Cannot infer architecture from $name" ;;
esac
for d in "${grub_dirs[@]}"; do
    [[ -d "/usr/lib/grub/$d" ]] || die "GRUB platform $d missing (install grub-*-bin for $arch)"
done
OUT="${BUILD_DIR}/${name}.iso"

WORK="$(mktemp -d /tmp/sisura-iso.XXXXXX)"
trap 'rm -rf --one-file-system "$WORK"' EXIT

log "Extracting $(basename "$ROOTFS_TAR")"
mkdir -p "$WORK/root" "$WORK/iso/live" "$WORK/iso/boot/grub"
zstd -dc "$ROOTFS_TAR" \
    | tar --numeric-owner --xattrs --xattrs-include='*' --acls -xpf - -C "$WORK/root"

[[ -d "$WORK/root/usr/lib/live/boot" ]] \
    || die "live-boot is not installed in this rootfs - rebuild it with the current build_rootfs.sh"

# Live system: root is the live-boot overlay, there is no LABEL=sisura-root disk.
sed -i '/LABEL=sisura-root/d' "$WORK/root/etc/fstab"

vmlinuz="$(find "$WORK/root/boot" -maxdepth 1 -name 'vmlinuz-*' | sort -V | tail -n1)"
initrd="$(find "$WORK/root/boot" -maxdepth 1 -name 'initrd.img-*' | sort -V | tail -n1)"
[[ -n "$vmlinuz" && -n "$initrd" ]] || die "Kernel/initrd not found in rootfs /boot"
cp "$vmlinuz" "$WORK/iso/live/vmlinuz"
cp "$initrd"  "$WORK/iso/live/initrd.img"
ok "Kernel $(basename "$vmlinuz")"

log "Creating squashfs (zstd)"
mksquashfs "$WORK/root" "$WORK/iso/live/filesystem.squashfs" \
    -comp zstd -Xcompression-level 19 -b 1M -noappend -xattrs -no-progress \
    -e boot >/dev/null
ok "squashfs $(du -h "$WORK/iso/live/filesystem.squashfs" | cut -f1)"

cmdline=""
if [[ -r "$BUILD_DIR/${name}.cmdline" ]]; then
    cmdline="$(<"$BUILD_DIR/${name}.cmdline")"
    # vsyscall= is x86-only.
    [[ "$arch" == "amd64" ]] || cmdline="$(sed -E 's/(^| )vsyscall=[^ ]*//g' <<<"$cmdline")"
fi
version="${name#sisura-} $(date -u +%Y-%m-%d)"
sed -e "s|@VERSION@|${version}|g" \
    -e "s|@CONSOLE@|${console}|g" \
    -e "s|@CMDLINE@|${cmdline}|g" \
    "$GRUB_TEMPLATE" > "$WORK/iso/boot/grub/grub.cfg"

log "Building ISO (grub-mkrescue: ${grub_dirs[*]})"
grub-mkrescue -o "${OUT}.tmp" "$WORK/iso" >/dev/null 2>&1 \
    || { grub-mkrescue -o "${OUT}.tmp" "$WORK/iso"; die "grub-mkrescue failed"; }
mv -f "${OUT}.tmp" "$OUT"
(cd "$BUILD_DIR" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")
ok "ISO: $OUT ($(du -h "$OUT" | cut -f1))"
