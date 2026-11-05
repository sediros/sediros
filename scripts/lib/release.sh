# shellcheck shell=bash
# =============================================================================
#  Sisura - release <-> Debian suite mapping (sourced by the build scripts).
#
#  A Sisura release is "<debian major>.<point>", e.g. 13.1 = Debian 13 (trixie),
#  first Sisura point release. The suite is derived from the major number;
#  SISURA_SUITE may only override it with sid/unstable (development builds).
#
#  Input : SISURA_RELEASE (default: RELEASE from config/sisura.conf)
#          SISURA_SUITE   (optional)
#  Output: RELEASE, SUITE
# =============================================================================

sisura_default_release() {
    local conf
    conf="$(dirname "${BASH_SOURCE[0]}")/../../config/sisura.conf"
    sed -n 's/^RELEASE[[:space:]]*?=[[:space:]]*//p' "$conf" 2>/dev/null | head -n1
}

sisura_suite_for_major() {
    case "$1" in
        12) echo bookworm ;;
        13) echo trixie ;;
        14) echo forky ;;
        *)  return 1 ;;
    esac
}

# Sets RELEASE and SUITE, or prints an error and returns 1.
sisura_resolve_release() {
    RELEASE="${SISURA_RELEASE:-$(sisura_default_release)}"
    if [[ ! "$RELEASE" =~ ^[0-9]+\.[0-9]+$ ]]; then
        echo "Invalid release '${RELEASE}' (expected <debian major>.<point>, e.g. 13.1)" >&2
        return 1
    fi

    local mapped=""
    mapped="$(sisura_suite_for_major "${RELEASE%%.*}")" || mapped=""

    case "${SISURA_SUITE:-}" in
        "")
            [[ -n "$mapped" ]] || { echo "No Debian suite known for release ${RELEASE}" >&2; return 1; }
            SUITE="$mapped" ;;
        sid|unstable)
            SUITE="$SISURA_SUITE" ;;
        "$mapped")
            SUITE="$mapped" ;;
        *)
            echo "Suite '${SISURA_SUITE}' does not match release ${RELEASE} (${mapped:-unknown})" >&2
            return 1 ;;
    esac
}

# Build identifier: <UTC yyyymmdd.HHMM>-<git short sha>[-dirty]. "-dirty"
# means the tree had uncommitted changes. Computed once on the host (git is
# not available in the Docker re-exec) and passed on as SISURA_BUILD_ID.
sisura_build_id() {
    if [[ -n "${SISURA_BUILD_ID:-}" ]]; then
        [[ "$SISURA_BUILD_ID" =~ ^[0-9A-Za-z._-]{1,64}$ ]] \
            || { echo "Invalid SISURA_BUILD_ID '${SISURA_BUILD_ID}'" >&2; return 1; }
        printf '%s\n' "$SISURA_BUILD_ID"
        return 0
    fi
    local root sha dirty=""
    root="$(dirname "${BASH_SOURCE[0]}")/../.."
    if sha="$(git -C "$root" rev-parse --short=7 HEAD 2>/dev/null)"; then
        [[ -z "$(git -C "$root" status --porcelain 2>/dev/null)" ]] || dirty="-dirty"
    else
        sha="nogit"
    fi
    printf '%s-%s%s\n' "$(date -u +%Y%m%d.%H%M)" "$sha" "$dirty"
}
