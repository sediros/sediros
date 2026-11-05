#!/usr/bin/env bash
# =============================================================================
#  Sisura Linux - build the sisura-install binary for the image architecture
#
#  Usage : ./scripts/build_installer.sh
#  Host  : Linux with cargo (native build), or any host with Docker
#          (debian:trixie + Debian's cargo, so the binary links against the
#          same glibc as the image). Needs network for crates.io once; the
#          registry is cached in the 'sisura-cargo-registry' Docker volume.
#
#  Output: build/bin/sisura-install-<arch>   (packaged by build_packages.sh)
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly SCRIPT_DIR PROJECT_ROOT

CRATE="${PROJECT_ROOT}/sisura-install"
OUT_DIR="${PROJECT_ROOT}/build/bin"
DOCKER_IMAGE="${SISURA_RUST_DOCKER_IMAGE:-debian:trixie}"

log() { printf '==> %s\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
die() { printf '  ✗ %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
    -h|--help) awk 'NR>2 && /^# =====/ {exit} NR>2 {sub(/^#/, ""); sub(/^ /, ""); print}' "$0"; exit 0 ;;
    "") ;;
    *) die "Unknown argument: $1" ;;
esac

[[ -f "$CRATE/Cargo.lock" ]] || die "sisura-install/Cargo.lock missing (run: cargo generate-lockfile)"
mkdir -p "$OUT_DIR"

if [[ "$(uname -s)" == Linux ]] && command -v cargo >/dev/null 2>&1 \
   && command -v dpkg >/dev/null 2>&1; then
    arch="$(dpkg --print-architecture)"
    log "Building sisura-install natively ($arch)"
    CARGO_TARGET_DIR="$PROJECT_ROOT/build/cargo-installer" \
        cargo build --release --locked --manifest-path "$CRATE/Cargo.toml"
    install -m 0755 "$PROJECT_ROOT/build/cargo-installer/release/sisura-install" \
        "$OUT_DIR/sisura-install-$arch"
    ok "build/bin/sisura-install-$arch"
    exit 0
fi

command -v docker >/dev/null 2>&1 || die "Need cargo on a Debian-based Linux host, or docker."
log "Building sisura-install in ${DOCKER_IMAGE}"
docker run --rm \
    -v "${PROJECT_ROOT}:/sisura" \
    -v sisura-cargo-registry:/root/.cargo/registry \
    -e CARGO_TARGET_DIR=/sisura/build/cargo-installer \
    -w /sisura/sisura-install \
    "$DOCKER_IMAGE" bash -c '
        set -e
        apt-get update -qq
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
            cargo rustc ca-certificates >/dev/null
        cargo build --release --locked
        arch="$(dpkg --print-architecture)"
        install -m 0755 /sisura/build/cargo-installer/release/sisura-install \
            "/sisura/build/bin/sisura-install-$arch"
        echo "  ✓ build/bin/sisura-install-$arch"'
