#!/usr/bin/env bash
# =============================================================================
#  build_download_page.sh - the download page at https://sediros.github.io/sediros/
#
#  Usage : scripts/build_download_page.sh --release <release.json> --sums <SHA256SUMS> --out <index.html>
#
#  <release.json> is the newest published release (pre-releases included):
#    gh release view <tag> --json tagName,url,publishedAt,isPrerelease,assets
#  The page links the release's files directly and shows the checksum of
#  each ISO, so nobody has to dig through the release's asset list.
#  The apt-repo workflow runs it after every published release.
# =============================================================================
set -Eeuo pipefail

die() { printf 'build_download_page: %s\n' "$*" >&2; exit 1; }

REL="" SUMS="" OUT=""
while (($#)); do
    case "$1" in
        --release) REL="${2:-}"; shift ;;
        --sums)    SUMS="${2:-}"; shift ;;
        --out)     OUT="${2:-}"; shift ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
    shift
done
[[ -r "$REL" && -r "$SUMS" && -n "$OUT" ]] || die "usage: --release <json> --sums <SHA256SUMS> --out <file>"
command -v jq >/dev/null || die "jq is required"

tag="$(jq -r .tagName "$REL")"
url="$(jq -r .url "$REL")"
date="$(jq -r '.publishedAt[0:10]' "$REL")"
pre="$(jq -r .isPrerelease "$REL")"
version="${tag#v}"
[[ "$tag" == v* && "$url" == https://github.com/* ]] || die "unexpected release data in $REL"

# Download URL and size (MB) of an asset, by exact name.
asset_url()  { jq -r --arg n "$1" '.assets[] | select(.name == $n) | .url' "$REL"; }
asset_mb()   { jq -r --arg n "$1" '.assets[] | select(.name == $n) | (.size / 1048576 | floor)' "$REL"; }
asset_sha()  { awk -v n="$1" '$2 == n || $2 == "*"n {print $1}' "$SUMS"; }

# The ISO names carry the Debian-based release (13.1), not the rc suffix.
iso_row() {
    local arch="$1" who="$2" name
    name="$(jq -r --arg a "$arch" '.assets[].name | select(test("^sediros-.*-install-" + $a + "\\.iso$"))' "$REL" | head -n1)"
    [[ -n "$name" ]] || die "no installer ISO for $arch in $tag"
    local u sha mb
    u="$(asset_url "$name")"; mb="$(asset_mb "$name")"; sha="$(asset_sha "$name")"
    [[ -n "$sha" ]] || die "$name is not in SHA256SUMS"
    cat <<EOF
<div class="card">
  <h3>${arch}</h3>
  <p class="who">${who}</p>
  <p><a class="button" href="${u}">Download ISO</a> <span class="muted">${mb} MB</span></p>
  <p class="sha">SHA-256 <code>${sha}</code></p>
</div>
EOF
}

notice=""
if [[ "$pre" == true ]]; then
    notice="<p class=\"notice\">This is a release candidate: it is meant for testing and may still change before the final release.</p>"
fi

sums_url="$(asset_url SHA256SUMS)"
amd64="$(iso_row amd64 'Most PCs and servers (Intel, AMD), and VMs on them: VirtualBox, VMware, Proxmox, QEMU/KVM, Hyper-V.')"
arm64="$(iso_row arm64 'Macs with Apple Silicon (in UTM, Parallels, VMware Fusion) and ARM servers.')"

mkdir -p "$(dirname "$OUT")"
cat > "$OUT" <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Download SedirOS</title>
<style>
  :root { color-scheme: light dark; --fg: #1d1d1f; --bg: #fff; --muted: #6e6e73; --code: #f2f2f4; --accent: #0a66c2; --warn: #fff4d6; }
  @media (prefers-color-scheme: dark) { :root { --fg: #f5f5f7; --bg: #111113; --muted: #a1a1a6; --code: #1e1e22; --accent: #4c9bf5; --warn: #3a3220; } }
  body { margin: 0 auto; max-width: 46rem; padding: 2rem 1rem; font: 16px/1.55 system-ui, sans-serif; color: var(--fg); background: var(--bg); }
  h1 { font-size: 1.7rem; margin-bottom: .2rem; } h2 { font-size: 1.15rem; margin-top: 2.2rem; } h3 { margin: 0; font-size: 1.1rem; }
  p.lead, .muted, .who { color: var(--muted); } p.lead { margin-top: 0; }
  .notice { background: var(--warn); padding: .6rem .9rem; border-radius: 6px; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr)); gap: 1rem; }
  .card { border: 1px solid var(--code); border-radius: 10px; padding: 1rem; }
  .card p { margin: .5rem 0; }
  .button { display: inline-block; background: var(--accent); color: #fff; text-decoration: none; padding: .45rem .9rem; border-radius: 6px; font-weight: 600; }
  .sha code { word-break: break-all; font-size: 11px; }
  pre, code { font: 13px/1.5 ui-monospace, Menlo, monospace; background: var(--code); border-radius: 6px; }
  pre { padding: .8rem 1rem; overflow-x: auto; } code { padding: .1rem .3rem; }
  ol li { margin: .35rem 0; }
  a { color: var(--accent); }
</style>
</head>
<body>
<h1>Download SedirOS</h1>
<p class="lead">SedirOS ${version} &middot; published ${date} &middot; <a href="${url}">release notes and all files</a></p>
${notice}

<h2>1. Download the installer ISO</h2>
<div class="cards">
${amd64}
${arm64}
</div>

<h2>2. Check the download</h2>
<p>Compare the checksum with the one above:</p>
<pre>shasum -a 256 sediros-*-install-*.iso      # macOS
sha256sum sediros-*-install-*.iso          # Linux</pre>
<p class="muted">All checksums: <a href="${sums_url}">SHA256SUMS</a>.</p>

<h2>3. Install</h2>
<ol>
  <li><strong>Boot the ISO.</strong> On a PC: write it to a USB stick (balenaEtcher, or <code>dd</code>) and boot from it in UEFI mode. In a VM: attach it as a CD/DVD, with UEFI firmware and the guest type Linux.</li>
  <li><strong>Start the installer</strong> in the live session that opens: <code>sudo sediros-install install</code></li>
  <li><strong>Answer the questions:</strong> disk, hostname, keyboard, user, password, SSH key (optional), disk encryption passphrase.</li>
  <li><strong>Power off and remove the ISO</strong> (in a VM: clear the CD/DVD drive while the VM is off). Otherwise the installer starts again.</li>
  <li><strong>Boot from the disk,</strong> enter the encryption passphrase, log in.</li>
  <li><strong>No SSH key given?</strong> Run <code>sudo sediros-ssh-enroll</code> and follow it: it lets you add your key with <code>ssh-copy-id</code>.</li>
</ol>
<p class="muted">Details and troubleshooting: <a href="https://github.com/sediros/sediros#installing-to-disk">Installing to disk</a>.</p>

<h2>Or: SedirOS VMs on your Mac or Linux machine</h2>
<pre>brew tap sediros/sediros https://github.com/sediros/sediros
brew trust sediros/sediros
brew install sediros
sediros launch dev
sediros shell dev</pre>
<p class="muted">See <a href="https://github.com/sediros/sediros#trying-sediros-in-a-vm">Trying SedirOS in a VM</a>.</p>

<h2>Updates</h2>
<p>Installed systems update themselves: Debian's security updates from Debian, SedirOS's own packages from the <a href="apt/">SedirOS apt repository</a>.</p>
</body>
</html>
EOF
echo "download page: $OUT ($tag)"
