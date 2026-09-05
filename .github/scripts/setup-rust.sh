#!/usr/bin/env bash
set -euo pipefail

rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="$rustup_home" CARGO_HOME="$cargo_home"
export PATH="$cargo_home/bin:$PATH"
mkdir -p "$rustup_home"

# The macOS runner slots share one rustup installation. Serialize manifest
# component installation as well as target installation, like setup-elan.sh.
# No component files or global default toolchain are modified outside rustup.
if [[ "${GENTS_RUST_INSTALL_LOCKED:-0}" != "1" ]]; then
  if ! command -v lockf >/dev/null 2>&1; then
    echo "::error::macOS Rust setup requires lockf to serialize shared rustup installation."
    exit 1
  fi
  script_dir="$(cd "$(dirname "$0")" && pwd)"
  exec lockf -k "$rustup_home/.gents-install.lock" \
    env GENTS_RUST_INSTALL_LOCKED=1 \
    "$script_dir/$(basename "$0")" "$@"
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "::error::The self-hosted Rust runner must have rustup installed."
  exit 1
fi
workspace_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$workspace_root"
# Let rustup read channel, profile and components from rust-toolchain.toml;
# do not introduce another toolchain version list or change the global default.
unset RUSTUP_TOOLCHAIN
rustup toolchain install --no-self-update
# rustup's no-name install branch honors the manifest but does not forward
# CLI target options; add the runtime's WASM target explicitly under this lock.
rustup target add wasm32-unknown-unknown
active_toolchain="$(rustup show active-toolchain)"
active_toolchain="${active_toolchain%% *}"
if [[ -z "$active_toolchain" || "$active_toolchain" == *$'\n'* ]]; then
  echo "::error::rustup did not report one active toolchain."
  exit 1
fi
export RUSTUP_TOOLCHAIN="$active_toolchain"
rustc --version --verbose
cargo --version
if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "$cargo_home/bin" >> "$GITHUB_PATH"
fi
if [[ -n "${GITHUB_ENV:-}" ]]; then
  echo "CARGO_HOME=$cargo_home" >> "$GITHUB_ENV"
  echo "RUSTUP_TOOLCHAIN=$active_toolchain" >> "$GITHUB_ENV"
fi
