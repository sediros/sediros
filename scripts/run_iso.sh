#!/usr/bin/env bash
# =============================================================================
#  Sisura - boot a live ISO in QEMU with a serial console (Ctrl-A X to quit).
#  Usage: scripts/run_iso.sh build/sisura-<release>-install-<arch>.iso
#         scripts/run_iso.sh --no-iso build/sisura-<release>-install-<arch>.iso
#  The live session logs in automatically on the serial console (sisura-live).
#  The ISO has no SSH key; add one in the live session if you need SSH (port 2223).
#
#  Installer test: SISURA_ISO_DISK=build/test-disk.qcow2 attaches that disk
#  (created empty, 16G, if missing) as /dev/vda. After installing, boot it
#  with --no-iso (the ISO argument then only selects the architecture).
# =============================================================================
set -Eeuo pipefail

NO_ISO=0
if [[ "${1:-}" == --no-iso ]]; then NO_ISO=1; shift; fi
ISO="${1:?usage: $0 [--no-iso] <iso>}"
[[ -r "$ISO" ]] || { echo "ISO not found: $ISO" >&2; exit 1; }
MEM="${SISURA_ISO_MEM:-2048}"
PORT="${SISURA_ISO_SSH_PORT:-2223}"

arch="$(basename "$ISO" .iso)"; arch="${arch##*-}"
case "$arch" in
    arm64) qemu=qemu-system-aarch64; machine=virt; native=aarch64 ;;
    amd64) qemu=qemu-system-x86_64;  machine=q35;  native=x86_64 ;;
    *) echo "Cannot infer architecture from $ISO" >&2; exit 1 ;;
esac
command -v "$qemu" >/dev/null || { echo "$qemu not found (install qemu)" >&2; exit 1; }

accel=tcg; cpu=max
if [[ "$(uname -m)" == "$native" || ( "$native" == aarch64 && "$(uname -m)" == arm64 ) ]]; then
    if [[ "$(uname -s)" == Darwin ]]; then accel=hvf; cpu=host
    elif [[ -w /dev/kvm ]]; then accel=kvm; cpu=host; fi
fi

firmware=()
if [[ "$arch" == arm64 ]]; then
    # UEFI is mandatory on arm64 virt. Homebrew ships edk2; Debian: qemu-efi-aarch64.
    qemu_share="$(cd "$(dirname "$(command -v "$qemu")")/../share/qemu" 2>/dev/null && pwd || true)"
    fw=""
    for f in "$qemu_share/edk2-aarch64-code.fd" \
             /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
             /usr/share/AAVMF/AAVMF_CODE.fd; do
        [[ -r "$f" ]] && { fw="$f"; break; }
    done
    [[ -n "$fw" ]] || { echo "No aarch64 UEFI firmware found (brew install qemu / apt install qemu-efi-aarch64)" >&2; exit 1; }
    firmware=(-bios "$fw")
fi

media=()
if ((NO_ISO == 0)); then
    media+=(-device virtio-scsi-pci
            -drive "if=none,id=cd,media=cdrom,readonly=on,file=$ISO"
            -device scsi-cd,drive=cd,bootindex=0)
fi
if [[ -n "${SISURA_ISO_DISK:-}" ]]; then
    if [[ ! -e "$SISURA_ISO_DISK" ]]; then
        "$(dirname "$(command -v "$qemu")")/qemu-img" create -q -f qcow2 "$SISURA_ISO_DISK" 16G
        echo "Created empty disk $SISURA_ISO_DISK (16G)"
    fi
    media+=(-drive "if=none,id=hd,format=qcow2,file=$SISURA_ISO_DISK"
            -device virtio-blk-pci,drive=hd,bootindex=1)
elif ((NO_ISO)); then
    echo "--no-iso needs SISURA_ISO_DISK" >&2; exit 1
fi

what="$ISO"; ((NO_ISO)) && what="$SISURA_ISO_DISK"
echo "Booting $what ($arch, $accel, ${MEM}M) - SSH on 127.0.0.1:$PORT - quit: Ctrl-A X"
exec "$qemu" \
    -machine "$machine" -accel "$accel" -cpu "$cpu" -smp 2 -m "$MEM" \
    "${firmware[@]}" \
    "${media[@]}" \
    -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${PORT}-:22" \
    -device virtio-net-pci,netdev=net0 \
    -device virtio-rng-pci \
    -nographic
