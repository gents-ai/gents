#!/usr/bin/env bash
set -euo pipefail

if ! command -v sccache >/dev/null 2>&1; then
  if ! command -v brew >/dev/null 2>&1; then
    echo "::error::sccache is missing and Homebrew is not available to install it."
    exit 1
  fi
  brew install sccache
fi

if [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
  echo "::error::CARGO_BUILD_JOBS must be set for Studio runner builds."
  exit 1
fi

if [[ -z "${SCCACHE_DIR:-}" ]]; then
  echo "::error::SCCACHE_DIR must be set for disk-backed Studio runner caching."
  exit 1
fi

mkdir -p "${SCCACHE_DIR}"

echo "Using CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"
echo "Using RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}"
echo "Using SCCACHE_DIR=${SCCACHE_DIR}"
echo "Using SCCACHE_CACHE_SIZE=${SCCACHE_CACHE_SIZE:-sccache default}"

sccache --version
# Studio runners share a long-lived sccache daemon across jobs. A prior job that
# cleaned its workdir can leave the daemon with a dead cwd, which surfaces as
# "Couldn't determine current working directory" / missing dep-info files mid
# compile. Always recycle the server so each job starts from a live process.
if sccache --stop-server >/dev/null 2>&1; then
  echo "Stopped existing sccache server."
else
  echo "No sccache server was running (or stop failed); starting fresh."
fi
# Brief settle so the old process releases the port / lock before we restart.
sleep 1
if ! sccache --start-server; then
  echo "::error::Failed to start sccache server after recycle."
  exit 1
fi
sccache --show-stats >/dev/null
