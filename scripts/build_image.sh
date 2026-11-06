#!/usr/bin/env bash
# =============================================================================
#  Sisura Linux - RootFS archive -> bootable qcow2 base image
#
#  Usage : ./scripts/build_image.sh [--size 4G] [--rootfs <tar.zst>]
#  Host  : Linux (root, to preserve file ownership), or macOS with Docker:
#          on a non-Linux host the script re-runs itself in a debian container.
#
#  Input : build/sisura-rootfs-<release>-<arch>.tar.zst  (from build_rootfs.sh)
#  Output: build/sisura-<release>-<arch>.qcow2           (read-only base image)
#
#  The image has no partition table and no bootloader: the root filesystem
#  (ext4, LABEL=sisura-root) is the whole disk and QEMU boots the kernel
#  directly (-kernel/-initrd). VMs use copy-on-write overlays on top of it.
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly SCRIPT_DIR PROJECT_ROOT

BUILD_DIR="${SISURA_BUILD_DIR:-${PROJECT_ROOT}/build}"
IMAGE_SIZE="${SISURA_IMAGE_SIZE:-4G}"
ROOTFS_TAR=""
FS_LABEL="sisura-root"
DOCKER_IMAGE="${SISURA_DOCKER_IMAGE:-debian:bookworm}"
# Containers run with the host's architecture: a local image tag that was
# pulled for another platform would otherwise build for that one (emulated).
docker_platform() {
    case "$(uname -m)" in
        arm64|aarch64) echo linux/arm64 ;;
        x86_64|amd64)  echo linux/amd64 ;;
        *)             uname -m ;;
    esac
}

log()  { printf '==> %s\n' "$*"; }
ok()   { printf '  ✓ %s\n' "$*"; }
die()  { printf '  ✗ %s\n' "$*" >&2; exit 1; }

usage() {
    awk 'NR>2 && /^# =====/ {exit} NR>2 {sub(/^#/, ""); sub(/^ /, ""); print}' "${BASH_SOURCE[0]}"
    exit "${1:-0}"
}

ORIG_ARGS=("$@")
while (($#)); do
    case "$1" in
        --size)    [[ $# -ge 2 ]] || die "--size needs a value"; IMAGE_SIZE="$2"; shift 2 ;;
        --rootfs)  [[ $# -ge 2 ]] || die "--rootfs needs a value"; ROOTFS_TAR="$2"; shift 2 ;;
        -h|--help) usage 0 ;;
        *)         printf 'Unknown argument: %s\n' "$1" >&2; usage 1 ;;
    esac
done
[[ "$IMAGE_SIZE" =~ ^[0-9]+[MG]$ ]] || die "Invalid --size: $IMAGE_SIZE (e.g. 2048M, 4G)"

# ----------------------------------------------------------------------------
# Non-Linux host: re-exec inside a throwaway Debian container.
# ----------------------------------------------------------------------------
if [[ "$(uname -s)" != "Linux" ]]; then
    command -v docker >/dev/null 2>&1 || die "Non-Linux host and no docker found."
    [[ "$BUILD_DIR" == "$PROJECT_ROOT"/* ]] \
        || die "On macOS SISURA_BUILD_DIR must be inside the project (bind-mounted)."
    log "Non-Linux host, re-running inside ${DOCKER_IMAGE}"
    exec docker run --rm --platform "$(docker_platform)" \
        -v "${PROJECT_ROOT}:/sisura" -w /sisura \
        -e SISURA_BUILD_DIR="/sisura/${BUILD_DIR#"$PROJECT_ROOT"/}" \
        -e SISURA_IMAGE_SIZE="$IMAGE_SIZE" \
        "$DOCKER_IMAGE" bash -c '
            set -e
            apt-get update -qq
            DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
                e2fsprogs zstd qemu-utils >/dev/null
            exec ./scripts/build_image.sh "$@"' _ "${ORIG_ARGS[@]+"${ORIG_ARGS[@]}"}"
fi

# ----------------------------------------------------------------------------
# Linux path
# ----------------------------------------------------------------------------
[[ $EUID -eq 0 ]] || die "Must run as root (file ownership inside the image)."
for cmd in mkfs.ext4 zstd tar qemu-img e2fsck; do
    command -v "$cmd" >/dev/null 2>&1 || die "Missing tool: $cmd (apt install e2fsprogs zstd qemu-utils)"
done

if [[ -z "$ROOTFS_TAR" ]]; then
    ROOTFS_TAR="$(find "$BUILD_DIR" -maxdepth 1 -name 'sisura-rootfs-*.tar.zst' -print \
                  | sort -V | tail -n1)"
fi
[[ -n "$ROOTFS_TAR" && -r "$ROOTFS_TAR" ]] \
    || die "RootFS archive not found in $BUILD_DIR - run build_rootfs.sh first."

# Integrity check against the .sha256 written by build_rootfs.sh.
if [[ -r "${ROOTFS_TAR}.sha256" ]]; then
    (cd "$(dirname "$ROOTFS_TAR")" && sha256sum -c --quiet "$(basename "$ROOTFS_TAR").sha256") \
        || die "Checksum mismatch: $ROOTFS_TAR"
    ok "Checksum verified"
fi

name="$(basename "$ROOTFS_TAR" .tar.zst)"     # sisura-rootfs-13.1-arm64
name="sisura-${name#sisura-rootfs-}"           # sisura-13.1-arm64
OUT="${BUILD_DIR}/${name}.qcow2"

# Work in container/host-local tmp, never on a macOS bind mount
# (case-insensitive APFS, no real ownership).
WORK="$(mktemp -d /tmp/sisura-image.XXXXXX)"
trap 'rm -rf --one-file-system "$WORK"' EXIT

log "Extracting $(basename "$ROOTFS_TAR")"
mkdir "$WORK/root"
zstd -dc "$ROOTFS_TAR" \
    | tar --numeric-owner --xattrs --xattrs-include='*' --acls -xpf - -C "$WORK/root"
ok "Extracted ($(du -sh "$WORK/root" | cut -f1))"

log "Creating ext4 (${IMAGE_SIZE}, LABEL=${FS_LABEL})"
# -d populates the filesystem straight from the directory: no loop device,
# no mount, works unprivileged w.r.t. the kernel (root only for ownership).
mkfs.ext4 -q -F \
    -L "$FS_LABEL" \
    -m 1 \
    -E lazy_itable_init=0,lazy_journal_init=0 \
    -d "$WORK/root" \
    "$WORK/disk.raw" "$IMAGE_SIZE"
e2fsck -fn "$WORK/disk.raw" >/dev/null || die "e2fsck reported errors"
ok "Filesystem OK"

log "Converting to qcow2"
qemu-img convert -f raw -O qcow2 -o cluster_size=65536 "$WORK/disk.raw" "${OUT}.tmp"
mv -f "${OUT}.tmp" "$OUT"
chmod 0444 "$OUT"   # base image is immutable; VMs write to overlays
(cd "$BUILD_DIR" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")

ok "Image: $OUT"
qemu-img info "$OUT" | sed -n 's/^\(virtual size\|disk size\): /    \1: /p'
