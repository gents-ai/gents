#!/usr/bin/env bash
# Downloads the core packs the Release core packs workflow built once for
# TAG and attached to its GitHub release, waiting for them while that build
# runs, and verifies them against the attached checksums. Every platform leg
# embeds these same archives; the checksum file names them.
# Usage: fetch-release-core-packs.sh TAG DEST [WAIT_SECONDS]
set -euo pipefail
tag="$1"
dest="$2"
wait_seconds="${3:-7200}"
repo="${GITHUB_REPOSITORY:-gents-ai/gents}"
base="https://github.com/${repo}/releases/download/${tag}"
mkdir -p "$dest"
deadline=$(( $(date +%s) + wait_seconds ))
until curl -fsSL -o "$dest/SHA256SUMS-core-packs.txt" "$base/SHA256SUMS-core-packs.txt"; do
  if (( $(date +%s) >= deadline )); then
    echo "::error::The ${tag} release has no core packs after ${wait_seconds}s. Run the Release core packs workflow on ${tag}, then rerun this job." >&2
    exit 1
  fi
  echo "Waiting for the core packs of ${tag}..."
  sleep 60
done
packs=()
while read -r _ name; do
  [[ "$name" =~ ^gents\.[a-z0-9_]+\.pack$ ]] || { echo "::error::Unexpected entry in SHA256SUMS-core-packs.txt: ${name}" >&2; exit 1; }
  packs+=("$name")
done < "$dest/SHA256SUMS-core-packs.txt"
(( ${#packs[@]} )) || { echo "::error::SHA256SUMS-core-packs.txt of ${tag} lists no packs." >&2; exit 1; }
for name in "${packs[@]}"; do
  curl -fsSL -o "$dest/$name" "$base/$name"
done
cd "$dest"
if command -v sha256sum >/dev/null; then sha256sum -c SHA256SUMS-core-packs.txt; else shasum -a 256 -c SHA256SUMS-core-packs.txt; fi
