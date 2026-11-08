#!/usr/bin/env bash
# =============================================================================
#  SedirOS - end-to-end test of sediros-firstboot.
#
#  Usage : tests/firstboot.sh build/debs/sediros-firstboot_<ver>_<arch>.deb
#
#  Installs the package in a throwaway VM and runs it twice as a new
#  instance: from a NoCloud seed directory (#cloud-config with keys and
#  ignored keys), then from a fake EC2-style metadata service on the host
#  (user-data script). Checks: new SSH host keys per instance, hostname,
#  the admin user's SSH keys, nothing done for a known instance, user-data
#  never run and reported at login.
#  Everything runs in one SSH session: new host keys would make the CLI's
#  next connection fail its host key check.
#
#  Env: SEDIROS_BIN, SEDIROS_IMAGE, SEDIROS_KEEP_VM=1 as in tests/smoke.sh
# =============================================================================
set -Eeuo pipefail

DEB="${1:?usage: $0 <sediros-firstboot .deb>}"
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SEDIROS="${SEDIROS_BIN:-${PROJECT_ROOT}/sediros-cli/target/release/sediros}"
VM="fb-$$"
IMAGE_ARGS=()
[[ -n "${SEDIROS_IMAGE:-}" ]] && IMAGE_ARGS=(--image "$SEDIROS_IMAGE")
[[ -r "$DEB" ]] || { echo "package not found: $DEB (make firstboot)" >&2; exit 1; }
[[ -x "$SEDIROS" ]] || { echo "sediros binary not found: $SEDIROS (make cli)" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 1; }

SEDIROS_HOME="$(mktemp -d "${TMPDIR:-/tmp}/sis.XXXXXX")"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sedfb.XXXXXX")"
export SEDIROS_HOME SEDIROS_PROJECT_DIR="$PROJECT_ROOT"
SERVER_PID=""
cleanup() {
    [[ -z "$SERVER_PID" ]] || kill "$SERVER_PID" 2>/dev/null || true
    if [[ "${SEDIROS_KEEP_VM:-0}" == "1" ]]; then
        echo "Keeping VM: SEDIROS_HOME=$SEDIROS_HOME $SEDIROS shell $VM"
        return
    fi
    "$SEDIROS" purge --yes --images "$VM" >/dev/null 2>&1 || true
    rm -rf "$SEDIROS_HOME" "$WORK"
}
trap cleanup EXIT

# --- fake metadata service (EC2 paths, IMDSv2 token refused like Vultr) -------
mkdir -p "$WORK/latest/meta-data/public-keys/0"
printf 'i-imds-2\n' > "$WORK/latest/meta-data/instance-id"
printf 'Web-Imds.example.net\n' > "$WORK/latest/meta-data/local-hostname"
printf '0=laptop\n' > "$WORK/latest/meta-data/public-keys/index"
ssh-keygen -q -t ed25519 -N '' -C imds-key -f "$WORK/imds_key"
cp "$WORK/imds_key.pub" "$WORK/latest/meta-data/public-keys/0/openssh-key"
printf '#!/bin/sh\ntouch /root/PWNED\n' > "$WORK/latest/user-data"
ssh-keygen -q -t ed25519 -N '' -C seed-key -f "$WORK/seed_key"
cat > "$WORK/server.py" <<'EOF'
import http.server, os, sys
root = sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_PUT(self):
        self.send_response(405); self.end_headers()
    def do_GET(self):
        p = self.path
        if p == "/latest/meta-data/public-keys/":
            p += "index"
        f = os.path.join(root, p.lstrip("/"))
        if not os.path.isfile(f):
            self.send_response(404); self.end_headers(); return
        body = open(f, "rb").read()
        self.send_response(200); self.send_header("Content-Length", str(len(body))); self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
EOF
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
python3 "$WORK/server.py" "$PORT" "$WORK" &
SERVER_PID=$!
disown "$SERVER_PID"

echo "==> Launching $VM"
if ! out="$("$SEDIROS" launch "$VM" --timeout 120 "${IMAGE_ARGS[@]+"${IMAGE_ARGS[@]}"}" 2>&1)"; then
    echo "$out"; echo "FAIL  launch"; exit 1
fi
"$SEDIROS" transfer "$DEB" "$VM:/tmp/sediros-firstboot.deb"
"$SEDIROS" transfer "$WORK/seed_key.pub" "$VM:/tmp/seed_key.pub"
"$SEDIROS" transfer "$WORK/imds_key.pub" "$VM:/tmp/imds_key.pub"

echo "==> First boot as two instances"
# The guest reaches the host's 127.0.0.1 as 10.0.2.2 (QEMU user network).
if ! "$SEDIROS" shell "$VM" -- sudo bash -s -- "$PORT" <<'GUEST'
set -uo pipefail
PORT="$1"
FAILS=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s: %s\n' "$1" "$2"; FAILS=$((FAILS + 1)); }
user="$(sed -n 's/^USER=//p' /etc/sediros/build-info)"
home="$(getent passwd "$user" | cut -d: -f6)"
hostfp() { ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub | awk '{print $2}'; }

# Its dependencies (fdisk) come from Debian: the image has no package lists.
if apt-get update -qq >/dev/null 2>&1 \
   && apt-get install -y -qq /tmp/sediros-firstboot.deb >/dev/null 2>&1 \
   && systemctl is-enabled -q sediros-firstboot.service; then
    pass "package installed, service enabled"
else
    fail "install" "$(apt-get install -y /tmp/sediros-firstboot.deb 2>&1 | grep -E '^E:|Depends' | head -3)"
    exit 1
fi

# --- 1. NoCloud seed ----------------------------------------------------------
seed=/tmp/seed; mkdir -p "$seed"
printf 'instance-id: iid-seed-1\nlocal-hostname: seedhost\n' > "$seed/meta-data"
{ echo '#cloud-config'; echo 'ssh_authorized_keys:'; echo "  - $(cat /tmp/seed_key.pub)"
  echo 'packages:'; echo '  - nginx'; echo 'runcmd:'; echo '  - [touch, /root/PWNED]'; } > "$seed/user-data"
fp0="$(hostfp)"
out="$(sediros-firstboot --seed "$seed" 2>&1)"; rc=$?
echo "$out" | sed 's/^/      /'
[[ $rc -eq 0 ]] && pass "seed: exit 0" || fail "seed" "exit $rc"
[[ "$(hostname)" == seedhost && "$(cat /etc/hostname)" == seedhost ]] \
    && grep -q $'^127.0.1.1\tseedhost$' /etc/hosts \
    && pass "seed: hostname seedhost" || fail "seed hostname" "$(hostname)"
grep -qF "$(cut -d' ' -f2 /tmp/seed_key.pub)" "$home/.ssh/authorized_keys" \
    && [[ "$(stat -c %U:%a "$home/.ssh/authorized_keys")" == "$user:600" ]] \
    && pass "seed: SSH key added for $user" || fail "seed key" "not in $home/.ssh/authorized_keys"
fp1="$(hostfp)"
[[ -n "$fp1" && "$fp1" != "$fp0" ]] && pass "seed: new SSH host keys" || fail "seed host keys" "$fp0 -> $fp1"
grep -q 'packages, runcmd' /etc/motd.d/50-sediros-firstboot 2>/dev/null \
    && pass "seed: ignored cloud-config keys reported at login" || fail "seed notice" "no login notice"

out="$(sediros-firstboot --seed "$seed" 2>&1)"
[[ "$out" == *"already set up"* && "$(hostfp)" == "$fp1" ]] \
    && pass "seed: same instance again, nothing changed" || fail "seed rerun" "$out"

# --- 2. metadata service (new instance id, as after cloning the disk) --------
out="$(sediros-firstboot --imds "10.0.2.2:$PORT" --wait 10 2>&1)"; rc=$?
echo "$out" | sed 's/^/      /'
[[ $rc -eq 0 ]] && pass "imds: exit 0" || fail "imds" "exit $rc"
[[ "$(hostname)" == web-imds ]] && pass "imds: hostname web-imds (from Web-Imds.example.net)" \
    || fail "imds hostname" "$(hostname)"
grep -qF "$(cut -d' ' -f2 /tmp/imds_key.pub)" "$home/.ssh/authorized_keys" \
    && pass "imds: SSH key added" || fail "imds key" "missing"
[[ "$(hostfp)" != "$fp1" ]] && pass "imds: new instance, new SSH host keys" || fail "imds host keys" "unchanged"
[[ "$(cat /var/lib/sediros/firstboot/instance-id)" == i-imds-2 ]] \
    && pass "imds: instance id recorded" || fail "imds state" "$(cat /var/lib/sediros/firstboot/instance-id)"
grep -q 'script ignored' /etc/motd.d/50-sediros-firstboot \
    && pass "imds: user-data script reported at login" || fail "imds notice" "missing"
[[ ! -e /root/PWNED ]] && pass "user-data never run" || fail "user-data" "/root/PWNED exists"

# --- 3. no cloud at all --------------------------------------------------------
out="$(sediros-firstboot --imds 10.0.2.2:1 --wait 1 2>&1)"; rc=$?
[[ $rc -eq 0 && "$out" == *"nothing to do"* ]] && pass "no metadata: nothing to do" || fail "no metadata" "$out"

sshd -t && pass "sshd accepts the new host keys" || fail "sshd" "config test failed"
exit $((FAILS > 0))
GUEST
then
    FAIL=1
fi

echo
if [[ -z "${FAIL:-}" ]]; then echo "FIRSTBOOT TEST PASSED"
else echo "FIRSTBOOT TEST FAILED"; exit 1; fi
