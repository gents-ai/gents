#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo install --profile dev-install --locked --force --path crates/defra-agent-cli "$@"
cargo install --profile dev-install --locked --force --path crates/defra-agent-desktop "$@"
