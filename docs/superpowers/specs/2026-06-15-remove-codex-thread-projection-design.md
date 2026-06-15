# Remove `CodexThreadProjection`, derive Codex sidecar state

**Issue:** #494
**Branch:** `codex-projection-494`
**Date:** 2026-06-15

## Problem

`CodexThreadProjection` is a dedicated, replicated DefraDB collection that stores
Codex-TUI sidecar state (`cwd`, `name`, `archived`, `loaded`, `memory_mode`,
`settings_json`, `goal_json`, `rollback_user_turn`, `git_info_json`,
`created_at`, `updated_at`). It is written and read only by the Codex shim
(`crates/defra-agent-cli/src/commands/codex_shim/thread_projection/`).

The collection is misnamed (it is not a projection — it is a write-backed
sidecar) and, more importantly, it is **redundant**. A field-by-field audit shows
that every value either already exists on `AgentConversation`/`AgentSession`, is
derivable from runtime state, or is ephemeral per-process UI state that has no
business being persisted or replicated. It is a compatibility shim that should
not exist.

This work removes the collection entirely and derives or relocates every field.
While in this code, it also closes an adjacent visibility gap: the shim never
surfaces token usage to the TUI, even though the data is available.

## Scope guard: this is NOT bundled with #490

#494 is deliberately separate from the conversation-replication work (#490).
`CodexThreadProjection` was explicitly excluded from the conversation
replication template as local UI state (`p2p_reconcile/templates.rs:84`). This
PR does not touch the replication template or #490's collections beyond the
mechanical removal of `CodexThreadProjection` from the collection registries.

## Field-by-field audit and resolution

| Field | Current writer (Codex RPC) | Resolution |
|---|---|---|
| `session_id` | key | Shared key with `AgentConversation`/`AgentSession`. |
| `name` | `ThreadSetName` | **Relocate** to `AgentConversation.title` with `title_source="user"`. The runtime's `existing_title_state` (session/conversation.rs:375) preserves a non-fallback title source against auto-titling, so a user-set name survives. `json.rs` already reads `conversation.title`. |
| `created_at` / `updated_at` | projection write | **Derive** from `AgentConversation.created_at`/`updated_at`. `json.rs:132-155` already prefers the conversation timestamps. |
| `preview` | (from conversation) | **Derive** from `AgentConversation.preview_text` (already). |
| `cwd` | `ThreadSettingsUpdate` / create | **Derive** from the in-memory per-thread override (`ConnectionState.thread_cwds`) falling back to `ShimState.cwd`. This is already the live source of truth for turn execution (`turn.rs:54`, `turn.rs:170`). |
| `git_info_json` | `ThreadMetadataUpdate` (client-supplied) | **Derive at read time** from `cwd` using the existing `host_runtime/git.rs` helpers. Always accurate to the real repo state; stops storing derivable data. |
| `loaded` | subscribe/unsubscribe | **Drop persistence.** Track an in-process subscribed-thread set. One TUI talks to one shim process; replicating one process's UI focus is meaningless. |
| `archived` | `ThreadArchive`/`ThreadUnarchive` | **In-process set (faked).** Cannot ride on `AgentConversation.status` (the runtime rewrites it on every request transition — `transition.rs`, `recovery.rs`). A dedicated `AgentConversation.archived` field would persist but is a core-collection schema change with reconcile/desktop fan-out — out of scope for "minimal churn." Archive works per session and resets on restart; usability is retained. |
| `memory_mode` | `ThreadMemoryModeSet` | **In-process map, default `"disabled"`.** Memory is not actually wired in this shim, so the honest default is disabled (was `"enabled"`). |
| `settings_json` | `ThreadSettingsUpdate` | **In-process map, default `{}`.** The only meaningful part (`cwd`) is already tracked in `thread_cwds`; model/approval/sandbox are hardcoded by the shim in `thread_response_json`. |
| `goal_json` | `ThreadGoalSet`/`Clear` | **In-process map.** Pure display state; `tokens_used`/`time_used_seconds` are now populated with real values (Part B) instead of 0 stubs. |
| `rollback_user_turn` | always `-1`, no setter | **Delete.** Dead: written as a constant, never read anywhere. |

### Why no Lean / ApplyReconcile change

`CodexThreadProjection` is **not** in the config-managed `Collection` enum
(`crates/defra-agent/src/collection.rs`) and **not** modeled in
`ApplyReconcile.lean`. The config-import fence
(`config_import.rs`) only requires Lean coverage for config-managed
collections. The only Codex-related Lean (`Proofs/CodexShim/Projection.lean`)
models the *turn-phase* projection, which is unrelated to this storage
collection. Therefore this is a pure Rust + schema change. No `.lean` edit and
no `lake build` gate is required.

## Architecture

### Part A — remove the collection

**In-process sidecar state.** Add shared, per-server-process maps to `ShimState`
(behind `Arc<Mutex<...>>`, keyed by `session_id`) for the genuinely-ephemeral
Codex-only toggles:

- `loaded: BTreeSet<String>`
- `archived: BTreeSet<String>`
- `memory_mode: BTreeMap<String, String>` (default `"disabled"`)
- `settings: BTreeMap<String, String>` (default `{}`)
- `goal: BTreeMap<String, StoredGoal>`

These live on `ShimState` (not `ConnectionState`) so they survive TUI
reconnects within a single server run, matching the persistence boundary of the
existing `thread_cwds` semantics as closely as is sensible. Small accessor
helpers gate each map.

**`thread_projection.rs` (module root).** `CodexThreadRecord` and the public
API (`create_codex_thread`, `resume_codex_thread`, `load_codex_thread`,
`list_codex_threads_by_archived`, `store_forked_codex_thread`) are kept so
callers in `handlers/thread.rs`, `thread_routes.rs`, and `history_projection.rs`
are unchanged. The record is now assembled from:

- `AgentConversation` (title→name, preview, timestamps, status, fork lineage) —
  already loaded via `load_conversation`.
- runtime cwd (`thread_cwds` → `state.cwd`).
- the in-process sidecar maps (loaded/archived/memory_mode/settings/goal).

**`thread_projection/storage.rs`.** Drops `ProjectionRow`, `ProjectionUpdate`,
`upsert_projection`, `update_projection_loaded_cwd`, `load_projection`,
`list_projection_rows`. Keeps `ensure_agent_session`,
`ensure_agent_session_pinning`, and `load_conversation`. `list_codex_threads_by_archived`
now lists `AgentConversation` rows and filters by the in-process archived set.

**`thread_projection/mutations.rs`.** Each setter writes the in-process map
instead of a GraphQL mutation, except `set_codex_thread_name`, which writes
`AgentConversation.title` (`title_source="user"`).

**`thread_projection/goal.rs`.** Reads/writes the in-process goal map.
`tokens_used`/`time_used_seconds` populated from Part B.

**`thread_projection/json.rs`.** `gitInfo` is derived from `record.cwd` via
`host_runtime/git.rs` instead of decoding a stored `git_info_json`.

**Registry deletions.**

- `crates/defra-agent-schemas/`: delete `schemas/agent/codex_thread_projection.graphql`
  and the `CODEX_THREAD_PROJECTION*` consts; remove from `ALL`,
  `ALL_COLLECTION_NAMES`, `BRANCHABLE_COLLECTION_NAMES`.
- `crates/defra-agent/src/agent/p2p_reconcile/profiles.rs`: remove from
  `RUNTIME_COLLECTIONS` and `CHAT_REQUEST_COLLECTIONS`.
- `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`: remove the
  "deliberately excluded" assertion that references the name.
- `crates/defra-agent-cli/src/main.rs`: remove the `SCHEMA_COLLECTION_CHECKS`
  entry.
- `crates/defra-agent-desktop-core/`: remove any enumeration of the name
  (`client/schema.rs`, `client/collection_resolver.rs`) if present.

### Part B — wire real token usage (visibility)

The Codex v2 protocol exposes `ThreadTokenUsageUpdatedNotification`
(`token_usage: { total, last, model_context_window }`) and a turn-level `Usage`.
The shim emits neither, so the TUI token meter is dark.

**Real numbers are already in the DB.** The provider's `rig::completion::Usage`
(real `input_tokens`/`output_tokens`) is persisted per inference call on
`InferenceCall.prompt_tokens`/`completion_tokens`, keyed by `request_id @index`
(`admission/persistence.rs`). This is distinct from the approximate
`AgentResponse.token_count` word-count proxy (`streaming.rs:597`). Part B reads
the real numbers.

**Source and fallback.**

- `session_token_usage(state, session_id) -> TokenTotals`: resolves the session's
  request ids and sums `InferenceCall.prompt_tokens` (→ `input_tokens`) and
  `completion_tokens` (→ `output_tokens`) over them. `total_tokens` = input +
  output.
- `InferenceCall.usage` is `Option` — some providers (notably local
  `llama-server` in the Mac demo) return no usage, leaving the fields null. When
  a request has no real usage, fall back to that request's
  `AgentResponse.token_count` proxy for `output_tokens` so the counter is not
  silently zero. Partial/proxy contributions are summed in alongside real ones.
- `input_tokens`/`cached_input`/`reasoning_output` breakdown beyond
  input/output, and `model_context_window`, are out of scope here — see the
  follow-up issue. Those sub-fields are reported as 0 / `None`.

**Emission.**

- Emit `ServerNotification::ThreadTokenUsageUpdated` at turn completion (next to
  the existing `TurnStarted` emit in `turn.rs`): `last` = the just-completed
  request's usage, `total` = session cumulative.
- `goal.tokens_used` = session cumulative `total_tokens`; `time_used_seconds` =
  wall-clock seconds since `goal.created_at`.

**Follow-up issue ([#498](https://github.com/sourcenetwork/defra-agent/issues/498), not this PR):** full per-turn breakdown
(`input`/`cached_input`/`reasoning_output`), `model_context_window` from the
bound model, uniform handling of providers that return no usage, and retiring
the `AgentResponse.token_count` proxy once real usage is universally available.

## Testing

- `crates/defra-agent-cli/tests/cli_codex_shim.rs` (~line 187, ~line 1812)
  queries `CodexThreadProjection` directly. Rewrite to assert through observable
  behavior: thread get/list responses, `AgentConversation.title` for a user-set
  name, and (Part B) a `ThreadTokenUsageUpdated` notification / non-zero
  `goal.tokens_used` after a turn.
- Add a test asserting `memory_mode` defaults to `"disabled"`.
- Add a test asserting `git_info` is derived from cwd (a thread under a git repo
  reports sha/branch without any `ThreadMetadataUpdate` call).

## Gate

```
cargo test -p defra-agent && cargo test -p defra-agent-cli
```

No `lake build` (no Lean / ApplyReconcile touched — verified).

## Accepted tradeoff

Restarting the embedded node resets Codex-only UI sugar (archive flag, memory
toggle, goal, thread settings) to defaults. Conversations, history, and titles
(including user-set names, now on `AgentConversation.title`) persist. This is the
cost of deleting an unnecessary replicated collection, and is consistent with the
single-process, local nature of the Codex shim.
