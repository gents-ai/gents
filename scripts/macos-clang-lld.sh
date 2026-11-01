#!/usr/bin/env bash
set -euo pipefail

# Prefer lld for macOS host links because it is much faster than Apple's ld.
# An lld build can lag behind a new SDK: Xcode 27's MacOSX27.0.sdk TBDs list
# `arm64e.x1-macos` targets, and rust-lld 1.99 rejects them with
# "could not load TAPI file ... malformed file", so nothing links. Before using
# an lld, link a trivial program against the active SDK once and cache the
# result per lld binary and SDK; when the probe fails, use Apple's linker.

lld_links_sdk() {
  local lld_dir="$1" sdk key cache probe status
  sdk="$(xcrun --show-sdk-path 2>/dev/null)" || return 1
  key="$(printf '%s|%s|%s' "$(realpath "$lld_dir/ld64.lld")" "$(realpath "$sdk")" \
    "$(stat -f %m "$sdk/usr/lib/libSystem.tbd" 2>/dev/null)" | cksum | tr ' ' '-')"
  cache="${TMPDIR:-/tmp}/gents-macos-lld-probe/$key"
  if [ -f "$cache" ]; then
    [ "$(cat "$cache")" = ok ]
    return
  fi
  probe="$(mktemp -d)"
  printf 'int main(void) { return 0; }\n' >"$probe/probe.c"
  status=fail
  if PATH="$lld_dir:$PATH" clang -fuse-ld=lld "$probe/probe.c" -o "$probe/probe" >/dev/null 2>&1; then
    status=ok
  fi
  rm -rf "$probe"
  mkdir -p "${cache%/*}"
  printf '%s\n' "$status" >"$cache.$$" && mv -f "$cache.$$" "$cache"
  [ "$status" = ok ]
}

use_lld() {
  local lld_dir="$1"
  shift
  if lld_links_sdk "$lld_dir"; then
    export PATH="$lld_dir:$PATH"
    exec clang -fuse-ld=lld "$@"
  fi
}

if lld_path="$(command -v ld64.lld)"; then
  use_lld "${lld_path%/*}" "$@"
fi

for llvm_bin in /opt/homebrew/opt/llvm/bin /usr/local/opt/llvm/bin; do
  if [ -x "$llvm_bin/ld64.lld" ]; then
    use_lld "$llvm_bin" "$@"
  fi
done

rust_sysroot="$(rustc --print sysroot)"
rust_host="$(rustc -vV | sed -n 's/^host: //p')"
rust_lld_dir="$rust_sysroot/lib/rustlib/$rust_host/bin/gcc-ld"
if [ -x "$rust_lld_dir/ld64.lld" ]; then
  use_lld "$rust_lld_dir" "$@"
fi

exec clang "$@"
