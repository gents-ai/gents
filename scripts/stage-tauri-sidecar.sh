#!/usr/bin/env bash
# Stage an already-built Gents CLI using Tauri's target-suffixed sidecar name.
# This deliberately does not build the CLI or install/register a host service.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: stage-tauri-sidecar.sh --source PATH [--target TARGET_TRIPLE]

Copies an existing gents CLI to:
  apps/gents-desktop/src-tauri/binaries/gents-<target-triple>[.exe]

Tauri's bundle.externalBin entry is the unsuffixed logical name
"binaries/gents". The Tauri CLI selects this target-suffixed staged file.
EOF
}

source_binary=""
target_triple=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      source_binary="$2"
      shift 2
      ;;
    --target)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      target_triple="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[[ -n "$source_binary" ]] || { echo "--source is required" >&2; usage >&2; exit 2; }
if [[ -z "$target_triple" ]]; then
  target_triple="$(rustc --print host-tuple 2>/dev/null || rustc -Vv | awk '/^host:/ { print $2 }')"
fi
[[ -n "$target_triple" ]] || { echo "could not determine the Rust target triple" >&2; exit 1; }
case "$target_triple" in
  *[!A-Za-z0-9._-]*)
    echo "target triple contains unsupported characters: $target_triple" >&2
    exit 2
    ;;
esac
[[ -f "$source_binary" && -x "$source_binary" ]] || {
  echo "Gents CLI is not an executable file: $source_binary" >&2
  exit 1
}

case "$target_triple" in
  *-windows-*)
    extension=".exe"
    [[ "$source_binary" == *.exe ]] || {
      echo "Windows sidecar source must end in .exe: $source_binary" >&2
      exit 2
    }
    ;;
  *) extension="" ;;
esac

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
stage_dir="$repo_root/apps/gents-desktop/src-tauri/binaries"
stage_path="$stage_dir/gents-$target_triple$extension"

mkdir -p "$stage_dir"
install -m 0755 "$source_binary" "$stage_path"
printf 'Staged Tauri sidecar: %s\n' "$stage_path"
