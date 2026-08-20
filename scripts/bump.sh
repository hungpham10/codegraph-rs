#!/usr/bin/env bash

######################################################################
# @author      : Hung Nguyen Xuan Pham (hung0913208@gmail.com)
# @file        : bump
# @created     : Thursday Aug 20, 2026 14:48:42 +07
#
# @description : 
#   bump-version.sh — bump the codegraph-rs version across all release artifacts.
#
#   The Rust side has a single source of truth: root Cargo.toml's
#   `[workspace.package] version` propagates to every crate via
#   `version.workspace = true`, so only the root Cargo.toml is edited for the
#   binaries. Native packaging manifests carry their own version field and are
#   updated here too, so every release artifact stays internally consistent.
#
#   Usage:
#     ./scripts/bump.sh 2.1.0
#     ./scripts/bump.sh v2.1.0      # a leading 'v' is stripped
#
#   After running, review the diff and then commit + tag:
#     git add -A && git commit -m "Bump version to v2.1.0"
#     git tag v2.1.0 && git push origin v2.1.0
######################################################################


set -euo pipefail
 
if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <new-version>   (e.g. 2.1.0 or v2.1.0)" >&2
  exit 1
fi
 
NEW="${1#v}"   # strip an optional leading 'v'
 
if [[ ! "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]]; then
  echo "Error: '$NEW' is not a valid semver (expected MAJOR.MINOR.PATCH)" >&2
  exit 1
fi
 
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
 
# Read the current version from the single source of truth.
CURRENT="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)".*/\1/')"
echo "Bumping version: $CURRENT -> $NEW"
 
# Replace the exact current version string with the new one in a file.
# (Used for files whose version equals the crate version.)
replace_current() {
  local file="$1"
  [[ -f "$file" ]] || { echo "  ! skip (missing): $file"; return; }
  # escape dots in CURRENT so it is matched as a fixed string, not a regex
  local fixed="${CURRENT//./\.}"
  sed -i.bak -E "s/${fixed}/$NEW/g" "$file"
  rm -f "$file.bak"
  echo "  ✓ $file"
}
 
echo "Updating manifests:"
# 1. Cargo workspace package version (root only; surrounding quotes preserved)
sed -i.bak -E "s/\"$CURRENT\"/\"$NEW\"/" Cargo.toml && rm -f Cargo.toml.bak && echo "  ✓ Cargo.toml"
 
# 2-4. Native packaging manifests (their version tracks the crate version)
replace_current packaging/choco/codegraph.nuspec
replace_current packaging/winget/codegraph.yaml
replace_current scripts/install.ps1
 
# 5. AUR -bin PKGBUILD ships a 0.0.0 placeholder; bump via format, not CURRENT.
if [[ -f packaging/aur/codegraph-rs-bin/PKGBUILD ]]; then
  sed -i.bak -E "s/^pkgver=[0-9]+\.[0-9]+\.[0-9]+/pkgver=$NEW/" packaging/aur/codegraph-rs-bin/PKGBUILD
  rm -f packaging/aur/codegraph-rs-bin/PKGBUILD.bak
  echo "  ✓ packaging/aur/codegraph-rs-bin/PKGBUILD"
fi
 
# Refresh Cargo.lock workspace package versions (best-effort; needs cargo).
if command -v cargo >/dev/null 2>&1; then
  echo "Refreshing Cargo.lock..."
  cargo metadata --format-version=1 >/dev/null 2>&1 \
    || echo "  (cargo metadata skipped — Cargo.lock will refresh on next build)"
fi
 
echo
echo "Done. Review the diff (git diff), then:"
echo "  git add -A && git commit -m \"Bump version to v$NEW\""
echo "  git tag v$NEW && git push origin v$NEW"

