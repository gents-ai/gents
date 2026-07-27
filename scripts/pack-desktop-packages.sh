#!/usr/bin/env bash
# Packed-artifact gate: npm pack each package into a clean dir (phase 5).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/target/npm-pack}"
rm -rf "$OUT"
mkdir -p "$OUT"
cd "$ROOT"
for pkg in gents-desktop-tokens gents-desktop-client gents-desktop-chat gents-desktop-fleet gents-desktop-operations; do
  echo "packing $pkg"
  npm pack -w "@source-inc/$pkg" --pack-destination "$OUT"
done
ls -la "$OUT"
