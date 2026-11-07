#!/usr/bin/env bash
# =============================================================================
#  Sisura - end-to-end test of the apt repository and the update path.
#
#  Boots the current base image in a throwaway VM and serves it a repository
#  signed with a throwaway key (tests/apt_fixture.sh). The VM keeps its
#  shipped sisura.sources, apt pin and unattended-upgrades settings; only the
#  key and https -> http are swapped, and sisura-linux.github.io is pointed
#  at the host. Checks:
#    - apt accepts the signed repository
#    - unattended-upgrade installs the newer sisura-hardening by itself
#    - the pin refuses a non-Sisura package (fake openssh-server 1:99)
#    - a repository signed with another key is rejected
#    - the hardening checks still pass after the update
#
#  Env: SISURA_BIN, SISURA_IMAGE, SISURA_KEEP_VM=1 as in tests/smoke.sh;
#       SISURA_APT_MAX_RAM_MB  RAM budget after the update (default: 512)
#  Needs: python3 (HTTP server); dpkg-dev, apt-utils, gnupg on Linux, else Docker
# =============================================================================
set -Eeuo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SISURA="${SISURA_BIN:-${PROJECT_ROOT}/sisura-cli/target/release/sisura}"
VM="apt-$$"
WORK="$PROJECT_ROOT/build/apt-test"   # under the project: Docker can mount it
# shellcheck source=scripts/lib/release.sh
source "${PROJECT_ROOT}/scripts/lib/release.sh"
sisura_resolve_release
IMAGE_ARGS=()
[[ -n "${SISURA_IMAGE:-}" ]] && IMAGE_ARGS=(--image "$SISURA_IMAGE")

[[ -x "$SISURA" ]] || { echo "sisura binary not found: $SISURA (make cli)" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 1; }

SISURA_HOME="$(mktemp -d "${TMPDIR:-/tmp}/sis.XXXXXX")"
export SISURA_HOME SISURA_PROJECT_DIR="$PROJECT_ROOT"
SERVER_PID=""
cleanup() {
    [[ -z "$SERVER_PID" ]] || kill "$SERVER_PID" 2>/dev/null || true
    if [[ "${SISURA_KEEP_VM:-0}" == "1" ]]; then
        echo "Keeping VM: SISURA_HOME=$SISURA_HOME $SISURA shell $VM"
        return
    fi
    "$SISURA" purge --yes --images "$VM" >/dev/null 2>&1 || true
    rm -rf "$SISURA_HOME"
}
trap cleanup EXIT

# --- packages of this checkout (build/debs may hold older versions too) --------
case "$(uname -m)" in arm64|aarch64) host_arch=arm64 ;; *) host_arch=amd64 ;; esac
debs=()
for control in "$PROJECT_ROOT"/packages/*/DEBIAN/control; do
    name="$(sed -n 's/^Package: *//p' "$control")"
    ver="$(sed -n 's/^Version: *//p' "$control")"
    arch="$(sed -n 's/^Architecture: *//p' "$control")"
    [[ "$ver" == @SISURA_RELEASE@ ]] && ver="$RELEASE"
    [[ "$arch" == @SISURA_ARCH@ ]] && arch="$host_arch"
    deb="build/debs/${name}_${ver}_${arch}.deb"
    if [[ -f "$PROJECT_ROOT/$deb" ]]; then debs+=("$deb")
    elif [[ "$name" == sisura-hardening ]]; then echo "$deb missing (make deb)" >&2; exit 1; fi
done

echo "==> Test repositories"
mkdir -p "$WORK"
if command -v dpkg-scanpackages >/dev/null && command -v apt-ftparchive >/dev/null && command -v gpg >/dev/null; then
    (cd "$PROJECT_ROOT" && tests/apt_fixture.sh "$WORK" "${debs[@]}")
else
    docker run --rm --platform "linux/$host_arch" -v "$PROJECT_ROOT:/sisura" -w /sisura debian:trixie bash -c '
        set -e
        apt-get update -qq >/dev/null
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
            dpkg-dev apt-utils gnupg gpgv xz-utils >/dev/null
        tests/apt_fixture.sh build/apt-test "${@:2}"
        chown -R "$1" build/apt-test' _ "$(id -u):$(id -g)" "${debs[@]}"
fi

PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$WORK/www" >/dev/null 2>&1 &
SERVER_PID=$!
disown "$SERVER_PID"   # no "Terminated" message from the shell at exit

echo "==> Launching $VM"
if ! out="$("$SISURA" launch "$VM" --timeout 120 "${IMAGE_ARGS[@]+"${IMAGE_ARGS[@]}"}" 2>&1)"; then
    echo "$out"; echo "FAIL  launch"; exit 1
fi
"$SISURA" transfer "$WORK/test.pgp" "$VM:/tmp/sisura-test.pgp"

echo "==> Update through the repository"
# The guest reaches the host's 127.0.0.1 as 10.0.2.2 (QEMU user network).
if ! "$SISURA" shell "$VM" -- sudo bash -s -- "$PORT" <<'GUEST'
set -uo pipefail
PORT="$1"
FAILS=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s: %s\n' "$1" "$2"; FAILS=$((FAILS + 1)); }
src=/etc/apt/sources.list.d/sisura.sources

# Route the real repository host to the test server; keep only the Sisura
# source so the test does not depend on Debian's mirrors.
cp -a "$src" /root/sisura.sources.orig
mv /etc/apt/sources.list.d/debian.sources /root/debian.sources.off
echo "10.0.2.2 sisura-linux.github.io" >> /etc/hosts
nft add table ip apttest
nft add chain ip apttest out '{ type nat hook output priority -100; }'
nft add rule ip apttest out ip daddr 10.0.2.2 tcp dport 80 dnat to "10.0.2.2:$PORT"
install -m 0644 /tmp/sisura-test.pgp /usr/share/keyrings/sisura-test.pgp
sed -i -e 's|^URIs: https://|URIs: http://|' \
       -e 's|^Signed-By: .*|Signed-By: /usr/share/keyrings/sisura-test.pgp|' "$src"

before_ssh="$(dpkg-query -W -f='${Version}' openssh-server)"
before="$(dpkg-query -W -f='${Version}' sisura-hardening)"

if out="$(apt-get update 2>&1)" && ! grep -q '^[WE]:' <<<"$out"; then
    pass "apt update: signed repository accepted"
else
    fail "apt update" "$(grep '^[WE]:' <<<"$out" | head -3 | tr '\n' ' ')"
fi

pol="$(apt-cache policy openssh-server)"
if grep -q "Candidate: ${before_ssh}\$" <<<"$pol" && grep -Eq '1:99\.0 +-1' <<<"$pol"; then
    pass "pin: openssh-server 1:99.0 from the Sisura repository refused (priority -1)"
else
    fail "pin" "$(tr -s ' \n' ' ' <<<"$pol")"
fi

unattended-upgrade >/dev/null 2>&1 || true
after="$(dpkg-query -W -f='${Version}' sisura-hardening)"
if [[ "$after" == *+apttest1 && "$after" != "$before" ]] && dpkg --verify sisura-hardening; then
    pass "unattended-upgrade: sisura-hardening $before -> $after"
else
    fail "unattended-upgrade" "sisura-hardening still $after"
    tail -n 20 /var/log/unattended-upgrades/unattended-upgrades.log 2>/dev/null | sed 's/^/      /'
fi
[[ "$(dpkg-query -W -f='${Version}' openssh-server)" == "$before_ssh" ]] \
    || fail "pin" "openssh-server changed during the upgrade"

# Same server and path layout, signed with a key the system does not trust.
sed -i 's|/sisura/apt$|/sisura/evil|' "$src"
if out="$(apt-get update 2>&1)"; then
    fail "wrong key" "apt update accepted a repository signed with another key"
elif grep -Eq 'NO_PUBKEY|not signed|signatures (were )?invalid' <<<"$out"; then
    pass "wrong key: repository signed with another key rejected"
else
    fail "wrong key" "unexpected error: $(grep '^[WE]:' <<<"$out" | head -2 | tr '\n' ' ')"
fi

# Back to the shipped configuration for the hardening checks.
cp -a /root/sisura.sources.orig "$src"
mv /root/debian.sources.off /etc/apt/sources.list.d/debian.sources
sed -i '/ sisura-linux\.github\.io$/d' /etc/hosts
nft delete table ip apttest
rm -f /usr/share/keyrings/sisura-test.pgp /var/lib/apt/lists/*sisura*
exit $((FAILS > 0))
GUEST
then
    FAIL=1
fi

echo "==> Hardening after the update"
# The idle-RAM budget is checked by tests/smoke.sh on a fresh boot; after
# apt update and an upgrade the guest legitimately uses more.
"$SISURA" shell "$VM" -- bash -s -- "${SISURA_APT_MAX_RAM_MB:-512}" "$RELEASE" \
    < "$PROJECT_ROOT/tests/guest_checks.sh" || FAIL=1

echo
if [[ -z "${FAIL:-}" ]]; then echo "APT REPOSITORY TEST PASSED"
else echo "APT REPOSITORY TEST FAILED"; exit 1; fi
