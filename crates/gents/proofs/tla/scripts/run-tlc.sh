#!/usr/bin/env bash
# Runs TLC on one model config with the pinned tools and prints its full
# output, counterexample included. `tla_check.py run --config <MC>` compares
# the result with the config's header instead.
set -euo pipefail
cd "$(dirname "$0")/.."

MODULE="${1:?usage: run-tlc.sh <module> [extra TLC args...]}"
shift

tools="$(python3 scripts/tla_check.py install)"
JAVA="$(sed -n 's/^java: //p' <<<"$tools")"
JAR="$(sed -n 's/^tla2tools: //p' <<<"$tools")"

mkdir -p states
exec "$JAVA" -XX:+UseParallelGC -DTLA-Library="$PWD/generated" -cp "$JAR" tlc2.TLC \
  -workers auto \
  -metadir states \
  "$@" \
  "$MODULE"
