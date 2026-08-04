#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <runtime|cli|desktop|support> [--list-packages]" >&2
  exit 2
fi

suite="$1"
mode="${2:-build}"
packages=()
separate_package=""

case "${suite}" in
  runtime)
    packages=(gents)
    ;;
  cli)
    packages=(gents-cli)
    ;;
  desktop)
    packages=(
      gents-desktop-core
      gents-desktop
      gents-desktop-bridge
      fixture-domain-plugin
      gents-fixture-host
    )
    # Keep Tauri in its own Cargo invocation; its target surface and feature
    # graph are substantially heavier than the reusable desktop crates.
    separate_package="gents-desktop-tauri"
    ;;
  support)
    packages=(
      gents-lean-contract
      gents-migration
      gents-lens-fixture-add-label
      gents-fs-runner
      gents-protocol
      gents-schemas
    )
    ;;
  *)
    echo "unknown Rust CI suite: ${suite}" >&2
    exit 2
    ;;
esac

if [[ "${mode}" == "--list-packages" ]]; then
  printf '%s\n' "${packages[@]}"
  if [[ -n "${separate_package}" ]]; then
    printf '%s\n' "${separate_package}"
  fi
  exit 0
fi
if [[ "${mode}" != "build" ]]; then
  echo "unknown mode: ${mode}" >&2
  exit 2
fi

cargo_args=()
for package in "${packages[@]}"; do
  cargo_args+=(-p "${package}")
done
cargo test "${cargo_args[@]}" --all-targets --no-run

if [[ -n "${separate_package}" ]]; then
  cargo test -p "${separate_package}" --all-targets --no-run
fi
