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

rust_sysroot="$(rustc --print sysroot)"
rust_host="$(rustc -vV | sed -n 's/^host: //p')"
rust_lld_dir="$rust_sysroot/lib/rustlib/$rust_host/bin/gcc-ld"
if [ -x "$rust_lld_dir/ld64.lld" ]; then
  export PATH="$rust_lld_dir:$PATH"
  exec clang -fuse-ld=lld "$@"
fi

exec clang "$@"
