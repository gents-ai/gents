#!/usr/bin/env bash
set -euo pipefail

if [[ "$OSTYPE" == darwin* ]]; then
  # macOS strips Cargo's DYLD_* when starting Bash; rust-lld needs this path.
  compiler="$(command -v "$1")"
  compiler_lib="${compiler%/bin/rustc}/lib"
  export DYLD_FALLBACK_LIBRARY_PATH="${compiler_lib}${DYLD_FALLBACK_LIBRARY_PATH:+:${DYLD_FALLBACK_LIBRARY_PATH}}"
fi
exec "$@"
