# R5: Cross-Deployment Subagent Execution — Design

**Status:** Draft for Jack approval, revision 1
**Date:** 2026-05-14
**Tracks:** R5 (cross-deployment subagents)
**Refs:** #155 (cross-boundary verification strategy), #162 (reverse-pairing TLA+), #168 (persist-before-ack), #176 (cross-deployment completion projection TLA+), #178 (reverse-pairing handler idempotence), #183 (formal-coverage audit follow-ups), #188 (cross-deployment cancel propagation TLA+), #107 (P2P admin reconcile + harness substrate), #180 (P2P admin auth — gates multi-tenant)

**Depends on:**

- `docs/superpowers/specs/2026-05-14-tool-backgrounding-design.md` (R6: parametric `BackgroundedKind`, `Proofs/Background/*` module rename, renamed Rust files). R5 imports the canonical post-R6 vocabulary.
- `docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md` (#176).
- `docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-tla-design.md` (#188).
- `docs/superpowers/specs/2026-05-13-issue-107-p2p-admin-rpc-design.md` (#107: pairing reconcile + conformance harness pattern).

## 1. Goal

R5 turns "agents on deployment A can spawn subagents whose behavior lives on deployment B" from substrate into product, under a **trusted-fleet** trust model (closed-source single-org). The agent surface (`spawn_subagent`, `wait_subagent`, `cancel_subagent`) is unchanged from R4; the parent agent never observes that the spawn target is on a different node. Every cross-boundary behavior R5 needs is already verified by `SubagentCompletion.tla` (#176) or `SubagentCancelPropagation.tla` (#188), and every Lean transition R5 invokes is in the existing `Proofs/Background/*` module set (post-R6 rename).

R5's design discipline is **substrate-first reuse**. The strategic claim of the cross-boundary verification thread (#155 → #162 → #176 → #178 → #188 → #107) is that the substrate generalizes; R5 is the proof that the verification investment paid for product capability. Most of this spec is plumbing — the §6 derived requirements from #176 and §6 from #188 are *consumed*, not re-derived.

This spec produces an implementation plan, not implementation.

## 2. Trust model

**Trusted-fleet only.** R5 v1 is for closed-source single-org fleets where every deployment trusts every other paired peer. Concretely:

- B's `SubagentSource` will materialize a child `AgentRequest` for any replicated parent `AgentToolCall` whose authoring DID (recovered from DefraDB's per-doc identity binding) is in B's paired-peer DID set (read from B's local `PeerPairingDesired` view).
- B does **not** re-validate the spawn against the parent's `ToolSelectionDocument` — that document lives on A and is not replicated. A's spawn-time check is the only authorization gate.
- B's `cancel_cascade_intent_at` mirroring observer trusts any replicated bridge row from a paired peer.

**Multi-tenant deployment is out of scope.** Gate: #180 (P2P admin auth + NAC). Without #180, R5's trust contract is structurally unsafe outside a closed fleet. When #180 closes, NAC gates at the wire layer using DefraDB's existing identity binding; the proofs' structural NAC-bound-auth assumption then holds.

This trust posture is the explicit annotation that #107's spec §11 already names: *"R5 functionally ships under a trusted-fleet trust model without #180 closed; R5 is not appropriate for multi-tenant deployment until #180 closes."*

## 3. Verified obligations and reuse map

Every #176/#188 derived requirement either lands in this spec or in an existing module R5 invokes. Zero new Lean modules; one new recovery predicate. R5's strategic shape is a row in this table per obligation, each landing in an existing artifact.

| Obligation | Source | Reuse target | Notes |
|---|---|---|---|
| Persist-before-projection on A (read from durable local rows) | #176-R1 | `crates/defra-agent/src/background_completion.rs` (post-R6 rename of `subagent_completion.rs`) | Unchanged: existing code already re-loads from A's local DefraDB inside the `EventName::Update` callback. Replicated docs are local rows by the time the projector reads them. |
| Persist-before-observe on B | #176-R2 | defradb.rs replication contract | Wire-contract obligation owed by the replication layer; R5 documents and does not implement. |
| Final response durable before child terminal observable | #176-R3 | defradb.rs replication ordering | Same as above. R5 documents the dependency. |
| Atomic coalesced wake-up insert | #176-R4 | `crates/defra-agent/src/lifecycle/queue.rs` (R4a queue) | Existing; same code path, no cross-deployment dispatch. |
| Projection-side idempotency | #176-R5 | `crates/defra-agent/src/background_completion.rs` | Existing: bridge terminal-state check before invoking `bridge_complete` / `bridge_failure` is already present. |
| Notification-before-wake-up | #176-R6 | `crates/defra-agent/src/background_completion.rs` | Existing ordering preserved by reuse. |
| Cancellation drain source filter | #176-R7 | `crates/defra-agent/src/lifecycle/queue.rs` | Existing; queue source `background_completion` filter unchanged. |
| Late child terminal after cancellation = no-op | #176-R8 | `Proofs/Background/Transition.lean` (post-R6) bridge transitions | Existing: `bridge_complete` / `bridge_failure` are gated on `bridgeState = Running`. Cross-deployment realization is identical. |
| Persist cascade intent before remote delivery (on A) | #188-R1 | New field `cancel_cascade_intent_at` on `AgentToolCall` (§6.4) | Single-writer doc; A persists in its own row. Replication delivers the field to B. |
| Retry cancel from durable intent | #188-R2 | DefraDB replication (at-least-once) + B-side idempotent mirroring | Realized by replication's redelivery + B's idempotent observer. A does not run a retry worker on its own side. |
| B-side persist-before-ack | #188-R3 | B's existing interrupt path | B writes `interruptRequestedAt` on its locally-owned child request durably; B's interrupt handler terminalizes the child as `Interrupted`. R5 chooses the **ackless implementation shape** anticipated by #188's "Open questions" — there is no separate ack channel; A learns of B's handling via the replicated child terminal. The safety boundary is `cancelHandledB` (durable B-side handling); ack-liveness is structurally unnecessary. |
| B-side handler idempotency | #188-R4 | B's mirroring observer | Idempotent on `(child_request_id, cancel_cascade_intent_at)`: re-applying the same intent to a child with `interruptRequestedAt` already set is a no-op. |
| Natural-terminal stable after cancel | #188-R5 | `Proofs/Background/Transition.lean` (R4 invariant B1, post-R6 parametric) | Existing: the child request transition is monotonic; once natural-terminal, it cannot rewrite to `Interrupted`. The mirrored `interruptRequestedAt` is absorbed. |
| Timeouts are liveness-only | #188-R6 | `unclaimed_deadline_at` on bridge row (§6.4) | The only R5 deadline that drives a safety decision is `unclaimed_deadline_at`, which terminalizes the **bridge row on A**, not the child request on B. Cancel-delivery to B has no deadline. |
| Ack visibility diagnostic, not safety | #188-R7 | `cancel_pending_remote_ack` + `stuck_since` on bridge row | A's reconciler clears `cancel_pending_remote_ack` when the child's terminal propagates back. Observability only — not a safety boundary. |

**One new Lean addition** — covered in §8: a new `ToolRecoveryCause` constructor (`unclaimedCrossDeploymentSpawn`) in `Proofs/Recovery/Sweeps.lean`, plus one new entry in `recoverySweepCases` in `Proofs/Recovery/ContractCases.lean`. No new modules; no new structures; the existing `toolCallRecoverySweep` already enumerates non-detached running bridge rows, so the cross-deployment unclaimed case is a strict subset of its existing predicate. Only the cause variant and the terminalState clause for it are new.

**Two existing budgets inherited unchanged:**
- **R6's B7** (`MAX_BACKGROUNDED_TOOLS_PER_PARENT = 8`): every cross-deployment subagent bridge row on A is `await_mode = background` and non-terminal during the wait, so it counts against the same per-parent-request budget that R6 introduced. R5 does not relax or extend B7; the parametric `bridge_spawn` precondition that R6 adds is the same gate R5 spawns must pass.
- **R4's `MAX_SUBAGENT_DEPTH`**: the Subagent-kind recursion bound applies identically across deployments. The child's `subagent_depth = parent_depth + 1` is propagated through the cross-deployment spawn (B's `SubagentSource` reads parent depth from the replicated parent `AgentRequest`).

## 4. Architecture

### 4.1 Roles

- **A** — the deployment hosting the parent behavior. Writes the parent `AgentToolCall` bridge row. Observes the child request terminal via replication; projects via existing `bridge_complete` / `bridge_failure`. Writes its own bridge fields for cascade-cancel intent. Owns the bridge row's lifecycle.
- **B** — the deployment hosting the child behavior. Materializes the child `AgentRequest` from the replicated parent `AgentToolCall`. Executes the child agent loop. Mirrors A's cascade-cancel intent onto its locally-owned child's `interruptRequestedAt`. Owns the child request's lifecycle.
- **Operator** — configures pairing via #107's `PeerPairingDesired` (per-peer collections list), and configures `subagent_targets` on A's `ToolSelectionDocument` to authorize the cross-deployment spawn at A's call site.

The agent on A never observes that B is a separate node. The agent on B never observes that its caller is on a separate node. Cross-deployment is a property of routing, not of the agent surface.

### 4.2 Replicated document set

`PeerPairingDesired` (operator-configured) must include the following collections between paired peers for R5:

- `AgentToolCall` — A → B (parent bridge row, with `child_request_id` and cancel-intent fields)
- `AgentRequest` — both directions (A's parent request; B's child request; replication carries terminal state and `interruptRequestedAt` back to A)
- `AgentResponse` — B → A (final response for the child request)
- `AgentMessage` — B → A (child transcript content for the parent's `<subagent-notification>` payload)

Read direction matters for ACP: each doc is written only by its owner. A writes only A-owned docs; B writes only B-owned docs. There are no cross-peer field writes in v1, deliberately — that lets R5 ship before ACP cross-peer grants land.

### 4.3 Action mapping (TLA+ → real wire)

The action vocabularies of `SubagentCompletion.tla` and `SubagentCancelPropagation.tla` map to the real wire as follows. The conformance harness uses this mapping verbatim.

| TLA+ action | Real-wire realization |
|---|---|
| `OperatorWrite` | Operator writes `PeerPairingDesired` doc (covered by #107) and `ToolSelectionDocument` doc (existing). |
| `Reconcile` | #107's supervisor reconcile tick reads desired/actual and emits collection-install RPCs. |
| `Send`, `Deliver` | DefraDB replication propagates docs from writer's node to paired-peer node. Conformance harness simulates as a `ReplicateDoc(from, to, collection, doc_id)` action. |
| `Drop` | Replication-layer message drop; harness simulates as a no-op for a named doc. |
| `PersistChildTerminal` | B's runtime durably writes the child `AgentRequest` terminal state + `AgentResponse` final response. |
| `EmitTerminalObservation` | Replication carries B's writes to A. |
| `DeliverObservation` / `PersistObservationOnA` | DefraDB replication ingest on A persists the row into A's local replica. A's `EventName::Update` subscription fires on the persisted update. |
| `ProjectTerminal` | A's `background_completion.rs` observer reads the local replica and invokes `bridge_complete` or `bridge_failure`. |
| `AppendNotification` | A's observer writes the `<subagent-notification>` transcript message (existing R4 path). |
| `EnqueueWakeup` | A's observer enqueues the coalesced wake-up via the existing R4a queue. |
| `EnqueueUserRequest` | A's request claim path (existing). |
| `CancelParent` | A's `bridge_cancel_cascade` writes `cancel_cascade_intent_at` on the bridge row and immediately terminalizes the bridge as `Cancelled`. |
| `CancelDrain` | Existing R4a drain in the same-session queue. |
| `ProcessCancel` | B's mirroring observer writes `interruptRequestedAt` on the B-owned child `AgentRequest`. B's existing interrupt path terminalizes the child as `Interrupted` (or absorbs against natural terminal). |
| `Crash(A)` / `Crash(B)` | Process kill; persisted DefraDB state survives. |
| `Timeout` | A's `unclaimed_deadline_at` reconciler tick (the only R5 timeout; see §3 #188-R6). |

## 5. Spawn path (A writes; B claims)

1. **Authorize at A.** Agent on A invokes `spawn_subagent(behavior_id, prompt, await_mode = background)`. The existing R4/R6 spawn hook gates on A's `ToolSelectionDocument.subagent_targets` (operator-configured allowlist). No discovery probe — operator's allowlist is the entire pre-flight contract.
2. **Enforce budgets.** Before any row is written:
   - **R4 depth check** — parent's `subagent_depth + 1 ≤ MAX_SUBAGENT_DEPTH` (unchanged from single-deployment; the child's depth is still parent_depth + 1 across the wire).
   - **R6 B7 budget** — A's parent request's currently non-terminal `await_mode = background` tool-call count plus one ≤ `MAX_BACKGROUNDED_TOOLS_PER_PARENT` (8). Cross-deployment subagent bridges count against the same budget as R6's tool-kind backgrounded rows. Rejection: structured `argument_invalid` error matching R6's existing budget-exceeded payload.
3. **Write parent bridge row.** A pre-allocates `child_request_id` and writes the parent `AgentToolCall`:
   - `await_mode = background`
   - `cancel_policy = cascade`
   - `child_request_id = <pre-allocated>`
   - `unclaimed_deadline_at = now + unclaimed_spawn_timeout_seconds`. Default **60 s**; this is the wall-clock budget A gives every paired peer to claim the spawn and replicate a child `AgentRequest` row back. The default is intentionally tunable per the v1 dogfood trajectory; see §5.1.
   - `cancel_cascade_intent_at = null`, `cancel_pending_remote_ack = false`, `stuck_since = null`
4. **Replicate.** DefraDB replicates the `AgentToolCall` to every paired peer in `PeerPairingDesired`.
5. **B claims (or every peer ignores).** Each paired peer's `SubagentSource` (the existing global `EventName::Update` subscription) fires on the replicated `AgentToolCall`. The existing per-peer filter applies:
   - `snapshot.behavior(&spawn_args.behavior_id).is_some()` — if the peer hosts the target behavior, it claims. If no peer hosts, no peer claims.
   - **New dispatch on parent's authoring DID.** If the parent `AgentRequest`'s DID equals B's local principal DID, B follows the existing single-deployment path (re-load `ToolSelectionDocument`). If the parent's DID is a paired-peer DID (read from B's `PeerPairingDesired` peer set), B skips the auth re-check (trusted-fleet contract; see §2).
6. **B materializes the child.** B's `SubagentSource` calls the existing `create_subagent_request_with_request_id` with the pre-allocated `child_request_id`. The child is created in B's local DefraDB store with `caused_by_parent_request_id`, `caused_by_parent_tool_call_id`, and `subagent_depth = parent_depth + 1`. B's `TriggerEngine` claims the request and runs the agent loop.
7. **A clears the unclaimed deadline.** A's reconciler observes the replicated child `AgentRequest` row (matching the bridge's `child_request_id`) and clears `unclaimed_deadline_at` on its bridge row. From this point the bridge is "linked" and progress is governed by the child's terminal state, not the spawn deadline.

**Unclaimed-spawn failure.** If no paired peer claims within `unclaimed_deadline_at`:
- A's reconciler tick observes the bridge has no replicated child row matching `child_request_id` and current time > `unclaimed_deadline_at`.
- A fires `bridge_failure` with `FailureClass::ServiceUnavailable` and reason `no_peer_claimed_spawn`. Structured error payload to the agent:
  ```json
  {
    "ok": false,
    "failure_class": "service_unavailable",
    "path": "/behavior_id",
    "message": "no paired peer claimed the cross-deployment spawn within unclaimed_spawn_timeout_seconds",
    "retryable": false,
    "service_id": "subagent",
    "tool_name": "spawn_subagent",
    "requested_behavior_id": "<id>",
    "unclaimed_deadline_at": "<ts>"
  }
  ```
- Same predicate is the body of the new `unclaimedCrossDeploymentSpawn` `ToolRecoveryCause` (§8); steady-state reconciler and startup recovery share the implementation.

### 5.1 Tuning the unclaimed-spawn timeout

The 60 s default is chosen for the v1 dogfood trajectory and is **explicitly tunable**. The cost of a false negative (a permanent `bridge_failure` on a healthy-but-slow paired peer under replication lag) is high — the agent gets a `service_unavailable` error even though the child would have materialized seconds later. The cost of a false positive (a long wait on a peer that will never claim) is low — the operator can always cancel the parent.

Two configuration layers, in resolution order:

- **Per-parent-behavior override.** `ToolSelectionDocument` on the parent behavior gains an optional field `cross_deployment_spawn_timeout_seconds: Option<u32>`. This sits next to `subagent_targets` — the operator who knows which behaviors live remotely is the same operator who knows the expected response time. Per-behavior tuning is recommended for any behavior whose target deployment is known to be slow under load.
- **Global default.** `60 s`. Applied when no per-behavior override is set. Revisited from real dogfood telemetry once the cross-deployment flow is exercised under load.

The implementation plan picks the exact resolution helper (likely a thin extension to the existing `ToolSelectionDocument` loader); the spec only commits to the configurability path.

## 6. Completion path (B terminalizes; A projects)

### 6.1 Wire shape

1. B's child request reaches a terminal state — `Completed`, `Failed`, `Dead`, `Interrupted`, or `Superseded` — through the existing single-deployment agent loop. B writes `AgentRequest.state`, `AgentResponse` (final response for `Completed`), and any final `AgentMessage`s durably (#176-R2/R3 obligation; wire-contract on the replication layer).
2. Replication carries the durable rows to A.
3. A's `EventName::Update` subscription in `background_completion.rs` fires on the ingested update.
4. The existing observer code reads the child row + final response from A's **local replica** — satisfying #176-R1 persist-before-projection without any code change. The observer never reads from the volatile subscription event payload; it always re-loads from DefraDB.

### 6.2 Bridge projection

The observer's existing flow (`project_background_subagent_completion`) runs verbatim:

- `Completed` child → `bridge_complete(final_response)` on the parent bridge tool row.
- Non-completed terminal (`Failed`, `Dead`, `Interrupted`, `Superseded`) → `bridge_failure(terminal)`.

Both are R6-parametric `Proofs/Background/Transition.lean` constructors on Subagent-kind rows; the Lean transition fires identically whether the child is local or replicated. The observer then appends the `<subagent-notification>` transcript message (R4 existing path) and enqueues the coalesced wake-up under `background_completion:<parent_session_id>` (R4a queue, existing).

### 6.3 Cancellation-then-completion race

If A's parent has already terminalized the bridge as `Cancelled` (§7) before a late child terminal propagates, the observer's existing bridge-terminal check is a no-op (#176-R8 / `ParentCancelAbsorbsLateTerminal`). Concretely: `background_completion.rs` already gates projection on `edge.lifecycle_state == "running"` and short-circuits with `AlreadyProjected` otherwise. Cross-deployment doesn't change that.

### 6.4 New fields on `AgentToolCall`

| Field | Type | Default | Meaning |
|---|---|---|---|
| `unclaimed_deadline_at` | `Timestamp?` | `now + unclaimed_spawn_timeout_seconds` at spawn (default 60 s; per-behavior tunable per §5.1) | When the parent bridge row should give up waiting for a paired peer to materialize the child. Set on spawn; cleared once the replicated child row appears. |
| `cancel_cascade_intent_at` | `Timestamp?` | `null` | Timestamp at which A's `bridge_cancel_cascade` fired (the durable cascade intent). Single-writer field; A only. |
| `cancel_pending_remote_ack` | `Bool` | `false` | True while A has set `cancel_cascade_intent_at` and not yet observed the child's `interruptRequestedAt` or terminal state in its local replica. Observability only. |
| `stuck_since` | `Timestamp?` | `null` | Timestamp at which `cancel_pending_remote_ack = true` was first observed past `STUCK_CANCEL_THRESHOLD` (default 5 min). Mirrors #107's stuck-retry pattern. Diagnostic only. |

The migration is additive — existing single-deployment rows treat all four fields as defaulted, and the existing transitions are unchanged.

## 7. Cancel path (A intends; B mirrors)

### 7.1 A-side cascade-cancel cascade

When A's parent terminalizes under `cancel_policy = cascade` (parent deadline, explicit user cancel, agent-invoked `cancel_subagent`, R6 `wait_subagent`/`wait_tool` deadline-out), R6's parametric `bridge_cancel_cascade` fires on the bridge row. R5's cross-deployment realization for Subagent kind:

1. A writes `cancel_cascade_intent_at = now` on its own `AgentToolCall` bridge row in the same atomic batch as the bridge state transition to `Cancelled`. The bridge state goes **directly to `Cancelled` terminal** (matches #176's `CancelParent` action — bridge is no longer `Running`; subsequent late child terminals are no-ops on A).
2. A sets `cancel_pending_remote_ack = true` in the same write.
3. The agent caller (whoever invoked `cancel_subagent` or whose `wait_subagent` was preempted) sees the cancel result immediately. The cascade signal converges to B in background under DefraDB's replication; A does not block on B's acknowledgement.

The proof claim from #188-R1 — "persist cascade intent before remote delivery" — is satisfied by atomic in-row persistence on A.

### 7.2 B-side cancel mirror

A new B-side observer (or an extension of `SubagentSource`) subscribes to `EventName::Update` for `AgentToolCall`. When it sees a replicated row with `cancel_cascade_intent_at != null` and a locally-owned child `AgentRequest` matching `child_request_id`:

1. **Trust check.** The parent `AgentToolCall`'s authoring DID must be in B's paired-peer DID set (same check as the spawn path, §5 step 5).
2. **Idempotency check.** If the child request is already terminal, or its `interruptRequestedAt` is already set, no-op (#188-R4).
3. **Mirror.** B writes `interruptRequestedAt = cancel_cascade_intent_at` on the (B-owned) child `AgentRequest` row. Single-row durable write.
4. **Trigger interrupt.** B's existing interrupt path (the same one that handles single-deployment interrupts) terminalizes the child as `Interrupted` — or, if the child has already reached a natural terminal, absorbs the intent without rewriting state (`monotonic progress` property already proven for `AgentRequest`).

There is no separate ack channel. The observation that B has handled the cancel propagates back to A via the existing completion-projection path (§6): the child request's terminal state (`Interrupted` or the absorbed natural terminal) replicates to A; A's `background_completion.rs` observer fires `bridge_complete` / `bridge_failure` on the already-`Cancelled` bridge row, which is a no-op per #176-R8.

### 7.3 A-side observability reconciler

A separate reconciler tick on A (call it `cancel_ack_observer`, can run as part of the same `background_completion` worker):

- For each bridge row with `cancel_pending_remote_ack = true`, check whether A's local replica of the child `AgentRequest` shows `state ∈ Terminal ∨ interruptRequestedAt != null`. If yes, clear `cancel_pending_remote_ack` on the bridge row.
- For each bridge row with `cancel_pending_remote_ack = true` and `cancel_cascade_intent_at` older than `STUCK_CANCEL_THRESHOLD` (default 5 min), set `stuck_since = now` if not already set. Pure observability; emits a tracing warning and surfaces on `ClientPeerStatus`-style runtime dashboards.

`cancel_pending_remote_ack` and `stuck_since` are **never used as safety boundaries**. They cannot terminalize the child; they cannot rewrite the bridge state. They are diagnostic flags only. This preserves #188-R6 (timeouts are liveness-only) and #188-R7 (ack visibility is diagnostic, not safety).

### 7.4 Retry semantics under partition / restart

Because A writes `cancel_cascade_intent_at` once into a single durable row, and DefraDB replication is at-least-once, the retry semantic of #188-R2 (re-emit cancel from durable intent) is realized as:

- **Partition during replication.** Replication retries on its own; once partition heals, the bridge row's `cancel_cascade_intent_at` propagates to B.
- **A crash mid-cancel.** A's bridge row is already persisted with `cancel_cascade_intent_at` set; restart re-runs the existing replication outbox. No A-side retry worker is needed.
- **B crash mid-mirror.** B's mirroring observer is at-least-once; on restart, B's `EventName::Update` subscription (or an explicit sweep, see §8) re-fires for any replicated `AgentToolCall` with `cancel_cascade_intent_at != null` and no `interruptRequestedAt` yet on the local child.

The proof obligation of "fair retry until handling" is realized by replication's at-least-once delivery + B's idempotent observer — no application-layer retry loop on A. This is a cleaner realization than #188's TLA+ shape (which models explicit RPC retries) but every modeled property holds.

## 8. Recovery

### 8.1 New Lean recovery cause

The existing `toolCallRecoverySweep` in `Proofs/Recovery/Sweeps.lean` keys on `row.call.state = .running ∧ ¬ isDetachedBridgeCall row.call` — a predicate that already covers cross-deployment cascade-policy bridges (non-detached, running). R5 does **not** introduce a new sweep; it widens the existing one by adding a new constructor to `ToolRecoveryCause`:

```lean
inductive ToolRecoveryCause where
  | deadlineExceeded
  | parentInterrupted
  | parentTerminal
  | childCompleted
  | childFailed
  | childDead
  | childInterrupted
  | childSuperseded
  | unclaimedCrossDeploymentSpawn   -- NEW (R5)
```

**`terminalState` clause.**

```lean
def terminalState : ToolRecoveryCause → ToolCallState
  | …
  | .unclaimedCrossDeploymentSpawn => .failed
```

**Stale-row predicate (Rust-side, dispatched into the existing sweep).**

```
row.call.state = .running
∧ row.call.awaitMode = .background
∧ row.call.cancelPolicy = .cascade
∧ row.call.childRequestId.isSome
∧ ¬∃ local row in AgentRequest. request_id = row.call.childRequestId
∧ now > row.call.unclaimedDeadlineAt
```

This is a strict subset of `toolCallRecoveryStale`; the existing `h_recover_terminal` and `h_recover_zero` theorem proofs extend automatically by the `cases cause` enumeration. The terminalState mapping (`.failed`) means `bridge_failure` (post-R6 parametric) is the effective transition; `FailureClass::ServiceUnavailable` with reason `no_peer_claimed_spawn` is the Rust-side failure payload.

`Proofs/Recovery/ContractCases.lean` gains one new entry in `recoverySweepCases` named `tool_running_unclaimed_cross_deployment_spawn_to_failed`, with `deadlineAuditRef = "r5-cross-deployment-subagents-design"` (or a stable audit reference picked at implementation time). `recoverySweepCases_registered_sweeps` and `recoverySweepCases_decrease_to_zero` both close by `native_decide` against the widened enum.

**No new module, no new structure, no new sweep, no new transition.** The widening is one new enum constructor + one match-arm + one new test-vector entry. The existing `toolCallRecoverySweep` registration covers both the new cause and all existing causes.

### 8.2 Cross-deployment failure modes that reuse existing sweeps

| Failure | Reuse |
|---|---|
| A crashes mid-wait | A's existing `AgentToolCall` recovery sweep already terminalizes orphaned bridge rows whose parent request is terminal. For cross-deployment, the same sweep applies; the bridge row's state is fully recoverable from durable DefraDB. |
| A's `background_completion` observer crashes after observing terminal but before firing `bridge_complete` | A's existing startup recovery sweep over `AgentToolCall` enumerates bridge rows with `lifecycle_state = .running ∧ await_mode = .background` and, for each one whose local replica of the child `AgentRequest` is already terminal, invokes the projection (#176-R5 idempotency makes this safe even if the subscription also fires). The recovery path lives alongside the existing #189 enumeration — no new sweep predicate is needed because the predicate is a strict subset of what the existing `AgentToolCall` sweep already evaluates. |
| B crashes mid-execution | B's existing `AgentRequest` recovery sweep (#189's `TerminalizeStuckRunning` family) reaches the child request and terminalizes per its own deadline. A observes the terminalization via the existing completion projection. |
| B crashes mid-mirror (cancel cascade intent observed but not yet written to child) | B's `EventName::Update` subscription on restart sees the still-replicated `AgentToolCall` with `cancel_cascade_intent_at != null` and re-runs the mirror observer. Idempotent against an already-set `interruptRequestedAt`. |
| B crashes between spawn observation and child materialization | B's `SubagentSource` re-reads on restart (existing path; the existing `child_request_exists` check is the idempotency gate). |
| Replication paused / partitioned indefinitely | Falls back to `unclaimed_deadline_at` on A (new) for the spawn case, or to existing parent-request `deadline_at` for the in-flight case. |

No further additions to `Proofs/Recovery/Sweeps.lean` are required beyond the new `ToolRecoveryCause` constructor described in §8.1 — the existing `toolCallRecoverySweep` registration already enumerates non-detached running bridge rows, which is exactly the scope R5 needs.

### 8.3 Cancel-retry is not a recovery sweep

A's cancel-retry is **steady-state reconciler behavior**, not a recovery action: each tick of the `cancel_ack_observer` (§7.3) reads `cancel_pending_remote_ack` and checks for B's terminal. Restart of A simply re-runs the same reconciler against the persisted bridge state. No recovery predicate is needed — the persisted `cancel_cascade_intent_at` is the durable intent, replication is the delivery, and the observer is the idempotent applier. This matches #188-R2 ("retry cancel from durable intent") via replication infrastructure, not via an application retry loop.

## 9. Authorization and ACP

### 9.1 v1 (trusted-fleet)

- **A's spawn authorization** is unchanged from R4/R6: gated by `ToolSelectionDocument.subagent_targets` on A.
- **B's spawn-claim trust check** is new: B's `SubagentSource` reads the replicated parent `AgentToolCall`'s authoring DID. If that DID is in B's paired-peer set (read from `PeerPairingDesired`), B trusts the spawn intent without re-validating against any A-side document. Single-deployment claims (where parent DID = local DID) continue to follow the existing `load_parent_subagent_authorization` path.
- **B's cancel-mirror trust check** is identical: the replicated `AgentToolCall` with `cancel_cascade_intent_at != null` is honored if its authoring DID is in B's paired-peer set.
- **No new schema annotations** for actor identity. DefraDB's per-doc identity binding (writer DID recoverable from doc metadata) is the lineage. The future #180 NAC gate plugs into the wire layer using this binding; no schema migration needed.

### 9.2 Cross-peer document writes are deliberately avoided

In R5 v1, **no peer writes to a doc it does not own.** A writes only A-owned docs (parent `AgentRequest`, parent `AgentToolCall`, A-side `AgentMessage`). B writes only B-owned docs (child `AgentRequest`, child `AgentResponse`, child-side `AgentMessage`). The cancel-cascade signal travels by A writing its own bridge row's `cancel_cascade_intent_at` field; B observes via replication and writes its own child row's `interruptRequestedAt`.

This sidesteps the DefraDB ACP question entirely for v1. Cross-peer field-level grants (e.g., letting A mutate a specific field on a B-owned row) is a separate workstream and not required to ship R5.

### 9.3 Multi-tenant (gated on #180)

When #180 closes:
- DefraDB's wire-layer NAC checks every replicated write against an actor identity bound to an operator-published policy.
- B's spawn-claim trust check becomes a NAC-enforced operation rather than a local DID-set lookup.
- Cross-peer field grants become available; a follow-up may simplify the cancel-mirror by letting A write directly to the child's `interruptRequestedAt` (matching the in-process Lean transition literally), with NAC gating who can write that field.

R5 v1 does not depend on #180 closing, but its trust posture is unambiguous: **trusted-fleet only until #180 closes**.

## 10. Conformance harness

R5 extends `crates/defra-agent/tests/support/pairing_conformance/` — the existing two-node scaffold from #107. The harness orchestrates two embedded DefraDB nodes, drives a JSON scenario IR through them, and checks invariants against the TLA+ action vocabulary. Replication between nodes is **simulated** by an explicit `ReplicateDoc` action, not by real libp2p — matching the conformance pattern #107 established. (End-to-end libp2p replication is exercised by separate desktop-core integration tests, not by the conformance harness.)

### 10.1 New scenario IR additions

Existing actions: `OperatorWrite`, `Reconcile`, `Drop`, `Crash`, `WaitForConvergence`.

R5 adds:

| Action | Effect on the two-node fixture |
|---|---|
| `WriteParentToolCall { node, parent_request_id, parent_tool_call_id, child_request_id, behavior_id, await_mode, unclaimed_deadline_at }` | Write the parent `AgentToolCall` on the named node. |
| `WriteAgentRequest { node, request_id, did, behavior_id, state, caused_by_parent_request_id?, caused_by_parent_tool_call_id? }` | Write or update an `AgentRequest` on the named node. |
| `ReplicateDoc { from, to, collection, doc_id }` | Copy a doc from one node's DefraDB store to the other's. Simulates `Send` + `Deliver` + `Process` + `PersistObservation` from the TLA+ models. |
| `TerminalizeChildOnB { request_id, terminal, final_response? }` | Write the child's terminal state and optional final response on B. |
| `CancelParentOnA { parent_request_id, parent_tool_call_id }` | Trigger `bridge_cancel_cascade` on A's bridge row (writes `cancel_cascade_intent_at` and terminalizes the bridge as `Cancelled`). |
| `RunBackgroundCompletionObserverOnA` | Tick A's `background_completion` observer once. |
| `RunCancelMirrorObserverOnB` | Tick B's cancel-mirror observer once. |
| `RunUnclaimedSpawnReconcilerOnA` | Tick A's unclaimed-spawn reconciler once. |
| `RunRecoverySweepOn { node }` | Tick the named node's recovery sweep once (covers cross-restart re-attachment). |
| `AdvanceClockOn { node, seconds }` | Move a node's monotonic clock forward — drives deadline-bearing reconcilers without sleeping the test. |

The scenario IR is line-for-line derivable from the §4.3 action mapping, satisfying #155's "harness IR derives from the action map" requirement.

### 10.2 Invariants checked

After each action and at scenario end, the harness evaluates the safety invariants from `SubagentCompletion.tla` (`BridgeTerminalUnique`, `ProjectionRequiresBDurableTerminal`, `ProjectionMatchesLeanBridgeMapping`, `NotificationIdempotent`, `WakeupCoalesced`, `WakeupCausal`, `CancelDrainPreservesUserPending`, `ParentCancelAbsorbsLateTerminal`) and `SubagentCancelPropagation.tla` (`CancelIntentDurable`, `CancelHandledIdempotent`, `InterruptExactlyOnce`, `CascadeInterruptsOnlyRunning`, `NaturalTerminalStableAfterCancel`, `InterruptedOnlyByCascade`). Liveness targets (`DurableTerminalSettles`, `LiveBridgeTerminalProjects`, `CancelDeliveryProgress`, `LiveCancelInterruptsOrNaturalWins`) are evaluated after a `WaitForConvergence` action.

### 10.3 v1 scenarios

Five scenarios land in v1, each as a JSON file under `crates/defra-agent/tests/fixtures/r5_scenarios/`:

1. **`happy_path.json`** — A writes parent tool call → replicate to B → B materializes child → B terminalizes child as `Completed` with final response → replicate to A → A's observer projects `bridge_complete` → notification appended, wake-up enqueued.
2. **`b_crash_mid_execution.json`** — As (1), but `Crash(B)` mid-execution. After restart, B's recovery sweep terminalizes the child as `Failed` (or `Dead` if past child deadline). Replication delivers to A; A projects `bridge_failure`.
3. **`a_crash_mid_wait.json`** — As (1), but `Crash(A)` is interleaved at two points and exercised independently: (a) after the spawn-write replicates and before B's terminal arrives — A restart re-attaches the `EventName::Update` subscription; B's later terminal then propagates and projects normally; and (b) after B's terminal has already replicated to A but before A's observer fires the projection — A restart runs the recovery sweep (see §8.2), which finds the bridge row with a terminal child locally available and runs the projection path. Both interleavings end in the same observed post-state.
4. **`partition_during_cancel.json`** — A writes parent tool call; B materializes child. `CancelParentOnA` writes `cancel_cascade_intent_at` and terminalizes bridge as `Cancelled`. `Drop` the `AgentToolCall` replication. Tick A's `cancel_ack_observer` — `stuck_since` flips after threshold. Restore replication via `ReplicateDoc`. B's cancel-mirror observer runs, writes `interruptRequestedAt`, B's child terminalizes as `Interrupted`. Replicate back; A's observer fires `bridge_failure` (absorbed as no-op since bridge already `Cancelled`); `cancel_pending_remote_ack` clears.
5. **`multi_completion_coalesce.json`** — Two children spawned in same session. Both terminalize on B simultaneously. Replicate both terminals to A. Two `RunBackgroundCompletionObserverOnA` ticks. Verify: exactly one pending `background_completion:<session_id>` queue row exists (coalesced); both `<subagent-notification>` transcript messages appended; bridge state preserved.

### 10.4 Lean conformance vector

A new conformance vector family `r5_cross_deployment_subagent_*` lands in `crates/defra-agent/proofs/conformance_vectors/` (existing pattern). Each scenario's expected post-state is encoded as a Lean term and exported to JSON; the Rust harness loads the JSON and asserts the observed two-node state matches. No new tooling; reuse existing.

## 11. Out of scope

- **Multi-tenant deployment.** Gated on #180.
- **Foreground cross-deployment subagents.** `await_mode = foreground` cross-deployment is deferred; introduces parent-progress liveness obligations not modeled in #176 or #188.
- **Detach cancel policy.** Cascade only in v1.
- **Cross-deployment subagent discovery surface.** No `HostedBehavior` or `AgentBehavior` replication. Operator's `subagent_targets` + `unclaimed_deadline_at` is the entire pre-flight contract.
- **R6 cross-deployment backgrounded tools** (`background_tool` for `bash`/MCP across nodes). R6 v1 is single-deployment; cross-deployment R6 is a future composition of R5 + R6.
- **Cross-peer ACP grants** (e.g., A writing directly to a B-owned doc's specific field). Sidestepped in v1 by the doc-mirror design.
- **A-side cancel-retry worker.** Replication is the retry; no application-layer retry loop on A.
- **Forced-local-cancel (orphan acceptance).** A's bridge does not unilaterally terminalize on stuck cascade-ack; only observability surfaces. Strict #188 conformance.
- **Real-libp2p replication in the conformance harness.** Harness simulates replication as an action. End-to-end libp2p coverage is the responsibility of existing desktop-core integration tests.
- **R4c surfaces** (`list_*` / `read_*_transcript` / `steer_*`). Sibling brainstorm; R5 does not touch.

## 12. Approval checklist

Before implementation planning, Jack should approve:

- the trust posture: trusted-fleet only in v1, multi-tenant gated on #180
- spawn-locus decision: B materializes the child from a replicated parent `AgentToolCall`; A only writes A-owned docs
- discovery-locus decision: no discovery doc; operator's `subagent_targets` + `unclaimed_deadline_at` is the pre-flight contract
- unclaimed-spawn deadline mechanic: new field `unclaimed_deadline_at`, default 30 s, fires `bridge_failure(service_unavailable, no_peer_claimed_spawn)`
- cancel-wire decision: A writes `cancel_cascade_intent_at` on its own bridge row; B mirrors onto its own child's `interruptRequestedAt`; no cross-peer ACP grant required
- cancel state semantics: bridge terminalizes `Cancelled` immediately on A; cascade signal converges in background; `cancel_pending_remote_ack` + `stuck_since` are observability flags only
- authorization model: B trusts paired-peer authoring DID for both spawn-claim and cancel-mirror; no new actor-DID annotation; DefraDB's per-doc identity binding is the lineage; #180 plugs into the wire layer
- replicated doc set: `AgentRequest`, `AgentResponse`, `AgentToolCall`, `AgentMessage` between paired peers
- the Lean recovery widening: one new `ToolRecoveryCause` constructor (`unclaimedCrossDeploymentSpawn`) on the existing `toolCallRecoverySweep` + one new entry in `recoverySweepCases`; everything else reuses existing sweeps verbatim
- R5 inherits R6's B7 budget (`MAX_BACKGROUNDED_TOOLS_PER_PARENT = 8`) and R4's depth bound unchanged
- the cross-deployment unclaimed-spawn timeout default of 60 s with per-behavior tunability through `ToolSelectionDocument.cross_deployment_spawn_timeout_seconds`
- harness scope: extend `pairing_conformance/`; five v1 scenarios; replication simulated as an action; invariants from both TLA+ artifacts
- explicit deferral of multi-tenant, foreground, detach, R6-cross-deployment, ACP grants, forced-local-cancel, R4c, real-libp2p in conformance, and discovery surfaces
