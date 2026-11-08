#!/usr/bin/env bash
# =============================================================================
#  SedirOS - end-to-end smoke test.
#  Boots the current base image in a throwaway VM (isolated SEDIR_HOME),
#  checks boot time, image freshness and in-guest hardening, then purges it.
#
#  Env: SEDIR_BIN           sedir binary (default: sedir-cli/target/release/sedir)
#       SEDIR_MAX_BOOT_SECS boot-to-SSH budget  (default: 30)
#       SEDIR_MAX_RAM_MB    idle RAM budget     (default: 256)
#       SEDIR_RELEASE       expected release    (default: config/sedir.conf)
#       SEDIR_IMAGE         base image          (default: newest build/*.qcow2)
#       SEDIR_KEEP_VM=1     keep the VM for debugging
# =============================================================================
set -Eeuo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SEDIR="${SEDIR_BIN:-${PROJECT_ROOT}/sedir-cli/target/release/sedir}"
MAX_BOOT="${SEDIR_MAX_BOOT_SECS:-30}"
MAX_RAM="${SEDIR_MAX_RAM_MB:-256}"
VM="smoke-$$"
# shellcheck source=scripts/lib/release.sh
source "${PROJECT_ROOT}/scripts/lib/release.sh"
RELEASE="${SEDIR_RELEASE:-$(sedir_default_release)}"
IMAGE_ARGS=()
[[ -n "${SEDIR_IMAGE:-}" ]] && IMAGE_ARGS=(--image "$SEDIR_IMAGE")

[[ -x "$SEDIR" ]] || { echo "sedir binary not found: $SEDIR (make cli)" >&2; exit 1; }

# Isolated state dir: never touches the developer's VMs.
# Kept short: macOS limits unix socket paths (QMP) to 104 bytes.
SEDIR_HOME="$(mktemp -d "${TMPDIR:-/tmp}/sis.XXXXXX")"
export SEDIR_HOME SEDIR_PROJECT_DIR="$PROJECT_ROOT"

cleanup() {
    if [[ "${SEDIR_KEEP_VM:-0}" == "1" ]]; then
        echo "Keeping VM: SEDIR_HOME=$SEDIR_HOME $SEDIR shell $VM"
        return
    fi
    "$SEDIR" purge --yes --images "$VM" >/dev/null 2>&1 || true
    rm -rf "$SEDIR_HOME"
}
trap cleanup EXIT

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1" | awk '{print $1}'
    else shasum -a 256 "$1" | awk '{print $1}'; fi
}

FAILS=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s: %s\n' "$1" "$2"; FAILS=$((FAILS + 1)); }

echo "==> Launching $VM"
if ! out="$("$SEDIR" launch "$VM" --timeout 120 "${IMAGE_ARGS[@]+"${IMAGE_ARGS[@]}"}" 2>&1)"; then
    echo "$out"
    echo "FAIL  launch"
    exit 1
fi
while IFS= read -r line; do printf '    %s\n' "$line"; done <<<"$out"

boot="$(sed -n 's/.*ready in \([0-9.]*\)s.*/\1/p' <<<"$out")"
if [[ -n "$boot" ]] && awk -v b="$boot" -v m="$MAX_BOOT" 'BEGIN {exit !(b <= m)}'; then
    pass "boot to SSH: ${boot}s (limit ${MAX_BOOT}s)"
else
    fail "boot to SSH" "${boot:-?}s, limit ${MAX_BOOT}s"
fi

# Image built from the current repo config? (catches "forgot to rebuild")
want="$(sha256 "$PROJECT_ROOT/packages/sedir-hardening/etc/sysctl.d/99-sedir.conf")"
got="$("$SEDIR" shell "$VM" -- sha256sum /etc/sysctl.d/99-sedir.conf | awk '{print $1}')"
if [[ "$want" == "$got" ]]; then pass "image matches repo sysctl config"
else fail "image freshness" "99-sedir.conf differs from repo - run 'make build'"; fi

# Same build as build/<image>.build-info? (the VM runs a cached copy)
if [[ -n "${SEDIR_IMAGE:-}" && -f "${SEDIR_IMAGE%.qcow2}.build-info" ]]; then
    want_build="$(sed -n 's/^BUILD_ID=//p' "${SEDIR_IMAGE%.qcow2}.build-info")"
    got_build="$("$SEDIR" shell "$VM" -- sed -n 's/^BUILD_ID=//p' /etc/sedir/build-info)"
    if [[ -n "$want_build" && "$want_build" == "$got_build" ]]; then pass "build id: $got_build"
    else fail "build id" "image ${want_build:-?}, VM ${got_build:-?}"; fi
fi

# sedir transfer: host -> VM -> host round trip, recursive
xfer="$SEDIR_HOME/xfer"
mkdir -p "$xfer/in/sub" "$xfer/out"
head -c 65536 /dev/urandom > "$xfer/in/sub/blob"
if "$SEDIR" transfer -r "$xfer/in" "$VM:/tmp" \
    && "$SEDIR" transfer "$VM:/tmp/in/sub/blob" "$xfer/out/" \
    && [[ "$(sha256 "$xfer/in/sub/blob")" == "$(sha256 "$xfer/out/blob")" ]]; then
    pass "transfer round trip (host -> VM -> host)"
else
    fail "transfer" "copied file missing or different"
fi

# sedir forward: a live port forward reaches a server in the VM
fwd_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
"$SEDIR" shell "$VM" -- 'mkdir -p /tmp/fwd && echo sedir-forward-ok > /tmp/fwd/index.html
    nohup python3 -m http.server 8000 --directory /tmp/fwd >/dev/null 2>&1 </dev/null &' || true
if "$SEDIR" forward "$VM" "$fwd_port:8000" >/dev/null \
    && [[ "$(curl -s -m 5 "http://127.0.0.1:$fwd_port/")" == sedir-forward-ok ]]; then
    pass "forward: 127.0.0.1:$fwd_port reaches port 8000 in the VM"
else
    fail "forward" "no answer on 127.0.0.1:$fwd_port"
fi
"$SEDIR" forward "$VM" --remove "$fwd_port" >/dev/null || fail "forward" "--remove failed"
"$SEDIR" shell "$VM" -- 'pkill -f "http.server 8000"' || true

echo "==> Guest checks"
if ! "$SEDIR" shell "$VM" -- bash -s -- "$MAX_RAM" "$RELEASE" < "$PROJECT_ROOT/tests/guest_checks.sh"; then
    FAILS=$((FAILS + 1))
fi

echo "==> Shutdown"
start=$(date +%s)
if "$SEDIR" stop "$VM" --timeout 30 >/dev/null; then
    pass "graceful stop in $(( $(date +%s) - start ))s"
else
    fail "stop" "sedir stop failed"
fi

echo "==> Snapshot and restore"
# Snapshot of the stopped VM, a change after it, then back to the snapshot.
if "$SEDIR" snapshot "$VM" smoke >/dev/null \
    && "$SEDIR" launch "$VM" --timeout 120 >/dev/null \
    && "$SEDIR" shell "$VM" -- 'touch ~/after-snapshot && sync' \
    && "$SEDIR" stop "$VM" --timeout 30 >/dev/null \
    && "$SEDIR" restore "$VM" smoke --yes >/dev/null \
    && "$SEDIR" launch "$VM" --timeout 120 >/dev/null \
    && "$SEDIR" shell "$VM" -- 'test ! -e ~/after-snapshot'; then
    pass "snapshot/restore: change after the snapshot is gone"
else
    fail "snapshot/restore" "VM did not return to the snapshot"
fi
"$SEDIR" stop "$VM" --timeout 30 >/dev/null || true

echo
if ((FAILS == 0)); then
    echo "SMOKE TEST PASSED"
else
    echo "SMOKE TEST FAILED ($FAILS)"
    exit 1
fi
