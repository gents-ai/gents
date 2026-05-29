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
sccache --start-server
