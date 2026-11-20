#!/usr/bin/env bash
# Installs the system packages Playwright's browsers need on a hosted Ubuntu
# runner: usage `install-browser-deps.sh <archives dir> <browser>...`, run from
# the desktop app's directory.
#
# `playwright install --with-deps` downloads with apt one package at a time
# from the image's mirror, which took 27 to 30+ minutes on some runners (runs
# 44319819879, 44343419806). This lists the same packages, downloads the ones
# not already in the archives directory in parallel (falling back to
# archive.ubuntu.com for a file the image's mirror does not serve in time), and
# lets apt install from that directory, which the workflow caches.
set -euo pipefail

archives="$1"
shift
mkdir -p "${archives}/partial"

if report="$(npx playwright install-deps --dry-run "$@" 2>&1)"; then
  echo "the image already has the browsers' system packages"
  exit 0
fi
packages="$(awk '
  /^Missing system dependencies \([0-9]+\):$/ {
    count = $NF; gsub(/[^0-9]/, "", count); listing = 1; next
  }
  listing && NF == 1 && $1 ~ /^[a-z0-9][a-z0-9.+:-]+$/ { names[++n] = $1; next }
  listing { exit }
  END { if (n == count && n > 0) for (i = 1; i <= n; i++) print names[i] }
' <<< "${report}")"
if [[ -z "${packages}" ]]; then
  echo "::warning::could not read Playwright's missing package list; using its own install"
  printf '%s\n' "${report}"
  npx playwright install-deps "$@"
  exit 0
fi
echo "missing system packages: $(tr '\n' ' ' <<< "${packages}")"

sudo apt-get update -qq
mapfile -t names <<< "${packages}"
# Each line: 'URI' file size checksum. A package already installed is not listed.
sudo apt-get install -y --no-install-recommends --print-uris -qq "${names[@]}" \
  | sed -n "s/^'\([^']*\)' \([^ ]*\) .*/\1 \2/p" > "${archives}/uris.txt"
echo "$(wc -l < "${archives}/uris.txt") packages to fetch for $(wc -l <<< "${packages}") listed"

fetch() {
  local uri="$1" file="$2" target="${archives}/$2"
  [[ -s "${target}" ]] && return 0
  local source
  local -a sources=()
  case "${uri}" in http://*|https://*) sources+=("${uri}");; esac
  if [[ "${uri}" == */pool/* ]]; then
    sources+=("https://archive.ubuntu.com/ubuntu/pool/${uri#*/pool/}")
  fi
  for source in "${sources[@]}"; do
    if curl -fsSL --retry 2 --connect-timeout 15 --speed-limit 20000 --speed-time 20 \
      -o "${target}.part" "${source}"; then
      mv "${target}.part" "${target}"
      return 0
    fi
  done
  rm -f "${target}.part"
  echo "::warning::could not fetch ${file}; apt will try"
}
export -f fetch
export archives
# shellcheck disable=SC2016 # expanded by the inner bash
xargs -r -P 16 -n 2 bash -c 'fetch "$1" "$2"' _ < "${archives}/uris.txt"

sudo apt-get install -y --no-install-recommends -o "Dir::Cache::Archives=${archives}" "${names[@]}"
