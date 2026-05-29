#!/usr/bin/env bash
set -euo pipefail

if command -v sccache >/dev/null 2>&1; then
  sccache --show-stats || true
fi

if [[ -n "${SCCACHE_ERROR_LOG:-}" && -f "${SCCACHE_ERROR_LOG}" ]]; then
  echo "::group::sccache error log"
  cat "${SCCACHE_ERROR_LOG}"
  echo "::endgroup::"
fi
