#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: install-local.sh [cli|desktop|all] [cargo-install-args...]

  cli      install the Gents CLI only
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
  cargo install --profile dev-install --locked --force --path crates/gents-cli "$@"
fi

if [[ "$target" == "desktop" || "$target" == "all" ]]; then
  cargo install --profile dev-install --locked --force --path crates/gents-desktop "$@"

  npm ci
  npm run build:packages
  (
    cd apps/gents-desktop
    npm run tauri -- build --debug --no-bundle
  )

  install -m 755 \
    "target/debug/gents-desktop-tauri" \
    "${CARGO_HOME:-$HOME/.cargo}/bin/gents-desktop-tauri"
fi
