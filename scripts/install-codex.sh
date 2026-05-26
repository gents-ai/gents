#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$BIN_DIR"

cd "$ROOT"
cargo install --profile dev-install --locked --force --path crates/defra-agent-cli "$@"

cat <<EOF
Installed:
  $BIN_DIR/defra-agent

Use:
  defra-agent server --codex-shim
  CODEX_HOME="\$HOME/.defra-agent/codex-ui" codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:9292/
EOF
