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
| `name` | `ThreadSetName` | **Relocate** to `AgentConversation.title` with `title_source="user"`, **create-if-absent**. The existing `update_conversation_title_with_source` (session/conversation.rs:282) is a no-op when no conversation row exists yet — and a freshly-started thread has no conversation until its first turn. So `ThreadSetName` must *upsert* the conversation (identity from `ShimState`), not just update it, or an early rename is silently lost. The runtime's `existing_title_state` (session/conversation.rs:375) preserves a `"user"` title source against auto-titling, so the name survives later turns. `json.rs` already reads `conversation.title`. |
| `created_at` / `updated_at` | projection write | **Derive** from `AgentConversation.created_at`/`updated_at` when a conversation exists, falling back to **`AgentSession.started`** for zero-turn threads (which have no conversation row). `createdAt`, `updatedAt`, and list ordering all use this fallback. `json.rs:132-155` already prefers conversation timestamps; extend the fallback chain to `AgentSession.started`. |
| `preview` | (from conversation) | **Derive** from `AgentConversation.preview_text` (already). |
| `cwd` | `ThreadSettingsUpdate` / create | **Derive** from a server-scoped per-thread cwd override (moved from `ConnectionState.thread_cwds` to the `ShimState` sidecar — see below) falling back to `ShimState.cwd`. The per-thread cwd is already the live source of truth for turn execution (`turn.rs:54`, `turn.rs:170`); it must move to `ShimState` because record-assembly APIs only receive `&ShimState`, and `ConnectionState` is recreated per websocket. |
| `git_info_json` | `ThreadMetadataUpdate` (client-supplied) | **Derive at read time** from `cwd` via a **new** non-fatal `thread_git_info(cwd) -> Option<GitInfo>` helper (`host_runtime/git.rs` today exposes only `git_diff_to_remote`, not sha/branch/origin). Non-git directories or git failures yield no `gitInfo`, never a failed thread read. |
| `loaded` | subscribe/unsubscribe | **Drop persistence.** Track an in-process subscribed-thread set. One TUI talks to one shim process; replicating one process's UI focus is meaningless. |
| `archived` | `ThreadArchive`/`ThreadUnarchive` | **In-process set (faked).** Cannot ride on `AgentConversation.status` (the runtime rewrites it on every request transition — `transition.rs`, `recovery.rs`). A dedicated `AgentConversation.archived` field would persist but is a core-collection schema change with reconcile/desktop fan-out — out of scope for "minimal churn." Archive works per session and resets on restart; usability is retained. |
| `memory_mode` | `ThreadMemoryModeSet` | **In-process map, default `"disabled"`.** Memory is not actually wired in this shim, so the honest default is disabled (was `"enabled"`). |
| `settings_json` | `ThreadSettingsUpdate` | **In-process map, default `{}`.** The only meaningful part (`cwd`) is tracked in the server-scoped cwd sidecar (see `cwd` above); model/approval/sandbox are hardcoded by the shim in `thread_response_json`. |
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

**Server-scoped sidecar state.** Add one shared sidecar (behind
`Arc<Mutex<...>>`, keyed by `session_id`) to `ShimState`, holding the
per-thread cwd override plus the genuinely-ephemeral Codex-only toggles:

- `cwd: BTreeMap<String, PathBuf>` (**moved** out of `ConnectionState.thread_cwds`)
- `loaded: BTreeSet<String>`
- `archived: BTreeSet<String>`
- `memory_mode: BTreeMap<String, String>` (default `"disabled"`)
- `settings: BTreeMap<String, String>` (default `{}`)
- `goal: BTreeMap<String, StoredGoal>`

This **must** be on `ShimState`, not `ConnectionState`: record-assembly APIs
(`load_codex_thread`, `list_codex_threads_by_archived`, `store_forked_codex_thread`)
only receive `&ShimState`, and `ConnectionState` is recreated per websocket
(`codex_shim.rs:62`, `:193`) so connection-scoped state cannot back a derived
record. Moving `thread_cwds` here also means the existing connection-level
readers (`turn.rs:54`, `turn.rs:170`, `handlers/thread.rs:279`) read the
server-scoped map instead. Server scope also lets the toggles survive TUI
reconnects within a single server run. Small accessor helpers gate each map.

**Existence spine: `AgentSession`, not `AgentConversation`.** `AgentSession` is
created eagerly at `ThreadStart`/`ThreadResume` via `ensure_agent_session`, so it
exists for every thread including zero-turn ones. `AgentConversation` is created
lazily from the first request, so a fresh thread has no conversation row yet, and
tests list a thread immediately after start. Therefore:

- `load_codex_thread` and `list_codex_threads_by_archived` resolve threads from
  `AgentSession`, left-joining `AgentConversation` for title/preview/timestamps
  when present, and filter by the in-process archived set.
- **The `AgentSession` query must be scoped to the shim's `agent_did` and
  `behavior_id`**, or `thread/list` will surface unrelated runtime sessions that
  happen to share the node. This requires fixing `ensure_agent_session`
  (`storage.rs:89`), which today writes `agent_name` + `behavior_id` but **omits
  `agent_did`** (unlike the runtime helper `session/sessions.rs:24`). Write
  `agent_did` at session create so the scoped filter matches the shim's own
  sessions. `agent_did` is `@immutable`, so this is write-once-at-create.
- Zero-turn timestamps fall back to `AgentSession.started` (see field audit).
- This avoids eagerly creating empty `AgentConversation` rows for every thread
  (which would pollute the desktop chat list). A conversation row appears only
  once the thread has a turn — or once the user explicitly names it (see `name`
  above), which is a deliberate action.

**`thread_projection.rs` (module root).** `CodexThreadRecord` and the public
API (`create_codex_thread`, `resume_codex_thread`, `load_codex_thread`,
`list_codex_threads_by_archived`, `store_forked_codex_thread`) are kept so
callers in `handlers/thread.rs`, `thread_routes.rs`, and `history_projection.rs`
are unchanged. The record is now assembled from:

- `AgentSession` (existence spine) + `AgentConversation` when present
  (title→name, preview, timestamps, status, fork lineage).
- per-thread cwd from the `ShimState` sidecar (→ `state.cwd`).
- the in-process sidecar maps (loaded/archived/memory_mode/settings/goal).
- `git_info` derived from cwd via the new `thread_git_info` helper.

**`thread_projection/storage.rs`.** Drops `ProjectionRow`, `ProjectionUpdate`,
`upsert_projection`, `update_projection_loaded_cwd`, `load_projection`,
`list_projection_rows`. Keeps `ensure_agent_session`,
`ensure_agent_session_pinning`, and `load_conversation`.
`list_codex_threads_by_archived` lists `AgentSession` rows (the eager spine),
left-joins `AgentConversation`, and filters by the in-process archived set.

**`thread_projection/mutations.rs`.** Each setter writes the in-process sidecar
map instead of a GraphQL mutation, except `set_codex_thread_name`, which
**upserts** `AgentConversation.title` (`title_source="user"`, create-if-absent)
via a new runtime helper (the existing `update_conversation_title_with_source` is
update-only and no-ops when the row is absent).

**`thread_projection/goal.rs`.** Reads/writes the in-process goal map.
`tokens_used`/`time_used_seconds` populated from Part B.

**`thread_projection/json.rs`.** `gitInfo` is derived from `record.cwd` via the
new non-fatal `thread_git_info` helper instead of decoding a stored
`git_info_json`.

**`host_runtime/git.rs`.** Add `thread_git_info(cwd) -> Option<GitInfo>`
(sha/branch/origin via `run_git`); returns `None` for non-git dirs or any git
failure.

**Registry deletions.**

- `crates/defra-agent-schemas/`: delete `schemas/agent/codex_thread_projection.graphql`
  and the `CODEX_THREAD_PROJECTION*` consts; remove from `ALL`,
  `ALL_COLLECTION_NAMES`, `BRANCHABLE_COLLECTION_NAMES`; update the
  `all_contains_every_agent_schema` test count (`23 → 22`, `src/lib.rs:137`).
- `crates/defra-agent-protocol/src/schemas.rs`: remove the `CODEX_THREAD_PROJECTION`
  / `CODEX_THREAD_PROJECTION_NAME` re-exports (lines 17-18), and the entries in
  the protocol-level `ALL` (line 65) and `ALL_COLLECTION_NAMES` (line 94); update
  the `all_contains_every_schema` test count (`27 → 26`, `src/schemas.rs:118`).
- `crates/defra-agent/src/schema.rs` and `crates/defra-agent/src/lib.rs:151`:
  remove the `CODEX_THREAD_PROJECTION_SCHEMA` re-exports.
- `crates/defra-agent/src/agent/p2p_reconcile/profiles.rs`: remove from
  `RUNTIME_COLLECTIONS` and `CHAT_REQUEST_COLLECTIONS`.
- `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`: remove the
  "deliberately excluded" assertion that references the name.
- `crates/defra-agent-cli/src/main.rs`: remove the `SCHEMA_COLLECTION_CHECKS`
  entry.
- `crates/defra-agent-desktop-core/`: remove any enumeration of the name
  (`client/schema.rs`, `client/collection_resolver.rs`) if present.
- Final `grep -rn "CodexThreadProjection\|CODEX_THREAD_PROJECTION" crates/` must
  come back empty before the PR is considered complete. (Scope to `crates/` — the
  gate deliberately excludes `docs/`, where this spec and the older #490
  spec/plan reference the name as historical context.)

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

- Emit `ServerNotification::ThreadTokenUsageUpdated` at turn completion (in
  `turn.rs`): `total` = session cumulative; `last` = the **Codex turn delta**.
  A single Codex turn can fold a chain of steering requests
  (`stream.rs:206-208` advances `current.request_id` while folding), so `last`
  must sum usage across *all* request ids in that turn's chain, not just the
  final request. The turn loop already tracks the chain; sum their
  `InferenceCall` usage.
- **Replay on resume/attach.** Upstream replays token usage when a client
  attaches to an existing thread. Emit `ThreadTokenUsageUpdated` with the session
  total on `ThreadResume`/subscribe as well, so resumed historical threads are
  not left dark.
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
- **Zero-turn thread is listed** immediately after `ThreadStart`, before any turn
  (regression guard for the `AgentSession`-spine listing).
- **Early rename persists:** `ThreadSetName` before the first turn, then a thread
  get/list reflects the name (regression guard for create-if-absent conversation
  upsert).
- Add a test asserting `memory_mode` defaults to `"disabled"`.
- Add a test asserting `git_info` is derived from cwd (a thread under a git repo
  reports sha/branch without any `ThreadMetadataUpdate` call) and that a non-git
  cwd yields no `gitInfo` (no error).
- **Token replay on resume:** resuming a thread with prior turns emits
  `ThreadTokenUsageUpdated` with the session total.

## Gate

```
cargo test -p defra-agent && cargo test -p defra-agent-cli
```

No `lake build` (no Lean / ApplyReconcile touched — verified).

## Accepted tradeoffs

**Restart resets UI sugar.** Restarting the embedded node resets Codex-only UI
sugar (archive flag, memory toggle, goal, thread settings) to defaults.
Conversations, history, and titles (including user-set names, now on
`AgentConversation.title`) persist. This is the cost of deleting an unnecessary
replicated collection, and is consistent with the single-process, local nature
of the Codex shim.

**No backfill of pre-upgrade data (accepted loss).** Existing
`CodexThreadProjection` rows are not migrated. User-set names revert to
auto-generated conversation titles; archive flags, goals, and settings reset.
Additionally, **both** `load_codex_thread` and the list scope by
`agent_did + behavior_id`, and pre-upgrade shim `AgentSession` rows were written
without `agent_did` (which is `@immutable` and cannot be backfilled). So
pre-upgrade threads are fully retired from the shim view: they drop off
`thread/list` **and** explicit `ThreadResume`/`ThreadRead` by id will not find
them (no legacy direct-load exception). Their conversations and history remain
intact in the runtime collections; only the Codex-shim view and UI sugar are
affected. This is acceptable for local, pre-release tooling and avoids both a
one-time migration path and a legacy-session special case in the load path.
