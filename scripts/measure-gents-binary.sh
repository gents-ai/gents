#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
if [[ "$target_dir" != /* ]]; then
  target_dir="$repo_root/$target_dir"
fi

build_elapsed_seconds=skipped
if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  build_started_at=$(date +%s)
  cargo build --release --locked -p gents-cli --bin gents
  build_elapsed_seconds=$(($(date +%s) - build_started_at))
fi

binary="$target_dir/release/gents"
if [[ ! -f "$binary" ]]; then
  echo "missing $binary; run without SKIP_BUILD=1 first" >&2
  exit 1
fi

binary_bytes=$(wc -c < "$binary" | tr -d ' ')
resolved_packages=$(cargo tree --locked -p gents-cli --edges normal --prefix none --format '{p}' | sort -u | wc -l | tr -d ' ')
codex_packages=$(cargo tree --locked -p gents-cli --edges normal --prefix none --format '{p}' | sed -n 's/^\(codex-[^ ]*\).*/\1/p' | sort -u | wc -l | tr -d ' ')

echo "binary: $binary"
echo "build_elapsed_seconds: $build_elapsed_seconds"
echo "binary_bytes: $binary_bytes"
echo "resolved_packages: $resolved_packages"
echo "codex_packages: $codex_packages"
size "$binary"
