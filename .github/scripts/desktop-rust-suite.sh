#!/usr/bin/env bash
set -euo pipefail

mode="${1:-test}"
if [[ $# -gt 1 ]]; then
  echo "usage: $0 [build|test|--list-packages]" >&2
  exit 2
fi

packages=(
  gents-desktop-core
  gents-desktop
  gents-desktop-bridge
  gents-client-host
  gents-media-host
  fixture-domain-plugin
  gents-fixture-host
  gents-desktop-tauri
)

if [[ "${mode}" == --list-packages ]]; then
  printf '%s\n' "${packages[@]}"
  exit 0
fi

common=()
for package in "${packages[@]}"; do
  common+=(-p "${package}")
done
common+=(--features notify/macos_kqueue)

case "${mode}" in
  build) cargo test "${common[@]}" --all-targets --no-run ;;
  test) cargo test "${common[@]}" --no-fail-fast ;;
  *) echo "unknown desktop Rust suite mode: ${mode}" >&2; exit 2 ;;
esac
