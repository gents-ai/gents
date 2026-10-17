#!/usr/bin/env bash
set -euo pipefail

if command -v ld64.lld >/dev/null 2>&1; then
  exec clang -fuse-ld=lld "$@"
fi

for llvm_bin in /opt/homebrew/opt/llvm/bin /usr/local/opt/llvm/bin; do
  if [ -x "$llvm_bin/ld64.lld" ]; then
    export PATH="$llvm_bin:$PATH"
    exec clang -fuse-ld=lld "$@"
  fi
done

if command -v rustc >/dev/null 2>&1; then
  rust_target_libdir="$(rustc --print target-libdir 2>/dev/null)" || rust_target_libdir=""
  rust_llvm_bin="${rust_target_libdir%/lib}/bin/gcc-ld"
  if [ -x "$rust_llvm_bin/ld64.lld" ]; then
    export PATH="$rust_llvm_bin:$PATH"
    exec clang -fuse-ld=lld "$@"
  fi
fi

exec clang "$@"
