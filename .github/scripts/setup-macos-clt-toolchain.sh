#!/usr/bin/env bash
set -euo pipefail

# Studio runners select Xcode-beta via `xcode-select`, but a Command Line Tools
# update repointed the default SDK to MacOSX27.2.sdk. Xcode-beta 26.4 ships
# ld-1266.5, which cannot read that SDK: every link that resolves the default
# SDK dies with
#   ld: tapi error: malformed file
#   .../MacOSX27.2.sdk/usr/lib/libSystem.B.tbd:4:20: error: unknown architecture
# Rust and test jobs need no Xcode, so select the CLT toolchain explicitly.
# That fixes both halves of the failure: cc-rs crates (which ask
# `xcrun --show-sdk-path`) and the C fixtures the tests spawn with /usr/bin/cc,
# which inherit this environment.
#
# A job that needs Xcode (xcodebuild, or an iOS SDK that ships only with Xcode)
# must `unset DEVELOPER_DIR SDKROOT` in that step.

clt_dir="${GENTS_CLT_DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"
probe_cc="${GENTS_CLT_PROBE_CC:-/usr/bin/cc}"

if [[ ! -d "${clt_dir}" ]]; then
  echo "::error::Command Line Tools not found at ${clt_dir}."
  exit 1
fi
if [[ ! -x "${clt_dir}/usr/bin/clang" ]]; then
  echo "::error::${clt_dir}/usr/bin/clang is missing; cannot select the CLT toolchain."
  exit 1
fi
if ! command -v xcrun >/dev/null 2>&1; then
  echo "::error::xcrun is missing; cannot resolve the CLT macOS SDK."
  exit 1
fi

export DEVELOPER_DIR="${clt_dir}"
sdk_path="$(DEVELOPER_DIR="${clt_dir}" xcrun --sdk macosx --show-sdk-path)"
if [[ -z "${sdk_path}" || ! -d "${sdk_path}" ]]; then
  echo "::error::xcrun reported no macOS SDK under ${clt_dir} (got '${sdk_path}')."
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
  echo "::error::${probe_cc} cannot link with DEVELOPER_DIR=${clt_dir} SDKROOT=${sdk_path}:"
  printf '%s\n' "${probe_output}"
  exit 1
fi

echo "Selected CLT toolchain: DEVELOPER_DIR=${clt_dir} SDKROOT=${sdk_path}"
if [[ -n "${GITHUB_ENV:-}" ]]; then
  {
    echo "DEVELOPER_DIR=${clt_dir}"
    echo "SDKROOT=${sdk_path}"
  } >> "${GITHUB_ENV}"
fi
