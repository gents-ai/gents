#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
script="$repo_root/scripts/stage-tauri-sidecar.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/gents-sidecar-test.XXXXXX")"
target="test-sidecar-$$"
staged="$repo_root/apps/gents-desktop/src-tauri/binaries/gents-$target"
trap 'rm -rf "$scratch"; rm -f "$staged"' EXIT

source="$scratch/gents"
printf '#!/usr/bin/env sh\nexit 0\n' > "$source"
chmod 0755 "$source"

"$script" --source "$source" --target "$target"
[[ -x "$staged" ]]
cmp "$source" "$staged"

if "$script" --source "$source" --target '../escape' >/dev/null 2>&1; then
  echo "accepted a path-traversal target triple" >&2
  exit 1
fi

if "$script" --source "$source" --target 'x86_64-pc-windows-msvc' >/dev/null 2>&1; then
  echo "accepted a non-.exe Windows sidecar" >&2
  exit 1
fi
