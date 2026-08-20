#!/usr/bin/env bash
#
# Vendors the starter template into the anubis package, for publishing.
#
# `anubis/build.rs` embeds the starter tree so `anubis new` stamps offline. In
# the monorepo that tree is `starter/`, one level above the package, which
# `cargo package` cannot reach: a published crate carries only files under
# `anubis/`. This script copies the template to `anubis/starter/`, where
# build.rs prefers it, so the published crate is self-contained.
#
#   bash scripts/vendor-starter.sh            # vendor the template
#   bash scripts/vendor-starter.sh --clean    # remove the vendored copy
#
# The copy is a build artifact, not source: `anubis/starter/` is gitignored,
# and `include` in anubis/Cargo.toml is what puts it in the package anyway
# (an `include` list makes cargo walk the filesystem instead of asking git).
# Running this twice in a row leaves the same tree, and `--clean` leaves none.
#
# What gets copied is what git tracks under `starter/`, read from the working
# tree. That definition needs no skip list of its own: node_modules, dist, and
# the Playwright artifacts are gitignored already, so they can never leak into
# a release. A file staged nowhere is a file the release would not ship, so
# untracked ones are reported rather than copied.
#
# Manifests are renamed on the way in. Cargo's packager prunes any directory
# holding a `Cargo.toml`, taking it for a nested package, which would drop the
# starter's whole backend from the crate. Manifests therefore travel under a
# suffix, and `anubis/build.rs` strips it back off when it embeds them.
#
# Windows developers run this through git-bash, so it stays POSIX: no GNU-only
# flags, no process substitution, and forward slashes throughout.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

source_dir="starter"
vendored_dir="anubis/starter"
# Kept in step with `VENDORED_SUFFIX` in anubis/build.rs, which strips it.
manifest_suffix=".vendored"

mode="${1:-vendor}"
if [ $# -gt 1 ] || { [ "$mode" != "vendor" ] && [ "$mode" != "--clean" ]; }; then
  echo "usage: bash scripts/vendor-starter.sh [--clean]" >&2
  exit 2
fi

# Both modes start by removing the previous copy, which is what makes the
# script idempotent: a file deleted from the template never survives in it.
rm -rf "$vendored_dir"

if [ "$mode" = "--clean" ]; then
  echo "removed $vendored_dir"
  exit 0
fi

# Untracked template files are the one way this copy can differ from what a
# release ships. Naming them costs a line and saves a mystified bug report.
untracked="$(git ls-files --others --exclude-standard "$source_dir")"
if [ -n "$untracked" ]; then
  echo "warning: untracked files under $source_dir/ are not vendored:" >&2
  echo "$untracked" | sed 's/^/  /' >&2
fi

git ls-files -z "$source_dir" | while IFS= read -r -d '' file; do
  relative="${file#"$source_dir/"}"
  if [ "$(basename "$relative")" = "Cargo.toml" ]; then
    relative="$relative$manifest_suffix"
  fi
  destination="$vendored_dir/$relative"
  mkdir -p "$(dirname "$destination")"
  cp "$file" "$destination"
done

count="$(git ls-files "$source_dir" | wc -l | tr -d ' ')"
echo "vendored $count files into $vendored_dir"
