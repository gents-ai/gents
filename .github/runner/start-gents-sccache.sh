#!/usr/bin/env bash
set -euo pipefail

runner_parent="/Users/admin/.ghrunner"
base_dirs=()
for runner_config in "${runner_parent}"/*/.runner; do
  [[ -f "${runner_config}" ]] || continue
  runner_root="${runner_config%/.runner}"
  base_dirs+=("${runner_root}/_work/gents/gents")
done

if (( ${#base_dirs[@]} == 0 )); then
  echo "No GitHub runner registrations found beneath ${runner_parent}." >&2
  exit 1
fi

export SCCACHE_BASEDIRS="$(IFS=:; echo "${base_dirs[*]}")"
exec /opt/homebrew/bin/sccache --start-server
