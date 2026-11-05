#!/usr/bin/env bash
# =============================================================================
#  Sisura - in-guest hardening checks.
#  Runs inside the VM as the admin user (needs sudo), streamed over SSH by
#  tests/smoke.sh:  sisura shell <vm> -- bash -s -- <max_ram_mb> <release> < guest_checks.sh
#  Prints one PASS/FAIL line per check, exits non-zero if anything failed.
# =============================================================================
set -uo pipefail

MAX_RAM_MB="${1:-256}"
WANT_RELEASE="${2:-}"
FAILS=0

pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s: %s\n' "$1" "$2"; FAILS=$((FAILS + 1)); }
trim() { local s="$1"; s="${s#"${s%%[![:space:]]*}"}"; printf '%s' "${s%"${s##*[![:space:]]}"}"; }

sudo -n true 2>/dev/null || { echo "FAIL  sudo: passwordless sudo required (vm profile image needed: make build PROFILE=vm)"; exit 1; }

# --- boot ------------------------------------------------------------------
state="$(systemctl is-system-running --wait 2>/dev/null)"
if [[ "$state" == "running" ]]; then
    pass "systemd state: running"
else
    failed="$(systemctl --failed --plain --no-legend | awk '{print $1}' | tr '\n' ' ')"
    fail "systemd state" "$state (failed units: ${failed:-none})"
fi

# --- release identification -----------------------------------------------------
# shellcheck disable=SC1091
read -r os_id os_rel < <(. /etc/os-release && echo "$ID ${SISURA_RELEASE:-none}")
if [[ "$os_id" == sisura && "$os_rel" != none && ( -z "$WANT_RELEASE" || "$os_rel" == "$WANT_RELEASE" ) ]]; then
    pass "os-release: Sisura $os_rel"
else
    fail "os-release" "ID=$os_id SISURA_RELEASE=$os_rel, want sisura ${WANT_RELEASE:-<any>}"
fi

# --- sysctl: every value in 99-sisura.conf is live --------------------------
bad=()
checked=0
while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%%#*}"
    [[ "$line" == *=* ]] || continue
    key="$(trim "${line%%=*}")"
    want="$(trim "${line#*=}")"
    optional=0
    if [[ "$key" == -* ]]; then optional=1; key="${key#-}"; fi
    path="/proc/sys/${key//./\/}"
    if [[ ! -e "$path" ]]; then
        ((optional)) || bad+=("$key: not supported by kernel")
        continue
    fi
    got="$(sudo cat "$path" | tr -s '[:space:]' ' ')"
    got="$(trim "$got")"
    want="$(trim "$(tr -s '[:space:]' ' ' <<<"$want")")"
    checked=$((checked + 1))
    [[ "$got" == "$want" ]] || bad+=("$key: want '$want' got '$got'")
done < /etc/sysctl.d/99-sisura.conf
if ((${#bad[@]} == 0)); then
    pass "sysctl: $checked keys applied"
else
    fail "sysctl" "$(printf '%s; ' "${bad[@]}")"
fi

# --- AppArmor -----------------------------------------------------------------
if sudo aa-status --enabled 2>/dev/null; then
    loaded="$(sudo aa-status 2>/dev/null | awk '/profiles are loaded/ {print $1; exit}')"
    if [[ "${loaded:-0}" -gt 0 ]]; then pass "apparmor: enabled, $loaded profiles loaded"
    else fail "apparmor" "enabled but no profiles loaded"; fi
else
    fail "apparmor" "not enabled"
fi

# --- UFW ------------------------------------------------------------------------
ufw="$(sudo ufw status verbose 2>&1)"
if grep -q '^Status: active' <<<"$ufw" \
   && grep -q 'deny (incoming)' <<<"$ufw" \
   && grep -qE '^22/tcp +LIMIT IN' <<<"$ufw"; then
    pass "ufw: active, deny incoming, 22/tcp LIMIT"
else
    fail "ufw" "$(tr '\n' ' ' <<<"$ufw")"
fi

# --- only SSH listens on non-loopback TCP --------------------------------------
external="$(ss -tlnH | awk '{print $4}' \
    | grep -vE '^(127\.|\[::1\]|\[::ffff:127\.)|%lo:' \
    | grep -vE ':22$' || true)"
if [[ -z "$external" ]]; then pass "listeners: only :22 exposed"
else fail "listeners" "unexpected: $(tr '\n' ' ' <<<"$external")"; fi

# --- setuid/setgid ---------------------------------------------------------------
mapfile -t allowed < <(sed -e 's/#.*//' -e '/^[[:space:]]*$/d' /etc/sisura/suid-allowlist)
extra=()
while IFS= read -r f; do
    keep=0
    for a in "${allowed[@]}"; do [[ "$f" == "$a" ]] && keep=1 && break; done
    ((keep)) || extra+=("$f")
done < <(sudo find / -xdev -type f -perm /6000 2>/dev/null)
if ((${#extra[@]} == 0)); then pass "suid/sgid: allowlist only"
else fail "suid/sgid" "${extra[*]}"; fi

# --- sshd effective config --------------------------------------------------------
sshd="$(sudo sshd -T 2>/dev/null)"
bad=()
for kv in "permitrootlogin no" "passwordauthentication no" \
          "kbdinteractiveauthentication no" "x11forwarding no" "allowtcpforwarding no" \
          "allowgroups sudo" "allowgroups sisura-ssh"; do
    grep -qx "$kv" <<<"$sshd" || bad+=("$kv")
done
if ((${#bad[@]} == 0)); then pass "sshd: key-only, no root, no forwarding, AllowGroups"
else fail "sshd" "missing: ${bad[*]}"; fi

# --- Sisura packages --------------------------------------------------------------
for p in sisura-base-files sisura-hardening sisura-boot-hardening; do
    pkg="$(dpkg-query -W -f='${Status} ${Version}' "$p" 2>/dev/null)"
    if [[ "$pkg" != "install ok installed "* ]]; then
        fail "package" "$p not installed (${pkg:-missing})"
    elif ! sudo dpkg --verify "$p"; then
        fail "package" "$p: dpkg --verify reports modified files"
    else
        pass "package: $p ${pkg##* } installed, files unmodified"
    fi
done
for unit in sisura-ssh-hostkeys.service sisura-firstboot-ufw.service sisura-netfilter.service; do
    [[ "$(systemctl is-enabled "$unit" 2>/dev/null)" == "enabled" ]] \
        || fail "units" "$unit not enabled"
done

# --- extra packet filter (IPv4 TCP with IP options) ---------------------------------
if systemctl is-active -q sisura-netfilter.service \
   && sudo nft list table ip sisura 2>/dev/null | grep -q 'ip hdrlength > 5 .*drop'; then
    pass "nftables: TCP with IP options dropped"
else
    fail "nftables" "table ip sisura missing or sisura-netfilter.service inactive"
fi

# --- automatic security updates ---------------------------------------------------
apt_cfg="$(apt-config dump)"
bad=()
grep -q '^APT::Periodic::Unattended-Upgrade "1";' <<<"$apt_cfg" || bad+=("Periodic::Unattended-Upgrade")
grep -q '^APT::Periodic::Update-Package-Lists "1";' <<<"$apt_cfg" || bad+=("Periodic::Update-Package-Lists")
grep -q 'Origins-Pattern:: "origin=Debian,label=Debian-Security";' <<<"$apt_cfg" || bad+=("Origins-Pattern")
for t in apt-daily.timer apt-daily-upgrade.timer; do
    [[ "$(systemctl is-enabled "$t" 2>/dev/null)" == "enabled" ]] || bad+=("$t disabled")
done
if ((${#bad[@]} == 0)); then pass "unattended-upgrades: security archive, timers enabled"
else fail "unattended-upgrades" "${bad[*]}"; fi

# --- accounts -------------------------------------------------------------------
if [[ "$(sudo passwd -S root | awk '{print $2}')" == "L" ]]; then pass "root: password locked"
else fail "root" "password not locked"; fi

if grep -qE '^BUILD_ID=[0-9A-Za-z._-]+$' /etc/sisura/build-info 2>/dev/null; then
    pass "build-info: $(sed -n 's/^RELEASE=//p' /etc/sisura/build-info) build $(sed -n 's/^BUILD_ID=//p' /etc/sisura/build-info)"
else
    fail "build-info" "/etc/sisura/build-info missing or without BUILD_ID"
fi

profile="$(sed -n 's/^PROFILE=//p' /etc/sisura/build-info 2>/dev/null)"
if [[ "$profile" == vm && "$(sudo passwd -S "$USER" | awk '{print $2}')" == "L" ]] \
   && sudo test -f /etc/sudoers.d/90-sisura-vm; then
    pass "profile vm: $USER password locked, NOPASSWD sudo"
else
    fail "profile" "profile=${profile:-unknown}, $USER $(sudo passwd -S "$USER" | awk '{print $2}')"
fi

# --- sudo policy and PAM ----------------------------------------------------------
sudo_l="$(sudo -l 2>/dev/null)"
bad=()
for d in use_pty 'logfile=/var/log/sudo.log' timestamp_timeout=5 timestamp_type=tty passwd_tries=3; do
    grep -q "$d" <<<"$sudo_l" || bad+=("$d")
done
sudo visudo -cq >/dev/null 2>&1 || bad+=("visudo -c failed")
if ((${#bad[@]} == 0)); then pass "sudo: pty, audit log, 5 min timestamp"
else fail "sudo" "missing: ${bad[*]}"; fi

# pam_faillock must wrap pam_unix: preauth before it, authfail after it.
order="$(grep -oE 'pam_faillock\.so (preauth|authfail)|pam_unix\.so' /etc/pam.d/common-auth | awk '{print $NF}' | tr '\n' ' ')"
if [[ "$order" == "preauth pam_unix.so authfail " ]] \
   && grep -q '^password.*pam_pwquality\.so' /etc/pam.d/common-password \
   && [[ -s /var/cache/cracklib/cracklib_dict.pwd ]]; then
    pass "pam: faillock (5 fails/15 min) around pam_unix, pwquality"
else
    fail "pam" "common-auth order '${order}', pwquality $(grep -c pam_pwquality /etc/pam.d/common-password), cracklib dict $(ls /var/cache/cracklib/cracklib_dict.pwd 2>/dev/null || echo missing)"
fi

# --- kernel modules blacklisted ---------------------------------------------------
bad=()
for m in dccp sctp cramfs; do
    sudo modprobe -n -v "$m" 2>&1 | grep -q '/bin/false' || bad+=("$m")
done
if ((${#bad[@]} == 0)); then pass "modprobe: blacklist active"
else fail "modprobe" "loadable: ${bad[*]}"; fi

# --- core dumps -----------------------------------------------------------------
if [[ "$(ulimit -Hc)" == "0" ]]; then pass "core dumps: hard limit 0"
else fail "core dumps" "hard limit $(ulimit -Hc)"; fi

# --- memory footprint -------------------------------------------------------------
used_mb="$(awk '/^MemTotal/ {t=$2} /^MemAvailable/ {a=$2} END {printf "%d", (t-a)/1024}' /proc/meminfo)"
if ((used_mb <= MAX_RAM_MB)); then pass "memory: ${used_mb} MiB used (limit ${MAX_RAM_MB})"
else fail "memory" "${used_mb} MiB used, limit ${MAX_RAM_MB}"; fi

echo
if ((FAILS == 0)); then echo "guest checks: all passed"; else echo "guest checks: $FAILS failed"; fi
exit $((FAILS > 0))
