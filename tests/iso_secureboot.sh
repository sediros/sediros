#!/usr/bin/env bash
# =============================================================================
#  SedirOS - boot the live ISO with Secure Boot enforced and check it from the
#  live session.
#
#  Usage : tests/iso_secureboot.sh build/sediros-<release>-install-<arch>.iso
#  Needs : UEFI firmware with Secure Boot, see tests/lib/sbvm.sh
#
#  The firmware only starts shim if Microsoft signed it, shim only starts
#  GRUB if Debian signed it, and GRUB only starts a signed kernel; reaching
#  the live shell therefore proves the whole chain. The test then reads the
#  SecureBoot EFI variable inside the guest to make sure it was enforced.
# =============================================================================
set -Eeuo pipefail

ISO="${1:?usage: $0 <iso>}"
[[ -r "$ISO" ]] || { echo "ISO not found: $ISO" >&2; exit 1; }
arch="$(basename "$ISO" .iso)"; arch="${arch##*-}"

# shellcheck source=tests/lib/sbvm.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/sbvm.sh"
sb_init "$arch"

echo "Booting $ISO with Secure Boot ($arch, $SB_ACCEL)"
start=$SECONDS
sb_start -device virtio-scsi-pci \
    -drive "if=none,id=cd,media=cdrom,readonly=on,file=$ISO" \
    -device "scsi-cd,drive=cd,bootindex=0"

# Type only into the shell: keys at the GRUB menu would stop its timeout.
sb_wait ':~\$' 300
# Login banner from sediros-base-files' issue file, also with autologin.
sb_banner_ok
sb_mark
sb_send "$SB_CHECK"
sb_wait 'SEDIROS-SB-[01] ' 60
out="$(sb_console)"
grep -aq 'SEDIROS-SB-1 ' <<<"$out" || sb_fail "booted, but Secure Boot is not enforced"
lockdown="$(grep -ao 'SEDIROS-LOCKDOWN-.*' <<<"$out" | tail -n1)"
echo "PASS  Secure Boot enforced, live session reached in $((SECONDS - start))s (${lockdown#SEDIROS-})"
