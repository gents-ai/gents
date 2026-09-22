#!/bin/sh
# Refresh third_party/defradb-explorer from a local defradb-explorer checkout
# (gents-ai/defradb-explorer, branch gents-embed). Builds in embedded mode and
# vendors the dist that `gents serve` embeds at /explorer/.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
explorer="${DEFRADB_EXPLORER:-$root/../defradb-explorer}"
if [ ! -f "$explorer/package.json" ]; then
  echo "defradb-explorer not found at $explorer (set DEFRADB_EXPLORER)" >&2
  exit 1
fi
out="$root/third_party/defradb-explorer"
mkdir -p "$out"
(cd "$explorer" && npm ci && VITE_EMBEDDED=1 npm run build)
rm -rf "$out/dist"
cp -R "$explorer/dist" "$out/dist"
git -C "$explorer" rev-parse HEAD > "$out/PIN"
echo "pinned $(cat "$out/PIN")"
