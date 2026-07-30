#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_INPUT="${1:-$ROOT/target/npm-pack}"
mkdir -p "$OUT_INPUT"
OUT="$(cd "$OUT_INPUT" && pwd)"
find "$OUT" -maxdepth 1 -type f -name 'source-inc-gents-desktop-*.tgz' -delete
cd "$ROOT"
for pkg in gents-desktop-tokens gents-desktop-client gents-desktop-ui gents-desktop-chat gents-desktop-fleet gents-desktop-operations; do
  echo "packing $pkg"
  npm pack -w "@source-inc/$pkg" --pack-destination "$OUT"
done
ls -la "$OUT"

CONSUMER="$(mktemp -d "${TMPDIR:-/tmp}/gents-packed-consumer.XXXXXX")"
trap 'rm -rf "$CONSUMER"' EXIT
cp -R "$ROOT/scripts/packed-desktop-consumer/." "$CONSUMER/"
cd "$CONSUMER"
npm install --ignore-scripts --package-lock=false
npm install --ignore-scripts --package-lock=false "$OUT"/source-inc-gents-desktop-*.tgz
npm run build
npm run verify
echo "packed desktop consumer gate passed"
