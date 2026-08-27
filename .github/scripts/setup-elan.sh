#!/usr/bin/env bash
set -euo pipefail

elan_home="${ELAN_HOME:-$HOME/.elan}"
export ELAN_HOME="$elan_home"
mkdir -p "$elan_home"

# Multiple self-hosted runner processes share the same account and can enter
# the installer together. Re-enter under a host lock so proxy creation is
# atomic, then skip the network installer when a usable elan already exists.
if [[ "${GENTS_ELAN_INSTALL_LOCKED:-0}" != "1" ]] && command -v lockf >/dev/null 2>&1; then
  script_dir="$(cd "$(dirname "$0")" && pwd)"
  exec lockf -k "$elan_home/.gents-install.lock" \
    env GENTS_ELAN_INSTALL_LOCKED=1 ELAN_HOME="$elan_home" \
    "$script_dir/$(basename "$0")" "$@"
fi

if [[ ! -x "$elan_home/bin/elan" ]]; then
  curl https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh -sSf \
    | sh -s -- -y
fi

echo "$elan_home/bin" >> "$GITHUB_PATH"
"$elan_home/bin/elan" --version
