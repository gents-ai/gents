# R2 — Rust Subagent Data Plane Spec Design

**Status:** Design
**Date:** 2026-05-08
**Tracks:** subagent-management design branch (depends on R1's `ToolCallLifecycle` + B1's Lean spec)
**Scope:** Rust runtime data plane only — schema migrations, struct extensions, transition methods, conformance buckets. Spawn machinery (SubagentSource), agent-facing tools, hook integration are deferred to R3+.

## Background

The Lean spec at `docs/superpowers/specs/2026-05-08-subagent-lifecycle-design.md` formalizes a subagent lifecycle as an extension of the B1 ToolCall lifecycle along three orthogonal axes: multi-flight (`tools : List`), foreground/background, and child-request linkage. The spec is fully proven (zero `sorry`s) and lands in PR #154.

R1 (`docs/superpowers/specs/2026-05-08-r1-rust-toolcall-conformance-design.md`, PR #152) provides the Rust runtime foundation: `ToolCallLifecycle` struct with seven transitions (`start_running`, `complete`, `fail`, `spawn_failed`, `timeout`, `cancel_before_dispatch`, `cancel_during_run`), a v1→v2 schema migration adding `lifecycle_state`, and three conformance buckets.

R2 extends R1's data plane with the subagent-specific shape:
- New persisted vocabulary (`AwaitMode`, `CancelPolicy`).
- New `ToolCallLifecycle` fields and methods for the bridge edge.
- Schema migrations v2→v3 for AgentToolCall, AgentRequest, and ToolSelection.
- Conformance buckets covering the new vocabulary, transitions, and end-to-end flows.

R2 deliberately stops at the data plane. SubagentSource (the `TriggerSource` implementation that drives spawn flows) is R3. Agent-facing tools (`spawn_subagent` etc.) are R4. This phasing mirrors R1's discipline: the data plane lands first with full conformance, runtime wiring lands once the data plane is stable.

## Goals

- Add the `AwaitMode` and `CancelPolicy` enums with persisted-vocabulary round-trip.
- Extend `ToolCallLifecycle` with three new fields (`await_mode`, `cancel_policy`, `child_request_id`) and six new transition methods (`background`, `foreground`, `detach`, `bridge_complete`, `bridge_failure`, `bridge_cancel_cascade`).
- Add `create_subagent_request` helper as the public API for parent-linked child request creation (consumed by R3's SubagentSource).
- Land schema migrations v2→v3 covering AgentToolCall, AgentRequest, and ToolSelection field additions via a single unified lens crate.
- Conformance: three buckets (vocabulary, Lean transition matrix, runtime integration) covering all new types and transitions.

## Non-goals (out of scope for R2)

1. **R3 — `SubagentSource` and spawn machinery.** TriggerEngine integration; daemon interrupt dispatcher consuming `CascadeIntent`; spawn-time invariant enforcement (depth, callId freshness); cross-reference validation (`subagent_targets` resolution, `caused_by_parent_request_id` existence).
2. **R4 — Agent-facing tools.** `spawn_subagent`, `wait_task`, `get_task_result`, `cancel_task`, `read_subagent_transcript`, `send_message_to_subagent`, `list_tasks`, `background_task`; hook integration that routes them through `ToolCallLifecycle::new_subagent`.
3. **R5 — Validation polish + multi-flight stress.** Apply-time cross-reference validation; conformance Bucket 4 covering multi-flight scenarios.
4. **R6 — Cross-principal delegation.** Lands with sourcenetwork/defra-agent#9 (AgentPrincipal/Behavior split).
5. **Future** (no R-phase yet): token/cost budget propagation, output streaming, detach orphan reaper, subagent retry semantics, persistent subagents across daemon restarts.

## Architecture

### File layout

```
crates/defra-agent/src/
  tool_call_lifecycle.rs                # AMEND: + AwaitMode/CancelPolicy enums,
                                        #         + struct fields,
                                        #         + new_subagent constructor,
                                        #         + IllegalToolCallTransition variants
  tool_call_lifecycle/
    transition.rs                       # AMEND: + background/foreground/detach,
                                        #         + bridge_complete/failure/cancel_cascade
    rows.rs                             # AMEND: + new GraphQL row fields
    subagent_request.rs                 # NEW: create_subagent_request helper
  migration.rs                          # AMEND: + ensure_subagent_extensions_migrations
  document_config/
    tool_selection.rs                   # AMEND: + subagent_targets / subagent_*_enabled fields,
                                        #         + apply-time well-formedness validation

crates/defra-agent-protocol/schemas/agent/
  agent_tool_call.graphql               # AMEND: +4 fields (v3)
  agent_request.graphql                 # AMEND: +3 fields (v3)
  tool_selection.graphql                # AMEND: +4 fields (v3)

crates/defra-agent-lenses/
  agent_subagent_v2_to_v3/              # NEW lens crate (mirror of v1_to_v2/)
    Cargo.toml
    src/lib.rs                          # forward + inverse transforms

crates/defra-agent/tests/
  tool_call_subagent_lifecycle_conformance.rs   # NEW: Bucket 3 runtime integration
  state_machine_conformance.rs                  # AMEND: Bucket 2 — new transitions
```

### Schema migrations (v2 → v3)

Single unified lens crate `agent_subagent_v2_to_v3` covers all three collections in one WASM module. Forward transform adds new fields with defaults; inverse drops them for P2P backward-compat.

#### JSON Patches (one per collection, applied atomically)

**`AgentToolCall`** — four new String fields:

```
await_mode          (default: "foreground")
cancel_policy       (default: "cascade")
child_request_id    (default: null)
request_id          (default: null on backfill)
```

**`AgentRequest`** — three new fields:

```
subagent_depth                : Int    (default: 0)
caused_by_parent_request_id   : String (default: null)
caused_by_parent_tool_call_id : String (default: null)
```

**`ToolSelection`** — flattened-boolean shape (matches existing `enable_file_tools` style; DefraDB GraphQL doesn't use nested object types here):

```
subagent_targets             : [String]   (default: [])
subagent_spawn_enabled       : Boolean    (default: false)
subagent_steering_enabled    : Boolean    (default: false)
subagent_background_enabled  : Boolean    (default: false)
```

#### Migration orchestrator

```rust
// crates/defra-agent/src/migration.rs
pub async fn ensure_subagent_extensions_migrations(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    // 1. Detect already-migrated state by checking AgentToolCall has await_mode field.
    if has_await_mode_field(&node).await? { return Ok(()); }

    // 2. Apply three JSON Patches in order.
    let v3_atc = node.patch_collection("AgentToolCall", ADD_ATC_PATCH).await?;
    let v3_ar  = node.patch_collection("AgentRequest", ADD_AR_PATCH).await?;
    let v3_ts  = node.patch_collection("ToolSelection", ADD_TS_PATCH).await?;

    // 3. Register the unified lens.
    let forward = LensConfig::new(
        v2_atc_id, v3_atc, LensModule::from_path(SUBAGENT_V2_V3_LENS_WASM)
    );
    node.set_migration(forward).await?;

    // 4. Activate v3 per collection.
    node.set_active_collection_version(&v3_atc).await?;
    node.set_active_collection_version(&v3_ar).await?;
    node.set_active_collection_version(&v3_ts).await?;
    Ok(())
}
```

Invoked from `defra-agent-cli/src/commands/serve.rs` immediately after `ensure_tool_call_migrations()`. Idempotent.

#### Backfill — none needed

All defaults represent today's behavior exactly. No silent behavior change. Subagent surfaces don't activate unless behavior config opts in (R3 work).

### Type additions in `tool_call_lifecycle.rs`

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AwaitMode { Foreground, Background }

impl AwaitMode {
    pub fn as_str(self) -> &'static str { /* "foreground" | "background" */ }
    pub fn from_persisted(s: &str) -> Option<Self> { /* ... */ }
    pub const ALL: &'static [AwaitMode] = &[AwaitMode::Foreground, AwaitMode::Background];
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CancelPolicy { Cascade, Detach }

impl CancelPolicy {
    pub fn as_str(self) -> &'static str { /* "cascade" | "detach" */ }
    pub fn from_persisted(s: &str) -> Option<Self> { /* ... */ }
    pub const ALL: &'static [CancelPolicy] = &[CancelPolicy::Cascade, CancelPolicy::Detach];
}

pub enum ChildTerminal {
    Failed { reason: String, failure_class: FailureClass },
    Dead,
    Interrupted,
    Superseded,
}

impl ChildTerminal {
    /// Lean B2 projection: Interrupted → Cancelled, all others → Failed.
    pub fn projected_state(&self) -> ToolCallState {
        match self {
            ChildTerminal::Interrupted => ToolCallState::Cancelled,
            _ => ToolCallState::Failed,
        }
    }
    pub const ALL_KIND: &'static [&'static str] =
        &["failed", "dead", "interrupted", "superseded"];
}

pub struct CascadeIntent {
    pub child_request_id: String,
    pub at: chrono::DateTime<chrono::Utc>,
}
```

### `ToolCallLifecycle` struct extensions

```rust
pub struct ToolCallLifecycle {
    // ...existing fields (unchanged)...
    await_mode:       AwaitMode,        // default Foreground
    cancel_policy:    CancelPolicy,     // default Cascade
    child_request_id: Option<String>,   // None = native; Some = subagent invocation
}
```

### Constructors

The existing `ToolCallLifecycle::new(...)` keeps its current signature; defaults the three new fields to `Foreground` / `Cascade` / `None`. New sibling constructor:

```rust
pub fn new_subagent(
    node: Arc<EmbeddedNode>,
    session_id: String,
    tool_call_id: String,
    message_sequence: u32,
    tool_name: String,
    args: String,
    await_mode: AwaitMode,
    cancel_policy: CancelPolicy,
    child_request_id: String,        // required for subagent path
) -> Self
```

Both constructors are synchronous and don't persist (matches existing `new()` semantics; first transition creates the row).

### Transition methods (`tool_call_lifecycle/transition.rs`)

#### Mode-flips

```rust
impl ToolCallLifecycle {
    /// Pending|Running stays; await_mode .foreground → .background.
    pub async fn background(&mut self) -> Result<()>;

    /// Pending|Running stays; await_mode .background → .foreground.
    pub async fn foreground(&mut self) -> Result<()>;

    /// Pending|Running stays; cancel_policy .cascade → .detach (one-way).
    pub async fn detach(&mut self) -> Result<()>;
}
```

Each method:
1. `ensure_state(&[ToolCallState::Running])` (or `[Pending, Running]` for `detach`).
2. Pre-condition check on `await_mode` / `cancel_policy` — returns `IllegalToolCallTransition::ModeAlreadyBackground` etc. if violating.
3. Builds an UPDATE GraphQL mutation that writes only the field being changed.
4. Calls `execute_mutation_with_retry`.
5. Updates the in-memory field on success.

These don't touch `state` — orthogonal to the lifecycle state machine, exactly what the Lean spec promises.

#### Bridge transitions

```rust
impl ToolCallLifecycle {
    /// Bridge complete: parent tool .running → .completed when the linked
    /// child request has reached .completed (caller verifies). Persists
    /// child_result as the tool's `result` field.
    pub async fn bridge_complete(&mut self, child_result: String) -> Result<()>;

    /// Bridge failure: parent tool .running → .failed (or .cancelled for
    /// child .interrupted) when the child request reaches a non-.completed
    /// terminal. Projection per ChildTerminal::projected_state().
    pub async fn bridge_failure(&mut self, child_terminal: ChildTerminal) -> Result<()>;

    /// Bridge cancel cascade: returns the action that should be taken on the
    /// child AgentRequest. Caller (typically R3's daemon interrupt dispatcher)
    /// performs the actual write to set interrupt_requested_at.
    /// Returns None for native tools, detached subagents, or non-cancelled.
    pub async fn bridge_cancel_cascade(&self) -> Result<Option<CascadeIntent>>;
}
```

**Trust boundary:** `bridge_complete` and `bridge_failure` trust the caller to have verified the child's terminal state. This matches the Lean precondition (load-bearing on the caller, not on the constructor) and avoids a coupling between `ToolCallLifecycle` and `AgentRequest` reads. R3's SubagentSource will be the natural place to do the child-state read.

`bridge_cancel_cascade` is pure — no DB writes; returns an intent the caller executes. Tests can stub the executor cleanly.

### `IllegalToolCallTransition` new variants

```rust
ModeAlreadyBackground
ModeAlreadyForeground
PolicyAlreadyDetach
BridgeCompleteRequiresChildLink
BridgeFailureRequiresChildLink
CascadeRequiresCancelled
SubagentDepthExceeded
ParentLinkageIncoherent
```

### `create_subagent_request` helper (`tool_call_lifecycle/subagent_request.rs`)

```rust
pub const MAX_SUBAGENT_DEPTH: u32 = 3;

/// Create a new AgentRequest with subagent parent linkage. Validates
/// depth + 1 ≤ MAX_SUBAGENT_DEPTH and parent linkage coherence (both fields
/// set together).
pub async fn create_subagent_request(
    node: Arc<EmbeddedNode>,
    parent_request_id: String,
    parent_tool_call_id: String,
    parent_subagent_depth: u32,
    behavior_id: String,
    prompt: String,
    deadline: Option<chrono::DateTime<chrono::Utc>>,
    // additional fields mirroring existing AgentRequest creation
) -> Result<String /* new_request_id */>;
```

Internally builds a `CREATE AgentRequest` mutation with parent linkage fields populated and reuses the existing AgentRequest creation logic for the rest. Bucket 3 conformance can call this helper directly to set up real children for testing `bridge_complete` end-to-end without R3's SubagentSource.

### `AgentRequest` Rust DAO extensions

The Rust struct that reads `AgentRequest` rows (in `crates/defra-agent/src/runtime_snapshot/` or sibling) gains three optional fields mirroring the schema:

```rust
pub struct AgentRequestRow {
    // ...existing fields...
    pub subagent_depth: u32,                              // default 0
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
}
```

#### Apply-time validation (in `apply.rs` or wherever AgentRequest validates)

- `subagent_depth` non-negative (trivial via `u32`).
- **Coherence:** `caused_by_parent_request_id.is_some() ↔ caused_by_parent_tool_call_id.is_some()` (both or neither). Mixed → `ParentLinkageIncoherent`.
- `subagent_depth = 0` ↔ both parent fields `None` (top-level vs subagent mutually exclusive).

Cross-reference validation (does parent request exist? does subagent_target resolve?) is R3.

### `ToolSelection` Rust struct extension

```rust
pub struct ToolSelectionDocument {
    // ...existing fields (delegate_to, enable_file_tools, etc.)...
    pub subagent_targets:            Option<Vec<String>>,   // behavior_ids
    pub subagent_spawn_enabled:      Option<bool>,
    pub subagent_steering_enabled:   Option<bool>,
    pub subagent_background_enabled: Option<bool>,
}
```

Apply-time validation: well-formedness only — each entry in `subagent_targets` is non-empty; the three booleans are independent. R3 adds cross-reference validation.

## Conformance

### Bucket 1 — vocabulary round-trip (in-module)

In `tool_call_lifecycle.rs`:

- `AwaitMode::ALL.len() == 2`; round-trip: `from_persisted(v.as_str()) == Some(v)` for both variants.
- `CancelPolicy::ALL.len() == 2`; same round-trip.
- `ChildTerminal::ALL_KIND.len() == 4`; projection partition: `Interrupted → Cancelled`, others → `Failed`.
- `IllegalToolCallTransition` enum closure: every Lean-emitted error vocabulary value has a Rust variant.

### Bucket 2 — Lean transition matrix conformance

In `tests/state_machine_conformance.rs`. For each Lean-emitted legal transition (incl. 3 mode-flips, 3 bridge transitions): assert a corresponding Rust path succeeds. For each illegal: assert the Rust method returns `IllegalToolCallTransition`. Specific cases:

- `background` from `Pending` → `WrongState`.
- `background` from `Running` + already `Background` → `ModeAlreadyBackground`.
- `foreground` from `Background` (running) → ok.
- `detach` already `.detach` → `PolicyAlreadyDetach`.
- `bridge_complete` on tool with `child_request_id = None` → `BridgeCompleteRequiresChildLink`.
- `bridge_cancel_cascade` on non-`.cancelled` tool → `CascadeRequiresCancelled`.

### Bucket 3 — runtime integration

In `tests/tool_call_subagent_lifecycle_conformance.rs`. Real `EmbeddedNode` via `test_db()`. Cases:

- **Mode flip round-trip:** `new`, `start_running`, `background`, verify persisted `await_mode="background"`; `foreground`, verify back. Second `background` → `ModeAlreadyBackground`.
- **Detach one-way:** persisted `cancel_policy="detach"`; second `detach` errors.
- **Bridge complete:** `create_subagent_request` to spawn a child with parent linkage; force child to `.completed` via direct DB write; `new_subagent` parent tool linked; `start_running`; `bridge_complete(child_result)`; verify `state="completed"` and `result=child_result` persisted.
- **Bridge failure projections:** `Failed/Dead/Superseded → state="failed"`; `Interrupted → state="cancelled"`. Each is its own test case.
- **Cascade intent:** `Cascade` policy + child link → `bridge_cancel_cascade()` returns `Some(CascadeIntent)`. `Detach` policy → returns `None`. Native (no `child_request_id`) → returns `None`.
- **Migration round-trip:** insert a v2 row directly, run `ensure_subagent_extensions_migrations`, query, verify defaults populate.
- **`create_subagent_request` depth bound:** parent at depth=2 → child at depth=3 ok. Parent at depth=3 → `SubagentDepthExceeded`.
- **`create_subagent_request` parent coherence:** missing one of parent_request_id / parent_tool_call_id → `ParentLinkageIncoherent`.

### Hook integration — deferred to R3

Today's `hook/persistence.rs:on_tool_call()` constructs `ToolCallLifecycle::new()` (native). The hook doesn't recognize subagent tool names — that's R3 (when `spawn_subagent` and friends land). Bucket 3 tests bypass the hook and call lifecycle methods directly, exactly as R1's Bucket 3 does today.

## Risks

1. **Lens binary size.** Three collections in one WASM. Mitigation: monitor; split into per-collection lenses if past ~500KB.
2. **Migration ordering.** `ensure_subagent_extensions_migrations` must run after `ensure_tool_call_migrations`. Mitigation: explicit serialization in `serve.rs`; assertion at the start of v2→v3 that v1→v2 has run (`AgentToolCall.lifecycle_state` field exists).
3. **Coherence check too strict.** Reject hypothetical mixed-parent-linkage rows. Mitigation: not a real risk — fields are brand new; default-populated rows are coherent by construction.
4. **Bucket 3 fixture complexity.** Constructing a "child request in `.completed`" via direct DB write bypasses normal flow. Mitigation: extract a `make_completed_request(node, ...)` test helper; reuse across all bridge_* tests.
5. **Persisted vocabulary drift.** Lean's `toDefraDB` outputs vs Rust's `as_str()` could drift. Mitigation: Bucket 1's round-trip + Bucket 2's transition matrix consume Lean-emitted JSON as source of truth.
6. **Partial-failure mode of multi-collection migration.** Three sequential `patch_collection` calls; if the second or third fails after the first succeeds, the database is left half-migrated and the daemon refuses to start. The DefraDB migration framework has no automatic rollback (per R1's experience). Mitigation: idempotent detection at the head of `ensure_subagent_extensions_migrations` (re-running the function after a partial failure either picks up where it left off if the existing patch is detectable, or fails again at the same step). Operators may need to manually patch the un-migrated collection in pathological cases. Acceptable for now because partial failure is improbable in practice (the patches are simple field additions, not data transforms).

## References

- B1 Lean spec: `docs/superpowers/specs/2026-05-08-subagent-lifecycle-design.md`
- R1 spec (template): `docs/superpowers/specs/2026-05-08-r1-rust-toolcall-conformance-design.md`
- R1 plan: `docs/superpowers/plans/2026-05-08-r1-rust-toolcall-conformance.md`
- R1 lens crate (template): `crates/defra-agent-lenses/agent_tool_call_lifecycle_v1_to_v2/`
- B1 ToolCallLifecycle: `crates/defra-agent/src/tool_call_lifecycle.rs`, `tool_call_lifecycle/transition.rs`
- B1 migration: `crates/defra-agent/src/migration.rs:35-97`
- TriggerEngine (R3 extension target): `crates/defra-agent/src/trigger_engine/mod.rs`
- Project conventions: `CLAUDE.md`
