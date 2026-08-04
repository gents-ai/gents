#!/usr/bin/env bash
set -euo pipefail

# Cargo treats an empty RUSTC_WRAPPER as unset and falls back to the host's
# build.rustc-wrapper setting. Execute Cargo's rustc command directly so
# incremental suites cannot accidentally re-enable sccache through host config.
exec "$@"
