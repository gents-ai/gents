#!/usr/bin/env bash
set -euo pipefail

# The compile script owns both the commands and their package assignments, so
# this guard cannot drift away from what CI actually invokes.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
covered_packages="$({
  for suite in runtime cli desktop support; do
    "${script_dir}/compile-rust-ci-suite.sh" "${suite}" --list-packages
  done
} | sed '/^$/d' | sort -u)"

workspace_packages="$(
  cargo metadata --locked --no-deps --format-version 1 |
    jq -r '.workspace_members[] as $member | .packages[] | select(.id == $member) | .name' |
    sort -u
)"

if [[ "${covered_packages}" != "${workspace_packages}" ]]; then
  echo "::error::The rust-and-cli suite package lists do not cover the current Cargo workspace."
  diff -u \
    <(printf '%s\n' "${covered_packages}") \
    <(printf '%s\n' "${workspace_packages}") || true
  echo "Assign every added package in compile-rust-ci-suite.sh."
  exit 1
fi

echo "Rust CI covers all $(wc -l <<< "${workspace_packages}" | tr -d ' ') workspace packages."
