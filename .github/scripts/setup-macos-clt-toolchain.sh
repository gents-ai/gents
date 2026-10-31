#!/usr/bin/env bash
set -euo pipefail

# A Command Line Tools update repointed the default SDK to MacOSX27.2.sdk.
# Two different SDK resolutions disagree about it, and both fail:
#
#   xcrun --show-sdk-path                -> CLT 27.2 SDK   (what cc-rs asks for)
#   xcrun --sdk macosx --show-sdk-path   -> Xcode-beta 26.4 SDK
#
# The 27.2 SDK's libSystem.B.tbd declares arm64e.x1-macos targets that neither
# linker on the runner understands:
#   - Xcode-beta 26.4's ld-1266.5: `ld: tapi error: malformed file`
#   - rust-lld (forced by .cargo/config.toml via scripts/macos-clang-lld.sh):
#     `rust-lld: error: could not load TAPI file ... unknown target`
#
# So the SDK must match the linker that actually runs, not the newest one on
# disk. Pin SDKROOT to the macOS SDK of the selected developer directory
# (`xcode-select -p`, i.e. the toolchain the linker comes from) and export it,
# so cc-rs crates and the C fixtures the tests spawn with /usr/bin/cc all
# resolve the same readable SDK.
#
# A job that needs a different toolchain (xcodebuild, or the Xcode-only iOS
# SDK) must `unset DEVELOPER_DIR SDKROOT` in that step.

developer_dir="${GENTS_DEVELOPER_DIR:-}"
if [[ -z "${developer_dir}" ]]; then
  if ! command -v xcode-select >/dev/null 2>&1; then
    echo "::error::xcode-select is missing; cannot resolve the active developer directory."
    exit 1
  fi
  developer_dir="$(xcode-select -p)"
fi
probe_cc="${GENTS_PROBE_CC:-/usr/bin/cc}"

if [[ -z "${developer_dir}" || ! -d "${developer_dir}" ]]; then
  echo "::error::Developer directory not found: '${developer_dir}'."
  exit 1
fi
if ! command -v xcrun >/dev/null 2>&1; then
  echo "::error::xcrun is missing; cannot resolve the macOS SDK."
  exit 1
fi

export DEVELOPER_DIR="${developer_dir}"
sdk_path="$(DEVELOPER_DIR="${developer_dir}" xcrun --sdk macosx --show-sdk-path)"
if [[ -z "${sdk_path}" || ! -d "${sdk_path}" ]]; then
  echo "::error::xcrun reported no macOS SDK under ${developer_dir} (got '${sdk_path}')."
  exit 1
fi
export SDKROOT="${sdk_path}"

# The failing tests compile a C fixture with /usr/bin/cc and inherit this
# environment. Prove that exact path links here, so a broken toolchain fails in
# seconds with the compiler's own words instead of 17 minutes into a suite.
probe_dir="$(mktemp -d)"
trap 'rm -rf "${probe_dir}"' EXIT
printf 'int main(void) { return 0; }\n' > "${probe_dir}/probe.c"
if ! probe_output="$("${probe_cc}" "${probe_dir}/probe.c" -o "${probe_dir}/probe" 2>&1)"; then
  echo "::error::${probe_cc} cannot link with DEVELOPER_DIR=${developer_dir} SDKROOT=${sdk_path}:"
  printf '%s\n' "${probe_output}"
  exit 1
fi

echo "Selected toolchain: DEVELOPER_DIR=${developer_dir} SDKROOT=${sdk_path}"
if [[ -n "${GITHUB_ENV:-}" ]]; then
  {
    echo "DEVELOPER_DIR=${developer_dir}"
    echo "SDKROOT=${sdk_path}"
  } >> "${GITHUB_ENV}"
fi
