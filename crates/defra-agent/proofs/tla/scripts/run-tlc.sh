#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

JAR=".tools/tla2tools.jar"
MODULE="${1:?usage: run-tlc.sh <module> [extra TLC args...]}"
shift

if [[ ! -f "$JAR" ]]; then
  echo "Missing $JAR — run scripts/install-tools.sh first." >&2
  exit 1
fi

mkdir -p states
exec java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC \
  -workers auto \
  -metadir states \
  "$@" \
  "$MODULE"
