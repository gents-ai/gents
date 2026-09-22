#!/usr/bin/env bash
set -euo pipefail

# The full CLI suite compiles every supported Afterburner source language.
# Keep the non-Rust compilers out of mutable host state: install pinned,
# checksum-verified release artifacts into this job's temporary tool directory.
if [[ "$(uname -s)-$(uname -m)" != "Darwin-arm64" ]]; then
  echo "::error::Afterburner CI tool setup currently supports the macOS ARM64 CLI runner only."
  exit 1
fi

tool_root="${RUNNER_TEMP:?RUNNER_TEMP is required}/gents-afterburner-tools"
download_root="$tool_root/downloads"
bin_root="$tool_root/bin"
mkdir -p "$download_root" "$bin_root"

javy_version="8.1.1"
javy_archive="javy-arm-macos-v${javy_version}.gz"
javy_sha256="0ae154f026371aae1e82fb39381fd58e67ca6b2a2985fbce51d305b138dad59f"
javy_url="https://github.com/bytecodealliance/javy/releases/download/v${javy_version}/${javy_archive}"

binaryen_version="131"
binaryen_archive="binaryen-version_${binaryen_version}-arm64-macos.tar.gz"
binaryen_sha256="e441b48dc22163d209b4f05e44dc7210909b01237642b6c9ae48fd710a3ef83e"
binaryen_url="https://github.com/WebAssembly/binaryen/releases/download/version_${binaryen_version}/${binaryen_archive}"

curl --fail --location --retry 3 --output "$download_root/$javy_archive" "$javy_url"
echo "$javy_sha256  $download_root/$javy_archive" | shasum -a 256 --check
gzip --decompress --stdout "$download_root/$javy_archive" > "$bin_root/javy"
chmod +x "$bin_root/javy"

curl --fail --location --retry 3 --output "$download_root/$binaryen_archive" "$binaryen_url"
echo "$binaryen_sha256  $download_root/$binaryen_archive" | shasum -a 256 --check
tar -xzf "$download_root/$binaryen_archive" -C "$tool_root"
ln -sf "$tool_root/binaryen-version_${binaryen_version}/bin/wasm-opt" "$bin_root/wasm-opt"

# Native Rust plugins target WASI Preview 1. setup-rust.sh installs the
# migration fixture's wasm32-unknown-unknown target, which is distinct.
rustup target add wasm32-wasip1

"$bin_root/javy" --version
"$bin_root/wasm-opt" --version
go version
rustup target list --installed | grep -Fx wasm32-wasip1

echo "$bin_root" >> "${GITHUB_PATH:?GITHUB_PATH is required}"
