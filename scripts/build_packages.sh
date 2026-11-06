#!/usr/bin/env bash
# =============================================================================
#  Sisura Linux - build the Sisura .deb packages
#
#  Usage : ./scripts/build_packages.sh [package...]   (default: all packages)
#  Host  : Linux with dpkg-deb, or macOS with Docker (re-runs itself in a
#          container; dpkg-deb ships with the base image, no network needed).
#
#  Input : packages/<name>/   (DEBIAN/control + filesystem tree)
#  Output: build/debs/<name>_<version>_<arch>.deb
#
#  Placeholders replaced in every package file (incl. DEBIAN/control):
#    @SISURA_RELEASE@ (13.1)  @SISURA_MAJOR@ (13)  @SISURA_CODENAME@ (trixie)
#    @SISURA_ARCH@ (dpkg architecture of the build host = image arch)
#  taken from SISURA_RELEASE / SISURA_SUITE (scripts/lib/release.sh).
#
#  Compiled programs: packages/<name>/.sisura-binaries lists
#    <path in package> <file relative to the project root>
#  A package whose binaries are not built yet is skipped with a warning
#  (scripts/build_installer.sh builds sisura-install).
#
#  Files are owned by root:root inside the package regardless of the
#  checkout's owner (--root-owner-group); permissions are normalised here
#  so a sloppy checkout (umask, git) cannot leak into the package.
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly SCRIPT_DIR PROJECT_ROOT

PKG_SRC="${PROJECT_ROOT}/packages"
OUT_DIR="${SISURA_DEBS_DIR:-${PROJECT_ROOT}/build/debs}"
DOCKER_IMAGE="${SISURA_DOCKER_IMAGE:-debian:bookworm}"
# Containers run with the host's architecture: a local image tag that was
# pulled for another platform would otherwise build for that one (emulated).
docker_platform() {
    case "$(uname -m)" in
        arm64|aarch64) echo linux/arm64 ;;
        x86_64|amd64)  echo linux/amd64 ;;
        *)             uname -m ;;
    esac
}

# shellcheck source=scripts/lib/release.sh
source "${SCRIPT_DIR}/lib/release.sh"

log() { printf '==> %s\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
die() { printf '  ✗ %s\n' "$*" >&2; exit 1; }

# Only the named packages (rebuilding a .deb makes everything built from it
# look outdated to make, e.g. the VM image when only the installer changed).
ONLY=()
for arg in "$@"; do
    case "$arg" in
        -h|--help) awk 'NR>2 && /^# =====/ {exit} NR>2 {sub(/^#/, ""); sub(/^ /, ""); print}' "$0"; exit 0 ;;
        -*) die "Unknown option: $arg" ;;
        *) [[ -f "$PKG_SRC/$arg/DEBIAN/control" ]] || die "No package '$arg' under $PKG_SRC"
           ONLY+=("$arg") ;;
    esac
done

sisura_resolve_release || die "Release configuration error"

if ! command -v dpkg-deb >/dev/null 2>&1; then
    [[ "$(uname -s)" != "Linux" ]] || die "dpkg-deb not found (apt install dpkg)"
    command -v docker >/dev/null 2>&1 || die "Neither dpkg-deb nor docker found."
    log "No dpkg-deb on this host, re-running inside ${DOCKER_IMAGE}"
    exec docker run --rm --platform "$(docker_platform)" -v "${PROJECT_ROOT}:/sisura" -w /sisura \
        -e SISURA_RELEASE="$RELEASE" -e SISURA_SUITE="$SUITE" \
        "$DOCKER_IMAGE" ./scripts/build_packages.sh "$@"
fi

DEB_ARCH="$(dpkg --print-architecture)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sisura-pkg.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT_DIR"

built=0
for src in "$PKG_SRC"/*/; do
    src="${src%/}"
    [[ -f "$src/DEBIAN/control" ]] || continue
    if ((${#ONLY[@]})) && [[ " ${ONLY[*]} " != *" $(basename "$src") "* ]]; then
        continue
    fi

    stage="$WORK/$(basename "$src")"
    cp -R "$src" "$stage"
    find "$stage" -name '.DS_Store' -delete

    # Release placeholders (only files that contain one are rewritten).
    while IFS= read -r -d '' f; do
        sed -i -e "s|@SISURA_RELEASE@|${RELEASE}|g" \
               -e "s|@SISURA_MAJOR@|${RELEASE%%.*}|g" \
               -e "s|@SISURA_CODENAME@|${SUITE}|g" \
               -e "s|@SISURA_ARCH@|${DEB_ARCH}|g" "$f"
    done < <(grep -rlZ '@SISURA_[A-Z]*@' "$stage" || true)
    if leftover="$(grep -rl '@SISURA_[A-Z]*@' "$stage")"; then
        die "Unknown placeholder in: ${leftover//$stage/}"
    fi

    # Compiled programs built outside this script.
    if [[ -f "$stage/.sisura-binaries" ]]; then
        missing=""
        while read -r dest from; do
            [[ -z "$dest" || "$dest" == \#* ]] && continue
            if [[ -f "$PROJECT_ROOT/$from" ]]; then
                install -D -m 0755 "$PROJECT_ROOT/$from" "$stage/$dest"
            else
                missing+=" $from"
            fi
        done < "$stage/.sisura-binaries"
        rm -f "$stage/.sisura-binaries"
        if [[ -n "$missing" ]]; then
            printf '  - skipping %s: not built yet:%s\n' "$(basename "$src")" "$missing"
            continue
        fi
    fi

    control="$stage/DEBIAN/control"
    name="$(sed -n 's/^Package: *//p' "$control")"
    version="$(sed -n 's/^Version: *//p' "$control")"
    arch="$(sed -n 's/^Architecture: *//p' "$control")"
    [[ -n "$name" && -n "$version" && -n "$arch" ]] || die "$src: Package/Version/Architecture missing"
    [[ "$(basename "$src")" == "$name" ]] || die "$src: directory name must match Package ($name)"

    log "Building ${name} ${version}"

    # Normalise permissions.
    find "$stage" -type d -exec chmod 0755 {} +
    find "$stage" -type f -exec chmod 0644 {} +
    for f in "$stage"/DEBIAN/{preinst,postinst,prerm,postrm,config}; do
        [[ -f "$f" ]] && chmod 0755 "$f"
    done
    for d in usr/bin usr/sbin usr/libexec usr/lib/sisura usr/lib/systemd/system-generators; do
        [[ -d "$stage/$d" ]] && find "$stage/$d" -type f -exec chmod 0755 {} +
    done
    # sudoers drop-ins: 0440 like /etc/sudoers (sudo rejects writable ones).
    [[ -d "$stage/etc/sudoers.d" ]] && find "$stage/etc/sudoers.d" -type f -exec chmod 0440 {} +
    # update-grub only runs executable /etc/grub.d scripts.
    [[ -d "$stage/etc/grub.d" ]] && find "$stage/etc/grub.d" -type f -exec chmod 0755 {} +

    # Every conffile must exist, every /etc file must be a conffile.
    if [[ -f "$stage/DEBIAN/conffiles" ]]; then
        while IFS= read -r cf; do
            [[ -z "$cf" ]] && continue
            [[ -f "$stage$cf" ]] || die "$name: conffile $cf not in package tree"
        done < "$stage/DEBIAN/conffiles"
    fi
    if [[ -d "$stage/etc" ]]; then
        while IFS= read -r f; do
            rel="/${f#"$stage"/}"
            grep -qxF "$rel" "$stage/DEBIAN/conffiles" 2>/dev/null \
                || die "$name: $rel is under /etc but not listed in DEBIAN/conffiles"
        done < <(find "$stage/etc" -type f)
    fi

    # Shell syntax of maintainer scripts and shipped scripts.
    for f in "$stage"/DEBIAN/{preinst,postinst,prerm,postrm}; do
        [[ -f "$f" ]] && { sh -n "$f" || die "$name: syntax error in $(basename "$f")"; }
    done

    # Installed-Size (KiB) for apt's disk space accounting.
    size_kib="$(du -sk --exclude=DEBIAN "$stage" 2>/dev/null | cut -f1 || du -sk "$stage" | cut -f1)"
    sed -i '/^Installed-Size:/d' "$stage/DEBIAN/control"
    echo "Installed-Size: ${size_kib}" >> "$stage/DEBIAN/control"

    out="$OUT_DIR/${name}_${version}_${arch}.deb"
    dpkg-deb --root-owner-group -Zxz --build "$stage" "$out" >/dev/null
    ok "$(basename "$out") ($(du -h "$out" | cut -f1))"
    built=$((built + 1))
done

((built > 0)) || die "No packages found under $PKG_SRC"
