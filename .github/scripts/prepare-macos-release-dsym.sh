#!/usr/bin/env bash
set -euo pipefail

binary_path="${1:-}"
dsym_path="${2:-}"
if [[ -z "${binary_path}" || ! -f "${binary_path}" || -z "${dsym_path}" ]]; then
  echo "usage: $0 <unstripped-release-binary> <output.dSYM>" >&2
  exit 2
fi
if [[ "${dsym_path}" != *.dSYM ]]; then
  echo "::error::dSYM output path must end in .dSYM: ${dsym_path}" >&2
  exit 2
fi
for command in codesign nm strip xcrun; do
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "::error::${command} is required to prepare macOS release symbols" >&2
    exit 1
  fi
done

symbols_file="$(mktemp)"
lookup_file="$(mktemp)"
dsymutil_log="$(mktemp)"
trap 'rm -f "${symbols_file}" "${lookup_file}" "${dsymutil_log}"' EXIT

# dsymutil needs the linker's local symbol table to resolve the debug map back
# to object-file DWARF. Refuse a Cargo configuration that stripped too early.
nm "${binary_path}" > "${symbols_file}"
gents_rust_symbol_regex='(_ZN(5gents|9gents_cli)|_R[A-Za-z0-9_.$]*_(5gents|9gents_cli))'
if ! grep -Eq "${gents_rust_symbol_regex}" "${symbols_file}"; then
  echo "::error::${binary_path} has no gents Rust symbols; build with CARGO_PROFILE_RELEASE_STRIP=false before running dsymutil" >&2
  exit 1
fi
probe_addresses="$(awk -v symbol_regex="${gents_rust_symbol_regex}" '
  $0 ~ symbol_regex && $2 ~ /^[tT]$/ {
    print $1
    count++
    if (count == 64) exit
  }
' "${symbols_file}")"
if [[ -z "${probe_addresses}" ]]; then
  echo "::error::${binary_path} has no gents text symbols suitable for a symbolication probe" >&2
  exit 1
fi

rm -rf "${dsym_path}"
if ! xcrun dsymutil --verify-dwarf=output "${binary_path}" -o "${dsym_path}" 2> "${dsymutil_log}"; then
  sed -n '1,200p' "${dsymutil_log}" >&2
  exit 1
fi
dsymutil_warning_count="$(wc -l < "${dsymutil_log}" | tr -d ' ')"
if [[ "${dsymutil_warning_count}" -gt 0 ]]; then
  echo "::warning::dsymutil emitted ${dsymutil_warning_count} warning lines; showing the first 20"
  sed -n '1,20p' "${dsymutil_log}" >&2
fi

dwarf_path="${dsym_path}/Contents/Resources/DWARF/$(basename "${binary_path}")"
if [[ ! -s "${dwarf_path}" ]]; then
  echo "::error::dsymutil did not create ${dwarf_path}" >&2
  exit 1
fi
xcrun dwarfdump --verify --quiet "${dsym_path}"
xcrun dwarfdump --name 'crates/gents-cli/src/main.rs/@/.*' --regex "${dsym_path}" > "${lookup_file}"
if ! grep -q 'crates/gents-cli/src/main.rs' "${lookup_file}"; then
  echo "::error::${dsym_path} does not contain the gents CLI compilation unit" >&2
  exit 1
fi

# Strip the install binary back to its normal release footprint. Re-sign
# ad-hoc so unsigned workflow dry-runs can launch it; signed releases replace
# this signature with the Developer ID signature in the following step.
codesign --remove-signature "${binary_path}" 2>/dev/null || true
/usr/bin/strip "${binary_path}"
codesign --force --sign - "${binary_path}"

binary_uuid="$(xcrun dwarfdump --uuid "${binary_path}" | awk '{ print $2, $3 }')"
dsym_uuid="$(xcrun dwarfdump --uuid "${dsym_path}" | awk '{ print $2, $3 }')"
if [[ -z "${binary_uuid}" || "${binary_uuid}" != "${dsym_uuid}" ]]; then
  echo "::error::binary/dSYM UUID mismatch: binary='${binary_uuid}' dSYM='${dsym_uuid}'" >&2
  exit 1
fi
binary_arch="$(awk '{ value=$2; gsub(/[()]/, "", value); print value }' <<< "${binary_uuid}")"
probe_address=""
resolved_symbol=""
# Linker deduplication can make a valid Rust text symbol resolve only as
# `<deduplicated_symbol>`. Probe a bounded candidate set for a real frame.
while IFS= read -r candidate_address; do
  [[ -n "${candidate_address}" ]] || continue
  candidate_symbol="$(
    xcrun atos -o "${dwarf_path}" -arch "${binary_arch}" "${candidate_address}" 2>&1 || true
  )"
  if [[ ( "${candidate_symbol}" == *"gents::"* || "${candidate_symbol}" == *"gents_cli::"* ) && "${candidate_symbol}" != *"<deduplicated_symbol>"* ]]; then
    probe_address="${candidate_address}"
    resolved_symbol="${candidate_symbol}"
    break
  fi
done <<< "${probe_addresses}"
if [[ -z "${probe_address}" ]]; then
  echo "::error::dSYM failed to resolve a gents frame from the first 64 text-symbol candidates" >&2
  exit 1
fi

nm "${binary_path}" > "${symbols_file}"
if grep -Eq "${gents_rust_symbol_regex}" "${symbols_file}"; then
  echo "::error::${binary_path} still contains local gents Rust symbols after strip" >&2
  exit 1
fi

printf 'macOS release dSYM: %s (%s)\n' "${dsym_path}" "${dsym_uuid}"
printf 'dSYM symbolication probe: %s -> %s\n' "${probe_address}" "${resolved_symbol}"
printf 'stripped release binary: %s bytes\n' "$(stat -f '%z' "${binary_path}")"
