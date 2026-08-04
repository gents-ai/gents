#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${GITHUB_WORKSPACE:-}" ]]; then
  echo "::error::GITHUB_WORKSPACE must be set for the Lean build cache."
  exit 1
fi

if [[ -z "${LEAN_CACHE_ROOT:-}" ]]; then
  echo "::error::LEAN_CACHE_ROOT must be set for persistent Lean build output."
  exit 1
fi

if [[ -z "${RUNNER_NAME:-}" ]]; then
  echo "::error::RUNNER_NAME must be set to isolate Lean output per runner process."
  exit 1
fi

runner_cache_key="$(printf '%s' "${RUNNER_NAME}" | tr -c 'A-Za-z0-9._-' '_')"
cache_dir="${LEAN_CACHE_ROOT}/${runner_cache_key}"
lake_link="${GITHUB_WORKSPACE}/crates/gents/proofs/.lake"

mkdir -p "${cache_dir}"
if [[ -L "${lake_link}" && "$(readlink "${lake_link}")" == "${cache_dir}" ]]; then
  echo "Reusing Lean build cache ${cache_dir}"
  exit 0
fi

if [[ -e "${lake_link}" || -L "${lake_link}" ]]; then
  echo "::error::${lake_link} exists but is not the expected runner-local cache link."
  exit 1
fi

ln -s "${cache_dir}" "${lake_link}"
echo "Using Lean build cache ${cache_dir}"
