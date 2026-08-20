#!/usr/bin/env bash
#
# Asserts that everything a release stamps with a version agrees.
#
#   bash scripts/check-release-version.sh          # the versions agree with each other
#   bash scripts/check-release-version.sh v0.2.0   # ...and with the tag being released
#
# The crate and the npm package ship as one release, so they carry one version.
# Three places declare it, and a release that disagrees with any of them puts a
# crate on crates.io that names a package version npm never had:
#
#   Cargo.toml            [workspace.package] version   the crate's version
#   Cargo.toml            [workspace.dependencies]      what the starter resolves anubis to
#   frontend/package.json version                       the npm package's version
#
# .github/workflows/release.yml runs this before it publishes anything, so the
# check that fails a release is the one a developer can run first.
#
# Windows developers run this through git-bash, so it stays POSIX.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# `sed -n '/^\[section\]/,/^\[/p'` slices one TOML table: from its header to the
# next one. Enough for a manifest this repository writes and CI formats.
crate_version="$(
  sed -n '/^\[workspace.package\]/,/^\[/p' Cargo.toml |
    sed -n 's/^version = "\([^"]*\)".*/\1/p' | head -1
)"
dependency_version="$(
  sed -n '/^\[workspace.dependencies\]/,/^\[workspace.lints/p' Cargo.toml |
    sed -n 's/^anubis = .*version = "\([^"]*\)".*/\1/p' | head -1
)"
package_version="$(node -p "require('./frontend/package.json').version")"

echo "crate            $crate_version"
echo "crate dependency $dependency_version"
echo "npm package      $package_version"

failures=0
fail() {
  echo "error: $1" >&2
  failures=$((failures + 1))
}

if [ -z "$crate_version" ]; then
  fail "no version under [workspace.package] in Cargo.toml"
fi
if [ -z "$dependency_version" ]; then
  fail "no anubis version under [workspace.dependencies] in Cargo.toml"
fi
if [ "$dependency_version" != "$crate_version" ]; then
  fail "the anubis workspace dependency is $dependency_version, not $crate_version"
fi
if [ "$package_version" != "$crate_version" ]; then
  fail "frontend/package.json is $package_version, not $crate_version"
fi

if [ $# -gt 0 ]; then
  tag="$1"
  echo "tag              $tag"
  case "$tag" in
    v*) ;;
    *) fail "the tag $tag does not start with v" ;;
  esac
  if [ "${tag#v}" != "$crate_version" ]; then
    fail "the tag $tag does not name version $crate_version"
  fi
fi

if [ "$failures" -gt 0 ]; then
  echo "a release carries one version; bump all three together" >&2
  exit 1
fi

echo "ok"
