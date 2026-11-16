#!/usr/bin/env bash
set -euo pipefail

runner_parent="/Users/admin/.ghrunner"
# The target trees setup-rust-build-cache.sh gives each runner: the kept tree
# under the kept root and the job-scoped tree under the runner's temp dir, both
# keyed by the runner name with the same character mapping.
kept_parent="/Users/admin/.cache/gents-ci-target"
base_dirs=()
for runner_config in "${runner_parent}"/*/.runner; do
  [[ -f "${runner_config}" ]] || continue
  runner_root="${runner_config%/.runner}"
  runner_name="$(/usr/bin/plutil -extract agentName raw -o - "${runner_config}")"
  runner_key="$(printf '%s' "${runner_name}" | tr -c 'A-Za-z0-9._-' '_')"
  base_dirs+=("${runner_root}/_work/gents/gents")
  base_dirs+=("${kept_parent}/${runner_key}")
  base_dirs+=("${runner_root}/_work/_temp/gents-cargo-target/${runner_key}")
done

if (( ${#base_dirs[@]} == 0 )); then
  echo "No GitHub runner registrations found beneath ${runner_parent}." >&2
  exit 1
fi

SCCACHE_BASEDIRS="$(IFS=:; echo "${base_dirs[*]}")"
export SCCACHE_BASEDIRS
export SCCACHE_NO_DAEMON=1
export SCCACHE_START_SERVER=1

# launchd supervises the cache server; it must stay in the foreground.
exec /opt/homebrew/bin/sccache
