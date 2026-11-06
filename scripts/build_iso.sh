#!/usr/bin/env bash
# =============================================================================
#  Sisura Linux - RootFS archive -> live ISO
#
#  Usage : ./scripts/build_iso.sh [--rootfs <tar.zst>]
#  Host  : Linux (root), or macOS with Docker (re-runs itself in a container).
#
#  Input : build/sisura-rootfs-<release>-install-<arch>.tar.zst
#          (install profile: live-boot, sisura-live, installer tools)
#          build/sisura-<release>-install-<arch>.cmdline  (optional)
#  Output: build/sisura-<release>-install-<arch>.iso (+ .sha256)
#          amd64: hybrid BIOS + UEFI, bootable from CD or USB
#          arm64: UEFI
#
#  UEFI boots with Secure Boot: the EFI image holds Debian's shim (signed by
#  Microsoft) and Debian's signed GRUB (gcd*.efi), both taken from the rootfs
#  (shim-signed, grub-efi-*-signed in the install profile), so they match
#  the packages the installer later puts on disk. The kernel is Debian's
#  signed kernel. BIOS boot (amd64) uses GRUB from the build host.
#
#  Built from the same build_rootfs.sh as the qcow2 image (one hardening
#  source, no live-build config). Root is squashfs + tmpfs overlay
#  (live-boot). The squashfs is also what sisura-install copies to disk, so
#  it keeps /boot (kernel, config) but not the live initrd.
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly SCRIPT_DIR PROJECT_ROOT

BUILD_DIR="${SISURA_BUILD_DIR:-${PROJECT_ROOT}/build}"
GRUB_TEMPLATE="${PROJECT_ROOT}/config/iso/grub.cfg"
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
    exec docker run --rm --platform "$(docker_platform)" \
        -v "${PROJECT_ROOT}:/sisura" -w /sisura \
        -e SISURA_BUILD_DIR="/sisura/${BUILD_DIR#"$PROJECT_ROOT"/}" \
        "$DOCKER_IMAGE" bash -c '
            set -e
            case "$(dpkg --print-architecture)" in
                amd64) grub="grub-pc-bin" ;;
                arm64) grub="" ;;
            esac
            apt-get update -qq
            DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
                grub-common $grub xorriso mtools dosfstools squashfs-tools zstd >/dev/null
            exec ./scripts/build_iso.sh "$@"' _ "${ORIG_ARGS[@]+"${ORIG_ARGS[@]}"}"
fi

# ----------------------------------------------------------------------------
# Linux path
# ----------------------------------------------------------------------------
[[ $EUID -eq 0 ]] || die "Must run as root (file ownership inside the squashfs)."
for cmd in mksquashfs xorriso mcopy mkfs.vfat zstd tar; do
    command -v "$cmd" >/dev/null 2>&1 \
        || die "Missing tool: $cmd (apt install xorriso mtools dosfstools squashfs-tools zstd)"
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
    amd64) console="console=tty0 console=ttyS0,115200"; efi=x64;  grub_efi=x86_64-efi ;;
    arm64) console="console=tty0 console=ttyAMA0";      efi=aa64; grub_efi=arm64-efi ;;
    *)     die "Cannot infer architecture from $name" ;;
esac
EFI_BOOT="BOOT${efi^^}.EFI"
if [[ "$arch" == amd64 ]]; then
    if [[ ! -d /usr/lib/grub/i386-pc ]] || ! command -v grub-mkimage >/dev/null; then
        die "BIOS boot needs GRUB for i386-pc (apt install grub-common grub-pc-bin)"
    fi
fi
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
    -wildcards -e 'boot/initrd.img-*' >/dev/null
ok "squashfs $(du -h "$WORK/iso/live/filesystem.squashfs" | cut -f1)"

cmdline=""
if [[ -r "$BUILD_DIR/${name}.cmdline" ]]; then
    cmdline="$(<"$BUILD_DIR/${name}.cmdline")"
    # vsyscall= is x86-only.
    [[ "$arch" == "amd64" ]] || cmdline="$(sed -E 's/(^| )vsyscall=[^ ]*//g' <<<"$cmdline")"
fi
# "13.1 build 20261105.1432-47888b3 (arm64)" from the image's build-info.
info="$WORK/root/etc/sisura/build-info"
release="$(sed -n 's/^RELEASE=//p' "$info" 2>/dev/null)"
build_id="$(sed -n 's/^BUILD_ID=//p' "$info" 2>/dev/null)"
if [[ -n "$release" && -n "$build_id" ]]; then
    version="${release} build ${build_id} (${arch})"
else
    version="${name#sisura-} $(date -u +%Y-%m-%d)"
fi
sed -e "s|@VERSION@|${version}|g" \
    -e "s|@CONSOLE@|${console}|g" \
    -e "s|@CMDLINE@|${cmdline}|g" \
    "$GRUB_TEMPLATE" > "$WORK/iso/boot/grub/grub.cfg"

# ---------------------------------------------------------------------------
# UEFI (Secure Boot): FAT image with shim as the removable-media boot file
# and signed GRUB next to it as grub<efi>.efi, which shim loads and verifies.
# gcd*.efi finds the ISO by /.disk/info and reads /boot/grub/grub.cfg; the
# grub.cfg in the EFI image is a fallback that finds this ISO by its id.
# Signed GRUB cannot load modules, so the ISO's grub.cfg must only use
# commands built into it.
# ---------------------------------------------------------------------------
shim="$WORK/root/usr/lib/shim/shim${efi}.efi.signed"
mok="$WORK/root/usr/lib/shim/mm${efi}.efi.signed"
gcd="$WORK/root/usr/lib/grub/${grub_efi}-signed/gcd${efi}.efi.signed"
for f in "$shim" "$mok" "$gcd"; do
    [[ -r "$f" ]] || die "Missing ${f#"$WORK/root"} in the rootfs (shim-signed, grub-efi-${arch}-signed)"
done
iso_id="sisura-${build_id:-$(date -u +%Y%m%d.%H%M%S)}"
mkdir -p "$WORK/iso/.disk"
printf 'Sisura Linux %s\n' "$version" > "$WORK/iso/.disk/info"
: > "$WORK/iso/.disk/${iso_id}"
cat > "$WORK/efi-grub.cfg" <<GRUBCFG
search --no-floppy --set=root --file /.disk/${iso_id}
set prefix=(\$root)/boot/grub
configfile \$prefix/grub.cfg
GRUBCFG

log "Building EFI image (shim + signed GRUB)"
efi_img="$WORK/iso/boot/grub/efi.img"
truncate -s 8M "$efi_img"
mkfs.vfat -n SISURA_EFI "$efi_img" >/dev/null
mmd -i "$efi_img" ::/EFI ::/EFI/BOOT
mcopy -i "$efi_img" "$shim" "::/EFI/BOOT/${EFI_BOOT}"
mcopy -i "$efi_img" "$gcd" "::/EFI/BOOT/grub${efi}.efi"
mcopy -i "$efi_img" "$mok" "::/EFI/BOOT/mm${efi}.efi"
mcopy -i "$efi_img" "$WORK/efi-grub.cfg" ::/EFI/BOOT/grub.cfg
ok "EFI: ${EFI_BOOT} (shim) -> grub${efi}.efi (signed GRUB)"

xorriso_args=(-as mkisofs -o "${OUT}.tmp" -r -J -joliet-long -V "SISURA_${release//./_}")
if [[ "$arch" == amd64 ]]; then
    # BIOS: El Torito core image with GRUB's i386-pc modules on the ISO; the
    # hybrid MBR boots the same core from a USB stick.
    mkdir -p "$WORK/iso/boot/grub/i386-pc"
    cp /usr/lib/grub/i386-pc/*.mod /usr/lib/grub/i386-pc/*.lst "$WORK/iso/boot/grub/i386-pc/"
    grub-mkimage -O i386-pc-eltorito -p /boot/grub \
        -o "$WORK/iso/boot/grub/i386-pc/eltorito.img" biosdisk iso9660
    xorriso_args+=(-b boot/grub/i386-pc/eltorito.img -no-emul-boot
                   -boot-load-size 4 -boot-info-table --grub2-boot-info
                   --grub2-mbr /usr/lib/grub/i386-pc/boot_hybrid.img)
    ok "BIOS: GRUB i386-pc (El Torito + hybrid MBR)"
fi
# The EFI image is both an El Torito entry (CD) and a GPT/MBR partition (USB).
xorriso_args+=(--efi-boot boot/grub/efi.img -efi-boot-part --efi-boot-image
               --protective-msdos-label "$WORK/iso")

log "Building ISO (xorriso)"
xorriso "${xorriso_args[@]}" >/dev/null 2>&1 \
    || { xorriso "${xorriso_args[@]}"; die "xorriso failed"; }
mv -f "${OUT}.tmp" "$OUT"
(cd "$BUILD_DIR" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")
ok "ISO: $OUT ($(du -h "$OUT" | cut -f1))"
