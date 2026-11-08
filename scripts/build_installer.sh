#!/usr/bin/env bash
# =============================================================================
#  SedirOS - build a Rust tool of the image (sediros-install,
#  sediros-audit) for the image architecture
#
#  Usage : ./scripts/build_installer.sh [crate]   (default: sediros-install)
#  Host  : Linux with cargo (native build), or any host with Docker
#          (debian:trixie + Debian's cargo, so the binary links against the
#          same glibc as the image). Needs network for crates.io once; the
#          registry is cached in the 'sediros-cargo-registry' Docker volume.
#
#  Output: build/bin/<crate>-<arch>   (packaged by build_packages.sh)
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly SCRIPT_DIR PROJECT_ROOT

NAME="sediros-install"
OUT_DIR="${PROJECT_ROOT}/build/bin"
DOCKER_IMAGE="${SEDIROS_RUST_DOCKER_IMAGE:-debian:trixie}"
# Containers run with the host's architecture: a local image tag that was
# pulled for another platform would otherwise build for that one (emulated).
docker_platform() {
    case "$(uname -m)" in
        arm64|aarch64) echo linux/arm64 ;;
        x86_64|amd64)  echo linux/amd64 ;;
        *)             uname -m ;;
    esac
}

log() { printf '==> %s\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
die() { printf '  ✗ %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
    -h|--help) awk 'NR>2 && /^# =====/ {exit} NR>2 {sub(/^#/, ""); sub(/^ /, ""); print}' "$0"; exit 0 ;;
    "") ;;
    sediros-install|sediros-audit) NAME="$1" ;;
    *) die "Unknown argument: $1" ;;
esac
CRATE="${PROJECT_ROOT}/${NAME}"
# Separate cargo target dirs; sediros-install keeps its existing one.
if [[ "$NAME" == sediros-install ]]; then TARGET_DIR=build/cargo-installer; else TARGET_DIR="build/cargo-${NAME}"; fi

[[ -f "$CRATE/Cargo.lock" ]] || die "${NAME}/Cargo.lock missing (run: cargo generate-lockfile)"
mkdir -p "$OUT_DIR"

if [[ "$(uname -s)" == Linux ]] && command -v cargo >/dev/null 2>&1 \
   && command -v dpkg >/dev/null 2>&1; then
    arch="$(dpkg --print-architecture)"
    log "Building ${NAME} natively ($arch)"
    CARGO_TARGET_DIR="$PROJECT_ROOT/$TARGET_DIR" \
        cargo build --release --locked --manifest-path "$CRATE/Cargo.toml"
    install -m 0755 "$PROJECT_ROOT/$TARGET_DIR/release/${NAME}" \
        "$OUT_DIR/${NAME}-$arch"
    ok "build/bin/${NAME}-$arch"
    exit 0
fi

command -v docker >/dev/null 2>&1 || die "Need cargo on a Debian-based Linux host, or docker."
log "Building ${NAME} in ${DOCKER_IMAGE}"
docker run --rm --platform "$(docker_platform)" \
    -v "${PROJECT_ROOT}:/sediros" \
    -v sediros-cargo-registry:/root/.cargo/registry \
    -e CARGO_TARGET_DIR="/sediros/$TARGET_DIR" -e NAME="$NAME" \
    -w "/sediros/${NAME}" \
    "$DOCKER_IMAGE" bash -c '
        set -e
        apt-get update -qq
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
            cargo rustc ca-certificates >/dev/null
        cargo build --release --locked
        arch="$(dpkg --print-architecture)"
        install -m 0755 "$CARGO_TARGET_DIR/release/$NAME" "/sediros/build/bin/$NAME-$arch"
        echo "  ✓ build/bin/$NAME-$arch"'
