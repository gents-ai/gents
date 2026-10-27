#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <runtime|cli|desktop|support> [--list-packages]" >&2
  exit 2
fi

suite="$1"
mode="${2:-build}"
packages=()

if [[ "${suite}" == desktop ]]; then
  script_dir="$(dirname "${BASH_SOURCE[0]}")"
  if [[ "${mode}" == --list-packages ]]; then
    exec "${script_dir}/desktop-rust-suite.sh" --list-packages
  fi
  if [[ "${mode}" != build ]]; then
    echo "unknown mode: ${mode}" >&2
    exit 2
  fi
  "${script_dir}/desktop-rust-suite.sh" build
  "${script_dir}/desktop-runtime-journey.sh" build
  exit 0
fi

case "${suite}" in
  runtime)
    packages=(gents gents-loop)
    ;;
  cli)
    packages=(
      gents-cli
    )
    ;;
  support)
    packages=(
      gents-lean-contract
      gents-migration
      gents-lens-fixture-add-label
      gents-callback-fixture-create-workspace
      gents-fs-runner
      gents-protocol
      gents-schemas
      gents-chatgpt-login
      gents-claude-login
      gents-login-ui
      gents-codex-protocol
    )
    ;;
  *)
    echo "unknown Rust CI suite: ${suite}" >&2
    exit 2
    ;;
esac

if [[ "${mode}" == "--list-packages" ]]; then
  printf '%s\n' "${packages[@]}"
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
