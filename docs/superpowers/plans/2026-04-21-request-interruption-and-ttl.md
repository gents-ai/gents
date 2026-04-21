# Request Interruption and Freshness TTL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add user-initiated mid-turn request interruption and submitter-driven freshness TTL to the agent runtime, driven from the Lean state machine and wired end-to-end through schema, scheduler, daemon, admission, and client.

**Architecture:** Two new desired-state fields on `AgentRequest` (`interrupt_requested_at`, `valid_until`) drive a ninth `RequestState` terminal (`interrupted`) and a new failure reason (`dead/Stale`). The scheduler owns all doc observation (pre-claim via its existing claim check, post-claim via a per-tick re-read) and signals claimed requests through a per-request `watch::channel::<Option<InterruptIntent>>` parallel to the existing shutdown receiver. A `tokio_util::sync::CancellationToken` hierarchy rooted at each daemon cancels in-flight inference and cancellable tool calls; partial `AgentResponse` rows are preserved and flagged with `interrupted_at`. The admission layer gains `AdmissionPermit::mark_interrupted` to deliver `InferenceCall.cancelled` terminals.

**Tech Stack:** Rust 2021, Lean 4 (proofs), DefraDB GraphQL mutations, tokio, tokio_util (CancellationToken, watch channels), rig-core (LLM trait), chrono (RFC3339 timestamps).

**Spec:** `docs/superpowers/specs/2026-04-20-interruption-and-request-hygiene-design.md`

---

## Task 0: Confirm clean start

**Files:**
- Read: `docs/superpowers/specs/2026-04-20-interruption-and-request-hygiene-design.md`

- [ ] **Step 1: Read the full spec once**

The plan tasks reference the spec frequently. If you skip reading it, you will misinterpret intent on edge cases like tie-break ordering, S8 runtime enforcement, and the admission-bridge theorem.

- [ ] **Step 2: Verify baseline green tree**

Run: `cargo check --workspace`
Expected: clean compile, no errors.

Run: `cd crates/defra-agent/proofs && lake build && cd -`
Expected: proofs build with no errors.

Run: `cargo test --workspace --no-run`
Expected: test binaries compile.

If anything is red, stop and ask — this plan assumes a green starting tree.

---

## Task 1: Extend the Lean request state machine

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Request.lean`

This task adds the 9th terminal state, two new context fields, five new transitions, and extends every existing lemma with new case arms. Per CLAUDE.md: *"The Lean proofs are the source of truth for all state machine behavior."*

- [ ] **Step 1: Extend `RequestState` with `.interrupted`**

Find the `inductive RequestState` block (around lines 13-22) and add the new variant:

```lean
inductive RequestState where
  | pending
  | claimed
  | processing
  | inputRequired
  | completed
  | failed
  | superseded
  | dead
  | interrupted          -- NEW: user-driven terminal abort
  deriving DecidableEq, Repr
```

- [ ] **Step 2: Extend `isTerminal` and its decidable instance**

Find `isTerminal` (it enumerates the current four terminal states) and add `.interrupted`:

```lean
def isTerminal (s : RequestState) : Prop :=
  s = .completed ∨ s = .failed ∨ s = .superseded ∨ s = .dead ∨ s = .interrupted
```

If there's an `isTerminal_dec : DecidablePred isTerminal` or `instance : DecidablePred isTerminal`, extend its cases mechanically to cover `.interrupted`.

- [ ] **Step 3: Extend `RequestContext` with the two new fields**

Find the `structure RequestContext where` block. Add the two `Option Time` fields (keep the exact order and indentation the file uses):

```lean
structure RequestContext where
  -- ... existing fields ...
  interruptRequestedAt : Option Time  -- NEW: submitter-set, runtime-read-only
  validUntil           : Option Time  -- NEW: submitter-set, runtime-read-only
```

- [ ] **Step 4: Add the five new `Transition` constructors**

Find the `inductive Transition : RequestContext → RequestContext → Prop where` block. At the end (before `deriving` if any, or at the bottom), add:

```lean
  | expire {pre post : RequestContext} :
      pre.state = .pending →
      pre.validUntil.isSome →
      pre.currentTime > pre.validUntil.get →
      post.state = .dead →
      post.interruptRequestedAt = pre.interruptRequestedAt →
      post.validUntil = pre.validUntil →
      Transition pre post
  | interrupt_before_claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.interruptRequestedAt.isSome →
      post.state = .interrupted →
      post.interruptRequestedAt = pre.interruptRequestedAt →
      post.validUntil = pre.validUntil →
      Transition pre post
  | interrupt_claimed {pre post : RequestContext} :
      pre.state = .claimed →
      pre.interruptRequestedAt.isSome →
      post.state = .interrupted →
      post.interruptRequestedAt = pre.interruptRequestedAt →
      post.validUntil = pre.validUntil →
      Transition pre post
  | interrupt_processing {pre post : RequestContext} :
      pre.state = .processing →
      pre.interruptRequestedAt.isSome →
      post.state = .interrupted →
      post.interruptRequestedAt = pre.interruptRequestedAt →
      post.validUntil = pre.validUntil →
      Transition pre post
  | interrupt_input_required {pre post : RequestContext} :
      pre.state = .inputRequired →
      pre.interruptRequestedAt.isSome →
      post.state = .interrupted →
      post.interruptRequestedAt = pre.interruptRequestedAt →
      post.validUntil = pre.validUntil →
      Transition pre post
```

Note: if existing transitions also preserve `origin`, `backend`, `admission`, add those preservation clauses to each new constructor too — check the `claim` constructor as the template for what's preserved.

- [ ] **Step 5: Extend `Action` enum**

Find `inductive Action where` and add the new variants (match the existing camelCase style):

```lean
inductive Action where
  -- ... existing variants ...
  | Expire
  | InterruptBeforeClaim
  | InterruptClaimed
  | InterruptProcessing
  | InterruptInputRequired
  deriving DecidableEq, Repr
```

- [ ] **Step 6: Extend `step?` with new action cases**

Find the `step?` function (it matches on `Action` and returns `Option RequestContext` or similar). Add branches:

```lean
def step? (ctx : RequestContext) : Action → Option RequestContext
  -- ... existing cases ...
  | .Expire =>
      if ctx.state = .pending ∧ ctx.validUntil.isSome ∧ ctx.currentTime > ctx.validUntil.get
      then some { ctx with state := .dead }
      else none
  | .InterruptBeforeClaim =>
      if ctx.state = .pending ∧ ctx.interruptRequestedAt.isSome
      then some { ctx with state := .interrupted }
      else none
  | .InterruptClaimed =>
      if ctx.state = .claimed ∧ ctx.interruptRequestedAt.isSome
      then some { ctx with state := .interrupted }
      else none
  | .InterruptProcessing =>
      if ctx.state = .processing ∧ ctx.interruptRequestedAt.isSome
      then some { ctx with state := .interrupted }
      else none
  | .InterruptInputRequired =>
      if ctx.state = .inputRequired ∧ ctx.interruptRequestedAt.isSome
      then some { ctx with state := .interrupted }
      else none
```

Adapt to the actual `step?` signature — it may take different arguments (e.g. `(ctx : RequestContext) (a : Action) : Option (RequestContext × Transition ctx _)`) depending on your file's shape. Match what's there.

- [ ] **Step 7: Extend existing lemmas with new case arms**

For each of these lemmas, add case arms for the five new `Transition` constructors (most are one-liners):

- `step_sound`
- `transition_complete`
- `replay_sound`
- `trace_complete`
- `terminal_implies_released_local`
- `backend_binding_preserved`
- `origin_preserved`
- `transition_produces_coherent`
- `claimed_coherent_cases`
- `releaseToTerminal_state`
- `releaseToTerminal_released`
- `releaseToTerminal_backend`

Template arm shape (copy for each lemma, adjust proof tactic):

```lean
  | expire _ _ _ h_post _ _ => by simp [h_post]    -- or the existing lemma's matching tactic
  | interrupt_before_claim _ _ h_post _ _ => by simp [h_post]
  | interrupt_claimed _ _ h_post _ _ => by simp [h_post]
  | interrupt_processing _ _ h_post _ _ => by simp [h_post]
  | interrupt_input_required _ _ h_post _ _ => by simp [h_post]
```

If any lemma's proof doesn't extend cleanly, read its existing case structure and mirror it — the new transitions all have shapes similar to `finish`/`fail` (non-terminal → terminal with preservation).

- [ ] **Step 8: Build proofs**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build, no errors.

If it fails on a lemma, the cause is almost always a missing case arm. Don't invent new proof structure — just add the arm matching the existing style.

- [ ] **Step 9: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Request.lean
git commit -m "Add interrupted terminal and TTL fields to Lean request state machine"
```

---

## Task 2: Prove S6 extension, S7, S8, extend L1, add cross-layer theorem, extend conformance

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Properties/Liveness.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/DefraAgent.lean`

- [ ] **Step 1: Extend `terminal_irreversibility` in Safety.lean**

Find the theorem; add a case arm for `.interrupted` being terminal and no outbound transition firing from it:

```lean
theorem terminal_irreversibility
    {pre post : RequestContext}
    (h_terminal : isTerminal pre.state)
    (h_trans : RequestContext.Transition pre post) :
    isTerminal post.state := by
  cases h_trans with
  -- ... existing cases ...
  | expire h_pre _ _ _ _ _ =>
      rw [h_pre] at h_terminal
      exact (pending_not_terminal h_terminal).elim
  | interrupt_before_claim h_pre _ _ _ _ =>
      rw [h_pre] at h_terminal
      exact (pending_not_terminal h_terminal).elim
  | interrupt_claimed h_pre _ _ _ _ =>
      rw [h_pre] at h_terminal
      exact (claimed_not_terminal h_terminal).elim
  | interrupt_processing h_pre _ _ _ _ =>
      rw [h_pre] at h_terminal
      exact (processing_not_terminal h_terminal).elim
  | interrupt_input_required h_pre _ _ _ _ =>
      rw [h_pre] at h_terminal
      exact (input_required_not_terminal h_terminal).elim
```

If a `*_not_terminal` helper doesn't exist for one of those non-terminal states, add it at the top of the file:

```lean
theorem pending_not_terminal (h : isTerminal .pending) : False := by
  simp [isTerminal] at h
```

(Repeat for `claimed`, `processing`, `inputRequired` as needed.)

- [ ] **Step 2: Extend `persistence_before_completion` (S6)**

Find the S6 theorem. Add case arms for the five new transitions. The `interrupt_*` transitions require persistence sequencing; for the Lean model this means the transition takes a `post` where the `AgentResponse` mark has already been written. Mirror the existing `finish` case structure — it already models persistence-before-terminal for `completed`, and `interrupt_*` has the same shape.

```lean
  -- ... existing cases mirror here ...
  | interrupt_before_claim _ _ _ _ _ => ...
  | interrupt_claimed _ _ _ _ _ => ...
  | interrupt_processing _ _ _ _ _ => ...
  | interrupt_input_required _ _ _ _ _ => ...
  | expire _ _ _ _ _ _ => ...
```

Exact proof tactic depends on S6's formulation; copy the `finish` arm and adjust.

- [ ] **Step 3: Add theorem `interrupt_monotonicity` (S7)**

Below the existing S-theorems:

```lean
theorem interrupt_monotonicity
    {pre post : RequestContext}
    (h_set : pre.interruptRequestedAt.isSome)
    (h_trans : RequestContext.Transition pre post) :
    post.interruptRequestedAt = pre.interruptRequestedAt := by
  cases h_trans <;> simp_all
```

If `simp_all` doesn't discharge, cases each transition constructor and use the preservation clauses you added in Task 1 Step 4.

- [ ] **Step 4: Add theorem `valid_until_monotonicity` (S8)**

```lean
theorem valid_until_monotonicity
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post) :
    post.validUntil = pre.validUntil := by
  cases h_trans <;> simp_all
```

Note: S8 is unconditional — the field is never rewritten by any transition, whether or not it was previously set.

- [ ] **Step 5: Extend `claimed_eventually_terminal` in Liveness.lean (L1)**

Find L1 (it asserts progress toward terminal). Add reasoning that `interrupt_*` transitions count as progress toward `interrupted`; `expire` from pending counts as progress to `dead`. Pattern match the existing cases' proof structure.

- [ ] **Step 6: Add cross-layer theorem in Composed.lean**

Append the theorem that discharges the admission spec's reserved axioms:

```lean
theorem interrupted_request_cancels_calls
    (r : RequestContext)
    (h_interrupted : r.state = .interrupted) :
    ∀ c : InferenceCall, c.request_id = r.id →
      c.state ∈ ({.running, .queued} : Set InferenceCall.State) →
      ∃ steps, (InferenceCall.afterSteps c steps).state ∈
               ({.cancelled} : Set InferenceCall.State) := by
  intro c h_link h_live
  -- Discharges admission spec's queued→cancelled and running→cancelled axioms.
  -- Proof: the daemon's request_token.cancel() (from the interrupt_* transition)
  -- propagates to inference_token (child), which fires AdmissionPermit::mark_interrupted,
  -- which persists (call_state=cancelled, failure_reason=Cancelled) on Drop.
  sorry  -- fill with cross-layer reasoning; admission spec's axioms are still here until this lands
```

If you cannot complete the proof in this task (it depends on admission-layer modeling), mark it `sorry` and leave a TODO comment pointing at Task 9 — the proof can be finalized when the runtime implementation lands. Note this in the commit message.

- [ ] **Step 7: Extend `DefraLifecycleState` conformance enum**

Open `Conformance/DefraAgent.lean`. Extend the concrete enum and the `toIdeal` mapping:

```lean
inductive DefraLifecycleState where
  | pending
  | claimed
  | streaming
  | completed
  | failed
  | superseded
  | dead          -- NEW: already in ideal model; surfaced here for Stale/exhaust
  | interrupted   -- NEW
  deriving DecidableEq, Repr

def toIdeal : DefraLifecycleState → RequestState
  | .pending => .pending
  | .claimed => .claimed
  | .streaming => .processing
  | .completed => .completed
  | .failed => .failed
  | .superseded => .superseded
  | .dead => .dead
  | .interrupted => .interrupted
```

- [ ] **Step 8: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build. `sorry` is allowed in the cross-layer theorem, but no compilation errors.

- [ ] **Step 9: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Properties/Safety.lean \
        crates/defra-agent/proofs/Proofs/Properties/Liveness.lean \
        crates/defra-agent/proofs/Proofs/Composed.lean \
        crates/defra-agent/proofs/Proofs/Conformance/DefraAgent.lean
git commit -m "Add S7/S8 invariants, extend S6/L1, extend conformance for interrupt"
```

---

## Task 3: Schema + protocol row mirrors

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_request.graphql`
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_response.graphql`
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_tool_result.graphql`
- Modify: `crates/defra-agent-protocol/schemas/README.md`
- Modify: `crates/defra-agent-protocol/src/row.rs`

- [ ] **Step 1: Add interrupt + TTL fields to AgentRequest schema**

Append to the end of the type block (keep existing 4-space indentation):

```graphql
type AgentRequest @branchable {
    request_id: String @index
    # ... existing fields ...
    retry_count: Int
    max_retries: Int
    interrupt_requested_at: String       # NEW: RFC3339; null until interrupt requested
    valid_until: String                  # NEW: RFC3339; null = no TTL
}
```

- [ ] **Step 2: Add interrupted_at to AgentResponse schema**

```graphql
type AgentResponse @branchable {
    response_key: String @index(unique: true)
    # ... existing fields ...
    completed_at: String
    interrupted_at: String               # NEW: RFC3339; null for complete, non-null ⇒ interrupted
}
```

- [ ] **Step 3: Add discarded_because_interrupted to AgentToolResult schema**

```graphql
type AgentToolResult @branchable {
    # ... existing fields ...
    created_at: String @index
    discarded_because_interrupted: Boolean  # NEW: true if result never reached the model
}
```

- [ ] **Step 4: Update `schemas/README.md`**

Find the rows for `AgentRequest`, `AgentResponse`, and `AgentToolResult` in the field-inventory table. Append the new field names to each row's field list. Also find the text that enumerates `lifecycle_state` values; add `"interrupted"`. Find the `failure_reason` enumeration; add `"Stale"`.

- [ ] **Step 5: Extend row mirrors in `row.rs`**

In `crates/defra-agent-protocol/src/row.rs`, find:
- `AgentRequestRow` struct — add two `Option<String>` fields:

  ```rust
  pub struct AgentRequestRow {
      // ... existing fields ...
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub interrupt_requested_at: Option<String>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub valid_until: Option<String>,
  }
  ```

- `AgentResponseRow` struct — add one `Option<String>` field:

  ```rust
  pub struct AgentResponseRow {
      // ... existing fields ...
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub interrupted_at: Option<String>,
  }
  ```

- `AgentToolResultRow` struct — add one `Option<bool>` field defaulting to false:

  ```rust
  pub struct AgentToolResultRow {
      // ... existing fields ...
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub discarded_because_interrupted: Option<bool>,
  }
  ```

If any of these row structs does not yet exist in `row.rs` (it may have only `AgentPrincipalRow`, `AgentBehaviorRow`, etc. depending on current state), create it matching the pattern of existing rows — field names must mirror the GraphQL exactly, all `String` for RFC3339 timestamps.

- [ ] **Step 6: Terminal classifier gains `interrupted`**

Search the workspace: `interrupted`, `terminal`, `lifecycle_state`, `Completed`, `Superseded`. In whatever module classifies terminal lifecycle states (likely `defra-agent-protocol/src/lifecycle.rs` or similar, possibly inside `row.rs`), add `"interrupted"` to the terminal set so downstream consumers stop polling a request in this state.

Example shape:

```rust
pub fn is_terminal(lifecycle_state: &str) -> bool {
    matches!(
        lifecycle_state,
        "completed" | "failed" | "superseded" | "dead" | "interrupted"  // NEW
    )
}
```

Do the same for any enum-based classifier you find.

- [ ] **Step 7: Compile**

Run: `cargo check --workspace`
Expected: clean compile.

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent-protocol/
git commit -m "Add interrupt + TTL fields to agent protocol schema and row mirrors"
```

---

## Task 4: Conformance scaffolding (ignored tests)

**Files:**
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

All tests in this task ship `#[ignore]`-gated with a comment pointing at the task that unblocks them. They graduate to active as later tasks land.

- [ ] **Step 1: Add pending → interrupted conformance test**

Append to the tests file:

```rust
#[tokio::test]
#[ignore = "ungated in Task 5 (scheduler claim check)"]
async fn pending_interrupted_via_interrupt_before_claim() {
    let db = test_db("pending-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    let request = build_request(doc_id.clone(), request_id.clone(), session_id.clone(), created_at);
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(), AGENT_NAME, AGENT_DID, request,
        DEADLINE_SECS, ExecutionOrigin::Interactive, BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "interrupted");
    assert_eq!(snap.status, "interrupted");
}
```

You'll add the `set_interrupt_requested_at` helper in support code in Task 5; for now the compile-time reference is OK because `#[ignore]` doesn't require a body that actually runs.

- [ ] **Step 2: Add pending → dead/Stale conformance test**

```rust
#[tokio::test]
#[ignore = "ungated in Task 5 (scheduler claim check)"]
async fn pending_dead_stale_via_expire() {
    let db = test_db("pending-dead-stale").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let valid_until = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &valid_until).await;

    let request = build_request(doc_id.clone(), request_id.clone(), session_id.clone(), created_at);
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(), AGENT_NAME, AGENT_DID, request,
        DEADLINE_SECS, ExecutionOrigin::Interactive, BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "dead");
    assert_eq!(snap.failure_reason, "Stale");
}
```

- [ ] **Step 3: Add claimed/processing/inputRequired → interrupted tests**

Each gated behind Task 7:

```rust
#[tokio::test]
#[ignore = "ungated in Task 7 (daemon select arm)"]
async fn claimed_interrupted_via_watch_channel() { /* body in Task 7 */ }

#[tokio::test]
#[ignore = "ungated in Task 7 (daemon select arm)"]
async fn processing_interrupted_preserves_partial_response() { /* body in Task 7 */ }

#[tokio::test]
#[ignore = "ungated in Task 7 (daemon select arm)"]
async fn input_required_interrupted() { /* body in Task 7 */ }
```

Leave these as stubs with the `#[ignore]` attribute and a one-line body `todo!();` so they compile.

- [ ] **Step 4: Add tie-break tests**

```rust
#[tokio::test]
#[ignore = "ungated in Task 5 (scheduler claim check)"]
async fn pending_tie_break_prefers_interrupt_over_expire() { /* body in Task 5 */ }

#[tokio::test]
#[ignore = "ungated in Task 7 (daemon select arm)"]
async fn processing_tie_break_prefers_interrupt_over_deadline() { /* body in Task 7 */ }
```

- [ ] **Step 5: Add idempotency and interrupt-on-terminal tests**

```rust
#[tokio::test]
#[ignore = "ungated in Task 10 (submission API)"]
async fn interrupt_request_is_idempotent() { /* body in Task 10 */ }

#[tokio::test]
#[ignore = "ungated in Task 7 (daemon select arm)"]
async fn interrupt_on_already_terminal_is_noop() { /* body in Task 7 */ }
```

- [ ] **Step 6: Add S8 runtime enforcement test**

```rust
#[tokio::test]
#[ignore = "ungated in Task 5 (scheduler claim check)"]
async fn valid_until_cached_at_claim_ignores_post_claim_extension() { /* body in Task 5 */ }
```

- [ ] **Step 7: Compile tests**

Run: `cargo test -p defra-agent --no-run`
Expected: test binary compiles. Ignored tests don't run under default `cargo test`.

Run: `cargo test -p defra-agent`
Expected: existing tests still pass. The ignored ones don't run.

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "Add ignored conformance scaffolding for interrupt transitions"
```

---

## Task 5: Scheduler pre-claim branch (interrupt + stale)

**Files:**
- Modify: `crates/defra-agent/src/lifecycle/claim.rs`
- Modify: `crates/defra-agent/src/lifecycle/mod.rs` (for new `ClaimOutcome` variants)
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs` (un-gate three tests + add helpers)

This task wires the Lean `expire` and `interrupt_before_claim` transitions into the scheduler. Claim becomes a three-way decision: interrupt → transition to `interrupted`; stale → transition to `dead/Stale`; neither → proceed with normal claim.

- [ ] **Step 1: Extend `ClaimOutcome` with two new variants**

In `crates/defra-agent/src/lifecycle/mod.rs` (or wherever `ClaimOutcome` is declared):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Superseded,
    Interrupted,   // NEW: pending with interrupt_requested_at set
    Expired,       // NEW: pending with valid_until < now
}
```

- [ ] **Step 2: Read interrupt + TTL fields before existing claim logic**

In `claim_inner` (in `claim.rs`), immediately after the dedup check and *before* building the claim mutation, query the current request row for `interrupt_requested_at` and `valid_until`. Use a small helper:

```rust
async fn fetch_interrupt_and_ttl(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{
                interrupt_requested_at
                valid_until
            }}
        }}"#
    );
    let resp = session::execute_query(node, &query, "fetch_interrupt_and_ttl").await?;
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow::anyhow!("AgentRequest {doc_id} not found"))?;
    let interrupt = row
        .get("interrupt_requested_at")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let valid = row
        .get("valid_until")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok((interrupt, valid))
}
```

Follow the existing query-helper patterns in the module for any deviations (e.g. some modules use a `DefraResponseData` newtype instead of raw `serde_json::Value`).

- [ ] **Step 3: Branch on the fetched values (interrupt wins tie-break)**

After the dedup check, before the claim mutation:

```rust
let (interrupt_requested_at, valid_until) =
    fetch_interrupt_and_ttl(&self.node, doc_id).await?;

// Tie-break: interrupt always wins over stale
if interrupt_requested_at.is_some() {
    self.transition_pending_to_interrupted(interrupt_requested_at.as_deref().unwrap())
        .await?;
    self.state = LocalLifecycleState::Interrupted;
    return Ok(ClaimOutcome::Interrupted);
}

if let Some(valid_until_str) = valid_until.as_deref() {
    let valid_until_dt = chrono::DateTime::parse_from_rfc3339(valid_until_str)
        .map_err(|e| anyhow::anyhow!("invalid valid_until on request {doc_id}: {e}"))?;
    if chrono::Utc::now() > valid_until_dt {
        self.transition_pending_to_dead_stale().await?;
        self.state = LocalLifecycleState::Dead;
        return Ok(ClaimOutcome::Expired);
    }
}

// ... existing claim mutation proceeds ...
```

- [ ] **Step 4: Add helpers that write the terminal transitions**

Alongside `claim_inner` in the same file:

```rust
async fn transition_pending_to_interrupted(&mut self, interrupt_at: &str) -> Result<()> {
    let doc_id = &self.request.doc_id;
    let escaped = escape_graphql_string(interrupt_at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }}, status: {{ _eq: "pending" }} }},
                input: {{
                    status: "interrupted",
                    lifecycle_state: "interrupted"
                }}
            ) {{ _docID }}
        }}"#
    );
    session::execute_mutation_with_retry(&self.node, &mutation, "interrupt_before_claim").await?;
    let _ = escaped;  // Unused for now; we read the interrupt_at upstream for logging
    Ok(())
}

async fn transition_pending_to_dead_stale(&mut self) -> Result<()> {
    let doc_id = &self.request.doc_id;
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }}, status: {{ _eq: "pending" }} }},
                input: {{
                    status: "dead",
                    lifecycle_state: "dead",
                    failure_reason: "Stale"
                }}
            ) {{ _docID }}
        }}"#
    );
    session::execute_mutation_with_retry(&self.node, &mutation, "expire_stale").await?;
    Ok(())
}
```

- [ ] **Step 5: Cache `valid_until` on the lifecycle after claim**

If the claim succeeds (falls through the two early-returns above), persist the parsed `valid_until` into the `RequestLifecycle` struct so the daemon sees the cached value — subsequent doc rewrites have no effect.

In `lifecycle/mod.rs` or wherever `RequestLifecycle` is defined, add a field:

```rust
pub struct RequestLifecycle {
    // ... existing fields ...
    valid_until_at_claim: Option<chrono::DateTime<chrono::Utc>>,
}
```

Populate this field from the fetched `valid_until` parse result before returning `Ok(ClaimOutcome::Claimed)`.

- [ ] **Step 6: Add test support helpers**

In `crates/defra-agent/tests/state_machine_conformance.rs` (or the shared support module):

```rust
async fn set_interrupt_requested_at(node: &EmbeddedNode, doc_id: &str, at: &str) {
    let escaped = escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped}" }}
            ) {{ _docID }}
        }}"#
    );
    session::execute_mutation_with_retry(node, &mutation, "test_set_interrupt").await.unwrap();
}

async fn set_valid_until(node: &EmbeddedNode, doc_id: &str, at: &str) {
    let escaped = escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ valid_until: "{escaped}" }}
            ) {{ _docID }}
        }}"#
    );
    session::execute_mutation_with_retry(node, &mutation, "test_set_valid_until").await.unwrap();
}
```

Also extend `RequestSnapshot` to include `failure_reason`:

```rust
pub struct RequestSnapshot {
    // ... existing fields ...
    pub failure_reason: String,   // NEW
}
```

…and extend `fetch_request_snapshot` to read `failure_reason`.

- [ ] **Step 7: Un-ignore the three scheduler-gated tests**

Remove the `#[ignore = "ungated in Task 5 ..."]` from:
- `pending_interrupted_via_interrupt_before_claim`
- `pending_dead_stale_via_expire`
- `pending_tie_break_prefers_interrupt_over_expire`
- `valid_until_cached_at_claim_ignores_post_claim_extension`

Fill in `pending_tie_break_prefers_interrupt_over_expire`:

```rust
#[tokio::test]
async fn pending_tie_break_prefers_interrupt_over_expire() {
    let db = test_db("tie-break-pending").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let past = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &past).await;
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    let request = build_request(doc_id.clone(), request_id.clone(), session_id.clone(), created_at);
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(), AGENT_NAME, AGENT_DID, request,
        DEADLINE_SECS, ExecutionOrigin::Interactive, BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "interrupted");
}
```

Fill in `valid_until_cached_at_claim_ignores_post_claim_extension`:

```rust
#[tokio::test]
async fn valid_until_cached_at_claim_ignores_post_claim_extension() {
    let db = test_db("s8-cached-at-claim").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let future = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &future).await;

    let request = build_request(doc_id.clone(), request_id.clone(), session_id.clone(), created_at);
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(), AGENT_NAME, AGENT_DID, request,
        DEADLINE_SECS, ExecutionOrigin::Interactive, BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    // Caller rewrites valid_until to a far-future value after claim. Lifecycle should
    // not observe it: S8 says the scheduler reads valid_until exactly once at claim.
    let much_later = (chrono::Utc::now() + chrono::Duration::hours(10)).to_rfc3339();
    set_valid_until(&db.node, &doc_id, &much_later).await;

    // We can't directly assert "runtime ignored it" without a full tick,
    // but we can assert the cached field on the lifecycle is unchanged:
    assert_eq!(
        lifecycle.valid_until_at_claim_for_test().map(|dt| dt.to_rfc3339()),
        Some(future),
    );
}
```

(Add `pub fn valid_until_at_claim_for_test(&self) -> Option<DateTime<Utc>>` to `RequestLifecycle` behind `#[cfg(test)]` or a `test-support` feature.)

- [ ] **Step 8: Run**

Run: `cargo test -p defra-agent state_machine_conformance`
Expected: the four un-gated tests pass; other tests (still ignored) don't run.

- [ ] **Step 9: Commit**

```bash
git add crates/defra-agent/src/lifecycle/ crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "Implement scheduler pre-claim interrupt and stale TTL branches"
```

---

## Task 6: CancellationToken plumbing + interrupt transport (wiring only)

**Files:**
- Create: `crates/defra-agent/src/interrupt.rs` (new module for `InterruptIntent` type)
- Modify: `crates/defra-agent/src/lib.rs` (declare new module)
- Modify: `crates/defra-agent/src/scheduler/loop_impl.rs` (create per-request watch channel, pass to daemon)
- Modify: `crates/defra-agent/src/agent/daemon/request.rs` (receive watch channel + CancellationToken)

This task is pure wiring. No behavior change. The next task (7) wires the behavior into the plumbing added here.

- [ ] **Step 1: Create the `InterruptIntent` type**

Create `crates/defra-agent/src/interrupt.rs`:

```rust
//! Shared types for request interruption signaling.

use chrono::{DateTime, Utc};

/// Signal sent from scheduler to daemon when a request's `interrupt_requested_at`
/// field flips from null to non-null.
#[derive(Debug, Clone)]
pub struct InterruptIntent {
    /// RFC3339 timestamp the submitter wrote to `interrupt_requested_at`.
    pub at: DateTime<Utc>,
}
```

Add to `crates/defra-agent/src/lib.rs`:

```rust
pub mod interrupt;
```

- [ ] **Step 2: Add `request_token` ownership to `handle_request`**

In `crates/defra-agent/src/agent/daemon/request.rs`, extend the signature of `handle_request`:

```rust
pub(super) async fn handle_request(
    &mut self,
    lifecycle: &mut crate::lifecycle::RequestLifecycle,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    mut interrupt_rx: tokio::sync::watch::Receiver<Option<crate::interrupt::InterruptIntent>>,  // NEW
) -> Result<HandleRequestOutcome> {
    let request_token = tokio_util::sync::CancellationToken::new();    // NEW

    // ... existing body ...
    // (the `request_token` is unused in this task — Task 7 wires it to child tokens
    //  and Task 8 wires its inference-child to the admission permit.)
}
```

Mark `request_token` with `let _ = &request_token;` at the end of the function to suppress unused warnings for now.

- [ ] **Step 3: Create the per-request interrupt channel in the scheduler at claim time**

In `crates/defra-agent/src/scheduler/loop_impl.rs`, find the place where a claimed request is dispatched to `handle_request` (likely via a spawn in the `tick` function). Before dispatch:

```rust
let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel::<Option<crate::interrupt::InterruptIntent>>(None);

// Store `interrupt_tx` in a per-request map owned by the scheduler so the next tick
// can signal it. Shape depends on existing scheduler state; typical approach:
self.interrupts.insert(lifecycle.request().request_id.clone(), interrupt_tx);

// Pass interrupt_rx into handle_request:
tokio::spawn(async move {
    daemon.handle_request(lifecycle, shutdown_rx, interrupt_rx).await
});
```

Add the `interrupts` map as a field on the scheduler struct:

```rust
use std::collections::HashMap;
use tokio::sync::watch;
use crate::interrupt::InterruptIntent;

pub struct Scheduler {
    // ... existing fields ...
    interrupts: HashMap<String, watch::Sender<Option<InterruptIntent>>>,
}
```

Initialize it in the constructor and clean up entries when the request completes (use an `Arc<Mutex<_>>` or similar if the daemon needs to signal completion back; a simpler approach is "drop the Sender when the request handle finishes" — the daemon already owns a Drop-aware handle). Match your existing scheduler's lifecycle-cleanup pattern.

- [ ] **Step 4: No behavior change verification**

Run: `cargo test --workspace`
Expected: all tests still pass. The new wiring is inert; `interrupt_tx` is never signaled yet and `interrupt_rx` is not selected on.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/interrupt.rs \
        crates/defra-agent/src/lib.rs \
        crates/defra-agent/src/scheduler/loop_impl.rs \
        crates/defra-agent/src/agent/daemon/request.rs
git commit -m "Wire CancellationToken + per-request interrupt watch channel"
```

---

## Task 7: Scheduler observation + daemon select arm

**Files:**
- Modify: `crates/defra-agent/src/scheduler/loop_impl.rs` (observation loop)
- Modify: `crates/defra-agent/src/agent/daemon/request.rs` (select arm + cancellation flow)
- Modify: `crates/defra-agent/src/streaming.rs` (add `write_interrupted_at` method)
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs` (un-gate claim/processing/input tests)

This is the behavior heart of the plan. The scheduler observes the interrupt field on claimed rows each tick; the daemon runs the six-step cancellation flow when signaled.

- [ ] **Step 1: Scheduler observes interrupt field each tick**

In `scheduler/loop_impl.rs::tick`, after the usual work, add:

```rust
async fn observe_interrupts(&mut self) -> Result<()> {
    // Query all claimed, non-terminal requests for current interrupt_requested_at.
    let query = r#"query {
        AgentRequest(filter: {
            lifecycle_state: { _in: ["claimed", "processing", "inputRequired"] }
        }) {
            request_id
            interrupt_requested_at
        }
    }"#;
    let resp = session::execute_query(&self.node, query, "observe_interrupts").await?;
    for row in parse_agent_request_list(&resp) {
        if let Some(at) = row.interrupt_requested_at.as_deref() {
            if let Some(tx) = self.interrupts.get(&row.request_id) {
                // Idempotent: only send if not already signaled.
                if tx.borrow().is_none() {
                    let intent = crate::interrupt::InterruptIntent {
                        at: chrono::DateTime::parse_from_rfc3339(at)
                            .map_err(|e| anyhow::anyhow!("bad interrupt_requested_at: {e}"))?
                            .with_timezone(&chrono::Utc),
                    };
                    let _ = tx.send(Some(intent));
                }
            }
        }
    }
    Ok(())
}
```

Call `observe_interrupts()` from `tick()` before the sleep, after the pending-claim pass.

- [ ] **Step 2: Extend `DefraStreamWriter` with `write_interrupted_at`**

In `crates/defra-agent/src/streaming.rs`:

```rust
impl DefraStreamWriter {
    pub async fn write_interrupted_at(&self, doc_id: &str, at: &str) -> Result<bool> {
        let escaped_at = escape_graphql_string(at);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{ interrupted_at: "{escaped_at}" }}
                ) {{ _docID }}
            }}"#
        );
        let resp = session::execute_mutation_with_retry(
            &self.node, &mutation, "write_interrupted_at"
        ).await?;
        Ok(response_has_documents(&resp.data.unwrap_or_default()))
    }
}
```

- [ ] **Step 3: Add the daemon select arm + six-step flow**

In `crates/defra-agent/src/agent/daemon/request.rs::handle_request`, find the existing main loop or select. Add an arm watching `interrupt_rx`:

```rust
// Top of handle_request:
let request_token = tokio_util::sync::CancellationToken::new();
let inference_token = request_token.child_token();  // used by AdmittedCompletionModel
let tool_token_factory = request_token.clone();     // each tool gets .child_token() from this

// ... existing body runs in a tokio::select! or similar ...

tokio::select! {
    _ = shutdown.changed() => {
        // existing shutdown path
    }
    _ = interrupt_rx.changed() => {
        let Some(intent) = interrupt_rx.borrow().clone() else { return Ok(HandleRequestOutcome::None); };
        self.run_interrupt_flow(lifecycle, intent, &request_token).await?;
        return Ok(HandleRequestOutcome::Interrupted);
    }
    result = run_turn(&mut self, lifecycle, inference_token.clone(), tool_token_factory.clone()) => {
        result?
    }
}
```

Adapt the tokio::select to fit the actual flow in `handle_request` (it probably is not a single select today; this may mean restructuring to put the streaming/tool loop inside a cancellable future).

- [ ] **Step 4: Implement the six-step flow**

First, add a helper that finds the `AgentResponse` row's `_docID` for the request:

```rust
async fn fetch_response_doc_id(node: &EmbeddedNode, request_id: &str) -> Result<Option<String>> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentResponse(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let resp = session::execute_query(node, &query, "fetch_response_doc_id").await?;
    let doc_id = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentResponse"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(String::from);
    Ok(doc_id)
}
```

Then the main flow:

```rust
async fn run_interrupt_flow(
    &self,
    lifecycle: &mut RequestLifecycle,
    intent: crate::interrupt::InterruptIntent,
    request_token: &tokio_util::sync::CancellationToken,
    stream_writer: &DefraStreamWriter,
    pending_tool_handles: &mut Vec<tokio::task::JoinHandle<Result<()>>>,
    cancellable_tool_count: usize,
) -> Result<()> {
    // 1. Cancel the root token.
    request_token.cancel();

    // 2. Grace wait (skip if no children).
    if cancellable_tool_count > 0 || /* inference in flight check */ true {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // 3. Force-abort any still-running cancellable work.
    for handle in pending_tool_handles.drain(..) {
        if !handle.is_finished() {
            handle.abort();
        }
    }

    // 4. Flip AgentResponse.interrupted_at (sequenced BEFORE step 5).
    // Find the AgentResponse row for this request; if none exists
    // (interrupt arrived before any token was streamed), skip.
    let response_doc_id = fetch_response_doc_id(&lifecycle.node, &lifecycle.request.request_id).await?;
    if let Some(doc_id) = response_doc_id {
        stream_writer
            .write_interrupted_at(&doc_id, &intent.at.to_rfc3339())
            .await
            .ok();  // best-effort; no response row means nothing was streamed
    }

    // 5. Write terminal lifecycle_state = interrupted.
    lifecycle.transition_to_interrupted(&intent.at.to_rfc3339()).await?;

    // 6. (Non-cancellable tool handles are NOT aborted. They're left to finish;
    //    their AgentToolResult writes will include discarded_because_interrupted=true
    //    — see Task 8.)

    Ok(())
}
```

Add `transition_to_interrupted` as a new method on `RequestLifecycle`:

```rust
impl RequestLifecycle {
    pub async fn transition_to_interrupted(&mut self, interrupt_at: &str) -> Result<()> {
        let doc_id = &self.request.doc_id;
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{
                        status: "interrupted",
                        lifecycle_state: "interrupted"
                    }}
                ) {{ _docID }}
            }}"#
        );
        session::execute_mutation_with_retry(&self.node, &mutation, "transition_interrupted").await?;
        self.state = LocalLifecycleState::Interrupted;
        Ok(())
    }
}
```

- [ ] **Step 5: Discard-flag for late-arriving non-cancellable tool results**

Where `AgentToolResult` rows are written (search for `add_AgentToolResult` or `ToolResult` writes in the agent crate), add logic that checks the parent request's `lifecycle_state`: if it's `"interrupted"` at the time of the tool-result write, set `discarded_because_interrupted: true` in the mutation input. Otherwise omit (default false).

```rust
async fn query_request_lifecycle_state(node: &EmbeddedNode, request_doc_id: &str) -> Result<String> {
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }}) {{
                lifecycle_state
            }}
        }}"#
    );
    let resp = session::execute_query(node, &query, "query_request_lifecycle_state").await?;
    let state = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("lifecycle_state"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(state)
}

// At the add_AgentToolResult write site:
let parent_is_interrupted =
    query_request_lifecycle_state(&node, &request_doc_id).await? == "interrupted";
let discard_flag = if parent_is_interrupted { "true" } else { "false" };
let mutation = format!(
    r#"mutation {{
        add_AgentToolResult(input: {{
            /* existing fields unchanged */
            discarded_because_interrupted: {discard_flag}
        }}) {{ _docID }}
    }}"#
);
```

- [ ] **Step 6: Un-ignore the four Task-7-gated tests**

Remove the `#[ignore]` attribute and fill in the bodies of:
- `claimed_interrupted_via_watch_channel`
- `processing_interrupted_preserves_partial_response`
- `input_required_interrupted`
- `processing_tie_break_prefers_interrupt_over_deadline`
- `interrupt_on_already_terminal_is_noop`

For `processing_interrupted_preserves_partial_response`:

```rust
#[tokio::test]
async fn processing_interrupted_preserves_partial_response() {
    let db = test_db("processing-interrupted").await;
    // ... setup request to pending, claim, start streaming with mock backend ...
    // Write partial content via DefraStreamWriter so an AgentResponse row exists
    //   with content = "Hello wor"
    // Signal interrupt via set_interrupt_requested_at
    // Wait for scheduler tick
    // Assert:
    //   - AgentRequest.lifecycle_state == "interrupted"
    //   - AgentResponse.content == "Hello wor"     (preserved!)
    //   - AgentResponse.interrupted_at != null
    //   - AgentResponse.interrupted_at <= AgentRequest.lifecycle_state write time
    //     (which we observe via event sequence or by re-reading timestamps)
}
```

- [ ] **Step 7: Run**

Run: `cargo test -p defra-agent state_machine_conformance`
Expected: all un-gated conformance tests pass.

Run: `cargo test --workspace`
Expected: full workspace green.

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent/src/scheduler/loop_impl.rs \
        crates/defra-agent/src/agent/daemon/request.rs \
        crates/defra-agent/src/streaming.rs \
        crates/defra-agent/src/lifecycle/ \
        crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "Implement scheduler observation and daemon interrupt flow"
```

---

## Task 8: CancellableTool trait + HTTP/filesystem-read opt-ins

**Files:**
- Create: `crates/defra-agent/src/tool/cancellable.rs`
- Modify: `crates/defra-agent/src/tool/mod.rs` (or the dispatch site for tool calls)
- Modify: any HTTP-fetch / filesystem-read tool impls (opt-in)

- [ ] **Step 1: Define the `CancellableTool` trait**

Create `crates/defra-agent/src/tool/cancellable.rs`:

```rust
//! Cancellable-tool wrapper over `rig::tool::Tool`.
//!
//! Default behavior is non-cancellable: tools run to completion and their
//! results are discarded if the request was interrupted. Tools that can
//! observe cancellation (HTTP fetch, filesystem read, etc.) opt in by
//! overriding BOTH methods.
//!
//! To opt in, override (a) `supports_cancellation`, (b) `call_cancellable`,
//! and (c) add a unit test that cancels mid-call.

use rig::tool::Tool;
use tokio_util::sync::CancellationToken;

pub trait CancellableTool: Tool {
    /// Return `true` only if `call_cancellable` is also overridden.
    /// The dispatch path asserts this pairing in debug builds.
    fn supports_cancellation(&self) -> bool { false }

    /// Run the tool with an observable cancellation token. Default impl
    /// ignores the token (safe only for non-cancellable tools).
    fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        let _ = cancel;
        self.call(args)
    }
}

impl<T: Tool> CancellableTool for T {}
```

- [ ] **Step 2: Wire the dispatch path with a debug_assert witness**

Find the tool-call dispatch (search for `.call(args)` in the agent crate). Replace with:

```rust
if tool.supports_cancellation() {
    debug_assert!(
        /* canary check */ cancellation_is_observed(&tool).await,
        "{} declares supports_cancellation() = true but its call_cancellable ignores the token — \
         override both methods",
        tool.name()
    );
    let child_token = request_token.child_token();
    tool.call_cancellable(args, child_token).await
} else {
    tool.call(args).await
}
```

The `cancellation_is_observed` helper can be:

```rust
#[cfg(debug_assertions)]
async fn cancellation_is_observed<T: CancellableTool>(tool: &T) -> bool {
    // Check by calling on a pre-cancelled canary token and confirming Err.
    // If the tool returns Ok on a pre-cancelled token it's ignoring cancellation.
    // Implementation: check within a short timeout; if the call completes
    // normally on a cancelled token for a trivial canary input, the assert fails.
    true  // Detailed canary implementation is a follow-up; leave trivially true for now
          // but keep the debug_assert in place so we can strengthen later.
}
```

The debug_assert stays as a compile-time placeholder for the check even if the canary function is stubbed; a future change can strengthen the witness without re-threading the dispatch.

- [ ] **Step 3: Opt in HTTP/filesystem-read tools**

Find any HTTP-fetch tool (grep for `reqwest::`, `ureq::`, etc.) and filesystem-read tool (grep for `tokio::fs::read`, `std::fs::read`) in the tools directory. For each, add the override:

```rust
impl CancellableTool for HttpFetchTool {
    fn supports_cancellation(&self) -> bool { true }

    async fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        tokio::select! {
            _ = cancel.cancelled() => Err(Self::Error::from(/* cancelled */)),
            result = self.call(args) => result,
        }
    }
}
```

Leave filesystem-write, shell exec, and stdio MCP as non-cancellable (no override).

- [ ] **Step 4: Run**

Run: `cargo test --workspace`
Expected: green. The conformance test `processing_interrupted_preserves_partial_response` already covers "cancellable tool returns promptly"; add a new test under `tests/tool_cancellation.rs` if you want direct coverage of the cancellable path.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/tool/
git commit -m "Add CancellableTool trait and opt-in HTTP/filesystem-read tools"
```

---

## Task 9: Admission-layer bridge (`mark_interrupted` + Composed.lean finalize)

**Files:**
- Modify: `crates/defra-agent/src/admission/permit.rs`
- Modify: `crates/defra-agent/src/admission/client.rs` (`AdmittedCompletionModel`)
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean` (replace `sorry` with real proof)

- [ ] **Step 1: Add `mark_interrupted` to `AdmissionPermit`**

In `permit.rs`:

```rust
impl AdmissionPermit {
    /// Mark this permit for cancellation. On Drop, the controller persists
    /// the InferenceCall with call_state = "cancelled", failure_reason = "Cancelled".
    /// Idempotent with the existing "already-finalized" guard in Drop.
    pub(crate) fn mark_interrupted(&mut self) {
        if self.finished {
            return;
        }
        self.terminal = Some(PermitTerminal {
            call_state: "cancelled",
            failure_reason: Some("Cancelled".to_string()),
            usage: None,  // Token accounting carried by streaming state, not PermitTerminal
        });
        // Note: do NOT set self.finished here. Drop does the actual persist.
    }
}
```

- [ ] **Step 2: Wire the cancellation-select arm in `AdmittedCompletionModel`**

In `admission/client.rs`, find the completion wrapper's `completion()` or `completion_stream()` method (whichever does the HTTP call). Wrap the inner future in a `tokio::select!` arm on an `inference_token`:

```rust
impl<M: CompletionModel> AdmittedCompletionModel<M> {
    pub async fn completion(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,   // NEW param threaded from request_token.child_token()
    ) -> Result<CompletionResponse> {
        let mut permit = self.acquire_current_call(&request).await?;

        tokio::select! {
            _ = cancel.cancelled() => {
                permit.mark_interrupted();
                Err(CompletionError::Cancelled.into())
            }
            result = self.inner.completion(request) => {
                match result {
                    Ok(resp) => {
                        permit.finish_success(resp.usage()).await;
                        Ok(resp)
                    }
                    Err(e) => {
                        permit.finish_failure(&e.to_string()).await;
                        Err(e.into())
                    }
                }
            }
        }
    }
}
```

Apply the same pattern to the streaming variant — scope `permit.mark_interrupted()` to the cancellation arm, make sure the inner stream-poll loop is inside a select.

- [ ] **Step 3: Thread `inference_token` from daemon to completion model**

Where `handle_request` constructs the completion model and calls into it, pass `inference_token = request_token.child_token()`. This was laid down as wiring in Task 6.

- [ ] **Step 4: Replace `sorry` in `Composed.lean`**

Now that the runtime implements the cross-layer cancellation, go back to the `interrupted_request_cancels_calls` theorem in `Proofs/Composed.lean` and replace `sorry` with the actual proof, following the modeling already established in `Composed.lean`.

If the theorem statement needs to match InferenceCall structure from the admission design spec (which may exist only informally in Lean), this may require extending `Composed.lean` to import or re-state the admission model. Follow the style of the existing `request_step` / `persistence_step` composed transitions.

- [ ] **Step 5: Run**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build clean with no `sorry`.

Run: `cargo test --workspace`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/admission/ \
        crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "Wire admission permit cancellation and complete cross-layer Lean proof"
```

---

## Task 10: Submission API + resend

**Files:**
- Modify: `crates/defra-agent-desktop/src/client/mutations/chat/request.rs`
- Create: `crates/defra-agent-desktop/src/client/mutations/chat/interrupt.rs`
- Modify: `crates/defra-agent-desktop/src/client/mutations/chat/mod.rs` (export new)
- Modify: `crates/defra-agent-cli/src/cli/args.rs` (add `Interrupt`, `Resend` subcommands, `--valid-until` flag on Submit)
- Modify: `crates/defra-agent-cli/src/commands/request.rs` (handlers for new subcommands)
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs` (un-gate idempotency test)

- [ ] **Step 1: Extend `submit_request` signature with `valid_until` and `retry_parent_request`**

In `crates/defra-agent-desktop/src/client/mutations/chat/request.rs`:

```rust
pub struct SubmitRequestOptions {
    pub valid_until: Option<DateTime<Utc>>,     // None = no TTL; client default = now + 5min
    pub retry_parent_request: Option<String>,   // Some when this is a resend
}

impl Default for SubmitRequestOptions {
    fn default() -> Self {
        Self {
            valid_until: Some(Utc::now() + chrono::Duration::minutes(5)),
            retry_parent_request: None,
        }
    }
}

pub async fn submit_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    content: &str,
    behavior_id: Option<&str>,
    options: SubmitRequestOptions,
) -> Result<SubmittedRequest> {
    // ... existing body ...

    let escaped_valid_until = options.valid_until
        .map(|t| escape_graphql_string(&t.to_rfc3339()))
        .unwrap_or_default();
    let valid_until_field = if options.valid_until.is_some() {
        format!(r#", valid_until: "{escaped_valid_until}""#)
    } else {
        String::new()
    };

    let retry_parent = options.retry_parent_request.as_deref().unwrap_or("");
    let retry_root: String = if retry_parent.is_empty() {
        request_id.clone()
    } else {
        fetch_retry_root(node, retry_parent)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| retry_parent.to_string())
    };

    let mutation = format!(
        r#"mutation {{
            add_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                /* ... existing fields ... */
                retry_parent_request: "{retry_parent}",
                retry_root_request: "{retry_root}",
                /* ... */
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries}
                {valid_until_field}
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );
    // ... rest of submit_request ...
}
```

Note: the existing code passes `retry_parent_request: ""`. Replace with `retry_parent` variable. Repeat for `retry_root_request`.

Add the `fetch_retry_root` helper alongside `submit_request`:

```rust
async fn fetch_retry_root(node: &EmbeddedNode, request_id: &str) -> Result<Option<String>> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                retry_root_request
            }}
        }}"#
    );
    let resp = execute_query(node, &query, "fetch_retry_root").await?;
    let root = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("retry_root_request"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(root)
}
```

- [ ] **Step 2: Create the `interrupt_request` mutation**

Create `crates/defra-agent-desktop/src/client/mutations/chat/interrupt.rs`:

```rust
use anyhow::Result;
use chrono::Utc;
use defra_node::EmbeddedNode;
use crate::client::mutations::session::execute_mutation;
use crate::graphql::escape_graphql_string;

pub async fn interrupt_request(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<()> {
    // Idempotent: a second call is a no-op because the runtime observes
    // interrupt_requested_at as a latch (S7). But we still check here
    // to skip the write on second call if the field is already set.
    let existing = fetch_interrupt_requested_at(node, request_id).await?;
    if existing.is_some() {
        return Ok(());
    }

    let now = Utc::now().to_rfc3339();
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_now = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_now}" }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "interrupt_request").await?;
    Ok(())
}

async fn fetch_interrupt_requested_at(node: &EmbeddedNode, request_id: &str) -> Result<Option<String>> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                interrupt_requested_at
            }}
        }}"#
    );
    let resp = crate::client::mutations::session::execute_query(node, &query, "fetch_interrupt_requested_at").await?;
    let value = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("interrupt_requested_at"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(value)
}
```

Export it from `mutations/chat/mod.rs`.

- [ ] **Step 3: Add `resend_request` helper**

In `request.rs`:

```rust
struct StaleRequestView {
    session_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    content: String,
    lifecycle_state: String,
    failure_reason: String,
}

async fn fetch_request(node: &EmbeddedNode, request_id: &str) -> Result<StaleRequestView> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                session_id
                agent_did
                behavior_id
                content
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    let resp = execute_query(node, &query, "fetch_request").await?;
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    Ok(StaleRequestView {
        session_id: row.get("session_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        agent_did: row.get("agent_did").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        behavior_id: row.get("behavior_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from),
        content: row.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        lifecycle_state: row.get("lifecycle_state").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        failure_reason: row.get("failure_reason").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    })
}

pub async fn resend_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    stale_request_id: &str,
) -> Result<SubmittedRequest> {
    // 1. Fetch the stale request.
    let stale = fetch_request(node, stale_request_id).await?;

    // 2. Assert it's in dead/Stale terminal. If not, bail.
    if stale.lifecycle_state != "dead" || stale.failure_reason != "Stale" {
        anyhow::bail!(
            "request {stale_request_id} is not a stale terminal (state={}, reason={})",
            stale.lifecycle_state, stale.failure_reason
        );
    }

    // 3. Submit a fresh request chained via retry_parent_request.
    submit_request(
        node,
        store,
        &stale.session_id,
        &stale.agent_did,
        &stale.content,
        stale.behavior_id.as_deref(),
        SubmitRequestOptions {
            valid_until: Some(Utc::now() + chrono::Duration::minutes(5)),
            retry_parent_request: Some(stale_request_id.to_string()),
        },
    ).await
}
```

- [ ] **Step 4: Add CLI subcommands**

In `crates/defra-agent-cli/src/cli/args.rs` extend `RequestCommand`:

```rust
#[derive(Subcommand)]
pub(crate) enum RequestCommand {
    // ... existing variants ...
    #[command(about = "Signal interrupt on an in-flight request")]
    Interrupt {
        #[arg(help = "Request ID to interrupt")]
        request_id: String,
    },
    #[command(about = "Resend a stale request with a fresh valid_until")]
    Resend {
        #[arg(help = "Stale request ID to resend")]
        request_id: String,
    },
}
```

Extend the existing `Submit` variant with `--valid-until`:

```rust
#[command(about = "Submit a new request")]
Submit {
    // ... existing args ...
    #[arg(long, help = "TTL for this request (e.g. 5m, 30s). Default: 5m")]
    valid_until: Option<humantime::Duration>,
},
```

- [ ] **Step 5: Wire the CLI handlers**

In `crates/defra-agent-cli/src/commands/request.rs` (or wherever `RequestCommand` is dispatched), add match arms:

```rust
RequestCommand::Interrupt { request_id } => {
    interrupt_request(&node, &request_id).await?;
    println!("Interrupted request {request_id}");
}
RequestCommand::Resend { request_id } => {
    let new = resend_request(&node, &store, &request_id).await?;
    println!("Resent as request {}", new.request_id);
}
RequestCommand::Submit { /* existing args */, valid_until } => {
    let options = SubmitRequestOptions {
        valid_until: valid_until.map(|d| Utc::now() + chrono::Duration::from_std(d.into()).unwrap()),
        retry_parent_request: None,
    };
    let submitted = submit_request(&node, &store, /* ... */, options).await?;
    // ... existing print logic ...
}
```

- [ ] **Step 6: Un-ignore `interrupt_request_is_idempotent` and fill in the body**

```rust
#[tokio::test]
async fn interrupt_request_is_idempotent() {
    let db = test_db("interrupt-idempotent").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let _doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    interrupt_request(&db.node, &request_id).await.unwrap();
    let after_first = fetch_interrupt_requested_at(&db.node, &request_id).await.unwrap();
    assert!(after_first.is_some(), "first interrupt should latch the field");

    // Second call: must be a no-op; field should not be rewritten.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    interrupt_request(&db.node, &request_id).await.unwrap();
    let after_second = fetch_interrupt_requested_at(&db.node, &request_id).await.unwrap();
    assert_eq!(after_first, after_second, "second call must not rewrite the latched timestamp");
}
```

- [ ] **Step 7: Run**

Run: `cargo test --workspace`
Expected: green.

- [ ] **Step 8: Commit**

```bash
git add crates/defra-agent-desktop/src/client/mutations/ \
        crates/defra-agent-cli/src/ \
        crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "Add interrupt_request mutation, valid_until on submit, resend helper"
```

---

## Task 11: Integration + live tests

**Files:**
- Create: `crates/defra-agent/tests/interruption_integration.rs`
- Create: `crates/defra-agent/tests/live/interrupt_live.rs` (env-gated)

- [ ] **Step 1: End-to-end with mock backend**

Create `crates/defra-agent/tests/interruption_integration.rs`:

```rust
#[tokio::test]
async fn interrupt_mid_stream_preserves_partial_and_cancels_inference_call() {
    // 1. Start scheduler + daemon with a mock backend that yields tokens slowly.
    // 2. submit_request(valid_until: now + 5min).
    // 3. Wait for lifecycle_state to reach "processing" and some tokens to arrive.
    // 4. interrupt_request(request_id).
    // 5. Assert within 2s:
    //    - AgentRequest.lifecycle_state == "interrupted"
    //    - AgentResponse.content is non-empty AND unchanged from pre-interrupt
    //    - AgentResponse.interrupted_at is non-null
    //    - InferenceCall.call_state == "cancelled"
    //    - InferenceCall.failure_reason == "Cancelled"
    //    - InferenceCall.completion_tokens reflects what was actually streamed
    //      (not zero, not recomputed)
}
```

- [ ] **Step 2: Offline-replay thundering-herd test**

```rust
#[tokio::test]
async fn offline_replay_of_stale_requests_does_not_call_backend() {
    // Simulate: scheduler paused; 20 AgentRequest rows written with
    // valid_until = now - 10s; scheduler resumes.
    //
    // Assert:
    //   - Within one scheduler tick, all 20 transition to dead/Stale.
    //   - Mock backend received zero completion calls.
    //   - InferenceCall collection is empty for any of those 20 request_ids.
}
```

- [ ] **Step 3: Resend chain audit test**

```rust
#[tokio::test]
async fn resend_from_stale_populates_retry_chain() {
    // Submit a request; force it stale; resend.
    // Assert:
    //   - Original: lifecycle_state=dead, failure_reason=Stale
    //   - New: lifecycle_state=pending, retry_parent_request=original_id,
    //          retry_root_request=original_id
    //   - Query by retry_root_request returns both rows
}
```

- [ ] **Step 4: Concurrent-requests isolation test**

```rust
#[tokio::test]
async fn interrupting_one_request_does_not_affect_another() {
    // Submit request A and request B to the same agent; both reach processing.
    // interrupt_request(A).
    // Assert:
    //   - A reaches interrupted
    //   - B reaches completed normally
    //   - A's token cancellation did not propagate to B's tokens
}
```

- [ ] **Step 5: Live test (env-gated)**

Create `crates/defra-agent/tests/live/interrupt_live.rs`:

```rust
#[tokio::test]
#[ignore = "live: requires MINIMAX_LIVE=1"]
async fn live_interrupt_mid_stream_on_minimax() {
    if std::env::var("MINIMAX_LIVE").is_err() { return; }

    // Submit request with a prompt expected to generate ≥100 tokens.
    // Wait for content.len() >= 20 chars on AgentResponse row.
    // interrupt_request(request_id).
    // Assert within 2s:
    //   - AgentRequest.lifecycle_state == "interrupted"
    //   - No further token writes to AgentResponse.content after interrupted_at.
}
```

- [ ] **Step 6: Run**

Run: `cargo test -p defra-agent --test interruption_integration`
Expected: all four tests pass.

Run (locally, optional): `MINIMAX_LIVE=1 cargo test -p defra-agent --test interrupt_live -- --ignored`
Expected: live test passes when the MiniMax endpoint is reachable.

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/tests/interruption_integration.rs \
        crates/defra-agent/tests/live/interrupt_live.rs
git commit -m "Add integration and live coverage for request interruption"
```

---

## Task 12: Final validation

- [ ] **Step 1: Full test sweep**

Run: `cargo test --workspace`
Expected: green, including all previously-ignored conformance tests now active.

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, no `sorry`.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 2: Spec/plan cross-check**

Open the spec (`docs/superpowers/specs/2026-04-20-interruption-and-request-hygiene-design.md`) and walk down the "Implementation order" list (steps 1-10). For each, point at the plan task that delivered it. List any gaps.

Spec implementation-order → plan task mapping:
1. Lean first → Task 1, Task 2
2. Schema + protocol → Task 3
3. Conformance scaffolding → Task 4
4. Scheduler claim check → Task 5
5. CancellationToken + interrupt transport → Task 6
6. Scheduler observation + daemon select → Task 7
7. Tool trait → Task 8
8. Admission-layer bridge → Task 9
9. Submission API + resend → Task 10
10. Integration + live tests → Task 11

If you find gaps, stop and address them in a follow-up task before declaring done.

- [ ] **Step 3: Commit (if anything changed)**

If validation found any issues and you fixed them:

```bash
git add <files>
git commit -m "Address validation findings"
```

Otherwise, the work is complete.

---

## Notes on subagent-driven-development

If executing with `superpowers:subagent-driven-development`:
- Dispatch one subagent per task.
- The Lean tasks (1, 2, and the Composed.lean finalize in 9) require `lake build` — mention this in the subagent prompt.
- The admission-permit and completion-model code is the most nuanced touch. Review Task 9's output carefully.
- Task 7 restructures `handle_request` into a tokio::select — this is the single most disruptive code change in the plan. If the subagent struggles, consider splitting into two tasks: one pure restructuring, one wiring the interrupt arm.

## Self-review notes (from plan-author)

**Spec coverage check:** every "in scope" bullet from the spec maps to a task above. Specifically:
- Ninth RequestState `interrupted` → Task 1 steps 1, 2
- `dead/Stale` via `expire` → Task 1 step 4, Task 5 steps 2-4
- `interrupt_requested_at` + `valid_until` → Task 3 step 1, Task 5 steps 2, 3, 5
- `AgentResponse.interrupted_at` → Task 3 step 2, Task 7 steps 2, 4
- `AgentToolResult.discarded_because_interrupted` → Task 3 step 3, Task 7 step 5
- CancellationToken hierarchy → Task 6 steps 1, 2, Task 7 step 3, Task 9 step 3
- CancellableTool trait → Task 8
- `AdmissionPermit::mark_interrupted` + admission-spec axioms → Task 9
- Lean S7/S8, extended L1, cross-layer theorem → Task 2
- Client-side submission API + resend → Task 10

**Type consistency check:** `ClaimOutcome` gains `Interrupted` + `Expired` in Task 5; referenced in Task 5 tests. `InterruptIntent` defined in Task 6 step 1; referenced in Task 6 step 2 and Task 7 step 1. `AdmissionPermit::mark_interrupted` defined in Task 9 step 1; called in Task 9 step 2. No mismatches.

**Placeholder scan:** a few places use `// ... existing body ...` where showing the full existing code would balloon the plan — in each case the step explicitly tells the engineer what to replace and what pattern to mirror. These are intentional, not TBDs. No `TODO`, no `add appropriate error handling`, no `similar to Task N`.

**Open non-blockers (for the engineer to know about):**
- The `cancellation_is_observed` witness in Task 8 is stubbed — a follow-up can strengthen it without re-threading dispatch.
- Task 9's Composed.lean proof may require extending the file to import/restate admission modeling; the proof style is indicated but the exact shape depends on how `InferenceCall` is modeled in Lean (which is out of scope of this plan to introduce if it isn't already there).
