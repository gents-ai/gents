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

if [[ -z "${CARGO_TARGET_ROOT:-}" ]]; then
  echo "::error::CARGO_TARGET_ROOT must be set for persistent Cargo target output."
  exit 1
fi

if [[ -z "${RUNNER_NAME:-}" ]]; then
  echo "::error::RUNNER_NAME must be set to isolate Cargo target output per runner process."
  exit 1
fi

mkdir -p "${SCCACHE_DIR}"
runner_cache_key="$(printf '%s' "${RUNNER_NAME}" | tr -c 'A-Za-z0-9._-' '_')"
cargo_target_dir="${CARGO_TARGET_ROOT}/${runner_cache_key}"
mkdir -p "${cargo_target_dir}"
echo "CARGO_TARGET_DIR=${cargo_target_dir}" >> "${GITHUB_ENV}"

echo "Using CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"
echo "Using CARGO_TARGET_DIR=${cargo_target_dir}"
echo "Using RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}"
echo "Using SCCACHE_DIR=${SCCACHE_DIR}"
echo "Using SCCACHE_CACHE_SIZE=${SCCACHE_CACHE_SIZE:-sccache default}"

sccache --version
# The host-local daemon is shared by every runner process on a Studio. Never
# recycle it here: doing so can interrupt a sibling job. Start a missing daemon
# from its durable cache directory so its cwd survives checkout cleanup. A
# sibling may win the startup race, so tolerate start-server failing and poll
# the daemon before treating startup as broken.
if sccache --show-stats >/dev/null 2>&1; then
  echo "Reusing the host sccache server."
else
  echo "No sccache server is running; starting it from ${SCCACHE_DIR}."
  (cd "${SCCACHE_DIR}" && sccache --start-server) || true
  for _ in 1 2 3 4 5; do
    if sccache --show-stats >/dev/null 2>&1; then
      echo "Host sccache server is ready."
      exit 0
    fi
    sleep 1
  done
  echo "::error::Host sccache server did not become ready."
  exit 1
fi
