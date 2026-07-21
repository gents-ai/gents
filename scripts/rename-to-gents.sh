#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
cd "$REPO_ROOT"
# =============================================================================
# rename-to-gents.sh — Executable rename tool for the defra-agent → Gents
# hard cutover (source-inc/defra-agent#822, epic #811).
#
# Modes (all repository modes require a slice):
#   audit SLICE          — report paths and contents deterministically (read-only)
#   apply-moves SLICE    — perform only the reviewable git-mv phase
#   apply-content SLICE  — perform only scripted content substitutions
#   apply SLICE          — perform both mechanical phases
#   guard SLICE          — reject stale path/content tokens outside the allowlist
#   self-test            — exercise slices and scanner edge cases in fixtures
#
# Slices are core, desktop, release, docs, and all. The all slice always runs
# the four owned slices in that order. This lets #823–#826 use the same locked
# map without a later issue mechanically rewriting an earlier issue's files.
#
# Usage:
#   scripts/rename-to-gents.sh audit core
#   scripts/rename-to-gents.sh apply-moves desktop
#   scripts/rename-to-gents.sh apply-content release
#   scripts/rename-to-gents.sh guard all
#
# Design:
#   - Audit mode is read-only and prints affected-file + occurrence summaries.
#   - Apply mode performs only the selected slice's git mv and substitutions.
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
# Guard allowlist — old product names may remain only in:
#   - docs/gents-cutover.md
#   - scripts/rename-to-gents.sh (this file)
#   - narrowly justified machine-required references recorded in the guard
#
# Protected apply paths are a separate concept. docs/gents.md and generated
# Apple internals are never edited by this script. Cargo.lock is regenerated
# from the slice-correct manifests instead of being text-substituted. All three
# remain visible to their slice/final guards until their owners resolve them.
# =============================================================================
# ---------------------------------------------------------------------------
# Locked rename map (from #811 section "Locked naming contract")
# ---------------------------------------------------------------------------

# Core path renames (#823). The product-branded Lean file is handled after the
# runtime root moves so its source exists at the new root.
declare -a CORE_MV=(
  "crates/defra-agent:crates/gents"
  "crates/defra-agent-cli:crates/gents-cli"
  "crates/defra-agent-protocol:crates/gents-protocol"
  "crates/defra-agent-schemas:crates/gents-schemas"
  "crates/defra-agent-lean-contract:crates/gents-lean-contract"
  "crates/defra-agent-lenses:crates/gents-lenses"
  "crates/defra-native-fs-runner:crates/gents-fs-runner"
)
CORE_LEAN_OLD_BEFORE="crates/defra-agent/proofs/Proofs/Conformance/DefraAgent.lean"
CORE_LEAN_OLD_AFTER="crates/gents/proofs/Proofs/Conformance/DefraAgent.lean"
CORE_LEAN_NEW="crates/gents/proofs/Proofs/Conformance/Gents.lean"

# Desktop path renames (#824). Generated Apple internals beneath the app root
# move with their parent but are deliberately excluded from content edits.
declare -a DESKTOP_MV=(
  "crates/defra-agent-desktop:crates/gents-desktop"
  "crates/defra-agent-desktop-core:crates/gents-desktop-core"
  "apps/desktop-tauri:apps/gents-desktop"
)

# Release path renames (#825).
declare -a RELEASE_MV=(
  "scripts/enable-defra-agent-runner-session.sh:scripts/enable-gents-runner-session.sh"
)

declare -a SLICE_ORDER=(core desktop release docs)

# Content substitutions — applied in order from most specific to most general.
# Each entry is "old|new". Order matters: longer/more-specific patterns first.
declare -a CONTENT_SUBS=(
  # Exact identity/platform identifiers. Do not add broad com.sourcenetwork or
  # org.sourcenetwork substitutions: unknown identifiers require a hand fix.
  "defra-agent.identity|com.source-inc.gents.identity"
  "com.sourcenetwork.defra-agent-desktop|com.source-inc.gents"
  "__APPLE_TEAM_ID__.org.sourcenetwork.defra-agent|__APPLE_TEAM_ID__.com.source-inc.gents"
  "org.sourcenetwork.defra-agent|com.source-inc.gents.cli"
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

# #823 must update direct consumers of renamed core crates even where those
# consumers are owned by desktop or release. Keep this list exact: it must not
# rewrite desktop branding, release artifacts, or documentation ahead of their
# slices. Remaining semantic/selector fixes belong in the hand-fix commit.
declare -a CORE_CONSUMER_SUBS=(
  'defra-agent = { path = "../defra-agent" }|gents = { path = "../gents" }'
  'defra-agent = { path = "../../../crates/defra-agent" }|gents = { path = "../../../crates/gents" }'
  "defra-agent-protocol|gents-protocol"
  "defra_agent_protocol|gents_protocol"
  "defra-agent-schemas|gents-schemas"
  "defra_agent_schemas|gents_schemas"
  "defra-agent-lean-contract|gents-lean-contract"
  "defra_agent_lean_contract|gents_lean_contract"
  "defra-agent-lenses|gents-lenses"
  "defra_agent_lenses|gents_lenses"
  "defra-native-fs-runner|gents-fs-runner"
  "defra_native_fs_runner|gents_fs_runner"
  "/defra-agent/src/|/gents/src/"
  "crates/defra-agent/src/|crates/gents/src/"
  "defra_agent::|gents::"
  "crates/defra-agent/proofs|crates/gents/proofs"
)

# #824 has the same narrow cross-slice obligation for consumers of the renamed
# desktop paths and packages. Keep both the substitutions and file list exact:
# release artifact/signing names and general documentation remain owned by
# #825/#826 even when they contain broader old-product tokens.
declare -a DESKTOP_CONSUMER_SUBS=(
  "apps/desktop-tauri|apps/gents-desktop"
  "crates/defra-agent-desktop-core|crates/gents-desktop-core"
  "crates/defra-agent-desktop|crates/gents-desktop"
  "defra-agent-desktop-tauri|gents-desktop-tauri"
  "defra_agent_desktop_tauri_lib|gents_desktop_tauri_lib"
  "defra_agent_desktop_core|gents_desktop_core"
  "DEFRA_AGENT_TAURI|GENTS_TAURI"
  "DEFRA_AGENT_DESKTOP|GENTS_DESKTOP"
  'app: "desktop-tauri"|app: "gents-desktop"'
)

declare -a DESKTOP_CONSUMER_FILES=(
  ".github/workflows/ci.yml"
  ".github/workflows/desktop-ui-qa.yml"
  ".github/workflows/live-smoke.yml"
  "Makefile"
  "scripts/install-local.sh"
  "crates/gents/tests/support/conformance_consumers.rs"
  "crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean"
)

# Root workspace files are shared across slices, so they cannot receive the
# catch-all substitution map. These exact entries keep every intermediate
# workspace valid: #823 rewrites only core members/dependencies, then #824
# rewrites only desktop members/dependencies.
declare -a CORE_WORKSPACE_SUBS=(
  '"crates/defra-agent"|"crates/gents"'
  '"crates/defra-agent-cli"|"crates/gents-cli"'
  '"crates/defra-agent-lean-contract"|"crates/gents-lean-contract"'
  '"crates/defra-agent-lenses/|"crates/gents-lenses/'
  '"crates/defra-native-fs-runner"|"crates/gents-fs-runner"'
  '"crates/defra-agent-protocol"|"crates/gents-protocol"'
  '"crates/defra-agent-schemas"|"crates/gents-schemas"'
  "defra-agent-protocol =|gents-protocol ="
  "defra-agent-schemas =|gents-schemas ="
  "defra-agent-lean-contract =|gents-lean-contract ="
  "https://github.com/sourcenetwork/defra-agent|https://github.com/source-inc/gents"
  "https://github.com/source-inc/defra-agent|https://github.com/source-inc/gents"
  "defra-agent#|gents#"
)

declare -a DESKTOP_WORKSPACE_SUBS=(
  '"apps/desktop-tauri/src-tauri"|"apps/gents-desktop/src-tauri"'
  '"crates/defra-agent-desktop"|"crates/gents-desktop"'
  '"crates/defra-agent-desktop-core"|"crates/gents-desktop-core"'
  "defra-agent-desktop-core =|gents-desktop-core ="
)

# Cargo.lock is never rewritten mechanically. These exact old entries let a
# slice guard verify that `cargo generate-lockfile`/Cargo regeneration happened
# without mistaking still-deferred desktop packages for stale core packages.
declare -a CORE_LOCK_SUBS=(
  'name = "defra-agent"|name = "gents"'
  'name = "defra-agent-cli"|name = "gents-cli"'
  'name = "defra-agent-lean-contract"|name = "gents-lean-contract"'
  'name = "defra-agent-protocol"|name = "gents-protocol"'
  'name = "defra-agent-schemas"|name = "gents-schemas"'
  'name = "defra-native-fs-runner"|name = "gents-fs-runner"'
  '"defra-agent",|"gents",'
  '"defra-agent-lean-contract",|"gents-lean-contract",'
  '"defra-agent-protocol",|"gents-protocol",'
  '"defra-agent-schemas",|"gents-schemas",'
  '"defra-native-fs-runner",|"gents-fs-runner",'
)

declare -a DESKTOP_LOCK_SUBS=(
  'name = "defra-agent-desktop"|name = "gents-desktop"'
  'name = "defra-agent-desktop-core"|name = "gents-desktop-core"'
  'name = "defra-agent-desktop-tauri"|name = "gents-desktop-tauri"'
  '"defra-agent-desktop-core",|"gents-desktop-core",'
)

# Stale tokens for the guard scan (from #811 section 1)
declare -a STALE_TOKENS=(
  "defra-agent"
  "apps/desktop-tauri"
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
  # Product-owned shorthand which cannot be covered by a broad bare-`defra`
  # rule without corrupting legitimate DefraDB vocabulary.
  "did:defra:"
  "defra_exec"
  "defra_fs"
  "DEFRA_NATIVE_FS_RUNNER"
  "DEFRA_CODEX_SHIM_TRACE"
  "DEFRA_CHATGPT_CODEX_CLIENT_VERSION"
  "DefraToolCallProgress"
  "DefraCompactionProgress"
  "DefraBackedWorkflow"
  "DefraLifecycleState"
  "DefraProcessState"
  "DefraInferenceCallState"
  "defra_tool_"
  "defra_turn_"
  '"defra"'
  "defra-shim"
  "/defra/skills"
  ".defra.eval-jsonl"
  ".defra.jsonl"
  ".defra.json"
  "defra-exports"
  "defra:req"
  "defra-chatgpt-codex"
  "defra-child-stream"
  "defra-message"
  "defra-owned"
  "defra-reasoning"
  "defra-user"
  # Additional product-owned internal/test/invoked-tooling shorthand found by
  # the frozen-tree preflight. Keep these exact so DefraDB-derived names remain
  # outside the product rename.
  "stream_defra_turn"
  "start_defra_turn"
  "steer_defra_turn"
  "decode_defra_compaction_progress"
  "defra_model_selection_id"
  "defra_request_id"
  "is_defra_background_tool"
  "is_defra_file_change_tool"
  "is_defra_export_file"
  "model_provider_filter_allows_defra"
  "source_filters_classify_defra_spawned_children"
  "deepest_defra_steering"
  "streams_defra_response"
  "project_defra_sessions"
  "queues_defra_request"
  "live_defra_filesystem"
  "defra_binary"
  "msg_defra_"
  "defra-host-"
  "defra-process-"
  "defra-codex-"
  "defra-fs-"
  "defra-scripted-"
  "Defra-native"
  "Defra-owned"
  "Defra remote runtime"
  "Defra runtime"
  "Defra tools"
  "Defra import"
  "Defra Test"
  "defra-test@"
  "defra agent"
  "embedded Defra HTTP"
  "valid Defra identity DID"
  "DEFRA_FIXTURE_CAPTURE"
  "DEFRA_LANGGRAPH_PROVIDER_MODE"
  "DEFRA_LANGGRAPH_OPENAI_MODEL"
  "defra:"
  # This host is DefraDB-owned and must not be product-renamed.
  "schemas.gents.ai"
)

# Guard allowlist — files where old names are permitted to remain. In
# particular, docs/gents.md and generated Apple files are NOT allowlisted.
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

is_generated_apple() {
  local file="$1"
  case "$file" in
    apps/desktop-tauri/src-tauri/gen/apple/* | apps/gents-desktop/src-tauri/gen/apple/*)
      return 0
      ;;
  esac
  return 1
}

is_apply_protected() {
  local file="$1"
  case "$file" in
    Cargo.lock | docs/gents.md | docs/gents-cutover.md | scripts/rename-to-gents.sh)
      return 0
      ;;
  esac
  is_generated_apple "$file"
}

path_slice() {
  local file="$1"
  case "$file" in
    crates/defra-agent-desktop | crates/defra-agent-desktop/* | \
      crates/gents-desktop | crates/gents-desktop/* | \
      crates/defra-agent-desktop-core | crates/defra-agent-desktop-core/* | \
      crates/gents-desktop-core | crates/gents-desktop-core/* | \
      apps/desktop-tauri | apps/desktop-tauri/* | \
      apps/gents-desktop | apps/gents-desktop/*)
      PATH_SLICE=desktop
      ;;
    # Markdown remains documentation even when it lives beside invoked scripts.
    # #826 owns those active docs; the release slice must not rewrite them early.
    scripts/*.md)
      PATH_SLICE=docs
      ;;
    .github/* | release | release/* | scripts | scripts/* | Makefile)
      PATH_SLICE=release
      ;;
    crates | crates/*)
      PATH_SLICE=core
      ;;
    docs | docs/* | demo | demo/* | examples | examples/* | \
      AGENTS.md | CLAUDE.md | DEVELOPMENT.md | README.md | *.md)
      PATH_SLICE=docs
      ;;
    *)
      PATH_SLICE=core
      ;;
  esac
}

in_slice() {
  local requested="$1"
  local file="$2"
  [[ "$requested" == "all" ]] && return 0
  path_slice "$file"
  [[ "$PATH_SLICE" == "$requested" ]]
}

require_slice() {
  local slice="${1:-}"
  case "$slice" in
    core | desktop | release | docs | all)
      printf '%s\n' "$slice"
      ;;
    *)
      echo "slice is required and must be one of: core, desktop, release, docs, all" >&2
      return 2
      ;;
  esac
}

literal_occurrences_in_string() {
  local value="$1"
  local token="$2"
  local count=0
  while [[ "$value" == *"$token"* ]]; do
    value="${value#*"$token"}"
    count=$((count + 1))
  done
  LITERAL_COUNT="$count"
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
  # sed uses basic regular expressions here. In BRE, + ? { } and | are
  # literals unless escaped; escaping them would turn a literal Cargo table
  # fragment such as `{ path = ... }` into an invalid repetition operator on
  # BSD sed. Escape only BRE-active characters plus our delimiter.
  old_escaped=$(printf '%s\n' "$old" | sed 's/[][\\.^$*\/&]/\\&/g')
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

path_exists() {
  [[ -e "$1" || -L "$1" ]]
}

preflight_move_array() {
  local array_name="$1"
  local pair old new
  local -a moves
  eval "moves=(\"\${${array_name}[@]}\")"
  for pair in "${moves[@]}"; do
    old="${pair%%:*}"
    new="${pair#*:}"
    if path_exists "$old" && path_exists "$new"; then
      echo "both rename source and destination exist: $old / $new" >&2
      return 1
    fi
    if ! path_exists "$old" && ! path_exists "$new"; then
      echo "neither rename source nor destination exists: $old / $new" >&2
      return 1
    fi
  done
}

preflight_core_lean_move() {
  local count=0
  path_exists "$CORE_LEAN_OLD_BEFORE" && count=$((count + 1))
  path_exists "$CORE_LEAN_OLD_AFTER" && count=$((count + 1))
  path_exists "$CORE_LEAN_NEW" && count=$((count + 1))
  if [[ "$count" -ne 1 ]]; then
    echo "ambiguous Lean rename state: expected exactly one of $CORE_LEAN_OLD_BEFORE, $CORE_LEAN_OLD_AFTER, or $CORE_LEAN_NEW" >&2
    return 1
  fi
}

preflight_moves_for_slice() {
  local slice="$1"
  case "$slice" in
    core)
      preflight_move_array CORE_MV
      preflight_core_lean_move
      ;;
    desktop)
      preflight_move_array DESKTOP_MV
      ;;
    release)
      preflight_move_array RELEASE_MV
      ;;
    docs)
      ;;
    all)
      local owned_slice
      for owned_slice in "${SLICE_ORDER[@]}"; do
        preflight_moves_for_slice "$owned_slice"
      done
      ;;
  esac
}

preflight_content_for_slice() {
  local slice="$1"
  local file
  while IFS= read -r -d '' file; do
    in_slice "$slice" "$file" || continue
    is_apply_protected "$file" && continue
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

apply_move_array() {
  local array_name="$1"
  local pair old new
  local -a moves
  eval "moves=(\"\${${array_name}[@]}\")"
  for pair in "${moves[@]}"; do
    old="${pair%%:*}"
    new="${pair#*:}"
    if path_exists "$old"; then
      echo "git mv $old -> $new"
      git mv "$old" "$new"
    fi
  done
}

apply_moves_for_slice() {
  local slice="$1"
  preflight_moves_for_slice "$slice"
  case "$slice" in
    core)
      apply_move_array CORE_MV
      if path_exists "$CORE_LEAN_OLD_AFTER"; then
        echo "git mv $CORE_LEAN_OLD_AFTER -> $CORE_LEAN_NEW"
        git mv "$CORE_LEAN_OLD_AFTER" "$CORE_LEAN_NEW"
      fi
      ;;
    desktop)
      apply_move_array DESKTOP_MV
      ;;
    release)
      apply_move_array RELEASE_MV
      ;;
    docs)
      echo "no path moves for docs slice"
      ;;
    all)
      local owned_slice
      for owned_slice in "${SLICE_ORDER[@]}"; do
        apply_moves_for_slice "$owned_slice"
      done
      ;;
  esac
}

apply_substitution_array_to_file() {
  local file="$1"
  local array_name="$2"
  local sub old new
  local -a substitutions
  eval "substitutions=(\"\${${array_name}[@]}\")"
  for sub in "${substitutions[@]}"; do
    old="${sub%%|*}"
    new="${sub#*|}"
    if grep -Fq -- "$old" "$file"; then
      replace_token "$file" "$old" "$new"
    fi
  done
}

apply_content_for_slice() {
  local slice="$1"
  if [[ "$slice" == "all" ]]; then
    local owned_slice
    preflight_content_for_slice all
    for owned_slice in "${SLICE_ORDER[@]}"; do
      apply_content_for_slice "$owned_slice"
    done
    return
  fi
  preflight_content_for_slice "$slice"
  local file first_checksum second_checksum
  local changed
  local count=0
  while IFS= read -r -d '' file; do
    in_slice "$slice" "$file" || continue
    is_apply_protected "$file" && continue
    [[ -L "$file" ]] && continue
    [[ -f "$file" ]] || continue
    grep -Iq . "$file" 2>/dev/null || continue
    first_checksum=$(cksum "$file")
    if [[ "$slice" == "core" && "$file" == "Cargo.toml" ]]; then
      apply_substitution_array_to_file "$file" CORE_WORKSPACE_SUBS
    else
      apply_substitution_array_to_file "$file" CONTENT_SUBS
    fi
    second_checksum=$(cksum "$file")
    if [[ "$first_checksum" != "$second_checksum" ]]; then
      git add "$file"
      count=$((count + 1))
    fi
  done < <(git ls-files -z)
  echo "$slice content substitutions applied to $count tracked files"

  # Core's exact consumer seam is deliberately narrower than CONTENT_SUBS.
  if [[ "$slice" == "core" ]]; then
    local consumer_count=0
    while IFS= read -r -d '' file; do
      path_slice "$file"
      case "$PATH_SLICE" in
        desktop | release) ;;
        *) continue ;;
      esac
      is_apply_protected "$file" && continue
      [[ -L "$file" ]] && continue
      [[ -f "$file" ]] || continue
      grep -Iq . "$file" 2>/dev/null || continue
      first_checksum=$(cksum "$file")
      apply_substitution_array_to_file "$file" CORE_CONSUMER_SUBS
      second_checksum=$(cksum "$file")
      if [[ "$first_checksum" != "$second_checksum" ]]; then
        git add "$file"
        consumer_count=$((consumer_count + 1))
      fi
    done < <(git ls-files -z)
    echo "core consumer substitutions applied to $consumer_count desktop/release files"
  fi

  if [[ "$slice" == "desktop" ]]; then
    local desktop_consumer_file desktop_consumer_count=0
    for desktop_consumer_file in "${DESKTOP_CONSUMER_FILES[@]}"; do
      [[ -f "$desktop_consumer_file" ]] || continue
      [[ -L "$desktop_consumer_file" ]] && continue
      first_checksum=$(cksum "$desktop_consumer_file")
      apply_substitution_array_to_file "$desktop_consumer_file" DESKTOP_CONSUMER_SUBS
      second_checksum=$(cksum "$desktop_consumer_file")
      if [[ "$first_checksum" != "$second_checksum" ]]; then
        git add "$desktop_consumer_file"
        desktop_consumer_count=$((desktop_consumer_count + 1))
      fi
    done
    echo "desktop consumer substitutions applied to $desktop_consumer_count exact files"
  fi

  if [[ "$slice" == "desktop" && -f Cargo.toml ]]; then
    first_checksum=$(cksum Cargo.toml)
    apply_substitution_array_to_file Cargo.toml DESKTOP_WORKSPACE_SUBS
    second_checksum=$(cksum Cargo.toml)
    if [[ "$first_checksum" != "$second_checksum" ]]; then
      git add Cargo.toml
      echo "desktop workspace substitutions applied to Cargo.toml"
    fi
  fi
}

apply_for_slice() {
  local slice="$1"
  local owned_slice
  preflight_moves_for_slice "$slice"
  preflight_content_for_slice "$slice"
  if [[ "$slice" == "all" ]]; then
    for owned_slice in "${SLICE_ORDER[@]}"; do
      apply_moves_for_slice "$owned_slice"
      apply_content_for_slice "$owned_slice"
    done
  else
    apply_moves_for_slice "$slice"
    apply_content_for_slice "$slice"
  fi
}

tracked_path_count() {
  local path="$1"
  local file count=0
  while IFS= read -r -d '' file; do
    if [[ "$file" == "$path" || "$file" == "$path/"* ]]; then
      count=$((count + 1))
    fi
  done < <(git ls-files -z)
  printf '%d\n' "$count"
}

print_move_array_audit() {
  local array_name="$1"
  local pair old new count
  local -a moves
  eval "moves=(\"\${${array_name}[@]}\")"
  for pair in "${moves[@]}"; do
    old="${pair%%:*}"
    new="${pair#*:}"
    count=$(tracked_path_count "$old")
    printf '  %s -> %s (%d tracked paths)\n' "$old" "$new" "$count"
    MOVE_AUDIT_TOTAL=$((MOVE_AUDIT_TOTAL + count))
  done
}

print_move_audit() {
  local slice="$1"
  local owned_slice lean_count
  MOVE_AUDIT_TOTAL=0
  echo "--- $slice path moves ---"
  case "$slice" in
    core)
      print_move_array_audit CORE_MV
      lean_count=$(tracked_path_count "$CORE_LEAN_OLD_BEFORE")
      printf '  %s -> %s (%d tracked paths)\n' \
        "$CORE_LEAN_OLD_BEFORE" "$CORE_LEAN_NEW" \
        "$lean_count"
      MOVE_AUDIT_TOTAL=$((MOVE_AUDIT_TOTAL + lean_count))
      ;;
    desktop)
      print_move_array_audit DESKTOP_MV
      ;;
    release)
      print_move_array_audit RELEASE_MV
      ;;
    docs)
      ;;
    all)
      for owned_slice in "${SLICE_ORDER[@]}"; do
        case "$owned_slice" in
          core)
            print_move_array_audit CORE_MV
            lean_count=$(tracked_path_count "$CORE_LEAN_OLD_BEFORE")
            printf '  %s -> %s (%d tracked paths)\n' \
              "$CORE_LEAN_OLD_BEFORE" "$CORE_LEAN_NEW" \
              "$lean_count"
            MOVE_AUDIT_TOTAL=$((MOVE_AUDIT_TOTAL + lean_count))
            ;;
          desktop) print_move_array_audit DESKTOP_MV ;;
          release) print_move_array_audit RELEASE_MV ;;
          docs) ;;
        esac
      done
      ;;
  esac
  echo "  Total move-source entries (nested moves may overlap): $MOVE_AUDIT_TOTAL"
}

is_regular_guard_excluded() {
  local slice="$1"
  local file="$2"
  [[ "$slice" == "core" && ( "$file" == "Cargo.toml" || "$file" == "Cargo.lock" ) ]]
}

count_literal_in_file() {
  local file="$1"
  local token="$2"
  local count
  count=$({ grep -F -o -- "$token" "$file" 2>/dev/null || true; } | wc -l | tr -d ' ')
  FILE_LITERAL_COUNT="$count"
}

scan_contract_array_in_file() {
  local file="$1"
  local array_name="$2"
  local label="$3"
  local sub old count
  local -a substitutions
  [[ -f "$file" ]] || return 0
  eval "substitutions=(\"\${${array_name}[@]}\")"
  for sub in "${substitutions[@]}"; do
    old="${sub%%|*}"
    count_literal_in_file "$file" "$old"
    count="$FILE_LITERAL_COUNT"
    if [[ "$count" -gt 0 ]]; then
      printf '  contract %-34s %6d occurrences\n' "$label: $old" "$count"
      printf '    content: %s\n' "$file"
      SCAN_TOTAL=$((SCAN_TOTAL + count))
      SCAN_VIOLATIONS=$((SCAN_VIOLATIONS + 1))
    fi
  done
}

scan_core_consumer_contracts() {
  local sub old file count token_total
  local -a substitutions files
  eval "substitutions=(\"\${CORE_CONSUMER_SUBS[@]}\")"
  for sub in "${substitutions[@]}"; do
    old="${sub%%|*}"
    token_total=0
    files=("")
    while IFS= read -r -d '' file; do
      path_slice "$file"
      case "$PATH_SLICE" in
        desktop | release) ;;
        *) continue ;;
      esac
      case "$file" in
        *.md) continue ;;
      esac
      is_allowlisted "$file" && continue
      is_apply_protected "$file" && continue
      [[ -L "$file" || ! -f "$file" ]] && continue
      count_literal_in_file "$file" "$old"
      count="$FILE_LITERAL_COUNT"
      if [[ "$count" -gt 0 ]]; then
        files+=("$file")
        token_total=$((token_total + count))
      fi
    done < <(git grep -I -l -z -F -e "$old" -- . 2>/dev/null || true)
    if [[ "$token_total" -gt 0 ]]; then
      printf '  contract %-34s %6d occurrences\n' "core consumers: $old" "$token_total"
      for file in "${files[@]}"; do
        [[ -n "$file" ]] || continue
        printf '    content: %s\n' "$file"
      done
      SCAN_TOTAL=$((SCAN_TOTAL + token_total))
      SCAN_VIOLATIONS=$((SCAN_VIOLATIONS + 1))
    fi
  done
}

scan_desktop_consumer_contracts() {
  local file
  for file in "${DESKTOP_CONSUMER_FILES[@]}"; do
    scan_contract_array_in_file "$file" DESKTOP_CONSUMER_SUBS "desktop consumer"
  done
}

scan_slice_contracts() {
  local slice="$1"
  case "$slice" in
    core)
      echo "--- Shared-file and consumer contracts (core) ---"
      scan_contract_array_in_file Cargo.toml CORE_WORKSPACE_SUBS "core workspace"
      scan_contract_array_in_file Cargo.lock CORE_LOCK_SUBS "core lockfile"
      scan_core_consumer_contracts
      ;;
    desktop)
      echo "--- Shared-file and consumer contracts (desktop) ---"
      scan_contract_array_in_file Cargo.toml DESKTOP_WORKSPACE_SUBS "desktop workspace"
      scan_contract_array_in_file Cargo.lock DESKTOP_LOCK_SUBS "desktop lockfile"
      scan_desktop_consumer_contracts
      ;;
  esac
}

scan_regex_token() {
  local slice="$1"
  local label="$2"
  local pattern="$3"
  local file count token_total=0 file_count=0
  local -a files
  files=("")

  while IFS= read -r -d '' file; do
    in_slice "$slice" "$file" || continue
    is_allowlisted "$file" && continue
    is_regular_guard_excluded "$slice" "$file" && continue
    [[ -L "$file" || ! -f "$file" ]] && continue
    count=$({ grep -E -o -- "$pattern" "$file" 2>/dev/null || true; } | wc -l | tr -d ' ')
    if [[ "$count" -gt 0 ]]; then
      files+=("$file")
      file_count=$((file_count + 1))
      token_total=$((token_total + count))
    fi
  done < <(git grep -I -l -z -E -e "$pattern" -- . 2>/dev/null || true)

  if [[ "$token_total" -gt 0 ]]; then
    printf '  %-45s %4d path files %4d content files %6d occurrences\n' \
      "$label" 0 "$file_count" "$token_total"
    for file in "${files[@]}"; do
      [[ -n "$file" ]] || continue
      printf '    content: %s\n' "$file"
    done
    SCAN_TOTAL=$((SCAN_TOTAL + token_total))
    SCAN_VIOLATIONS=$((SCAN_VIOLATIONS + 1))
  fi
}

scan_stale_tokens() {
  local slice="$1"
  local token file link_target match last_content_file
  local path_count link_count
  local path_occurrences content_occurrences_total symlink_occurrences
  local path_file_count content_file_count symlink_file_count
  local total=0
  local violations=0
  local -a path_files content_files symlink_files

  echo "--- Stale path and content tokens ($slice) ---"
  for token in "${STALE_TOKENS[@]}"; do
    path_occurrences=0
    content_occurrences_total=0
    symlink_occurrences=0
    path_file_count=0
    content_file_count=0
    symlink_file_count=0
    # Bash 3.2 with nounset treats expansion of an empty array as unbound.
    path_files=("")
    content_files=("")
    symlink_files=("")
    last_content_file=""

    while IFS= read -r -d '' file; do
      in_slice "$slice" "$file" || continue
      is_allowlisted "$file" && continue
      is_regular_guard_excluded "$slice" "$file" && continue

      literal_occurrences_in_string "$file" "$token"
      path_count="$LITERAL_COUNT"
      if [[ "$path_count" -gt 0 ]]; then
        path_files+=("$file")
        path_file_count=$((path_file_count + 1))
        path_occurrences=$((path_occurrences + path_count))
      fi

      if [[ -L "$file" ]]; then
        link_target=$(readlink "$file")
        literal_occurrences_in_string "$link_target" "$token"
        link_count="$LITERAL_COUNT"
        if [[ "$link_count" -gt 0 ]]; then
          symlink_files+=("$file")
          symlink_file_count=$((symlink_file_count + 1))
          symlink_occurrences=$((symlink_occurrences + link_count))
        fi
      fi
    done < <(git ls-files -z)

    # `git grep -o -z` emits one NUL-terminated path plus one newline-terminated
    # literal match per occurrence, preserving newline-bearing paths while
    # avoiding a process per file.
    while IFS= read -r -d '' file && IFS= read -r match; do
      in_slice "$slice" "$file" || continue
      is_allowlisted "$file" && continue
      is_regular_guard_excluded "$slice" "$file" && continue
      [[ -L "$file" ]] && continue
      content_occurrences_total=$((content_occurrences_total + 1))
      if [[ "$file" != "$last_content_file" ]]; then
        content_files+=("$file")
        content_file_count=$((content_file_count + 1))
        last_content_file="$file"
      fi
    done < <(git grep -I -o -z -F -e "$token" -- . 2>/dev/null || true)

    if [[ "$path_occurrences" -gt 0 || "$content_occurrences_total" -gt 0 || "$symlink_occurrences" -gt 0 ]]; then
      printf '  %-45s %4d path files %4d content files %6d occurrences\n' \
        "$token" "$path_file_count" "$((content_file_count + symlink_file_count))" \
        "$((path_occurrences + content_occurrences_total + symlink_occurrences))"
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
      total=$((total + path_occurrences + content_occurrences_total + symlink_occurrences))
      violations=$((violations + 1))
    fi
  done

  SCAN_TOTAL="$total"
  SCAN_VIOLATIONS="$violations"
  scan_regex_token \
    "$slice" \
    "standalone DEFRA" \
    '(^|[^[:alnum:]_])DEFRA([^[:alnum:]_]|$)'
  scan_slice_contracts "$slice"
}

run_audit() {
  local slice="$1"
  echo "=== Gents Rename Audit ($slice) ==="
  print_move_audit "$slice"
  scan_stale_tokens "$slice"
  echo "Audit: $SCAN_VIOLATIONS token classes, $SCAN_TOTAL occurrences"
}

run_guard() {
  local slice="$1"
  echo "=== Gents Rename Stale-Token Guard ($slice) ==="
  scan_stale_tokens "$slice"
  if [[ "$SCAN_VIOLATIONS" -eq 0 ]]; then
    echo "PASS: no stale path or content tokens outside the allowlist"
    return 0
  fi
  echo "FAIL: $SCAN_VIOLATIONS token classes, $SCAN_TOTAL occurrences"
  return 1
}

apply_substitutions_to_file() {
  local file="$1"
  apply_substitution_array_to_file "$file" CONTENT_SUBS
}

self_test_fixture() {
  local fixture_repo="$1"
  local core_path

  mkdir -p \
    "$fixture_repo/crates/defra-agent/proofs/Proofs/Conformance" \
    "$fixture_repo/crates/defra-agent/tests/support" \
    "$fixture_repo/crates/defra-agent-cli" \
    "$fixture_repo/crates/defra-agent-protocol" \
    "$fixture_repo/crates/defra-agent-schemas" \
    "$fixture_repo/crates/defra-agent-lean-contract" \
    "$fixture_repo/crates/defra-agent-lenses" \
    "$fixture_repo/crates/defra-native-fs-runner" \
    "$fixture_repo/crates/defra-agent-desktop" \
    "$fixture_repo/crates/defra-agent-desktop-core" \
    "$fixture_repo/apps/desktop-tauri/src-tauri/gen/apple" \
    "$fixture_repo/.github/workflows" \
    "$fixture_repo/release" \
    "$fixture_repo/scripts/adapter-interop" \
    "$fixture_repo/scripts" \
    "$fixture_repo/docs"

  printf '%s\n' \
    'service=defra-agent.identity' \
    'product=DefraAgent' \
    'home=.defra-agent' \
    'test_did=did:defra-agent:test' \
    'repo=git@github.com:sourcenetwork/defra-agent' \
    >"$fixture_repo/crates/defra-agent/runtime.txt"
  printf 'namespace Proofs.Conformance.DefraAgent\n' \
    >"$fixture_repo/$CORE_LEAN_OLD_BEFORE"
  for core_path in \
    crates/defra-agent-cli \
    crates/defra-agent-protocol \
    crates/defra-agent-schemas \
    crates/defra-agent-lean-contract \
    crates/defra-agent-lenses \
    crates/defra-native-fs-runner; do
    printf 'package=defra-agent-protocol\n' >"$fixture_repo/$core_path/fixture.txt"
  done

  printf '%s\n' \
    'defra-agent-protocol.workspace = true' \
    'defra-agent = { path = "../defra-agent" }' \
    >"$fixture_repo/crates/defra-agent-desktop-core/Cargo.toml"
  printf 'desktop brand: defra-agent-desktop\n' \
    >"$fixture_repo/crates/defra-agent-desktop/branding.txt"
  printf 'identifier=com.sourcenetwork.defra-agent-desktop\n' \
    >"$fixture_repo/apps/desktop-tauri/src-tauri/tauri.conf.json"
  printf 'generated=com.sourcenetwork.defra-agent-desktop\n' \
    >"$fixture_repo/apps/desktop-tauri/src-tauri/gen/apple/project.yml"

  printf '%s\n' \
    'cargo test -p defra-agent-protocol -p defra-agent' \
    'cargo test -p defra-agent-desktop-tauri' \
    'working-directory: apps/desktop-tauri' \
    'DEFRA_AGENT_TAURI_LIVE_MODEL_NAME=test' \
    >"$fixture_repo/.github/workflows/ci.yml"
  printf '%s\n' \
    'DESKTOP_DIR := apps/desktop-tauri' \
    'cargo build -p defra-agent-desktop-tauri' \
    'RELEASE_ARTIFACT := defra-agent-test' \
    >"$fixture_repo/Makefile"
  printf '%s\n' \
    'cargo install --path crates/defra-agent-desktop' \
    'cd apps/desktop-tauri' \
    'install target/debug/defra-agent-desktop-tauri' \
    'release_binary=defra-agent' \
    >"$fixture_repo/scripts/install-local.sh"
  printf '%s\n' \
    'app: "desktop-tauri"' \
    'source_path: "apps/desktop-tauri/tests/example.test.ts"' \
    >"$fixture_repo/crates/defra-agent/tests/support/conformance_consumers.rs"
  printf '%s\n' \
    '"apps/desktop-tauri/tests/example.test.ts::example"' \
    >"$fixture_repo/crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean"
  printf '__APPLE_TEAM_ID__.org.sourcenetwork.defra-agent\n' \
    >"$fixture_repo/release/entitlements.plist"
  printf 'launch defra-agent\n' \
    >"$fixture_repo/scripts/enable-defra-agent-runner-session.sh"
  printf 'adapter docs retain defra-agent until the docs slice\n' \
    >"$fixture_repo/scripts/adapter-interop/README.md"
  printf 'tool source-inc/defra-agent\n' \
    >"$fixture_repo/scripts/rename-to-gents.sh"

  printf 'owner draft keeps defra-agent until its author resolves it\n' \
    >"$fixture_repo/docs/gents.md"
  printf 'cutover mapping: defra-agent to Gents\n' \
    >"$fixture_repo/docs/gents-cutover.md"
  printf 'Run defra-agent from ~/.defra-agent.\n' >"$fixture_repo/README.md"

  cat >"$fixture_repo/Cargo.toml" <<'EOF'
[workspace]
members = [
  "apps/desktop-tauri/src-tauri",
  "crates/defra-agent",
  "crates/defra-agent-cli",
  "crates/defra-agent-desktop",
  "crates/defra-agent-desktop-core",
  "crates/defra-agent-protocol",
]
default-members = [
  "crates/defra-agent",
  "crates/defra-agent-desktop",
]

[workspace.package]
repository = "https://github.com/sourcenetwork/defra-agent"

[workspace.dependencies]
defra-agent-protocol = { path = "crates/defra-agent-protocol" }
defra-agent-desktop-core = { path = "crates/defra-agent-desktop-core" }
EOF
  cat >"$fixture_repo/Cargo.lock" <<'EOF'
[[package]]
name = "defra-agent"
dependencies = [
 "defra-agent-protocol",
 "defra-agent-desktop-core",
]

[[package]]
name = "defra-agent-desktop"
dependencies = [
 "defra-agent",
]
EOF

  git -C "$fixture_repo" init -q
  git -C "$fixture_repo" add -A
  git -C "$fixture_repo" \
    -c user.name='Gents Rename Self-Test' \
    -c user.email='self-test@example.invalid' \
    commit -qm fixture
}

self_test() (
  local test_dir test_file fixture_base fixture_slice fixture_sequential fixture_all
  local fixture_path first_checksum second_checksum owner_checksum apple_checksum
  local lock_checksum sequential_tree all_tree idempotent_tree guard_output
  test_dir=$(mktemp -d)
  test_file="$test_dir/substitutions.txt"
  trap 'rm -rf "$test_dir"' EXIT
  printf '%s\n' \
    'defra-agent.identity' \
    'git@github.com:sourcenetwork/defra-agent' \
    'git@github.com:source-inc/defra-agent' \
    'did:defra-agent:test' \
    'DefraAgent' \
    'DEFRA_AGENT_HOME' >"$test_file"

  apply_substitutions_to_file "$test_file"
  grep -Fxq 'com.source-inc.gents.identity' "$test_file"
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

  fixture_base="$test_dir/base"
  fixture_slice="$test_dir/slice"
  fixture_sequential="$test_dir/sequential"
  fixture_all="$test_dir/all"
  self_test_fixture "$fixture_base"
  cp -R "$fixture_base" "$fixture_slice"
  cp -R "$fixture_base" "$fixture_sequential"
  cp -R "$fixture_base" "$fixture_all"

  # A core run moves only core paths and applies only exact dependency/import
  # substitutions to desktop/release consumers. Branding remains untouched.
  cd "$fixture_slice"
  owner_checksum=$(cksum docs/gents.md)
  apple_checksum=$(cksum apps/desktop-tauri/src-tauri/gen/apple/project.yml)
  lock_checksum=$(cksum Cargo.lock)
  first_checksum=$(cksum crates/defra-agent-desktop/branding.txt)
  apply_for_slice core >/dev/null
  [[ -d crates/gents && ! -e crates/defra-agent ]]
  [[ -f crates/gents/proofs/Proofs/Conformance/Gents.lean ]]
  [[ -d apps/desktop-tauri && ! -e apps/gents-desktop ]]
  [[ -f scripts/enable-defra-agent-runner-session.sh ]]
  grep -Fq 'gents-protocol.workspace' crates/defra-agent-desktop-core/Cargo.toml
  grep -Fq 'gents = { path = "../gents" }' crates/defra-agent-desktop-core/Cargo.toml
  grep -Fq '"crates/gents"' Cargo.toml
  grep -Fq '"crates/gents-cli"' Cargo.toml
  grep -Fq 'gents-protocol = { path = "crates/gents-protocol" }' Cargo.toml
  grep -Fq '"crates/defra-agent-desktop"' Cargo.toml
  grep -Fq 'defra-agent-desktop-core = { path = "crates/defra-agent-desktop-core" }' Cargo.toml
  if grep -Fq '"crates/gents-desktop"' Cargo.toml; then
    echo "core slice rewrote a deferred desktop workspace member" >&2
    return 1
  fi
  [[ "$lock_checksum" == "$(cksum Cargo.lock)" ]]
  # Package selectors are an explicit #823 hand fix, not an unsafe catch-all
  # rewrite in shared release-owned files.
  grep -Fq 'cargo test -p gents-protocol -p defra-agent' .github/workflows/ci.yml
  second_checksum=$(cksum crates/defra-agent-desktop/branding.txt)
  [[ "$first_checksum" == "$second_checksum" ]]
  [[ "$owner_checksum" == "$(cksum docs/gents.md)" ]]
  [[ "$apple_checksum" == "$(cksum apps/desktop-tauri/src-tauri/gen/apple/project.yml)" ]]
  grep -Fq 'service=com.source-inc.gents.identity' crates/gents/runtime.txt
  grep -Fq 'test_did=did:defra-agent:test' crates/gents/runtime.txt
  if guard_output=$(run_guard core 2>&1); then
    echo "core guard unexpectedly accepted preserved did:defra-agent" >&2
    return 1
  fi
  grep -Fq 'did:defra-agent' <<<"$guard_output"

  # The desktop slice owns only the exact external consumer seam. It updates
  # build/test paths while leaving release artifact names for the release slice.
  apply_for_slice desktop >/dev/null
  [[ -d apps/gents-desktop && ! -e apps/desktop-tauri ]]
  grep -Fq 'cargo test -p gents-desktop-tauri' .github/workflows/ci.yml
  grep -Fq 'working-directory: apps/gents-desktop' .github/workflows/ci.yml
  grep -Fq 'GENTS_TAURI_LIVE_MODEL_NAME=test' .github/workflows/ci.yml
  grep -Fq 'DESKTOP_DIR := apps/gents-desktop' Makefile
  grep -Fq 'cargo build -p gents-desktop-tauri' Makefile
  grep -Fq 'RELEASE_ARTIFACT := defra-agent-test' Makefile
  grep -Fq 'cargo install --path crates/gents-desktop' scripts/install-local.sh
  grep -Fq 'cd apps/gents-desktop' scripts/install-local.sh
  grep -Fq 'install target/debug/gents-desktop-tauri' scripts/install-local.sh
  grep -Fq 'release_binary=defra-agent' scripts/install-local.sh
  grep -Fq 'app: "gents-desktop"' crates/gents/tests/support/conformance_consumers.rs
  grep -Fq 'apps/gents-desktop/tests/example.test.ts' \
    crates/gents/tests/support/conformance_consumers.rs
  grep -Fq 'apps/gents-desktop/tests/example.test.ts::example' \
    crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean

  apply_for_slice release >/dev/null
  grep -Fq 'adapter docs retain defra-agent until the docs slice' \
    scripts/adapter-interop/README.md

  # Explicit slice execution and `apply all` must materialize the same tree.
  cd "$fixture_sequential"
  owner_checksum=$(cksum docs/gents.md)
  apple_checksum=$(cksum apps/desktop-tauri/src-tauri/gen/apple/project.yml)
  for owned_slice in "${SLICE_ORDER[@]}"; do
    apply_for_slice "$owned_slice" >/dev/null
  done
  sequential_tree=$(git write-tree)
  [[ "$owner_checksum" == "$(cksum docs/gents.md)" ]]
  [[ "$apple_checksum" == "$(cksum apps/gents-desktop/src-tauri/gen/apple/project.yml)" ]]

  cd "$fixture_all"
  owner_checksum=$(cksum docs/gents.md)
  apple_checksum=$(cksum apps/desktop-tauri/src-tauri/gen/apple/project.yml)
  apply_for_slice all >/dev/null
  all_tree=$(git write-tree)
  [[ "$sequential_tree" == "$all_tree" ]]
  [[ "$owner_checksum" == "$(cksum docs/gents.md)" ]]
  [[ "$apple_checksum" == "$(cksum apps/gents-desktop/src-tauri/gen/apple/project.yml)" ]]
  grep -Fq 'service=com.source-inc.gents.identity' crates/gents/runtime.txt
  grep -Fxq '__APPLE_TEAM_ID__.com.source-inc.gents' release/entitlements.plist
  grep -Fq 'adapter docs retain gents until the docs slice' \
    scripts/adapter-interop/README.md
  grep -Fq '"apps/gents-desktop/src-tauri"' Cargo.toml
  grep -Fq '"crates/gents-desktop"' Cargo.toml
  grep -Fq 'gents-desktop-core = { path = "crates/gents-desktop-core" }' Cargo.toml

  # A second all run is a byte-for-byte no-op.
  idempotent_tree=$(git write-tree)
  apply_for_slice all >/dev/null
  [[ "$idempotent_tree" == "$(git write-tree)" ]]

  # Protected owner/generated files are still final-guard violations: apply
  # protection is intentionally not a stale-token allowlist.
  if guard_output=$(run_guard all 2>&1); then
    echo "global guard unexpectedly accepted protected stale files" >&2
    return 1
  fi
  grep -Fq 'docs/gents.md' <<<"$guard_output"
  grep -Fq 'apps/gents-desktop/src-tauri/gen/apple/project.yml' <<<"$guard_output"

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
  scan_stale_tokens all >/dev/null
  [[ "$SCAN_VIOLATIONS" -eq 1 ]]
  [[ "$SCAN_TOTAL" -eq 2 ]]

  trap - EXIT
  rm -rf "$test_dir"
  echo "self-test PASS"
)

usage() {
  echo "usage: $0 {audit|guard|apply-moves|apply-content|apply} {core|desktop|release|docs|all}" >&2
  echo "       $0 self-test" >&2
}

mode="${1:-}"
case "$mode" in
  self-test)
    [[ "$#" -eq 1 ]] || { usage; exit 2; }
    self_test
    ;;
  audit | guard | apply-moves | apply-content | apply)
    [[ "$#" -eq 2 ]] || { usage; exit 2; }
    slice=$(require_slice "$2")
    case "$mode" in
      audit) run_audit "$slice" ;;
      guard) run_guard "$slice" ;;
      apply-moves)
        check_clean
        apply_moves_for_slice "$slice"
        ;;
      apply-content)
        check_clean
        apply_content_for_slice "$slice"
        ;;
      apply)
        check_clean
        apply_for_slice "$slice"
        ;;
    esac
    ;;
  *)
    usage
    exit 2
    ;;
esac
