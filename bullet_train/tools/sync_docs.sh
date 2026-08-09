#!/usr/bin/env bash
#
# Refresh the vendored copy of the Bullet Train developer documentation.
#
# The pages published at bullettrain.co/docs are authored as Markdown in the
# bullet_train-core repository, so this fetches that source directly rather than
# scraping the rendered site. normalize_docs.py then strips the website-only
# markup and rewrites links to resolve on disk.
#
# Usage: tools/sync_docs.sh [ref]     (ref defaults to main)

set -euo pipefail

REPO="bullet-train-co/bullet_train-core"
UPSTREAM_DIR="bullet_train/docs"
REF="${1:-main}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
docs="$(cd "$here/.." && pwd)/docs"

mkdir -p "$docs"

# Ask the tree API for every blob under the docs directory, so new pages are
# picked up automatically instead of tracking a hand-maintained list.
paths="$(gh api "repos/$REPO/git/trees/$REF?recursive=1" \
  --jq ".tree[] | select(.path|startswith(\"$UPSTREAM_DIR/\")) | select(.type==\"blob\") | .path")"

count=0
while read -r path; do
  [ -n "$path" ] || continue
  relative="${path#"$UPSTREAM_DIR"/}"
  mkdir -p "$docs/$(dirname "$relative")"
  curl -sSf -o "$docs/$relative" "https://raw.githubusercontent.com/$REPO/$REF/$path"
  count=$((count + 1))
done <<<"$paths"

curl -sSf -o "$docs/MIT-LICENSE" \
  "https://raw.githubusercontent.com/$REPO/$REF/bullet_train/MIT-LICENSE"

echo "fetched $count page(s) from $REPO@$REF"
python "$here/normalize_docs.py" "$docs"

echo "pinned revision: $(gh api "repos/$REPO/commits?path=$UPSTREAM_DIR&sha=$REF&per_page=1" --jq '.[0].sha')"
