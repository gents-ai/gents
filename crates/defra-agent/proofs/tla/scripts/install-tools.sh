#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${TLA_VERSION:-v1.8.0}"
URL="https://github.com/tlaplus/tlaplus/releases/download/${VERSION}/tla2tools.jar"
DEST=".tools/tla2tools.jar"
# Shared, version-pinned cache so sibling git worktrees reuse one download
# instead of each fetching its own copy into a gitignored .tools/. Override
# the location with TLA_TOOLS_HOME.
CACHE_DIR="${TLA_TOOLS_HOME:-$HOME/.cache/tlaplus/${VERSION}}"
CACHE="${CACHE_DIR}/tla2tools.jar"

mkdir -p .tools
if [[ -f "$DEST" ]]; then
  echo "tla2tools.jar already present at $DEST"
else
  if [[ ! -f "$CACHE" ]]; then
    echo "Downloading tla2tools.jar ${VERSION} to shared cache ${CACHE}..."
    mkdir -p "$CACHE_DIR"
    TMP="${CACHE}.tmp"
    trap 'rm -f "$TMP"' EXIT
    curl -fL "$URL" -o "$TMP"
    mv "$TMP" "$CACHE"
    trap - EXIT
  else
    echo "Reusing cached tla2tools.jar from ${CACHE}"
  fi
  # Link the cached jar into this worktree's .tools/ (copy if symlinks fail).
  ln -sf "$CACHE" "$DEST" 2>/dev/null || cp "$CACHE" "$DEST"
fi

{ java -cp "$DEST" tlc2.TLC -help 2>&1 || true; } | grep -q "NAME" && echo "TLC $VERSION ready."
