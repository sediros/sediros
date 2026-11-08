#!/usr/bin/env bash
# =============================================================================
#  SedirOS - test repositories for tests/apt_repo.sh (runs on Debian: natively
#  on a Linux host with the tools, else in a container started by apt_repo.sh)
#
#  Usage : tests/apt_fixture.sh <workdir> <deb>...
#
#  <workdir>/test.pgp                 public test key (the VM trusts it)
#  <workdir>/www/sedir/apt/          repository signed with the test key:
#      the given packages, sedir-hardening once more with a higher version
#      (the update the VM must install unattended), and a fake
#      openssh-server 1:99 (which the apt pin must refuse)
#  <workdir>/www/sedir/evil/         same packages, signed by another key
# =============================================================================
set -Eeuo pipefail

WORK="${1:?usage: $0 <workdir> <deb>...}"; shift
(($#)) || { echo "no packages given" >&2; exit 1; }
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

stage="$WORK/stage"
rm -rf "$stage" "$WORK/www" "$WORK"/*.key "$WORK"/*.pgp
mkdir -p "$stage/debs"
cp "$@" "$stage/debs/"

# --- an update: sedir-hardening with a higher version, same content ---------
hard="$(ls "$stage"/debs/sedir-hardening_*.deb)"
ver="$(dpkg-deb -f "$hard" Version)"
dpkg-deb -R "$hard" "$stage/hard"
sed -i "s/^Version: .*/Version: ${ver}+apttest1/" "$stage/hard/DEBIAN/control"
dpkg-deb --root-owner-group -Zxz --build "$stage/hard" "$stage/debs/sedir-hardening_${ver}+apttest1_all.deb" >/dev/null

# --- a package the pin must refuse --------------------------------------------
mkdir -p "$stage/fake/DEBIAN"
cat > "$stage/fake/DEBIAN/control" <<'EOF'
Package: openssh-server
Version: 1:99.0
Architecture: all
Maintainer: SedirOS test <test@invalid>
Description: fake package, must never be installed from the SedirOS repository
EOF
dpkg-deb --root-owner-group --build "$stage/fake" "$stage/debs/openssh-server_99.0_all.deb" >/dev/null

# --- keys -----------------------------------------------------------------------
GNUPGHOME="$(mktemp -d)"; export GNUPGHOME
trap 'gpgconf --kill all >/dev/null 2>&1 || true; rm -rf "$GNUPGHOME"' EXIT
for k in test evil; do
    gpg --batch --quiet --passphrase '' --quick-gen-key "SedirOS $k key" ed25519 sign 1d 2>/dev/null
    fpr="$(gpg --batch --list-keys --with-colons "SedirOS $k key" 2>/dev/null | awk -F: '/^fpr/{print $10; exit}')"
    gpg --batch --export-secret-keys --armor "$fpr" > "$WORK/$k.key"
    gpg --batch --export "$fpr" > "$WORK/$k.pgp"
done

# The fake openssh-server is the point of the pin test.
export SEDIR_APT_ALLOW_FOREIGN=1
"$PROJECT_ROOT/scripts/build_apt_repo.sh" --debs "$stage/debs" --out "$WORK/www/sedir/apt" \
    --key "$WORK/test.key" --pubkey "$WORK/test.pgp"
"$PROJECT_ROOT/scripts/build_apt_repo.sh" --debs "$stage/debs" --out "$WORK/www/sedir/evil" \
    --key "$WORK/evil.key" --pubkey "$WORK/evil.pgp" >/dev/null
rm -rf "$stage" "$WORK"/*.key "$WORK/evil.pgp"
echo "fixture ready: $WORK (update: sedir-hardening ${ver}+apttest1)"
