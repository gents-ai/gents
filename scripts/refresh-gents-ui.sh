#!/bin/sh
# Refresh vendor/gents-design tarballs from a local gents-design checkout.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
design="${GENTS_DESIGN:-$root/../gents-design}"
if [ ! -x "$design/scripts/pack-for-gents.sh" ]; then
  echo "gents-design not found at $design (set GENTS_DESIGN)" >&2
  exit 1
fi
out="$root/third_party/gents-design"
mkdir -p "$out"
"$design/scripts/pack-for-gents.sh" "$out"
git -C "$design" rev-parse HEAD > "$out/PIN"
echo "pinned $(cat "$out/PIN")"
