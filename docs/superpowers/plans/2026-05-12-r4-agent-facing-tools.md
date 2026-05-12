# R4 Agent-Facing Subagent Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` for this plan. Execute one task at a time with a fresh implementation subagent, then a spec-compliance reviewer, then a code-quality reviewer. Do not start a later task until the current task is reviewed, formatted, verified, and committed.

**Goal:** Implement the approved R4 design in `docs/superpowers/specs/2026-05-12-r4-agent-facing-tools-design.md`.

Do not start coding until Jack approves this plan.

R4 ships in two ordered parts:

1. **R4a:** Lean queue model + claim-deadline preservation + Rust scheduler queue semantics.
2. **R4b:** v1 agent-facing subagent tools: `spawn_subagent`, `wait_subagent`, and `cancel_subagent`, plus R3 authorization hardening and hook/runtime integration.

R4c follow-ups (`list_subagents`, `read_subagent_transcript`, `steer_subagent`) are designed in the spec but deferred from this plan.

## Cadence

For every task:

1. Spawn a fresh implementation subagent with the task section as its prompt.
2. Tell the worker: "You are not alone in the codebase. Do not revert edits made by others. Own only the files listed in this task unless you discover a blocker."
3. After the implementation pass, run:

```bash
cargo fmt --all
```

4. Spawn a fresh spec-compliance reviewer. Ask it to compare the diff against the approved R4 spec and this task.
5. Spawn a fresh code-quality reviewer. Ask it to review only bugs, regressions, maintainability risks, and missing tests.
6. Address reviewer findings.
7. Run the task's focused verify commands.
8. Before commit, run the broader CI commands:

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent-cli
```

9. Commit one task at a time.

## Lean Properties To Re-Verify

R4a changes request/session scheduling semantics and request claim deadline behavior. Re-run and keep these properties green:

- **S1 terminal irreversibility:** `Proofs/Properties/Safety.lean::terminal_irreversibility`
- **S3 progress monotonicity:** `Proofs/Properties/Safety.lean::progress_monotonic`
- **S4 deadline bounding:** `Proofs/Properties/Safety.lean::completed_not_deadline_expired` and `deadline_structural_bound`
- **S5 recovery blocks claims:** `Proofs/Properties/Safety.lean::recovery_blocks_claims`
- **S6 persistence before completion:** `Proofs/Properties/Safety.lean::persistence_before_completion`
- **Request-local monotonic fields:** `interrupt_monotonicity` and `valid_until_monotonicity`
- **Composed foreground blocking:** `Proofs/Composed.lean` request-step guard and `all_tools_terminal_unblocks_request_progress`
- **Composed deadline/tool propagation:** `deadline_exceeded_request_timesOut_running_tools` and `deadline_exceeded_request_cancels_pending_tools`
- **Liveness:** `Proofs/Properties/Liveness.lean::phase_change_decreases_measure`, `claimed_eventually_terminal`, and `recovery_convergence`
- **Session request invariants:** `Proofs/SessionRecovery.lean::latestFlagInvariant` and reissue deadline/origin/backend preservation theorems, unless R4a introduces a new `Proofs/Session/*` queue model that supersedes them
- **Runtime reconcile admission:** `Proofs/RuntimeReconcile/*` request admission/generation/session binding theorems

Verify after every Lean task:

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r4-lean-contract.json
```

---

## R4a Task 1: Add Lean Session Queue Model

**Purpose:** Replace "same-session pending is duplicate" with a formal queue model: one active request per session, later same-session requests wait pending in created order, and automated wake-ups can coalesce by queue key.

**Files:**

- Modify or create: `crates/defra-agent/proofs/Proofs/Session/State.lean`
- Modify or create: `crates/defra-agent/proofs/Proofs/Session/Transition.lean`
- Modify or create: `crates/defra-agent/proofs/Proofs/Session/Executable.lean`
- Modify or create: `crates/defra-agent/proofs/Proofs/Session/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean`
- Modify: `crates/defra-agent/proofs/Proofs/SessionRecovery.lean` only if the new queue model should absorb retry/reissue semantics

**Steps:**

- [ ] Inspect current request and session-related models:

```bash
sed -n '1,240p' crates/defra-agent/proofs/Proofs/Request/State.lean
sed -n '1,240p' crates/defra-agent/proofs/Proofs/Request/Transition.lean
sed -n '1,220p' crates/defra-agent/proofs/Proofs/SessionRecovery.lean
sed -n '1,220p' crates/defra-agent/proofs/Proofs/RuntimeReconcile/State.lean
```

- [ ] Add a first-class session queue abstraction. Minimal vocabulary:

```lean
namespace SessionQueue

inductive QueueSource where
  | user
  | subagentCompletion
  | steering
  deriving DecidableEq, Repr

inductive QueuePolicy where
  | append
  | coalesce
  deriving DecidableEq, Repr

structure QueueEntry where
  requestId : RequestId
  createdAt : Time
  source : QueueSource
  policy : QueuePolicy
  queueKey : Option String
  queuedAfter : Option RequestId
  deriving Repr

structure SessionQueueState where
  sessionId : SessionId
  active : Option RequestId
  pending : List QueueEntry
  terminal : Finset RequestId
  deriving Repr

end SessionQueue
```

Adapt names and imports to local Lean style. If `String` is too concrete for queue keys in current proof conventions, introduce an opaque `QueueKey` in `Proofs/Basic.lean`.

- [ ] Define transitions:
  - append pending entry
  - coalesce pending entry by `(sessionId, queueKey)`
  - claim next pending only when `active = none`
  - finish active request
  - drain automated wake-ups by source/key on cancellation

- [ ] Prove queue invariants:
  - at most one active request per session
  - claim only selects the earliest pending entry by `createdAt`
  - coalescing never creates two pending entries with the same non-empty queue key
  - draining automated wake-ups does not delete terminal history

- [ ] Import the new module from `Proofs.lean`.

**Verify:**

```bash
cd crates/defra-agent/proofs && lake build
```

**Commit message:**

```text
Add Lean session queue model for R4a
```

---

## R4a Task 2: Update Lean Request Claim Deadline Semantics

**Purpose:** Make request claim preserve an existing explicit deadline instead of always setting `currentTime + 1`.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Request/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Request/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Request/Executable.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Request/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

**Steps:**

- [ ] Add an explicit optional submitter deadline field to `RequestContext`, leaving `deadline` as the effective runtime deadline used by S4 deadline proofs:

```lean
requestDeadline : Option Time := none
```

- [ ] Add an explicit helper for effective claim deadline selection:

```lean
namespace RequestContext

def claimDeadline (pre : RequestContext) : Time :=
  pre.requestDeadline.getD (pre.currentTime + 1)

end RequestContext
```

- [ ] Change relational `Transition.claim` and executable `Action.claim` so claim preserves explicit deadlines and only synthesizes a behavior-duration deadline when no explicit deadline exists.

- [ ] Re-prove S4 deadline bounding and the composed tool-deadline synchronization lemmas.

- [ ] Add Lean witness cases for both branches:
  - `requestDeadline = some t` persists `deadline = t` after claim
  - `requestDeadline = none` persists `deadline = currentTime + 1` after claim

**Verify:**

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
```

**Commit message:**

```text
Preserve explicit request deadlines in Lean claim model
```

---

## R4a Task 3: Emit Queue/Deadline Conformance Cases

**Purpose:** Ensure Rust tests can detect drift in queue admission and claim deadline preservation.

**Files:**

- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Types.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs`
- Add or modify: `crates/defra-agent/tests/state_machine_conformance.rs`

**Steps:**

- [ ] Emit deterministic witness rows for:
  - active request blocks later same-session claim without superseding it
  - terminal active request allows the next pending same-session request to claim
  - coalesce by `subagent_completion:<session_id>` keeps one pending wake-up
  - cancel drains automated wake-ups but preserves user pending work
  - claim preserves an explicit deadline

- [ ] Add `coverage_ledger` rows for each new emitted group.

- [ ] Register Rust consumers in `conformance_consumers.rs`.

- [ ] Add Rust conformance tests that parse the new Lean witness rows and assert the planned runtime behavior.

**Verify:**

```bash
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r4-contract.json
cargo test -p defra-agent --test state_machine_conformance
cargo test -p defra-agent --test state_machine_conformance lean_contract_coverage_ledger_accounts_for_every_emitted_domain
```

**Commit message:**

```text
Emit R4 queue and deadline conformance witnesses
```

---

## R4a Task 4: Implement Rust Queue Metadata Helpers

**Purpose:** Parse and write `AgentRequest.metadata.queue` without schema changes.

**Files:**

- Add: `crates/defra-agent/src/lifecycle/queue.rs`
- Modify: `crates/defra-agent/src/lifecycle.rs`
- Modify: `crates/defra-agent/src/watcher.rs`
- Modify: `crates/defra-agent/src/watcher/query.rs`
- Add tests in: `crates/defra-agent/src/lifecycle/queue.rs` under `#[cfg(test)]`

**Steps:**

- [ ] Add queue metadata structs:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RequestQueueMetadata {
    pub queue: QueueHints,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueueHints {
    pub source: QueueSource,
    pub policy: QueuePolicy,
    pub key: Option<String>,
    pub queued_after_request_id: Option<String>,
}
```

- [ ] Support string values:
  - `source`: `user`, `subagent_completion`, `steering`
  - `policy`: `append`, `coalesce`

- [ ] Add helpers:
  - `parse_queue_hints(metadata: Option<&str>) -> Option<QueueHints>`
  - `queue_metadata_json(hints: &QueueHints) -> String`
  - `is_automated_wakeup(metadata: Option<&str>) -> bool`

- [ ] Extend `AgentRequest` in `watcher.rs` and `watcher/query.rs` to carry `metadata`.

**Verify:**

```bash
cargo test -p defra-agent --lib lifecycle::queue
cargo test -p defra-agent --lib watcher
```

**Commit message:**

```text
Add AgentRequest queue metadata helpers
```

---

## R4a Task 5: Preserve Explicit Deadlines In Rust Claim

**Purpose:** Make `RequestLifecycle::claim_inner` preserve `AgentRequest.deadline` when present.

**Files:**

- Modify: `crates/defra-agent/src/watcher.rs`
- Modify: `crates/defra-agent/src/watcher/query.rs`
- Modify: `crates/defra-agent/src/lifecycle/claim.rs`
- Modify tests in: `crates/defra-agent/tests/lifecycle_claim.rs`
- Modify tests in: `crates/defra-agent/tests/subagent_source_conformance.rs`

**Steps:**

- [ ] Add `deadline: Option<String>` or parsed `DateTime<Utc>` to `watcher::AgentRequest`.

- [ ] Hydrate `deadline` in `AGENT_REQUEST_FIELDS`.

- [ ] In `claim_inner`, compute:

```rust
let synthesized_deadline_at = now + chrono::Duration::seconds(self.deadline_duration_secs as i64);
let deadline_at = self
    .request
    .deadline
    .as_deref()
    .and_then(parse_rfc3339_utc)
    .unwrap_or(synthesized_deadline_at);
```

Keep the parent-deadline bound in the subagent spawn path; do not silently extend a child beyond its parent.

- [ ] Add regression test:
  - create pending `AgentRequest` with explicit deadline earlier than behavior duration
  - claim it
  - assert persisted deadline equals explicit value

**Verify:**

```bash
cargo test -p defra-agent --test lifecycle_claim
cargo test -p defra-agent --test subagent_source_conformance create_subagent_request
```

**Commit message:**

```text
Preserve explicit AgentRequest deadlines through claim
```

---

## R4a Task 6: Change Scheduler Dedup To Queue Same-Session Requests

**Purpose:** Stop superseding later same-session pending requests merely because another same-session request is active.

**Files:**

- Modify: `crates/defra-agent/src/lifecycle/query.rs`
- Modify: `crates/defra-agent/src/lifecycle/claim.rs`
- Modify: `crates/defra-agent/src/lifecycle/transition.rs`
- Modify tests in: `crates/defra-agent/tests/lifecycle_claim.rs`
- Modify tests in: `crates/defra-agent/src/watcher/tests.rs`

**Steps:**

- [ ] Update `check_deduplication` so same-session later pending rows produce "wait behind active" rather than `dedup_lose`.

- [ ] Treat ordinary same-session pending rows as queued work by default, regardless of whether queue metadata is present. Queue metadata refines queue behavior for coalescing and cancellation drain; it does not opt rows into queueing.

- [ ] Preserve supersession only for explicit replacement paths that are already modeled outside same-session admission, such as retry/reissue/latest-only code paths with a concrete discriminator. Do not use "same session and another active row exists" as the discriminator.

- [ ] Ensure watcher pending-pickup still orders by `created_at ASC`.

- [ ] Add tests:
  - processing request plus later pending queued request: later row remains pending
  - once earlier request terminalizes, later pending row can be claimed
  - unqueued duplicate behavior stays covered by existing supersession tests

**Verify:**

```bash
cargo test -p defra-agent --test lifecycle_claim
cargo test -p defra-agent --lib watcher
```

**Commit message:**

```text
Queue same-session pending requests instead of superseding them
```

---

## R4a Task 7: Implement Queue Coalescing And Cancellation Drain

**Purpose:** Support automated subagent wake-up queue semantics.

**Files:**

- Modify: `crates/defra-agent/src/lifecycle/queue.rs`
- Modify: `crates/defra-agent/src/lifecycle/materialize.rs`
- Modify: `crates/defra-agent/src/interrupt.rs`
- Modify: `crates/defra-agent/src/agent/daemon/request.rs`
- Add tests in: `crates/defra-agent/tests/lifecycle_queue.rs`

**Steps:**

- [ ] Add helper to enqueue same-session request:

```rust
pub(crate) async fn enqueue_session_request(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    content: &str,
    execution_origin: ExecutionOrigin,
    queue_hints: QueueHints,
) -> Result<EnqueuedAgentRequest>
```

It writes the same `session_id` and behavior as the parent session.

- [ ] For `policy=coalesce`, first query for pending same-session requests with the same queue key and source. If one exists, return it instead of creating another.

- [ ] Add drain helper:

```rust
pub(crate) async fn drain_automated_wakeups(
    node: &EmbeddedNode,
    session_id: &str,
    reason: &str,
) -> Result<usize>
```

Terminalize matching pending wake-ups as `interrupted` or `superseded`; do not delete rows.

- [ ] Call drain helper from request/session cancellation paths.

- [ ] Add tests:
  - coalescing creates one pending wake-up for multiple subagent completions
  - cancel drains automated wake-ups
  - user/replacement pending requests are not drained

**Verify:**

```bash
cargo test -p defra-agent --test lifecycle_queue
cargo test -p defra-agent --lib interrupt
```

**Commit message:**

```text
Add same-session queue coalescing and wake-up drain
```

---

## R4b Task 8: Plumb Subagent Tool Selection Into Runtime Tool Surface

**Purpose:** Preserve R2 `ToolSelectionDocument.subagent_*` fields through runtime config and register only R4b tools.

**Files:**

- Modify: `crates/defra-agent/src/agent.rs`
- Modify: `crates/defra-agent/src/tool_surface/mod.rs`
- Modify: `crates/defra-agent/src/tool_surface/selection.rs`
- Modify: `crates/defra-agent/src/toolset/mod.rs`
- Add: `crates/defra-agent/src/toolset/subagent.rs`
- Add tests in: `crates/defra-agent/src/toolset/tests.rs`

**Steps:**

- [ ] Add a runtime struct:

```rust
#[derive(Debug, Clone, Default)]
pub(crate) struct SubagentToolConfig {
    pub targets: Vec<String>,
    pub spawn_enabled: bool,
    pub background_enabled: bool,
}
```

- [ ] Map `ToolSelectionDocument.subagent_targets`, `subagent_spawn_enabled`, and `subagent_background_enabled` into the behavior runtime config.

- [ ] Register only:
  - `spawn_subagent`
  - `wait_subagent`
  - `cancel_subagent`

- [ ] Do not register R4c tools yet.

**Verify:**

```bash
cargo test -p defra-agent --lib toolset
cargo test -p defra-agent --lib document_config
```

**Commit message:**

```text
Register R4b subagent tools from ToolSelection
```

---

## R4b Task 9: Add Subagent Query Helpers

**Purpose:** Centralize parent-child edge lookup, authorization, child session lookup, and final response extraction.

**Files:**

- Add: `crates/defra-agent/src/subagent_tools.rs`
- Modify: `crates/defra-agent/src/lib.rs` if needed for test visibility
- Add tests in: `crates/defra-agent/src/toolset/subagent/tests.rs`

**Steps:**

- [ ] Add helper to load parent request context for current tool execution:

```rust
pub(crate) struct ParentSubagentContext {
    pub session_id: String,
    pub request_id: String,
    pub behavior_id: String,
    pub subagent_depth: u32,
    pub request_deadline_at: chrono::DateTime<chrono::Utc>,
    pub allowed_targets: Vec<String>,
}
```

- [ ] Add helper to resolve an authorized child edge by `child_request_id`:

```rust
pub(crate) struct ChildEdge {
    pub parent_tool_call_id: String,
    pub child_request_id: String,
    pub child_session_id: String,
    pub behavior_id: String,
    pub await_mode: AwaitMode,
    pub lifecycle_state: String,
}
```

- [ ] Add `load_child_final_response` that uses `AgentResponse.materialized_message_sequence` and child `AgentMessage`, never `AgentResponse.content`.

- [ ] Add `ChildTerminal` projection helper from child `AgentRequest.status/lifecycle_state`.

**Verify:**

```bash
cargo test -p defra-agent --lib subagent
cargo test -p defra-agent --lib toolset::subagent
```

**Commit message:**

```text
Add subagent edge and child response query helpers
```

---

## R4b Task 10: Implement `spawn_subagent` Bridge Creation

**Purpose:** Agent-facing spawn creates a subagent bridge row, materializes the child request, and returns either a background receipt or waits foreground.

**Files:**

- Modify: `crates/defra-agent/src/hook.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle/subagent_request.rs`
- Modify: `crates/defra-agent/src/toolset/subagent.rs`
- Add tests in: `crates/defra-agent/tests/r4_subagent_tools.rs`

**Steps:**

- [ ] Define args:

```rust
#[derive(Debug, serde::Deserialize)]
struct SpawnSubagentArgs {
    behavior_id: String,
    prompt: String,
    #[serde(default = "default_foreground")]
    await_mode: AwaitModeArg,
    deadline: Option<chrono::DateTime<chrono::Utc>>,
}
```

- [ ] Validate:
  - spawn enabled
  - target in `subagent_targets`
  - background mode requires `subagent_background_enabled`
  - child deadline <= parent request deadline
  - `parent.subagent_depth + 1 <= MAX_SUBAGENT_DEPTH`

- [ ] Create `ToolCallLifecycle::new_subagent` with `CancelPolicy::Cascade`.

- [ ] Materialize child via `create_subagent_request_with_request_id` and return both child ids.

- [ ] Ensure unauthorized target persists `FailureClass::ServiceUnavailable` and raw `tool_not_allowed`.

- [ ] Ensure depth rejection persists `FailureClass::ArgumentInvalid`.

**Verify:**

```bash
cargo test -p defra-agent --test r4_subagent_tools spawn_subagent
cargo test -p defra-agent --test subagent_source_conformance create_subagent_request_enforces_depth_boundary
```

**Commit message:**

```text
Implement spawn_subagent bridge creation
```

---

## R4b Task 11: Implement Foreground Wait And Background Receipt Paths

**Purpose:** Complete `spawn_subagent` runtime behavior after child materialization.

**Files:**

- Modify: `crates/defra-agent/src/toolset/subagent.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle/runtime.rs`
- Add tests in: `crates/defra-agent/tests/r4_subagent_tools.rs`

**Steps:**

- [ ] For `await_mode=background`, return:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "await_mode": "background",
  "status": "running"
}
```

Do not `bridge_complete` the parent bridge row yet.

- [ ] For `await_mode=foreground`, poll/subscribe until child terminal, parent deadline, parent cancellation, or user/operator backgrounding.

- [ ] On child completed, call `bridge_complete(final_response)` and return terminal envelope.

- [ ] On child failed/dead/interrupted/superseded, call `bridge_failure(child_terminal)` and return terminal envelope.

- [ ] On parent deadline, call `bridge_failure(ChildTerminal::Dead)` and return synthetic failure.

- [ ] On parent cancellation, use existing cancel cascade path.

**Verify:**

```bash
cargo test -p defra-agent --test r4_subagent_tools foreground_spawn
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance integration_bridge
```

**Commit message:**

```text
Complete spawn_subagent foreground and background paths
```

---

## R4b Task 12: Implement Background Completion Projection And Wake-Up

**Purpose:** When a background child reaches terminal, project the bridge row, append parent transcript notification, and enqueue/coalesce a same-session wake-up.

**Files:**

- Add or modify: `crates/defra-agent/src/subagent_completion.rs`
- Modify: `crates/defra-agent/src/agent/runtime/startup.rs`
- Modify: `crates/defra-agent/src/session/history.rs`
- Modify: `crates/defra-agent/src/lifecycle/queue.rs`
- Add tests in: `crates/defra-agent/tests/r4_subagent_completion.rs`

**Steps:**

- [ ] Add a background observer for child `AgentRequest` terminal transitions linked by `caused_by_parent_request_id` / `caused_by_parent_tool_call_id`.

- [ ] Load the parent `AgentToolCall` bridge row by parent session/tool call id.

- [ ] Project terminal state:
  - completed -> `bridge_complete(final_response)`
  - failed/dead/interrupted/superseded -> `bridge_failure(child_terminal)`

- [ ] Append synthetic user-role `<subagent-notification>` into parent session.

- [ ] Enqueue/coalesce wake-up:

```rust
QueueHints {
    source: QueueSource::SubagentCompletion,
    policy: QueuePolicy::Coalesce,
    key: Some(format!("subagent_completion:{parent_session_id}")),
    queued_after_request_id: Some(parent_request_id),
}
```

- [ ] Add the interleaving test from the spec: foreground child A blocks, background child B completes, B notification appends immediately, wake-up remains pending until parent request terminalizes.

**Verify:**

```bash
cargo test -p defra-agent --test r4_subagent_completion
cargo test -p defra-agent --test r4_subagent_tools background_spawn
```

**Commit message:**

```text
Project background subagent completion into parent sessions
```

---

## R4b Task 13: Implement Hook-Intercepted `wait_subagent`

**Purpose:** `wait_subagent` waits on the existing bridge row without creating its own `AgentToolCall`.

**Files:**

- Modify: `crates/defra-agent/src/hook.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Modify: `crates/defra-agent/src/toolset/subagent.rs`
- Add tests in: `crates/defra-agent/tests/r4_subagent_tools.rs`

**Steps:**

- [ ] Register `wait_subagent` in tool schema, but intercept it before ordinary tool-call lifecycle persistence.

- [ ] Args:

```json
{
  "child_request_id": "..."
}
```

- [ ] Authorize through parent-child edge.

- [ ] Wait on the original bridge row/child terminal. Do not write a new `AgentToolCall`.

- [ ] Return the same terminal envelope as foreground `spawn_subagent`.

- [ ] Test that no `AgentToolCall` row exists with `tool_name = "wait_subagent"` after the call.

**Verify:**

```bash
cargo test -p defra-agent --test r4_subagent_tools wait_subagent
cargo test -p defra-agent --test tool_call_subagent_lifecycle_conformance
```

**Commit message:**

```text
Implement wait_subagent without a lifecycle row
```

---

## R4b Task 14: Implement `cancel_subagent`

**Purpose:** Let a parent stop an authorized child session, including active request, live descendants, and queued child work.

**Files:**

- Modify: `crates/defra-agent/src/toolset/subagent.rs`
- Modify: `crates/defra-agent/src/interrupt.rs`
- Modify: `crates/defra-agent/src/lifecycle/queue.rs`
- Modify: `crates/defra-agent/src/hook.rs`
- Add tests in: `crates/defra-agent/tests/r4_subagent_tools.rs`

**Steps:**

- [ ] Args:

```json
{
  "child_request_id": "...",
  "reason": "optional human-readable reason"
}
```

- [ ] Authorize through parent-child edge.

- [ ] Interrupt the child session's active request.

- [ ] Cascade-cancel live child descendants through existing bridge cancellation.

- [ ] Drain queued child-session requests created by automated wake-ups or future steering.

- [ ] Return compact envelope:

```json
{
  "child_request_id": "...",
  "child_session_id": "...",
  "status": "cancelled",
  "active_interrupted": true,
  "queued_drained": 2
}
```

**Verify:**

```bash
cargo test -p defra-agent --test r4_subagent_tools cancel_subagent
cargo test -p defra-agent --lib hook::tests::cancelling_cascade_subagent_tool_latches_child_interrupt
```

**Commit message:**

```text
Implement cancel_subagent for child sessions
```

---

## R4b Task 15: Harden R3 SubagentSource And Recovery Authorization

**Purpose:** Prevent non-R4 bridge-row writers from bypassing `subagent_targets`.

**Files:**

- Modify: `crates/defra-agent/src/trigger_engine/subagent_source.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle/recovery.rs`
- Modify: `crates/defra-agent/src/subagent_tools.rs`
- Modify tests in: `crates/defra-agent/tests/subagent_source_conformance.rs`

**Steps:**

- [ ] Add helper:

```rust
pub(crate) async fn parent_authorizes_subagent_target(
    node: &EmbeddedNode,
    parent_request_id: &str,
    target_behavior_id: &str,
) -> Result<bool>
```

Resolve parent request -> parent behavior -> `ToolSelectionDocument` -> `subagent_targets`.

- [ ] In `SubagentSource`, reject unauthorized targets before `create_subagent_request_with_request_id`.

- [ ] In orphan subagent recovery, reject unauthorized targets before materializing orphan child.

- [ ] Persist/log using `ServiceUnavailable` plus raw `tool_not_allowed` shape where a tool lifecycle row is failed.

- [ ] Add tests:
  - authorized source row materializes
  - unauthorized source row does not materialize
  - recovery does not materialize unauthorized orphan

**Verify:**

```bash
cargo test -p defra-agent --test subagent_source_conformance
cargo test -p defra-agent --lib tool_call_lifecycle::recovery
```

**Commit message:**

```text
Harden SubagentSource authorization for R4
```

---

## R4b Task 16: Add End-To-End R4 Tool Runtime Tests

**Purpose:** Cover the integrated path across tool registration, hook interception, queueing, lifecycle rows, cancellation, and background completion.

**Files:**

- Add or extend: `crates/defra-agent/tests/r4_subagent_tools.rs`
- Add or extend: `crates/defra-agent/tests/r4_subagent_completion.rs`
- Modify: `crates/defra-agent/tests/support/mod.rs`

**Scenarios:**

- [ ] Tool registration:
  - no tools when `subagent_spawn_enabled=false`
  - R4b tools present when enabled
  - background mode rejected when `subagent_background_enabled=false`

- [ ] Foreground spawn:
  - returns final response from child `AgentMessage`
  - maps `Failed`, `Dead`, `Interrupted`, `Superseded`
  - parent deadline returns synthetic `dead`

- [ ] Background spawn:
  - returns receipt
  - bridge row remains running until child terminal
  - completion appends notification and enqueues one coalesced wake-up

- [ ] Wait:
  - no `wait_subagent` `AgentToolCall` row
  - returns same terminal envelope as foreground spawn

- [ ] Cancel:
  - interrupts active child
  - cascades descendants
  - drains queued child-session wake-ups

- [ ] Authorization/depth:
  - unauthorized target gives `tool_not_allowed` / `ServiceUnavailable`
  - max depth rejects at parent depth `3`

**Verify:**

```bash
cargo test -p defra-agent --test r4_subagent_tools
cargo test -p defra-agent --test r4_subagent_completion
cargo test -p defra-agent --test subagent_source_conformance
```

**Commit message:**

```text
Add end-to-end R4 subagent tool coverage
```

---

## Task 17: Final Polish And Full CI

**Purpose:** Run broad verification, update docs if code drifted from the approved design, and prepare the final implementation branch.

**Files:**

- Modify only if needed:
  - `docs/superpowers/specs/2026-05-12-r4-agent-facing-tools-design.md`
  - `docs/superpowers/plans/2026-05-12-r4-agent-facing-tools.md`
  - `crates/defra-agent/proofs/README.md`

**Steps:**

- [ ] Run formatting and diff checks:

```bash
cargo fmt --all
git diff --check
```

- [ ] Run Lean:

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/r4-final-contract.json
```

- [ ] Run broader CI:

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent-cli
```

- [ ] Inspect for accidental R4c implementation:

```bash
rg -n "list_subagents|read_subagent_transcript|steer_subagent" crates/defra-agent/src crates/defra-agent/tests
```

Expected: only schema/design references or explicit deferred stubs; no registered R4c tools.

- [ ] Commit final docs/polish if needed.

**Commit message:**

```text
Polish R4 subagent tool implementation
```
