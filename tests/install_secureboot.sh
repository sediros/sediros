#!/usr/bin/env bash
# =============================================================================
#  Sisura - install from the ISO to an empty disk and boot the installed
#  system, both with Secure Boot enforced.
#
#  Usage : tests/install_secureboot.sh build/sisura-<release>-install-<arch>.iso
#  Needs : UEFI firmware with Secure Boot, see tests/lib/sbvm.sh
#  Env   : SISURA_SB_KEEP_DISK=<path>  keep the installed disk there
#
#  1. Live ISO: sisura-install --yes with a LUKS passphrase, an admin
#     password and a GRUB password (signed GRUB must handle password_pbkdf2
#     and still boot the default entry unattended).
#  2. Installed disk (same EFI variable store, so the firmware uses the
#     NVRAM entry the installer created): unlock LUKS on the serial console,
#     log in, check that Secure Boot is enforced.
#  Test-only secrets, typed into a throwaway VM.
# =============================================================================
set -Eeuo pipefail

ISO="${1:?usage: $0 <iso>}"
[[ -r "$ISO" ]] || { echo "ISO not found: $ISO" >&2; exit 1; }
arch="$(basename "$ISO" .iso)"; arch="${arch##*-}"

# shellcheck source=tests/lib/sbvm.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/sbvm.sh"
sb_init "$arch"

PASSPHRASE="sb-test-Luks-4711-passphrase"
PASSWORD="Kv7mPq2-Lx9wTz4R"
disk="$SB_WORK/disk.qcow2"
"$(dirname "$(command -v "$SB_QEMU")")/qemu-img" create -q -f qcow2 "$disk" 16G
hd=(-drive "if=none,id=hd,format=qcow2,file=$disk" -device "virtio-blk-pci,drive=hd,bootindex=1")

# --- 1. install ---------------------------------------------------------------
echo "1/2 Installing from $ISO with Secure Boot ($arch, $SB_ACCEL)"
start=$SECONDS
sb_start -device virtio-scsi-pci \
    -drive "if=none,id=cd,media=cdrom,readonly=on,file=$ISO" \
    -device "scsi-cd,drive=cd,bootindex=0" "${hd[@]}"
sb_wait ':~\$' 300
sb_mark
sb_send "printf '%s\\n' '$PASSPHRASE' > /tmp/pp; printf '%s\\n' '$PASSWORD' > /tmp/pw"
# A non-US layout: its keymap goes into the initramfs (LUKS prompt), and
# console-setup's ckbcomp prints a warning per unmapped key while building it.
sb_send "sudo sisura-install install --yes --disk vda --keyboard tr --passphrase-file /tmp/pp --password-file /tmp/pw --grub-password-file /tmp/pw; echo SISURA-INSTALL-RC-\$?"
sb_wait 'SISURA-INSTALL-RC-[0-9]+' 2400
grep -aq 'SISURA-INSTALL-RC-0' <<<"$(sb_console)" || sb_fail "sisura-install failed"
# Those warnings are filtered: dozens of them hid the real output.
if grep -aq 'Unknown X keysym' <<<"$(sb_console)"; then
    sb_fail "ckbcomp keymap warnings shown during the install"
fi
# tar warnings while copying the system: the squashfs has no xattr table
# (EOPNOTSUPP for every file), the ESP mount point takes no ACLs.
if grep -aq 'Operation not supported' <<<"$(sb_console)"; then
    sb_fail "'Operation not supported' warnings shown during the install"
fi
grep -aq 'source has no extended attributes' <<<"$(sb_console)" \
    || echo "      note: source squashfs has extended attributes"
# The live session runs on the serial console, so the installed system must
# use it too (otherwise the LUKS prompt only appears on the screen).
grep -aqE 'Serial console tty(S|AMA)0' <<<"$(sb_console)" \
    || sb_fail "sisura-install did not detect the serial console"
echo "      installed in $((SECONDS - start))s"
sb_send "sudo poweroff"
sb_stopped 120

# --- 2. boot the installed disk ---------------------------------------------
echo "2/2 Booting the installed disk with Secure Boot"
start=$SECONDS
sb_start "${hd[@]}"
sb_wait 'Please unlock disk' 600
sb_send "$PASSPHRASE"
sb_wait 'login: *$' 600
sb_banner_ok
sb_send "sisura"
sb_wait 'Password: *$' 60
sb_mark
sb_send "$PASSWORD"
sb_wait ':~\$' 120
sb_mark
sb_send "sudo -v"
sb_wait 'password for' 60
sb_mark
sb_send "$PASSWORD"
sb_wait ':~\$' 60
sb_mark
sb_send "$SB_CHECK"
sb_wait 'SISURA-SB-[01] ' 60
out="$(sb_console)"
grep -aq 'SISURA-SB-1 ' <<<"$out" || sb_fail "installed system booted, but Secure Boot is not enforced"
# Every package fully configured (no half-installed or triggers-pending).
sb_mark
# shellcheck disable=SC2016  # expanded in the guest
sb_send 'echo SISURA-AUDIT-$(sudo dpkg --audit | wc -l)-END'
sb_wait 'SISURA-AUDIT-[0-9]+-END' 60
grep -aq 'SISURA-AUDIT-0-END' <<<"$(sb_console)" || sb_fail "dpkg --audit reports packages on the installed system"
lockdown="$(grep -ao 'SISURA-LOCKDOWN-.*' <<<"$out" | tail -n1)"
loader="$(tr -d '\r' < "$SB_LOG" | grep -ao 'BdsDxe: starting [^"]*"[^"]*"' | head -n1 || true)"
echo "PASS  installed system boots with Secure Boot enforced in $((SECONDS - start))s"
echo "      firmware: ${loader:-?}"
echo "      kernel: ${lockdown#SISURA-}"

if [[ -n "${SISURA_SB_KEEP_DISK:-}" ]]; then
    sb_send "sudo poweroff"
    sb_stopped 120
    mv "$disk" "$SISURA_SB_KEEP_DISK"
    echo "      disk kept: $SISURA_SB_KEEP_DISK"
fi
