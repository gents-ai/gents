#!/usr/bin/env bash
set -euo pipefail

mode="${1:-run}"
if [[ $# -gt 1 || ( "${mode}" != build && "${mode}" != run ) ]]; then
  echo "usage: $0 [build|run]" >&2
  exit 2
fi

common=(
  --profile "${PROFILE:-test}"
  -p gents-cli
  -p gents-client-host
  -p gents-media-host
  --features gents-client-host/runtime-e2e
)

cargo build "${common[@]}" --bins --test runtime
if [[ "${mode}" == run ]]; then
  cargo test "${common[@]}" --test runtime
fi
