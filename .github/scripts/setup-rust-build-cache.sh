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
# Linux it is the role's systemd unit when the host installs one;
# otherwise each runner gets its own server (below).
# The job's runner role names its host cache supervisor in the inventory.
role_cache() {
  python3 - "$1" <<'PYTHON'
import json, os, sys
from pathlib import Path

role = os.environ.get("GENTS_CI_RUNNER_ROLE", "")
inventory = Path(os.environ.get("GITHUB_WORKSPACE", ".")) / ".github/ci-runners.json"
roles = json.loads(inventory.read_text())["roles"] if role and inventory.is_file() else {}
print(roles.get(role, {}).get("cache", {}).get(sys.argv[1], ""))
PYTHON
}
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
  sccache_label="$(role_cache launchd_label)"
  service_domain="${user_domain}/${sccache_label:-com.source.gents.sccache}"
  if ! launchctl print "${user_domain}" >/dev/null 2>&1; then
    cache_unavailable "$(id -un) has no GUI login session on $(hostname -s), so its LaunchAgents (the sccache service, the live Gents node) are gone. This usually follows a WindowServer crash. A person must log in as $(id -un) at the console or over Screen Sharing, or restart the host so it logs in automatically."
  elif ! service_print="$(launchctl print "${service_domain}" 2>/dev/null)"; then
    cache_unavailable "The sccache LaunchAgent ${service_domain} is not loaded on $(hostname -s). A person must load it as $(id -un): launchctl bootstrap ${user_domain} ~/Library/LaunchAgents/${sccache_label:-com.source.gents.sccache}.plist (installed from .github/runner/)."
  elif ! grep -q 'state = running' <<<"${service_print}"; then
    cache_unavailable "The sccache LaunchAgent ${service_domain} is loaded but not running on $(hostname -s). A person must start it as $(id -un): launchctl kickstart ${service_domain}, then read ~/Library/Logs/gents-sccache.stderr.log if it stops again."
  fi
  repair="restart it as $(id -un): launchctl kickstart -k ${service_domain}, and read ~/Library/Logs/gents-sccache.log"
else
  sccache_unit="$(role_cache systemd_unit)"
  sccache_unit="${sccache_unit:-gents-sccache.service}"
  if command -v systemctl >/dev/null 2>&1 && systemctl cat "${sccache_unit}" >/dev/null 2>&1; then
    if ! systemctl is-active --quiet "${sccache_unit}"; then
      cache_unavailable "The sccache systemd unit ${sccache_unit} is not active on $(hostname -s). A person must start it: systemctl start ${sccache_unit}."
    fi
    # Every runner on the host shares this one server, so a job must reach it
    # exactly as the unit serves it: the unit's own binary over its socket.
    # An older sccache on PATH ignores SCCACHE_SERVER_UDS, starts a private
    # TCP server inside the job, and the runner kills that server when the
    # job ends, under the other runners' compiles (2026-11-15).
    sccache_bin="$(systemctl show -p ExecStart --value "${sccache_unit}" | sed -n 's/^{ path=\([^ ;]*\).*/\1/p')"
    unit_socket=""
    unit_dir=""
    for assignment in $(systemctl show -p Environment --value "${sccache_unit}"); do
      case "${assignment}" in
        SCCACHE_SERVER_UDS=*) unit_socket="${assignment#*=}" ;;
        SCCACHE_DIR=*) unit_dir="${assignment#*=}" ;;
      esac
    done
    if [[ -z "${unit_socket}" || ! -x "${sccache_bin}" ]]; then
      cache_unavailable "The sccache systemd unit ${sccache_unit} on $(hostname -s) must run an sccache binary this account can execute and set SCCACHE_SERVER_UDS; it has binary '${sccache_bin}' and socket '${unit_socket}'. A person must fix the unit."
    fi
    export SCCACHE_SERVER_UDS="${unit_socket}" SCCACHE_DIR="${unit_dir:-${SCCACHE_DIR}}"
    repair="restart it: systemctl restart ${sccache_unit}"
  else
    # No host server: this runner gets its own, on a port and in a cache
    # directory derived from the runner's own directory. Only this runner's
    # jobs, one at a time, use it, so the runner stopping it at a job's end
    # never reaches another runner's compile.
    sccache_bin="$(command -v sccache)"
    runner_root="$(dirname "$(dirname "${RUNNER_WORKSPACE:?RUNNER_WORKSPACE must be set}")")"
    runner_key="$(printf '%s' "${runner_root}" | cksum | cut -d' ' -f1)"
    export SCCACHE_SERVER_PORT="$((20000 + runner_key % 20000))"
    export SCCACHE_DIR="${HOME}/.cache/sccache-runner-${runner_key}"
    unset SCCACHE_SERVER_UDS
    echo "SCCACHE_SERVER_PORT=${SCCACHE_SERVER_PORT}" >> "${GITHUB_ENV}"
    mkdir -p "${SCCACHE_DIR}"
    if ! "${sccache_bin}" --show-stats >/dev/null 2>&1; then
      "${sccache_bin}" --start-server
    fi
    repair="check that $(id -un) can run ${sccache_bin} --start-server with SCCACHE_DIR=${SCCACHE_DIR} and port ${SCCACHE_SERVER_PORT}"
  fi
  "${sccache_bin}" --version
  {
    echo "SCCACHE_DIR=${SCCACHE_DIR}"
    echo "RUSTC_WRAPPER=${sccache_bin}"
    if [[ -n "${SCCACHE_SERVER_UDS:-}" ]]; then
      echo "SCCACHE_SERVER_UDS=${SCCACHE_SERVER_UDS}"
    fi
  } >> "${GITHUB_ENV}"
fi
if ! "${sccache_bin:-sccache}" --show-stats >/dev/null 2>&1; then
  cache_unavailable "The sccache server does not answer on $(hostname -s). A person must ${repair}."
fi
echo "Using the sccache server ${SCCACHE_SERVER_UDS:-on port ${SCCACHE_SERVER_PORT:-4226}} (${sccache_bin:-sccache})."
# A server reads its base directories once, at start. A checkout or target tree
# missing from them keys this job's compiles by its own paths, so the host's
# other runners never hit them (both Studio servers, 2026-11-18).
base_dirs="$("${sccache_bin:-sccache}" --show-stats 2>/dev/null | sed -n 's/^Base directories  *//p')"
if [[ -n "${base_dirs}" ]]; then
  for dir in "${GITHUB_WORKSPACE:-}" "${cargo_target_dir}"; do
    if [[ -n "${dir}" && ", ${base_dirs}," != *", ${dir%/}/,"* ]]; then
      echo "::warning title=sccache base directories::The sccache server on $(hostname -s) does not normalize ${dir}, so this job's compiles miss what the host's other runners cached. A person must ${repair}; the service lists every registered runner's checkout and target trees when it starts (.github/runner/start-gents-sccache.sh)."
    fi
  done
fi
