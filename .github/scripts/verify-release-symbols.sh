#!/usr/bin/env bash
set -euo pipefail

binary_path="${1:-}"
if [[ -z "${binary_path}" || ! -f "${binary_path}" ]]; then
  echo "usage: $0 <release-binary>" >&2
  exit 2
fi
if ! command -v nm >/dev/null 2>&1; then
  echo "::error::nm is required to verify release symbols" >&2
  exit 1
fi

symbols_file="$(mktemp)"
trap 'rm -f "${symbols_file}"' EXIT
nm "${binary_path}" > "${symbols_file}"

# Rust's mangled names retain the crate path on both Mach-O and ELF. Requiring
# an in-repo crate name catches a fully stripped artifact without depending on
# a particular Rust symbol-mangling version or an optional demangler.
resolved_symbol="$(grep -E -m 1 'defra_agent(_cli)?' "${symbols_file}" || true)"
if [[ -z "${resolved_symbol}" ]]; then
  echo "::error::${binary_path} contains no defra-agent Rust symbols; production samples will not resolve in-binary frames" >&2
  exit 1
fi

printf 'release symbol check: %s\n' "${resolved_symbol}"
