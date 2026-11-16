#!/usr/bin/env bash
# Install the pinned cargo-nextest for the archived test shards (scripts/ci/gate.py).
set -euo pipefail

version="0.9.149"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    asset="cargo-nextest-${version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256="32d1d8c0567b7a80bee2bad8bb09125c846da96bc009b2de11ec08557290c770"
    ;;
  Darwin-*)
    asset="cargo-nextest-${version}-universal-apple-darwin.tar.gz"
    sha256="47b9941765a9ff94e304a3fe5444ac1c7593da655525b8b69f8f2b31a9217507"
    ;;
  *)
    echo "::error::No pinned cargo-nextest for $(uname -s) $(uname -m)."
    exit 1
    ;;
esac

directory="${RUNNER_TOOL_CACHE:-${HOME}/.cache}/cargo-nextest/${version}"
if [[ ! -x "${directory}/cargo-nextest" ]]; then
  mkdir -p "${directory}"
  staging="$(mktemp -d "${directory}.XXXXXX")"
  curl -fsSL --retry 3 -o "${staging}/${asset}" \
    "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${version}/${asset}"
  if command -v sha256sum >/dev/null 2>&1; then
    echo "${sha256}  ${staging}/${asset}" | sha256sum -c -
  else
    echo "${sha256}  ${staging}/${asset}" | shasum -a 256 -c -
  fi
  tar -xzf "${staging}/${asset}" -C "${staging}" cargo-nextest
  # Runners on one host share the tool cache; a rename publishes the binary whole.
  mv -f "${staging}/cargo-nextest" "${directory}/cargo-nextest"
  rm -rf "${staging}"
fi

echo "${directory}" >> "${GITHUB_PATH}"
"${directory}/cargo-nextest" nextest --version
