#!/usr/bin/env bash
# Puts the pinned ruff on PATH for the job, unpacked once into the runner
# account's cache: self-hosted Linux runners have no root and no python3-venv.
set -euo pipefail

version="0.17.3"
sha256="2b0890f5a55900af741f133918c2656919bbf18c2ab7741b4ca8403a90e647a9"
if [[ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ]]; then
  echo "::error::no pinned ruff release is configured for $(uname -s)-$(uname -m)."
  exit 1
fi
tools="${HOME}/.cache/gents-ci-tools/ruff-${version}"
if [[ ! -x "${tools}/ruff" ]]; then
  archive="$(mktemp "${RUNNER_TEMP:-/tmp}/ruff.XXXXXX")"
  curl -fsSL -o "${archive}" \
    "https://github.com/astral-sh/ruff/releases/download/${version}/ruff-x86_64-unknown-linux-gnu.tar.gz"
  echo "${sha256}  ${archive}" | sha256sum -c - >/dev/null
  staging="$(mktemp -d "${tools}.XXXXXX" 2>/dev/null || { mkdir -p "$(dirname "${tools}")"; mktemp -d "${tools}.XXXXXX"; })"
  tar -xzf "${archive}" -C "${staging}" --strip-components=1 "ruff-x86_64-unknown-linux-gnu/ruff"
  rm -f "${archive}"
  # Concurrent jobs on one host may race here; the first rename wins.
  mv -T "${staging}" "${tools}" 2>/dev/null || rm -rf "${staging}"
fi
echo "${tools}" >> "${GITHUB_PATH:?GITHUB_PATH must be set}"
"${tools}/ruff" --version
