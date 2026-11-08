# shellcheck shell=bash
# shellcheck disable=SC2034  # RELEASE, DEBIAN and SUITE are outputs for the sourcing script
# =============================================================================
#  SedirOS - release <-> Debian mapping (sourced by the build scripts).
#
#  A SedirOS release is "<major>.<minor>", SedirOS's own numbering. Each
#  major is based on one Debian release: SedirOS 1 = Debian 13 (trixie),
#  SedirOS 2 = Debian 14 (forky). The suite is derived from the major number;
#  SEDIROS_SUITE may only override it with sid/unstable (development builds).
#  (Release candidates before 1.0 were numbered 13.1-rcN after Debian 13.)
#
#  Input : SEDIROS_RELEASE (default: RELEASE from config/sediros.conf)
#          SEDIROS_SUITE   (optional)
#  Output: RELEASE, DEBIAN (Debian major), SUITE
# =============================================================================

sediros_default_release() {
    local conf
    conf="$(dirname "${BASH_SOURCE[0]}")/../../config/sediros.conf"
    sed -n 's/^RELEASE[[:space:]]*?=[[:space:]]*//p' "$conf" 2>/dev/null | head -n1
}

# SedirOS major -> Debian major
sediros_debian_for_major() {
    case "$1" in
        1) echo 13 ;;
        2) echo 14 ;;
        *) return 1 ;;
    esac
}

sediros_suite_for_debian() {
    case "$1" in
        13) echo trixie ;;
        14) echo forky ;;
        *)  return 1 ;;
    esac
}

# Sets RELEASE, DEBIAN and SUITE, or prints an error and returns 1.
sediros_resolve_release() {
    RELEASE="${SEDIROS_RELEASE:-$(sediros_default_release)}"
    if [[ ! "$RELEASE" =~ ^[1-9][0-9]*\.[0-9]+$ ]]; then
        echo "Invalid release '${RELEASE}' (expected <major>.<minor>, e.g. 1.0)" >&2
        return 1
    fi
    DEBIAN="$(sediros_debian_for_major "${RELEASE%%.*}")" \
        || { echo "No Debian release known for SedirOS ${RELEASE}" >&2; return 1; }

    local mapped=""
    mapped="$(sediros_suite_for_debian "$DEBIAN")" || mapped=""

    case "${SEDIROS_SUITE:-}" in
        "")
            [[ -n "$mapped" ]] || { echo "No Debian suite known for release ${RELEASE}" >&2; return 1; }
            SUITE="$mapped" ;;
        sid|unstable)
            SUITE="$SEDIROS_SUITE" ;;
        "$mapped")
            SUITE="$mapped" ;;
        *)
            echo "Suite '${SEDIROS_SUITE}' does not match release ${RELEASE} (${mapped:-unknown})" >&2
            return 1 ;;
    esac
}

# Build identifier: <UTC yyyymmdd.HHMM>-<git short sha>[-dirty]. "-dirty"
# means the tree had uncommitted changes. Computed once on the host (git is
# not available in the Docker re-exec) and passed on as SEDIROS_BUILD_ID.
sediros_build_id() {
    if [[ -n "${SEDIROS_BUILD_ID:-}" ]]; then
        [[ "$SEDIROS_BUILD_ID" =~ ^[0-9A-Za-z._-]{1,64}$ ]] \
            || { echo "Invalid SEDIROS_BUILD_ID '${SEDIROS_BUILD_ID}'" >&2; return 1; }
        printf '%s\n' "$SEDIROS_BUILD_ID"
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
