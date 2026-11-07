#!/usr/bin/env bash
# =============================================================================
#  Sisura - every changed package must have a new version
#
#  Usage : ./scripts/check_versions.sh            (also run by `make lint`)
#  Env   : SISURA_VERSION_BASE=<git ref>   compare against this ref instead of
#                                          the latest release tag (v*)
#
#  apt only upgrades a package whose version changed, so a file changed under
#  packages/<pkg>/ without a new Version would silently never reach installed
#  systems. Compares the working tree with the last release tag:
#    packages/<pkg>/          DEBIAN/control Version must differ
#    packages/sisura-base-files/  RELEASE in config/sisura.conf must differ
#                             (its version is the release)
#    sisura-cli/, sisura-install/, sisura-audit/  version in Cargo.toml must differ
#  Packages that did not exist at the base are new and always pass.
# =============================================================================
set -Eeuo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

base="${SISURA_VERSION_BASE:-}"
if [[ -z "$base" ]]; then
    if ! base="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null)"; then
        echo "versions: no release tag (v*) yet, nothing published, skipped"
        exit 0
    fi
fi
git rev-parse --verify -q "${base}^{commit}" >/dev/null || { echo "versions: unknown base '$base'" >&2; exit 1; }

failures=0
fail() { printf '  ✗ %s\n' "$*"; failures=$((failures + 1)); }

# changed <path>: does the working tree differ from $base under <path>?
# (tracked changes and untracked files both count)
changed() {
    ! git diff --quiet "$base" -- "$1" \
        || [[ -n "$(git ls-files --others --exclude-standard -- "$1")" ]]
}
existed() { git cat-file -e "${base}:$1" 2>/dev/null; }
at_base() { git show "${base}:$1" 2>/dev/null; }

field() { sed -n "s/^$1[[:space:]]*$2[[:space:]]*//p" | head -n1 | tr -d '"'; }

for dir in packages/*/; do
    dir="${dir%/}"
    pkg="$(basename "$dir")"
    existed "$dir/DEBIAN/control" || continue
    changed "$dir" || continue
    if [[ "$pkg" == sisura-base-files ]]; then
        old="$(at_base config/sisura.conf | field RELEASE '?=')"
        new="$(field RELEASE '?=' < config/sisura.conf)"
        [[ "$old" != "$new" ]] || fail "$pkg changed since $base but RELEASE is still $new (config/sisura.conf)"
        continue
    fi
    old="$(at_base "$dir/DEBIAN/control" | field Version :)"
    new="$(field Version : < "$dir/DEBIAN/control")"
    [[ "$old" != "$new" ]] || fail "$pkg changed since $base but Version is still $new ($dir/DEBIAN/control)"
done

for crate in sisura-cli sisura-install sisura-audit; do
    existed "$crate/Cargo.toml" || continue
    changed "$crate/src" || changed "$crate/Cargo.toml" || changed "$crate/Cargo.lock" || continue
    old="$(at_base "$crate/Cargo.toml" | field version =)"
    new="$(field version = < "$crate/Cargo.toml")"
    [[ "$old" != "$new" ]] || fail "$crate changed since $base but version is still $new ($crate/Cargo.toml)"
done

if ((failures)); then
    echo "versions: $failures package(s) changed without a new version (see SemVer rules in README)"
    exit 1
fi
echo "versions: ok (compared with $base)"
