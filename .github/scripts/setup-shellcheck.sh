#!/usr/bin/env bash
# Puts shellcheck on PATH for the job: the host's when it has one, otherwise a
# pinned release unpacked once into the runner account's cache (self-hosted
# runners have no root, and hosts differ in what they install).
set -euo pipefail

if command -v shellcheck >/dev/null 2>&1; then
  shellcheck --version | sed -n 2p
  exit 0
fi

version="v0.10.0"
sha256="6c881ab0698e4e6ea235245f22832860544f17ba386442fe7e9d629f8cbedf87"
if [[ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ]]; then
  echo "::error::shellcheck is missing and no pinned release is configured for $(uname -s)-$(uname -m); install it on the host."
  exit 1
fi
tools="${HOME}/.cache/gents-ci-tools/shellcheck-${version}"
if [[ ! -x "${tools}/shellcheck" ]]; then
  archive="$(mktemp "${RUNNER_TEMP:-/tmp}/shellcheck.XXXXXX")"
  curl -fsSL -o "${archive}" \
    "https://github.com/koalaman/shellcheck/releases/download/${version}/shellcheck-${version}.linux.x86_64.tar.xz"
  echo "${sha256}  ${archive}" | sha256sum -c - >/dev/null
  staging="$(mktemp -d "${tools}.XXXXXX" 2>/dev/null || { mkdir -p "$(dirname "${tools}")"; mktemp -d "${tools}.XXXXXX"; })"
  tar -xJf "${archive}" -C "${staging}" --strip-components=1 "shellcheck-${version}/shellcheck"
  rm -f "${archive}"
  # Concurrent jobs on one host may race here; the first rename wins.
  mv -T "${staging}" "${tools}" 2>/dev/null || rm -rf "${staging}"
fi
echo "${tools}" >> "${GITHUB_PATH:?GITHUB_PATH must be set}"
"${tools}/shellcheck" --version | sed -n 2p
