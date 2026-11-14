#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
  echo "::error::CARGO_BUILD_JOBS must be set for Studio runner builds."
  exit 1
fi

if [[ -z "${RUNNER_TEMP:-}" ]]; then
  echo "::error::RUNNER_TEMP must be set; Cargo target output is scoped to the job."
  exit 1
fi

if [[ -z "${RUNNER_NAME:-}" ]]; then
  echo "::error::RUNNER_NAME must be set to isolate Cargo target output per runner process."
  exit 1
fi

build_space="$(dirname "${BASH_SOURCE[0]}")/ensure-rust-build-space.py"
runner_cache_key="$(printf '%s' "${RUNNER_NAME}" | tr -c 'A-Za-z0-9._-' '_')"

# The runner empties RUNNER_TEMP around every job, so linked artifacts never
# outlive the job that built them. sccache cannot cache what links (test
# harnesses, binaries, proc macros, build scripts), so a job-scoped tree
# relinks all of them every job. The workflow sets GENTS_CI_KEEP_TARGET_TREE
# only for refs whose build outputs, build-script outputs included, may be
# reused by the next job of the same trust domain; a runner process runs one
# job at a time, so its kept tree is reused only serially. An uncapped kept
# tree once reached 1.7 TiB, so it is capped per runner and kept only while
# the host keeps its free-space floor.
export CARGO_TARGET_ROOT="${RUNNER_TEMP}/gents-cargo-target"
if [[ "${GENTS_CI_KEEP_TARGET_TREE:-}" == "true" ]]; then
  kept_root="${GENTS_CI_TARGET_ROOT:-${HOME}/.cache/gents-ci-target}"
  target_limits="$(python3 "${build_space}" --limits)"
  read -r target_max_gib target_floor_gib <<<"${target_limits}"
  mkdir -p "${kept_root}/${runner_cache_key}"
  keep_status=0
  CARGO_TARGET_ROOT="${kept_root}" python3 "${build_space}" --keep \
    "${kept_root}/${runner_cache_key}" || keep_status=$?
  case "${keep_status}" in
    0)
      export CARGO_TARGET_ROOT="${kept_root}" CARGO_TARGET_MAX_GIB="${target_max_gib}"
      echo "CARGO_TARGET_MAX_GIB=${CARGO_TARGET_MAX_GIB}" >> "${GITHUB_ENV}"
      echo "Keeping this runner's Cargo target tree, capped at ${target_max_gib} GiB."
      ;;
    10)
      echo "::warning title=Job-scoped Cargo target::$(hostname -s) has less than ${target_floor_gib} GiB free with ${RUNNER_NAME}'s kept tree at its ${target_max_gib} GiB cap, even after emptying it; this job builds in a job-scoped tree."
      ;;
    *)
      exit "${keep_status}"
      ;;
  esac
else
  echo "This ${GITHUB_EVENT_NAME:-unknown} run is not from a trusted ref of this repository; it builds in a job-scoped tree."
fi
# Keep the target path free of run IDs: sccache hashes CARGO_* environment
# values, so a per-runner stable path keeps hits warm.
echo "CARGO_TARGET_ROOT=${CARGO_TARGET_ROOT}" >> "${GITHUB_ENV}"

cargo_target_dir="${CARGO_TARGET_ROOT}/${runner_cache_key}"
mkdir -p "${cargo_target_dir}"
python3 "${build_space}" "${cargo_target_dir}"
echo "CARGO_TARGET_DIR=${cargo_target_dir}" >> "${GITHUB_ENV}"

echo "Using CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"
echo "Using CARGO_TARGET_DIR=${cargo_target_dir}"
echo "Using CARGO_BUILD_RUSTC_WRAPPER=${CARGO_BUILD_RUSTC_WRAPPER:-unset}"
echo "Using RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}"

if [[ "${RUSTC_WRAPPER:-}" != "sccache" ]]; then
  workspace_root="${GITHUB_WORKSPACE:-$(pwd -P)}"
  workspace_wrapper="${workspace_root}/.github/scripts/rustc-direct.sh"
  if [[ ! -x "${workspace_wrapper}" ]]; then
    echo "::error::Direct rustc wrapper is missing or not executable (${workspace_wrapper})."
    exit 1
  fi
  shared_cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
  runner_cargo_home="${CARGO_TARGET_ROOT}/.cargo-home/${runner_cache_key}"
  mkdir -p "${runner_cargo_home}"
  direct_wrapper="${runner_cargo_home}/rustc-direct.sh"
  install -m 0755 "${workspace_wrapper}" "${direct_wrapper}"
  for cache_entry in registry git .package-cache .package-cache-mutate; do
    if [[ -e "${shared_cargo_home}/${cache_entry}" ]]; then
      cache_link="${runner_cargo_home}/${cache_entry}"
      if [[ -L "${cache_link}" ]]; then
        if [[ "$(readlink "${cache_link}")" != "${shared_cargo_home}/${cache_entry}" ]]; then
          echo "::error::Cargo cache link points at the wrong host path (${cache_link})."
          exit 1
        fi
      elif [[ -e "${cache_link}" ]]; then
        echo "::error::Cargo cache overlay entry is not a symlink (${cache_link})."
        exit 1
      else
        ln -s "${shared_cargo_home}/${cache_entry}" "${cache_link}"
      fi
    fi
  done
  printf '[build]\nrustc-wrapper = "%s"\n' "${direct_wrapper}" > "${runner_cargo_home}/config.toml"
  {
    echo "CARGO_HOME=${runner_cargo_home}"
    echo "CARGO_BUILD_RUSTC_WRAPPER=${direct_wrapper}"
    echo "RUSTC_WRAPPER=${direct_wrapper}"
  } >> "${GITHUB_ENV}"
  echo "Installed isolated Cargo configuration at ${runner_cargo_home}/config.toml."
  echo "Installed stable direct rustc wrapper at ${direct_wrapper}."
  echo "Shared dependency caches remain rooted at ${shared_cargo_home}."
  echo "sccache is disabled for this suite; using the Cargo target tree ${cargo_target_dir}."
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
# Never start or recycle the shared host daemon here; the checked-in launchd
# service owns it.
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
