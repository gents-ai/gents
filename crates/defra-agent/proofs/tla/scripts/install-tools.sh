#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${TLA_VERSION:-v1.8.0}"
URL="https://github.com/tlaplus/tlaplus/releases/download/${VERSION}/tla2tools.jar"
DEST=".tools/tla2tools.jar"

mkdir -p .tools
if [[ -f "$DEST" ]]; then
  echo "tla2tools.jar already present at $DEST"
else
  echo "Downloading tla2tools.jar ${VERSION}..."
  curl -fL "$URL" -o "$DEST"
fi

java -cp "$DEST" tlc2.TLC -h | head -1
