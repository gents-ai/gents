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

# A cancelled Lake checkout can leave a Git lock in the persistent cache.
# Leave locked checkouts to their owner and reuse a separate warm generation.
generation=0
while compgen -G "${cache_dir}/packages/*/.git/index.lock" > /dev/null; do
  echo "Skipping Lean cache with a locked package checkout: ${cache_dir}"
  generation=$((generation + 1))
  cache_dir="${LEAN_CACHE_ROOT}/.generations/${runner_cache_key}/${generation}"
done
mkdir -p "${cache_dir}"
if [[ -L "${lake_link}" && "$(readlink "${lake_link}")" == "${cache_dir}" ]]; then
  echo "Reusing Lean build cache ${cache_dir}"
  exit 0
fi

# A link to another cache, or a real .lake left by a build before this runner's
# cache existed, is build output inside the job's own checkout: replace it.
# Anything else, or a proofs directory that resolves outside the checkout, is
# refused rather than removed.
proofs_dir="$(cd "$(dirname "${lake_link}")" && pwd -P)"
workspace_dir="$(cd "${GITHUB_WORKSPACE}" && pwd -P)"
if [[ "${proofs_dir}" != "${workspace_dir}/"* ]]; then
  echo "::error::${lake_link} resolves outside ${GITHUB_WORKSPACE}; refusing to replace it."
  exit 1
fi
if [[ -L "${lake_link}" ]]; then
  echo "Replacing stale Lean cache link ${lake_link} -> $(readlink "${lake_link}")"
  rm -f "${lake_link}"
elif [[ -d "${lake_link}" ]]; then
  echo "Replacing stray Lean build directory ${lake_link}"
  rm -rf "${lake_link}"
elif [[ -e "${lake_link}" ]]; then
  echo "::error::${lake_link} exists but is neither a directory nor a link; refusing to replace it."
  exit 1
fi

ln -s "${cache_dir}" "${lake_link}"
echo "Using Lean build cache ${cache_dir}"
