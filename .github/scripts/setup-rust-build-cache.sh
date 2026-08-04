#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
  echo "::error::CARGO_BUILD_JOBS must be set for Studio runner builds."
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

runner_cache_key="$(printf '%s' "${RUNNER_NAME}" | tr -c 'A-Za-z0-9._-' '_')"
cargo_target_dir="${CARGO_TARGET_ROOT}/${runner_cache_key}"
mkdir -p "${cargo_target_dir}"
echo "CARGO_TARGET_DIR=${cargo_target_dir}" >> "${GITHUB_ENV}"

echo "Using CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"
echo "Using CARGO_TARGET_DIR=${cargo_target_dir}"
echo "Using CARGO_BUILD_RUSTC_WRAPPER=${CARGO_BUILD_RUSTC_WRAPPER:-unset}"
echo "Using RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}"

if [[ "${RUSTC_WRAPPER:-}" != "sccache" ]]; then
  echo "sccache is disabled for this suite; using the runner-local Cargo target tree."
  exit 0
fi

if [[ -z "${SCCACHE_DIR:-}" ]]; then
  echo "::error::SCCACHE_DIR must be set for disk-backed Studio runner caching."
  exit 1
fi
if ! command -v sccache >/dev/null 2>&1; then
  if ! command -v brew >/dev/null 2>&1; then
    echo "::error::sccache is missing and Homebrew is not available to install it."
    exit 1
  fi
  brew install sccache
fi

mkdir -p "${SCCACHE_DIR}"
echo "Using SCCACHE_DIR=${SCCACHE_DIR}"
echo "Using SCCACHE_CACHE_SIZE=${SCCACHE_CACHE_SIZE:-sccache default}"
sccache --version
# The host-local daemon is shared by every runner process on a Studio. Never
# start or recycle it here: GitHub's runner cleanup can kill a workflow-owned
# daemon underneath a sibling compile. The checked-in launchd service owns it.
service_domain="gui/$(id -u)/com.source.gents.sccache"
if ! launchctl print "${service_domain}" 2>/dev/null | grep -q 'state = running'; then
  echo "::error::Host sccache launchd service is not running (${service_domain})."
  exit 1
fi
if ! sccache --show-stats >/dev/null 2>&1; then
  echo "::error::Host sccache service is unavailable; repair the launchd service before running Studio builds."
  exit 1
fi
echo "Reusing the host sccache server."
