#!/usr/bin/env bash
set -euo pipefail

# Cargo treats an empty RUSTC_WRAPPER as unset and falls back to the host's
# build.rustc-wrapper setting. Execute Cargo's rustc command directly so
# incremental suites cannot accidentally re-enable sccache through host config.
if [[ "$OSTYPE" == darwin* ]]; then
  # macOS strips Cargo's DYLD_* when starting Bash; rust-lld needs this path.
  compiler="$(command -v "$1")"
  compiler_lib="${compiler%/bin/rustc}/lib"
  export DYLD_FALLBACK_LIBRARY_PATH="${compiler_lib}${DYLD_FALLBACK_LIBRARY_PATH:+:${DYLD_FALLBACK_LIBRARY_PATH}}"
fi
exec "$@"
