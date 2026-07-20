#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
cd "$REPO_ROOT"
# =============================================================================
# rename-to-gents.sh — Executable rename tool for the defra-agent → Gents
# hard cutover (source-inc/defra-agent#822, epic #811).
#
# Modes:
#   audit          — report paths and contents deterministically (read-only)
#   apply-moves    — perform only the reviewable git-mv phase
#   apply-content  — perform only scripted content substitutions
#   apply          — perform both mechanical phases
#   guard          — reject stale path/content tokens outside the allowlist
#   self-test      — exercise the substitution edge cases in a temporary file
#
# Usage:
#   scripts/rename-to-gents.sh audit
#   scripts/rename-to-gents.sh apply-moves
#   scripts/rename-to-gents.sh apply-content
#   scripts/rename-to-gents.sh guard
#
# Design:
#   - Audit mode is read-only and prints affected-file + occurrence summaries.
#   - Apply mode performs git mv on paths and sed on file contents.
#   - Re-running apply is safe (no-op on already-renamed files).
#   - Scans both paths and contents.
#   - Explicitly scans old product tokens, both GitHub org coordinates,
#     SSH/HTTPS repository forms, and com.sourcenetwork / org.sourcenetwork.
#   - Preserves DefraDB/defradb.rs and domain-level agent vocabulary.
#
# Three-part review commit structure (documented for reviewers):
#   1. git mv only (directory/crate renames)
#   2. Scripted content substitutions (this script's apply mode)
#   3. Hand fixes, generated-file/lockfile regeneration, formatting
#
# Allowlist — old product names may remain only in:
#   - docs/gents-cutover.md
#   - scripts/rename-to-gents.sh (this file)
#   - narrowly justified machine-required references recorded in the guard
# =============================================================================
# ---------------------------------------------------------------------------
# Locked rename map (from #811 section "Locked naming contract")
# ---------------------------------------------------------------------------

# Crate directory renames (git mv)
declare -a CRATE_MV=(
  "crates/defra-agent:crates/gents"
  "crates/defra-agent-cli:crates/gents-cli"
  "crates/defra-agent-protocol:crates/gents-protocol"
  "crates/defra-agent-schemas:crates/gents-schemas"
  "crates/defra-agent-desktop:crates/gents-desktop"
  "crates/defra-agent-desktop-core:crates/gents-desktop-core"
  "crates/defra-agent-lean-contract:crates/gents-lean-contract"
  "crates/defra-agent-lenses:crates/gents-lenses"
  "crates/defra-native-fs-runner:crates/gents-fs-runner"
)

# Content substitutions — applied in order from most specific to most general.
# Each entry is "old|new". Order matters: longer/more-specific patterns first.
declare -a CONTENT_SUBS=(
  # Reverse-domain identifiers (most specific first — must run before
  # generic defra-agent rules to avoid partial matches)
  "com.sourcenetwork.defra-agent-desktop|com.source-inc.gents"
  "org.sourcenetwork.defra-agent|com.source-inc.gents.cli"
  "com.sourcenetwork|com.source-inc.gents"
  "org.sourcenetwork|com.source-inc.gents"
  # Repository coordinates — both org forms
  "git@github.com:sourcenetwork/defra-agent|git@github.com:source-inc/gents"
  "git@github.com:source-inc/defra-agent|git@github.com:source-inc/gents"
  "github.com/sourcenetwork/defra-agent|github.com/source-inc/gents"
  "github.com/source-inc/defra-agent|github.com/source-inc/gents"
  "sourcenetwork/defra-agent|source-inc/gents"
  "source-inc/defra-agent|source-inc/gents"
  # Product crate prefix (most specific first)
  "defra-agent-desktop-core|gents-desktop-core"
  "defra-agent-desktop|gents-desktop"
  "defra-agent-lean-contract|gents-lean-contract"
  "defra-agent-lenses|gents-lenses"
  "defra-agent-protocol|gents-protocol"
  "defra-agent-schemas|gents-schemas"
  "defra-agent-cli|gents-cli"
  "defra-native-fs-runner|gents-fs-runner"
  "defra_native_fs_runner|gents_fs_runner"
  "defra_agent_fs_runner|gents_fs_runner"
  # Also handle the defra_agent::fs_runner path form if it exists
  "defra_agent_fs|gents_fs"
  # Runtime crate/path/import (Rust module form)
  "defra_agent|gents"
  # Environment variable prefix
  "DEFRA_AGENT|GENTS"
  # Default home directory (literal dot, not regex)
  ".defra-agent|.gents"
  # Display/product name (proper case)
  "Defra Agent|Gents"
  "DefraAgent|Gents"
  # CLI executable / product name (kebab-case) — catch-all, run last
  "defra-agent|gents"
  # NOTE: did:defra-agent:* is NOT mechanically substituted.
  # Per #811: string-only fixtures → did:test:*; real crypto → did:key.
  # This is a hand-fix item for #823, not a scripted substitution.
  # The guard still scans for did:defra-agent as a stale token.
)

# Stale tokens for the guard scan (from #811 section 1)
declare -a STALE_TOKENS=(
  "defra-agent"
  "defra_agent"
  "DEFRA_AGENT"
  "Defra Agent"
  "DefraAgent"
  "defra-native-fs-runner"
  "defra_native_fs_runner"
  ".defra-agent"
  "did:defra-agent"
  "sourcenetwork/defra-agent"
  "source-inc/defra-agent"
  "github.com/sourcenetwork/defra-agent"
  "github.com/source-inc/defra-agent"
  "git@github.com:sourcenetwork/defra-agent"
  "git@github.com:source-inc/defra-agent"
  "com.sourcenetwork"
  "org.sourcenetwork"
  "com.sourcenetwork.defra-agent-desktop"
  "org.sourcenetwork.defra-agent"
)

# Guard allowlist — files where old names are permitted to remain
declare -a ALLOWLIST=(
  "docs/gents-cutover.md"
  "scripts/rename-to-gents.sh"
)

# Domain vocabulary to preserve (NOT substituted)
# These are explicitly listed for clarity; the script does not touch them.
# - defradb / defradb.rs
# - defra-core, defra-node, defra-p2p-adapter (upstream DefraDB crates)
# - agent_did, AgentRequest, AgentResponse, AgentToolCall (domain nouns)
# - agent-tool-call-lifecycle-v1-to-v2-lens (lens name = domain vocabulary)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

DID_SENTINEL="__GENTS_PRESERVE_PRODUCT_DID__"

is_allowlisted() {
  local file="$1"
  local allowed
  for allowed in "${ALLOWLIST[@]}"; do
    if [[ "$file" == "$allowed" ]]; then
      return 0
    fi
  done
  return 1
}

literal_occurrences_in_string() {
  local value="$1"
  local token="$2"
  local count=0
  while [[ "$value" == *"$token"* ]]; do
    value="${value#*"$token"}"
    count=$((count + 1))
  done
  printf '%d\n' "$count"
}

content_occurrences() {
  local token="$1"
  local file="$2"
  if [[ -L "$file" ]]; then
    literal_occurrences_in_string "$(readlink "$file")" "$token"
  elif [[ -f "$file" ]]; then
    { grep -IFo -- "$token" "$file" 2>/dev/null || true; } | wc -l | tr -d ' '
  else
    printf '0\n'
  fi
}

sed_inplace() {
  local file="$1"
  shift
  if [[ "$(uname)" == "Darwin" ]]; then
    sed -i '' "$@" "$file"
  else
    sed -i "$@" "$file"
  fi
}

replace_token() {
  local file="$1"
  local old="$2"
  local new="$3"
  local old_escaped
  local new_escaped
  old_escaped=$(printf '%s\n' "$old" | sed 's/[][\\.^$*+?{}|\/&]/\\&/g')
  new_escaped=$(printf '%s\n' "$new" | sed 's/[\\\/&]/\\&/g')

  if [[ "$old" == "defra-agent" ]]; then
    sed_inplace "$file" \
      -e "s/did:defra-agent/$DID_SENTINEL/g" \
      -e "s/$old_escaped/$new_escaped/g" \
      -e "s/$DID_SENTINEL/did:defra-agent/g"
  else
    sed_inplace "$file" -e "s/$old_escaped/$new_escaped/g"
  fi
}

check_clean() {
  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "working tree and index must be clean before apply modes" >&2
    return 1
  fi
}

preflight_moves() {
  local pair old new
  local sources=0
  local destinations=0
  for pair in "${CRATE_MV[@]}"; do
    old="${pair%%:*}"
    new="${pair#*:}"
    [[ -d "$old" ]] && sources=$((sources + 1))
    [[ -d "$new" ]] && destinations=$((destinations + 1))
    if [[ -e "$old" && -e "$new" ]]; then
      echo "both rename source and destination exist: $old / $new" >&2
      return 1
    fi
  done
  if [[ "$sources" -ne "${#CRATE_MV[@]}" && "$destinations" -ne "${#CRATE_MV[@]}" ]]; then
    echo "mixed or incomplete crate-rename state" >&2
    return 1
  fi
}

preflight_content() {
  local file
  while IFS= read -r -d '' file; do
    is_allowlisted "$file" && continue
    [[ -L "$file" ]] && continue
    [[ -f "$file" ]] || continue
    if grep -Fq -- "$DID_SENTINEL" "$file" 2>/dev/null; then
      echo "DID sentinel collision in $file" >&2
      return 1
    fi
    if [[ ! -w "$file" ]]; then
      echo "tracked file is not writable: $file" >&2
      return 1
    fi
  done < <(git ls-files -z)
}

apply_moves() {
  preflight_moves
  local pair old new
  for pair in "${CRATE_MV[@]}"; do
    old="${pair%%:*}"
    new="${pair#*:}"
    if [[ -d "$old" ]]; then
      echo "git mv $old -> $new"
      git mv "$old" "$new"
    fi
  done
}

apply_content() {
  preflight_content
  local file sub old new
  local changed
  local count=0
  while IFS= read -r -d '' file; do
    is_allowlisted "$file" && continue
    [[ -L "$file" ]] && continue
    [[ -f "$file" ]] || continue
    grep -Iq . "$file" 2>/dev/null || continue
    changed=0
    for sub in "${CONTENT_SUBS[@]}"; do
      old="${sub%%|*}"
      new="${sub#*|}"
      if grep -Fq -- "$old" "$file"; then
        replace_token "$file" "$old" "$new"
        changed=1
      fi
    done
    if [[ "$changed" -eq 1 ]]; then
      git add "$file"
      count=$((count + 1))
    fi
  done < <(git ls-files -z)
  echo "content substitutions applied to $count tracked files"
}

print_move_audit() {
  local pair old new file
  local count total=0
  echo "--- Crate-directory moves ---"
  for pair in "${CRATE_MV[@]}"; do
    old="${pair%%:*}"
    new="${pair#*:}"
    count=0
    while IFS= read -r -d '' file; do
      count=$((count + 1))
    done < <(git ls-files -z "$old/")
    if [[ "$count" -gt 0 ]]; then
      printf '  %s -> %s (%d files)\n' "$old" "$new" "$count"
      total=$((total + count))
    fi
  done
  echo "  Total files under crate-directory moves: $total"
}

scan_stale_tokens() {
  local token file allowed link_target
  local path_count link_count path_occurrences content_occurrences symlink_occurrences
  local path_file_count content_file_count symlink_file_count
  local total=0
  local violations=0
  local -a path_files content_files symlink_files pathspecs

  pathspecs=(-- .)
  for allowed in "${ALLOWLIST[@]}"; do
    pathspecs+=(":(exclude)$allowed")
  done

  echo "--- Stale path and content tokens ---"
  for token in "${STALE_TOKENS[@]}"; do
    path_occurrences=0
    symlink_occurrences=0
    path_file_count=0
    symlink_file_count=0
    path_files=("")
    symlink_files=("")
    while IFS= read -r -d '' file; do
      is_allowlisted "$file" && continue
      path_count=$(literal_occurrences_in_string "$file" "$token")
      if [[ "$path_count" -gt 0 ]]; then
        path_files+=("$file")
        path_file_count=$((path_file_count + 1))
        path_occurrences=$((path_occurrences + path_count))
      fi
      if [[ -L "$file" ]]; then
        link_target=$(readlink "$file")
        link_count=$(literal_occurrences_in_string "$link_target" "$token")
        if [[ "$link_count" -gt 0 ]]; then
          symlink_files+=("$file")
          symlink_file_count=$((symlink_file_count + 1))
          symlink_occurrences=$((symlink_occurrences + link_count))
        fi
      fi
    done < <(git ls-files -z)

    content_file_count=0
    content_files=("")
    while IFS= read -r -d '' file; do
      content_files+=("$file")
      content_file_count=$((content_file_count + 1))
    done < <(git grep -I -l -z -F -e "$token" "${pathspecs[@]}" 2>/dev/null || true)
    content_occurrences=$(
      { git grep -I -o -F -e "$token" "${pathspecs[@]}" 2>/dev/null || true; } |
        wc -l | tr -d ' '
    )

    if [[ "$path_occurrences" -gt 0 || "$content_occurrences" -gt 0 || "$symlink_occurrences" -gt 0 ]]; then
      printf '  %-45s %4d path files %4d content files %6d occurrences\n' \
        "$token" "$path_file_count" "$((content_file_count + symlink_file_count))" \
        "$((path_occurrences + content_occurrences + symlink_occurrences))"
      for file in "${path_files[@]}"; do
        [[ -n "$file" ]] || continue
        printf '    path: %s\n' "$file"
      done
      for file in "${content_files[@]}"; do
        [[ -n "$file" ]] || continue
        printf '    content: %s\n' "$file"
      done
      for file in "${symlink_files[@]}"; do
        [[ -n "$file" ]] || continue
        printf '    symlink target: %s\n' "$file"
      done
      total=$((total + path_occurrences + content_occurrences + symlink_occurrences))
      violations=$((violations + 1))
    fi
  done

  SCAN_TOTAL="$total"
  SCAN_VIOLATIONS="$violations"
}

run_audit() {
  echo "=== Gents Rename Audit ==="
  print_move_audit
  scan_stale_tokens
  echo "Audit: $SCAN_VIOLATIONS token classes, $SCAN_TOTAL occurrences"
}

run_guard() {
  echo "=== Gents Rename Stale-Token Guard ==="
  scan_stale_tokens
  if [[ "$SCAN_VIOLATIONS" -eq 0 ]]; then
    echo "PASS: no stale path or content tokens outside the allowlist"
    return 0
  fi
  echo "FAIL: $SCAN_VIOLATIONS token classes, $SCAN_TOTAL occurrences"
  return 1
}

apply_substitutions_to_file() {
  local file="$1"
  local sub old new
  for sub in "${CONTENT_SUBS[@]}"; do
    old="${sub%%|*}"
    new="${sub#*|}"
    if grep -Fq -- "$old" "$file"; then
      replace_token "$file" "$old" "$new"
    fi
  done
}

self_test() {
  local test_dir test_file fixture_repo fixture_path first_checksum second_checksum
  test_dir=$(mktemp -d)
  test_file="$test_dir/substitutions.txt"
  trap 'rm -rf "$test_dir"' EXIT
  printf '%s\n' \
    'git@github.com:sourcenetwork/defra-agent' \
    'git@github.com:source-inc/defra-agent' \
    'did:defra-agent:test' \
    'DefraAgent' \
    'DEFRA_AGENT_HOME' >"$test_file"

  apply_substitutions_to_file "$test_file"
  grep -Fxq 'git@github.com:source-inc/gents' "$test_file"
  grep -Fxq 'did:defra-agent:test' "$test_file"
  grep -Fxq 'Gents' "$test_file"
  grep -Fxq 'GENTS_HOME' "$test_file"
  if grep -Fq -- "$DID_SENTINEL" "$test_file"; then
    echo "self-test left a DID sentinel behind" >&2
    return 1
  fi

  first_checksum=$(cksum "$test_file")
  apply_substitutions_to_file "$test_file"
  second_checksum=$(cksum "$test_file")
  [[ "$first_checksum" == "$second_checksum" ]]

  # Exercise the real tracked-path and symlink-target scanner. The embedded
  # newline ensures this fails if tracked paths are ever read line-by-line.
  fixture_repo="$test_dir/path-fixture"
  fixture_path=$'nested/defra-native-fs-runner\nfixture'
  mkdir -p "$fixture_repo/nested"
  git -C "$fixture_repo" init -q
  printf 'path fixture\n' >"$fixture_repo/$fixture_path"
  ln -s 'defra-native-fs-runner-target' "$fixture_repo/nested/runner-link"
  git -C "$fixture_repo" add -A
  cd "$fixture_repo"
  ALLOWLIST=("__no_fixture_allowlist__")
  STALE_TOKENS=("defra-native-fs-runner")
  scan_stale_tokens >/dev/null
  [[ "$SCAN_VIOLATIONS" -eq 1 ]]
  [[ "$SCAN_TOTAL" -eq 2 ]]
  cd "$REPO_ROOT"

  rm -rf "$test_dir"
  trap - EXIT
  echo "self-test PASS"
}

case "${1:-}" in
  audit)
    run_audit
    ;;
  guard)
    run_guard
    ;;
  apply-moves)
    check_clean
    apply_moves
    ;;
  apply-content)
    check_clean
    apply_content
    ;;
  apply)
    check_clean
    preflight_moves
    preflight_content
    apply_moves
    apply_content
    ;;
  self-test)
    self_test
    ;;
  *)
    echo "usage: $0 {audit|guard|apply-moves|apply-content|apply|self-test}" >&2
    exit 2
    ;;
esac
