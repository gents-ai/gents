#!/usr/bin/env bash
# Packed-artifact gate: npm pack each package into a clean dir (phase 5).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/target/npm-pack}"
mkdir -p "$OUT"
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
node --input-type=module -e '
  const styles = [
    "@source-inc/gents-desktop-tokens/semantic.css",
    "@source-inc/gents-desktop-ui/styles.css",
    "@source-inc/gents-desktop-chat/styles.css",
    "@source-inc/gents-desktop-fleet/styles.css",
    "@source-inc/gents-desktop-fleet/local-runtime.css",
    "@source-inc/gents-desktop-operations/styles.css",
  ];
  for (const style of styles) import.meta.resolve(style);
'
echo "packed desktop consumer gate passed"
