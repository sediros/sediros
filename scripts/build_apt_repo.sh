#!/usr/bin/env bash
# =============================================================================
#  Sisura - build and sign the apt repository from .deb files
#
#  Usage : ./scripts/build_apt_repo.sh --debs <dir> --out <dir> --key <file>
#            [--pubkey <file>] [--suite <codename>] [--arch "amd64 arm64"]
#
#    --debs    directory with the .deb files to publish (all versions)
#    --out     output directory, created empty: becomes the repository root
#    --key     armored OpenPGP secret key that signs the repository
#    --pubkey  key the signature must verify with
#              (default: the key in sisura-archive-keyring)
#    --suite   Debian codename (default: from config/sisura.conf)
#
#  Host  : Linux with dpkg-dev, apt-utils and gnupg, or macOS with Docker
#          (re-runs itself in a container).
#
#  Output (<out>):
#    pool/main/s/<package>/<file>.deb
#    dists/<suite>/main/binary-<arch>/Packages{,.gz,.xz}
#    dists/<suite>/{Release,InRelease,Release.gpg}
#    sisura-archive-keyring.pgp     public key, for setting up older systems
#
#  The repository is rebuilt from scratch each time (no state). It is signed
#  with the key from --key and then checked with gpgv against --pubkey, so a
#  signing key that does not match the key shipped to systems never gets
#  published.
# =============================================================================
set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DOCKER_IMAGE="${SISURA_DOCKER_IMAGE:-debian:trixie}"
KEYRING_PKG="packages/sisura-archive-keyring/usr/share/keyrings/sisura-archive-keyring.pgp"

log() { printf '==> %s\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
die() { printf '  ✗ %s\n' "$*" >&2; exit 1; }

docker_platform() {
    case "$(uname -m)" in
        arm64|aarch64) echo linux/arm64 ;;
        x86_64|amd64)  echo linux/amd64 ;;
        *)             uname -m ;;
    esac
}

DEBS="" OUT="" KEY="" PUBKEY="" SUITE="" ARCHS="amd64 arm64"
while (($#)); do
    case "$1" in
        --debs)   DEBS="${2:?}"; shift 2 ;;
        --out)    OUT="${2:?}"; shift 2 ;;
        --key)    KEY="${2:?}"; shift 2 ;;
        --pubkey) PUBKEY="${2:?}"; shift 2 ;;
        --suite)  SUITE="${2:?}"; shift 2 ;;
        --arch)   ARCHS="${2:?}"; shift 2 ;;
        -h|--help) awk 'NR>2 && /^# =====/ {exit} NR>2 {sub(/^#/, ""); sub(/^ /, ""); print}' "$0"; exit 0 ;;
        *) die "Unknown argument: $1 (see --help)" ;;
    esac
done
[[ -n "$DEBS" && -n "$OUT" && -n "$KEY" ]] || die "--debs, --out and --key are required (see --help)"
[[ -d "$DEBS" ]] || die "Not a directory: $DEBS"
[[ -r "$KEY" ]] || die "Key not readable: $KEY"
PUBKEY="${PUBKEY:-$PROJECT_ROOT/$KEYRING_PKG}"
[[ -r "$PUBKEY" ]] || die "Public key not readable: $PUBKEY"
abspath() { (cd "$(dirname "$1")" && printf '%s/%s\n' "$(pwd)" "$(basename "$1")"); }
# gpg/gpgv read a bare file name from GNUPGHOME, not the current directory.
PUBKEY="$(abspath "$PUBKEY")"
if [[ -z "$SUITE" ]]; then
    # shellcheck source=scripts/lib/release.sh
    source "${SCRIPT_DIR}/lib/release.sh"
    sisura_resolve_release || die "Release configuration error"
fi

missing=()
for t in dpkg-scanpackages apt-ftparchive gpg gpgv xz gzip; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
done
if ((${#missing[@]})); then
    [[ "$(uname -s)" != Linux || -n "${SISURA_IN_DOCKER:-}" ]] \
        || die "Missing: ${missing[*]} (apt install dpkg-dev apt-utils gnupg gpgv xz-utils)"
    [[ -z "${SISURA_IN_DOCKER:-}" ]] || die "Missing in container: ${missing[*]}"
    command -v docker >/dev/null 2>&1 || die "Missing: ${missing[*]}, and no docker to run in"
    log "Re-running inside ${DOCKER_IMAGE} (missing: ${missing[*]})"
    mkdir -p "$OUT"
    debs="$(abspath "$DEBS")" out="$(abspath "$OUT")" key="$(abspath "$KEY")" pub="$(abspath "$PUBKEY")"
    exec docker run --rm --platform "$(docker_platform)" \
        -v "${PROJECT_ROOT}:/sisura:ro" -v "$debs:/in/debs:ro" -v "$out:/out" \
        -v "$key:/in/key.asc:ro" -v "$pub:/in/pub.pgp:ro" \
        -e SISURA_IN_DOCKER=1 "$DOCKER_IMAGE" bash -c '
            set -e
            apt-get update -qq >/dev/null
            DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends dpkg-dev apt-utils gnupg gpgv xz-utils >/dev/null
            /sisura/scripts/build_apt_repo.sh --debs /in/debs --out /out --key /in/key.asc \
                --pubkey /in/pub.pgp --suite "$1" --arch "$2"
            chown -R "$3" /out' _ "$SUITE" "$ARCHS" "$(id -u):$(id -g)"
fi

# --- layout -------------------------------------------------------------------
log "Building apt repository (${SUITE}, ${ARCHS})"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
[[ -z "$(ls -A "$OUT")" ]] || die "Output directory not empty: $OUT"

shopt -s nullglob
debs=("$DEBS"/*.deb)
((${#debs[@]})) || die "No .deb files in $DEBS"

declare -A seen=()
for deb in "${debs[@]}"; do
    pkg="$(dpkg-deb -f "$deb" Package)"
    ver="$(dpkg-deb -f "$deb" Version)"
    arch="$(dpkg-deb -f "$deb" Architecture)"
    # SISURA_APT_ALLOW_FOREIGN: tests only (tests/apt_fixture.sh checks the pin).
    [[ "$pkg" == sisura-* || -n "${SISURA_APT_ALLOW_FOREIGN:-}" ]] \
        || die "$(basename "$deb"): only sisura-* packages belong here"
    id="${pkg}_${ver}_${arch}"
    [[ -z "${seen[$id]:-}" ]] || die "$id provided twice ($(basename "$deb"), ${seen[$id]})"
    seen[$id]="$(basename "$deb")"
    dest="$OUT/pool/main/${pkg:0:1}/${pkg}/${id}.deb"
    mkdir -p "$(dirname "$dest")"
    cp "$deb" "$dest"
done
ok "${#debs[@]} packages in pool/"

dist="$OUT/dists/$SUITE"
for arch in $ARCHS; do
    dir="$dist/main/binary-$arch"
    mkdir -p "$dir"
    # Paths in Packages are relative to the repository root.
    (cd "$OUT" && dpkg-scanpackages --multiversion --arch "$arch" pool /dev/null 2>/dev/null) > "$dir/Packages"
    gzip -9nk "$dir/Packages"
    xz -9k "$dir/Packages"
    ok "binary-$arch: $(grep -c '^Package:' "$dir/Packages") entries"
done

(cd "$dist" && apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=Sisura \
    -o APT::FTPArchive::Release::Label=Sisura \
    -o APT::FTPArchive::Release::Suite="$SUITE" \
    -o APT::FTPArchive::Release::Codename="$SUITE" \
    -o APT::FTPArchive::Release::Architectures="$ARCHS" \
    -o APT::FTPArchive::Release::Components=main \
    -o APT::FTPArchive::Release::Description="Sisura Linux packages" \
    release . > "$OUT/Release.tmp")
mv "$OUT/Release.tmp" "$dist/Release"

# --- sign -----------------------------------------------------------------------
GNUPGHOME="$(mktemp -d)"; export GNUPGHOME
trap 'gpgconf --kill all >/dev/null 2>&1 || true; rm -rf "$GNUPGHOME"' EXIT
want="$(gpg --batch --show-keys --with-colons "$PUBKEY" | awk -F: '/^fpr/{print $10; exit}')"
[[ -n "$want" ]] || die "No key in $PUBKEY"
gpg --batch --quiet --import "$KEY" 2>/dev/null || die "Could not import the signing key"
gpg --batch --list-secret-keys "$want" >/dev/null 2>&1 \
    || die "Signing key does not match $(basename "$PUBKEY") (want $want)"

sign() { gpg --batch --yes --pinentry-mode loopback --passphrase '' --digest-algo SHA512 --local-user "$want" "$@"; }
sign --clearsign -o "$dist/InRelease" "$dist/Release"
sign --detach-sign --armor -o "$dist/Release.gpg" "$dist/Release"

# Check like apt does: only the published key counts.
gpgv --quiet --keyring "$PUBKEY" "$dist/InRelease" 2>/dev/null \
    || die "InRelease does not verify with $(basename "$PUBKEY")"
gpgv --quiet --keyring "$PUBKEY" "$dist/Release.gpg" "$dist/Release" 2>/dev/null \
    || die "Release.gpg does not verify with $(basename "$PUBKEY")"
ok "signed by $want"

cp "$PUBKEY" "$OUT/sisura-archive-keyring.pgp"

# --- index.html: what a browser shows at the repository URL ----------------------
# apt never reads it; without it GitHub Pages answers the URL with a 404.
keyring_deb="$(cd "$OUT" && find pool -name 'sisura-archive-keyring_*_all.deb' | sort -V | tail -n1)"
key_sha="$(sha256sum "$PUBKEY" | cut -d' ' -f1)"
fpr_spaced="$(sed -E 's/(.{4})/\1 /g; s/ $//' <<<"$want")"
rows="$(for arch in $ARCHS; do
            awk -v a="$arch" '/^Package:/{p=$2} /^Version:/{v=$2} /^Architecture:/{print p" "v" "$2}' \
                "$dist/main/binary-$arch/Packages"
        done | sort -u -V | awk '{printf "<tr><td>%s</td><td>%s</td><td>%s</td></tr>\n", $1, $2, $3}')"
cat > "$OUT/index.html" <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sisura apt repository</title>
<style>
  :root { color-scheme: light dark; --fg: #1d1d1f; --bg: #fff; --muted: #6e6e73; --code: #f2f2f4; }
  @media (prefers-color-scheme: dark) { :root { --fg: #f5f5f7; --bg: #111113; --muted: #a1a1a6; --code: #1e1e22; } }
  body { margin: 0 auto; max-width: 46rem; padding: 2rem 1rem; font: 16px/1.55 system-ui, sans-serif; color: var(--fg); background: var(--bg); }
  h1 { font-size: 1.6rem; margin-bottom: .2rem; } h2 { font-size: 1.1rem; margin-top: 2rem; }
  p.lead { color: var(--muted); margin-top: 0; }
  pre, code { font: 13px/1.5 ui-monospace, Menlo, monospace; background: var(--code); border-radius: 6px; }
  pre { padding: .8rem 1rem; overflow-x: auto; } code { padding: .1rem .3rem; }
  table { border-collapse: collapse; width: 100%; font-size: 14px; }
  td, th { text-align: left; padding: .3rem .6rem .3rem 0; border-bottom: 1px solid var(--code); }
  a { color: inherit; }
</style>
</head>
<body>
<h1>Sisura apt repository</h1>
<p class="lead">Updates of Sisura Linux's own packages (<code>sisura-*</code>) for Debian ${SUITE}. Debian's packages and security updates still come from Debian.</p>

<h2>Already set up</h2>
<p>Systems installed from Sisura 13.1-rc3 or later use this repository automatically: <code>sisura-archive-keyring</code> configures it, and unattended-upgrades installs the updates.</p>

<h2>Older Sisura systems (13.1-rc1, rc2)</h2>
<pre>curl -fsSLo /tmp/sisura-archive-keyring.deb \\
  https://sisura-linux.github.io/sisura/apt/${keyring_deb}
sudo apt install /tmp/sisura-archive-keyring.deb
sha256sum /usr/share/keyrings/sisura-archive-keyring.pgp
# must be ${key_sha}
sudo apt update &amp;&amp; sudo apt upgrade</pre>

<h2>Trust</h2>
<p>Signing key fingerprint: <code>${fpr_spaced}</code><br>
The source is bound to this key (<code>Signed-By</code>), and an apt pin only accepts <code>sisura-*</code> packages from this host, so the repository cannot replace Debian packages.</p>

<h2>Packages</h2>
<table>
<tr><th>Package</th><th>Version</th><th>Architecture</th></tr>
${rows}
</table>

<p style="margin-top:2rem"><a href="https://github.com/sisura-linux/sisura">github.com/sisura-linux/sisura</a></p>
</body>
</html>
EOF
ok "repository ready: $OUT"
