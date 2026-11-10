#!/usr/bin/env bash
set -euo pipefail

cmake_path=""
if [[ -x "/opt/homebrew/bin/cmake" ]]; then
  cmake_path="/opt/homebrew/bin/cmake"
elif command -v cmake >/dev/null 2>&1; then
  cmake_path="$(command -v cmake)"
else
  if ! command -v brew >/dev/null 2>&1; then
    echo "::error::cmake is missing and Homebrew is not available to install it."
    exit 1
  fi
  HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 brew install cmake
  cmake_path="$(brew --prefix cmake)/bin/cmake"
fi

if [[ ! -x "${cmake_path}" ]]; then
  echo "::error::cmake was not found at ${cmake_path}."
  exit 1
fi

dirname "${cmake_path}" >> "${GITHUB_PATH}"
echo "CMAKE=${cmake_path}" >> "${GITHUB_ENV}"
"${cmake_path}" --version
