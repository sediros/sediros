#!/usr/bin/env bash
# =============================================================================
#  Sisura - boot a live ISO in QEMU with a serial console (Ctrl-A X to quit).
#  Usage: scripts/run_iso.sh build/sisura-<release>-<arch>.iso
#  SSH (if the image has your key): ssh -p 2223 -i id_sisura sisura@127.0.0.1
# =============================================================================
set -Eeuo pipefail

ISO="${1:?usage: $0 <iso>}"
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

echo "Booting $ISO ($arch, $accel, ${MEM}M) - SSH on 127.0.0.1:$PORT - quit: Ctrl-A X"
exec "$qemu" \
    -machine "$machine" -accel "$accel" -cpu "$cpu" -smp 2 -m "$MEM" \
    "${firmware[@]}" \
    -device virtio-scsi-pci \
    -drive "if=none,id=cd,media=cdrom,readonly=on,file=$ISO" \
    -device scsi-cd,drive=cd \
    -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${PORT}-:22" \
    -device virtio-net-pci,netdev=net0 \
    -device virtio-rng-pci \
    -nographic
