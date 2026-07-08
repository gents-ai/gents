#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: install-local.sh [cli|desktop|all] [cargo-install-args...]

  cli      install the defra-agent CLI only
  desktop  install the desktop binary and Tauri app only
  all      install everything (default)

Extra arguments are forwarded to `cargo install`.
EOF
}

target="all"
case "${1:-}" in
  cli|desktop|all)
    target="$1"
    shift
    ;;
  -h|--help)
    usage
    exit 0
    ;;
esac

cd "$(dirname "$0")/.."

mkdir -p "${CARGO_HOME:-$HOME/.cargo}/bin"

if [[ "$target" == "cli" || "$target" == "all" ]]; then
  cargo install --profile dev-install --locked --force --path crates/defra-agent-cli "$@"
fi

if [[ "$target" == "desktop" || "$target" == "all" ]]; then
  cargo install --profile dev-install --locked --force --path crates/defra-agent-desktop "$@"

  (
    cd apps/desktop-tauri
    bun install
    bun run tauri build --debug --no-bundle
  )

  install -m 755 \
    "target/debug/defra-agent-desktop-tauri" \
    "${CARGO_HOME:-$HOME/.cargo}/bin/defra-agent-desktop-tauri"
fi
