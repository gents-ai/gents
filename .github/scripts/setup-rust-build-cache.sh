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

# The runner empties RUNNER_TEMP around every job, so linked artifacts never
# outlive the job that built them. The host sccache service is the shared,
# size-capped compiler cache. Keep this path free of run IDs: sccache hashes
# CARGO_* environment values, so a per-runner stable path keeps hits warm.
export CARGO_TARGET_ROOT="${RUNNER_TEMP}/gents-cargo-target"
echo "CARGO_TARGET_ROOT=${CARGO_TARGET_ROOT}" >> "${GITHUB_ENV}"

if [[ -z "${RUNNER_NAME:-}" ]]; then
  echo "::error::RUNNER_NAME must be set to isolate Cargo target output per runner process."
  exit 1
fi

runner_cache_key="$(printf '%s' "${RUNNER_NAME}" | tr -c 'A-Za-z0-9._-' '_')"
cargo_target_dir="${CARGO_TARGET_ROOT}/${runner_cache_key}"
mkdir -p "${cargo_target_dir}"
python3 "$(dirname "${BASH_SOURCE[0]}")/ensure-rust-build-space.py" "${cargo_target_dir}"
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
  echo "sccache is disabled for this suite; using the job-scoped Cargo target tree."
  exit 0
fi

if [[ -z "${SCCACHE_DIR:-}" ]]; then
  echo "::error::SCCACHE_DIR must be set for disk-backed Studio runner caching."
  exit 1
fi
if ! command -v sccache >/dev/null 2>&1; then
  if [[ "$(uname -s)" != "Darwin" ]] || ! command -v brew >/dev/null 2>&1; then
    echo "::error::sccache is missing; install it for the runner account (Homebrew on macOS, a release binary on Linux)."
    exit 1
  fi
  brew install sccache
fi

mkdir -p "${SCCACHE_DIR}"
echo "Using SCCACHE_DIR=${SCCACHE_DIR}"
echo "Using SCCACHE_CACHE_SIZE=${SCCACHE_CACHE_SIZE:-sccache default}"
sccache --version
# Never recycle a shared host daemon here; its supervisor owns it. On macOS
# that is the checked-in launchd service, a LaunchAgent, so it exists only
# while the runner user has a GUI login session: a WindowServer crash or a
# logout removes it (and the host's live Gents node with it), and the job must
# say so rather than build uncached on a host that has lost its session. On
# Linux it is the systemd unit GENTS_SCCACHE_UNIT when the host installs one;
# otherwise the server starts on demand under the runner account.
cache_unavailable() {
  local message="$1 Builds here stop until this is fixed; rerun the failed jobs afterwards."
  echo "::error title=Runner compiler cache unavailable on $(hostname -s)::${message}"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '### Runner compiler cache unavailable on %s\n\n%s\n' "$(hostname -s)" "${message}" >> "${GITHUB_STEP_SUMMARY}"
  fi
  exit 1
}
if [[ "$(uname -s)" == "Darwin" ]]; then
  user_domain="gui/$(id -u)"
  service_domain="${user_domain}/com.source.gents.sccache"
  if ! launchctl print "${user_domain}" >/dev/null 2>&1; then
    cache_unavailable "$(id -un) has no GUI login session on $(hostname -s), so its LaunchAgents (the sccache service, the live Gents node) are gone. This usually follows a WindowServer crash. A person must log in as $(id -un) at the console or over Screen Sharing, or restart the host so it logs in automatically."
  elif ! service_print="$(launchctl print "${service_domain}" 2>/dev/null)"; then
    cache_unavailable "The sccache LaunchAgent ${service_domain} is not loaded on $(hostname -s). A person must load it as $(id -un): launchctl bootstrap ${user_domain} ~/Library/LaunchAgents/com.source.gents.sccache.plist (installed from .github/runner/)."
  elif ! grep -q 'state = running' <<<"${service_print}"; then
    cache_unavailable "The sccache LaunchAgent ${service_domain} is loaded but not running on $(hostname -s). A person must start it as $(id -un): launchctl kickstart ${service_domain}, then read ~/Library/Logs/gents-sccache.stderr.log if it stops again."
  fi
  repair="restart it as $(id -un): launchctl kickstart -k ${service_domain}, and read ~/Library/Logs/gents-sccache.log"
else
  sccache_unit="${GENTS_SCCACHE_UNIT:-gents-sccache.service}"
  if command -v systemctl >/dev/null 2>&1 && systemctl cat "${sccache_unit}" >/dev/null 2>&1; then
    if ! systemctl is-active --quiet "${sccache_unit}"; then
      cache_unavailable "The sccache systemd unit ${sccache_unit} is not active on $(hostname -s). A person must start it: systemctl start ${sccache_unit}."
    fi
    repair="restart it: systemctl restart ${sccache_unit}"
  else
    if ! sccache --show-stats >/dev/null 2>&1; then
      sccache --start-server
    fi
    repair="check that $(id -un) can run sccache --start-server with SCCACHE_DIR=${SCCACHE_DIR}"
  fi
fi
if ! sccache --show-stats >/dev/null 2>&1; then
  cache_unavailable "The sccache server does not answer on $(hostname -s). A person must ${repair}."
fi
echo "Reusing the host sccache server."
