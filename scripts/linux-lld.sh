#!/usr/bin/env bash
set -euo pipefail


if command -v mold >/dev/null 2>&1; then
  exec cc -fuse-ld=mold "$@"
fi

if command -v ld.lld >/dev/null 2>&1; then
  exec cc -fuse-ld=lld "$@"
fi

sysroot="$(rustc --print sysroot 2>/dev/null || true)"
if [ -n "$sysroot" ]; then
  for gccld in "$sysroot"/lib/rustlib/*/bin/gcc-ld; do
    if [ -x "$gccld/ld.lld" ]; then
      exec cc -fuse-ld=lld -B"$gccld" "$@"
    fi
  done
fi

exec cc "$@"
