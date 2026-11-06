#!/usr/bin/env bash
# =============================================================================
#  Sisura - end-to-end smoke test.
#  Boots the current base image in a throwaway VM (isolated SISURA_HOME),
#  checks boot time, image freshness and in-guest hardening, then purges it.
#
#  Env: SISURA_BIN           sisura binary (default: sisura-cli/target/release/sisura)
#       SISURA_MAX_BOOT_SECS boot-to-SSH budget  (default: 30)
#       SISURA_MAX_RAM_MB    idle RAM budget     (default: 256)
#       SISURA_RELEASE       expected release    (default: config/sisura.conf)
#       SISURA_IMAGE         base image          (default: newest build/*.qcow2)
#       SISURA_KEEP_VM=1     keep the VM for debugging
# =============================================================================
set -Eeuo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SISURA="${SISURA_BIN:-${PROJECT_ROOT}/sisura-cli/target/release/sisura}"
MAX_BOOT="${SISURA_MAX_BOOT_SECS:-30}"
MAX_RAM="${SISURA_MAX_RAM_MB:-256}"
VM="smoke-$$"
# shellcheck source=scripts/lib/release.sh
source "${PROJECT_ROOT}/scripts/lib/release.sh"
RELEASE="${SISURA_RELEASE:-$(sisura_default_release)}"
IMAGE_ARGS=()
[[ -n "${SISURA_IMAGE:-}" ]] && IMAGE_ARGS=(--image "$SISURA_IMAGE")

[[ -x "$SISURA" ]] || { echo "sisura binary not found: $SISURA (make cli)" >&2; exit 1; }

# Isolated state dir: never touches the developer's VMs.
# Kept short: macOS limits unix socket paths (QMP) to 104 bytes.
SISURA_HOME="$(mktemp -d "${TMPDIR:-/tmp}/sis.XXXXXX")"
export SISURA_HOME SISURA_PROJECT_DIR="$PROJECT_ROOT"

cleanup() {
    if [[ "${SISURA_KEEP_VM:-0}" == "1" ]]; then
        echo "Keeping VM: SISURA_HOME=$SISURA_HOME $SISURA shell $VM"
        return
    fi
    "$SISURA" purge --yes --images "$VM" >/dev/null 2>&1 || true
    rm -rf "$SISURA_HOME"
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
if ! out="$("$SISURA" launch "$VM" --timeout 120 "${IMAGE_ARGS[@]+"${IMAGE_ARGS[@]}"}" 2>&1)"; then
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
want="$(sha256 "$PROJECT_ROOT/packages/sisura-hardening/etc/sysctl.d/99-sisura.conf")"
got="$("$SISURA" shell "$VM" -- sha256sum /etc/sysctl.d/99-sisura.conf | awk '{print $1}')"
if [[ "$want" == "$got" ]]; then pass "image matches repo sysctl config"
else fail "image freshness" "99-sisura.conf differs from repo - run 'make build'"; fi

# Same build as build/<image>.build-info? (the VM runs a cached copy)
if [[ -n "${SISURA_IMAGE:-}" && -f "${SISURA_IMAGE%.qcow2}.build-info" ]]; then
    want_build="$(sed -n 's/^BUILD_ID=//p' "${SISURA_IMAGE%.qcow2}.build-info")"
    got_build="$("$SISURA" shell "$VM" -- sed -n 's/^BUILD_ID=//p' /etc/sisura/build-info)"
    if [[ -n "$want_build" && "$want_build" == "$got_build" ]]; then pass "build id: $got_build"
    else fail "build id" "image ${want_build:-?}, VM ${got_build:-?}"; fi
fi

echo "==> Guest checks"
if ! "$SISURA" shell "$VM" -- bash -s -- "$MAX_RAM" "$RELEASE" < "$PROJECT_ROOT/tests/guest_checks.sh"; then
    FAILS=$((FAILS + 1))
fi

echo "==> Shutdown"
start=$(date +%s)
if "$SISURA" stop "$VM" --timeout 30 >/dev/null; then
    pass "graceful stop in $(( $(date +%s) - start ))s"
else
    fail "stop" "sisura stop failed"
fi

echo
if ((FAILS == 0)); then
    echo "SMOKE TEST PASSED"
else
    echo "SMOKE TEST FAILED ($FAILS)"
    exit 1
fi
