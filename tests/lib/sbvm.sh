# shellcheck shell=bash
# =============================================================================
#  Sisura - helpers for tests that boot a VM with Secure Boot enforced and
#  drive it over its serial console. Sourced by tests/iso_secureboot.sh and
#  tests/install_secureboot.sh.
#
#  Firmware: UEFI with Secure Boot and Microsoft's keys enrolled
#    amd64: OVMF_CODE_4M.secboot.fd + OVMF_VARS_4M.ms.fd  (apt install ovmf)
#    arm64: AAVMF_CODE.secboot.fd   + AAVMF_VARS.ms.fd    (apt install qemu-efi-aarch64)
#  in SISURA_FIRMWARE_DIR (default /usr/share/OVMF or /usr/share/AAVMF).
#  SISURA_SB_ACCEL overrides the QEMU accelerator (default: kvm if usable,
#  else tcg; Debian's AAVMF hangs right after start under macOS HVF).
# =============================================================================

SB_WORK=""
SB_PID=""
SB_LOG=""
SB_FROM=0   # console offset: sb_wait only looks at output after this byte

# sb_init <arch>: firmware, accelerator, work dir with a copy of the
# variable store (shared by all VMs of one test, like one machine's NVRAM).
sb_init() {
    local arch="$1" native
    case "$arch" in
        amd64)
            SB_QEMU="qemu-system-x86_64"; native="x86_64"
            local fwdir="${SISURA_FIRMWARE_DIR:-/usr/share/OVMF}"
            SB_CODE="$fwdir/OVMF_CODE_4M.secboot.fd"; SB_VARS="$fwdir/OVMF_VARS_4M.ms.fd"
            # OVMF's Secure Boot build keeps its variables in SMM-protected flash.
            SB_MACHINE=(-machine "q35,smm=on" -global "driver=cfi.pflash01,property=secure,value=on")
            ;;
        arm64)
            SB_QEMU="qemu-system-aarch64"; native="aarch64"
            local fwdir="${SISURA_FIRMWARE_DIR:-/usr/share/AAVMF}"
            SB_CODE="$fwdir/AAVMF_CODE.secboot.fd"; SB_VARS="$fwdir/AAVMF_VARS.ms.fd"
            SB_MACHINE=(-machine virt)
            ;;
        *) echo "unknown architecture: $arch" >&2; exit 1 ;;
    esac
    command -v "$SB_QEMU" >/dev/null || { echo "$SB_QEMU not found" >&2; exit 1; }
    local f
    for f in "$SB_CODE" "$SB_VARS"; do
        [[ -r "$f" ]] || { echo "Secure Boot firmware not found: $f (see tests/lib/sbvm.sh)" >&2; exit 1; }
    done

    SB_ACCEL=tcg
    local host; host="$(uname -m)"; [[ "$host" == arm64 ]] && host=aarch64
    if [[ "$host" == "$native" && "$(uname -s)" != Darwin && -w /dev/kvm ]]; then
        SB_ACCEL=kvm
    fi
    SB_ACCEL="${SISURA_SB_ACCEL:-$SB_ACCEL}"
    SB_CPU=max; [[ "$SB_ACCEL" != tcg ]] && SB_CPU=host

    SB_WORK="$(mktemp -d "${TMPDIR:-/tmp}/sisura-sb.XXXXXX")"
    cp "$SB_VARS" "$SB_WORK/vars.fd"
    trap sb_cleanup EXIT
}

# sb_start <qemu args...>: boot a VM with the serial console on FIFOs.
sb_start() {
    rm -f "$SB_WORK/serial.in" "$SB_WORK/serial.out"
    mkfifo "$SB_WORK/serial.in" "$SB_WORK/serial.out"
    SB_LOG="$SB_WORK/console.log"; : > "$SB_LOG"; SB_FROM=0
    "$SB_QEMU" "${SB_MACHINE[@]}" -accel "$SB_ACCEL" -cpu "$SB_CPU" -smp 2 -m 2048 \
        -drive "if=pflash,format=raw,unit=0,readonly=on,file=$SB_CODE" \
        -drive "if=pflash,format=raw,unit=1,file=$SB_WORK/vars.fd" \
        -device virtio-rng-pci -nic none \
        -display none -monitor none -serial "pipe:$SB_WORK/serial" "$@" &
    SB_PID=$!
    cat "$SB_WORK/serial.out" >> "$SB_LOG" &
    exec 3>"$SB_WORK/serial.in"
}

sb_console() { tail -c "+$((SB_FROM + 1))" "$SB_LOG" | tr -d '\r'; }

# sb_wait <extended regex> <seconds>: wait for console output (after the
# last sb_mark); fails on Secure Boot rejections and QEMU exit.
sb_wait() {
    local re="$1" limit="$2" start=$SECONDS out
    while ((SECONDS - start < limit)); do
        out="$(sb_console)"
        grep -aqE "$re" <<<"$out" && return 0
        grep -aqiE 'Verification failed|Access Denied|bad shim signature|invalid signature' <<<"$out" \
            && sb_fail "a boot component was rejected by Secure Boot"
        kill -0 "$SB_PID" 2>/dev/null || sb_fail "QEMU exited while waiting for: $re"
        sleep 2
    done
    sb_fail "timeout (${limit}s) waiting for: $re"
}

# sb_mark: later sb_wait calls ignore everything printed so far.
sb_mark() { SB_FROM="$(wc -c < "$SB_LOG")"; }

sb_send() { printf '%s\r' "$1" >&3; }

# sb_stopped <seconds>: wait for the VM to power off.
sb_stopped() {
    local start=$SECONDS
    while kill -0 "$SB_PID" 2>/dev/null; do
        ((SECONDS - start < $1)) || sb_fail "VM did not power off within $1s"
        sleep 2
    done
    wait "$SB_PID" 2>/dev/null || true
    exec 3>&-
    SB_PID=""
}

sb_fail() {
    echo "FAIL  $*" >&2
    if [[ -n "$SB_LOG" && -r "$SB_LOG" ]]; then
        echo "--- last console output:" >&2
        tail -n 40 "$SB_LOG" | tr -d '\r' >&2
    fi
    exit 1
}

sb_cleanup() {
    exec 3>&- 2>/dev/null || true
    if [[ -n "$SB_PID" ]]; then
        kill "$SB_PID" 2>/dev/null || true
        wait "$SB_PID" 2>/dev/null || true
    fi
    [[ -n "$SB_WORK" ]] && rm -rf "$SB_WORK"
}

# Check run inside a guest shell: prints SISURA-SB-<0|1> (last byte of the
# SecureBoot EFI variable) and the kernel lockdown mode. The command echo
# never contains "SISURA-SB-1 ", only its output does.
# shellcheck disable=SC2016,SC2034  # expanded in the guest; used by the tests
SB_CHECK='echo SISURA-SB-$(tail -c1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c | od -An -tu1 | tr -d " ") SISURA-LOCKDOWN-$(sudo cat /sys/kernel/security/lockdown)'
