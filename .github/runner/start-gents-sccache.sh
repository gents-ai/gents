#!/usr/bin/env bash
set -euo pipefail

runner_parent="/Users/admin/.ghrunner"
target_parent="/Users/admin/.cache/gents-cargo-target"
base_dirs=()
for runner_config in "${runner_parent}"/*/.runner; do
  [[ -f "${runner_config}" ]] || continue
  runner_root="${runner_config%/.runner}"
  runner_name="$(/usr/bin/plutil -extract agentName raw -o - "${runner_config}")"
  base_dirs+=("${runner_root}/_work/gents/gents")
  base_dirs+=("${target_parent}/${runner_name}")
done

if (( ${#base_dirs[@]} == 0 )); then
  echo "No GitHub runner registrations found beneath ${runner_parent}." >&2
  exit 1
fi

SCCACHE_BASEDIRS="$(IFS=:; echo "${base_dirs[*]}")"
export SCCACHE_BASEDIRS
export SCCACHE_NO_DAEMON=1
export SCCACHE_START_SERVER=1

# Foreground mode lets launchd own and supervise the actual cache server.
exec /opt/homebrew/bin/sccache
