#!/usr/bin/env bash
# Install the pinned cargo-hakari that lint uses to check gents-workspace-hack (scripts/ci/gate.py).
set -euo pipefail

version="0.9.39"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    asset="cargo-hakari-${version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256="699a3193a9bd1a8d914b970d3f50f3385f24fbd8b1294d0c0ff31fbe7c2c9f48"
    ;;
  Darwin-*)
    asset="cargo-hakari-${version}-universal-apple-darwin.tar.gz"
    sha256="c1f84026b805cc7fb31c271e8b0720abadb8d9416d126c76d6fc50a2a147d7f9"
    ;;
  *)
    echo "::error::No pinned cargo-hakari for $(uname -s) $(uname -m)."
    exit 1
    ;;
esac

directory="${RUNNER_TOOL_CACHE:-${HOME}/.cache}/cargo-hakari/${version}"
if [[ ! -x "${directory}/cargo-hakari" ]]; then
  mkdir -p "${directory}"
  staging="$(mktemp -d "${directory}.XXXXXX")"
  curl -fsSL --retry 3 -o "${staging}/${asset}" \
    "https://github.com/guppy-rs/guppy/releases/download/cargo-hakari-${version}/${asset}"
  if command -v sha256sum >/dev/null 2>&1; then
    echo "${sha256}  ${staging}/${asset}" | sha256sum -c -
  else
    echo "${sha256}  ${staging}/${asset}" | shasum -a 256 -c -
  fi
  tar -xzf "${staging}/${asset}" -C "${staging}" cargo-hakari
  # Runners on one host share the tool cache; a rename publishes the binary whole.
  mv -f "${staging}/cargo-hakari" "${directory}/cargo-hakari"
  rm -rf "${staging}"
fi

echo "${directory}" >> "${GITHUB_PATH}"
"${directory}/cargo-hakari" hakari --version
