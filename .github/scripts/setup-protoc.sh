#!/usr/bin/env bash
set -euo pipefail

protoc_path=""
if [[ -x "/opt/homebrew/bin/protoc" ]]; then
  protoc_path="/opt/homebrew/bin/protoc"
elif command -v protoc >/dev/null 2>&1; then
  protoc_path="$(command -v protoc)"
else
  if ! command -v brew >/dev/null 2>&1; then
    echo "::error::protoc is missing and Homebrew is not available to install protobuf."
    exit 1
  fi
  HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 brew install protobuf
  protoc_path="$(brew --prefix protobuf)/bin/protoc"
fi

if [[ ! -x "${protoc_path}" ]]; then
  echo "::error::protoc was not found at ${protoc_path}."
  exit 1
fi

dirname "${protoc_path}" >> "${GITHUB_PATH}"
echo "PROTOC=${protoc_path}" >> "${GITHUB_ENV}"
"${protoc_path}" --version
