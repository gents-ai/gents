#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export CARGO_INCREMENTAL=0

cargo check -p defra-agent -p defra-agent-cli
cargo test -p defra-agent --test backend_auth -- --nocapture --test-threads=1
cargo test -p defra-agent-cli --test cli_e2e -- --nocapture --test-threads=1
(cd crates/gents/proofs && lake build)
