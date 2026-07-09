#!/usr/bin/env bash
set -euo pipefail

# Linux linker wrapper that drives the system `cc` with a fast LLD linker.
#
# Mirrors scripts/macos-clang-lld.sh. Link time dominates incremental rebuilds
# of the large workspace binaries (defra-agent-cli, the Tauri app), and GNU
# `ld.bfd` — the default — is the slowest option. We prefer, in order:
#   1. a system `mold` (fastest),
#   2. a system `lld`,
#   3. the `rust-lld` bundled with the active toolchain (always present, so
#      this needs zero extra install).
# Falls back to plain `cc` (GNU ld) if none are usable.

if command -v mold >/dev/null 2>&1; then
  exec cc -fuse-ld=mold "$@"
fi

if command -v ld.lld >/dev/null 2>&1; then
  exec cc -fuse-ld=lld "$@"
fi

# Bundled rust-lld lives in the toolchain under
# lib/rustlib/<host>/bin/gcc-ld/ as `ld.lld`. Point the `cc` driver at that
# directory with -B so `-fuse-ld=lld` resolves to it.
sysroot="$(rustc --print sysroot 2>/dev/null || true)"
if [ -n "$sysroot" ]; then
  for gccld in "$sysroot"/lib/rustlib/*/bin/gcc-ld; do
    if [ -x "$gccld/ld.lld" ]; then
      exec cc -fuse-ld=lld -B"$gccld" "$@"
    fi
  done
fi

exec cc "$@"
