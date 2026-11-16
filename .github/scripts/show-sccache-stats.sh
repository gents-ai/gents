#!/usr/bin/env bash
set -euo pipefail

# The setup step points RUSTC_WRAPPER at the sccache that speaks to this
# host's server; an older one on PATH would start a server of its own.
sccache_bin="sccache"
if [[ "${RUSTC_WRAPPER:-}" == /* && "$(basename "${RUSTC_WRAPPER}")" == sccache ]]; then
  sccache_bin="${RUSTC_WRAPPER}"
fi
if command -v "${sccache_bin}" >/dev/null 2>&1; then
  "${sccache_bin}" --show-stats || true
fi

if [[ -n "${SCCACHE_ERROR_LOG:-}" && -f "${SCCACHE_ERROR_LOG}" ]]; then
  echo "::group::sccache error log"
  cat "${SCCACHE_ERROR_LOG}"
  echo "::endgroup::"
fi
