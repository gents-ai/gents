#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

mkdir -p "${CARGO_HOME:-$HOME/.cargo}/bin"

cargo install --profile dev-install --locked --force --path crates/defra-agent-cli "$@"
cargo install --profile dev-install --locked --force --path crates/defra-agent-desktop "$@"

(
  cd apps/desktop-tauri
  bun install
  bun run tauri build --debug --no-bundle
)

install -m 755 \
  "target/debug/defra-agent-desktop-tauri" \
  "${CARGO_HOME:-$HOME/.cargo}/bin/defra-agent-desktop-tauri"
