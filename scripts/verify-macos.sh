#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export CARGO_INCREMENTAL=0

cargo check -p gents -p gents-cli
cargo test -p gents --test backend_auth -- --nocapture --test-threads=1
cargo test -p gents-cli --test cli_e2e -- --nocapture --test-threads=1
(cd crates/gents/proofs && lake build)
