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

# Multiple self-hosted runner workers share this machine and cache directory.
# A single default sccache port makes their setup steps race: one job stops the
# daemon while another is compiling, then either job can seize the port during
# restart. Give every Actions job/attempt its own stable port while continuing
# to share the disk-backed cache. Persist it through GITHUB_ENV so later build
# and stats steps address the same daemon.
if [[ -z "${SCCACHE_SERVER_PORT:-}" ]]; then
  port_seed="${RUNNER_NAME:-studio}:${GITHUB_RUN_ID:-local}:${GITHUB_JOB:-job}:${GITHUB_RUN_ATTEMPT:-0}"
  port_checksum="$(printf '%s' "${port_seed}" | cksum)"
  port_checksum="${port_checksum%% *}"
  export SCCACHE_SERVER_PORT="$((30000 + port_checksum % 20000))"
  if [[ -n "${GITHUB_ENV:-}" ]]; then
    printf 'SCCACHE_SERVER_PORT=%s\n' "${SCCACHE_SERVER_PORT}" >> "${GITHUB_ENV}"
  fi
fi

echo "Using CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"
echo "Using RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}"
echo "Using SCCACHE_DIR=${SCCACHE_DIR}"
echo "Using SCCACHE_CACHE_SIZE=${SCCACHE_CACHE_SIZE:-sccache default}"
echo "Using SCCACHE_SERVER_PORT=${SCCACHE_SERVER_PORT}"

sccache --version
# Recycle only this job's daemon. A prior attempt that cleaned its workdir can
# leave its daemon with a dead cwd, which surfaces as "Couldn't determine
# current working directory" / missing dep-info files mid compile.
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
