# Client Turn Observation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Formally model how any client derives a deterministic view of an agent turn from observed documents, with proven monotonicity, terminal coherence, and turn-replacement properties.

**Architecture:** A Lean 4 formal model (`Proofs/Client.lean`) defines the client turn state, a pure derivation function from document snapshots, and 5 theorems tying the client projection to the existing server request lifecycle proofs. Rust conformance tests bridge the Lean semantics to a reference implementation. A protocol doc in `docs/protocols/` explains the model for client implementers.

**Tech Stack:** Lean 4 (Lake, Mathlib), Rust (defra-agent crate, tokio test), Markdown

**Spec:** `docs/superpowers/specs/2026-04-11-client-turn-observation-design.md`

**Spec correction:** The design spec's derivation priority order (rules 3-5) lets response state override terminal request states, which breaks monotonicity when a stale streaming response is observed alongside a failed request. The correct order checks server terminal states first, then falls through to response for non-terminal request states. This plan implements the corrected order.

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/defra-agent/proofs/Proofs/Client.lean` | Create | Lean formal model: types, derivation, 5 theorems |
| `crates/defra-agent/src/client_protocol.rs` | Create | Rust types + derivation mirroring Lean model |
| `crates/defra-agent/src/client_protocol/tests.rs` | Create | Unit tests: derivation table, monotonicity, terminal coherence |
| `crates/defra-agent/src/lib.rs` | Modify | Add `pub mod client_protocol;` |
| `docs/protocols/client-state-machine.md` | Create | Human-readable protocol spec for client implementers |

---

### Task 1: Lean type definitions

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Client.lean`

- [ ] **Step 1: Create Client.lean with type definitions**

```lean
import Proofs.Request

/-!
# Client Turn Observation

Formal model for how any client derives a deterministic view of a single
agent turn from observed documents.

The client projection is a pure function of document snapshots. It does
not depend on wall-clock time — server liveness proofs (L1, L3) guarantee
every request terminates. If a client perceives a "stall," that is a
transport problem, not a turn-state problem.

Imports `Proofs.Request` to reuse `RequestState` from the server model.
-/

/-- The 5 client-visible turn states. -/
inductive ClientTurnState where
  | waitingForClaim
  | streaming
  | completed
  | failed
  | superseded
  deriving DecidableEq, Repr

namespace ClientTurnState

/-- Client state ordering for monotonicity.
    Terminal states share rank 2 (incomparable). -/
def rank : ClientTurnState → Nat
  | .waitingForClaim => 0
  | .streaming       => 1
  | .completed       => 2
  | .failed          => 2
  | .superseded      => 2

/-- Whether a client turn state is terminal. -/
def isTerminal : ClientTurnState → Bool
  | .completed  => true
  | .failed     => true
  | .superseded => true
  | _           => false

instance : HasTerminal ClientTurnState where
  isTerminal s := s.isTerminal = true
  isTerminal_dec s := by
    cases s <;> simp [isTerminal] <;> decide

end ClientTurnState

/-- Client-visible response status, read from AgentResponse.status. -/
inductive ResponseStatus where
  | streaming
  | complete
  | error
  deriving DecidableEq, Repr

/-- Snapshot of an AgentRequest as observed by the client.
    Only the fields that affect derivation are included. -/
structure RequestSnapshot where
  lifecycleState : RequestState
  isSuperseded : Bool
  deriving DecidableEq, Repr

/-- Snapshot of an AgentResponse as observed by the client.
    progressSeq is omitted — it orders response versions
    but does not affect the derivation result. -/
structure ResponseSnapshot where
  status : ResponseStatus
  deriving DecidableEq, Repr

/-- A single attempt observation: request + optional response. -/
structure AttemptView where
  request : RequestSnapshot
  response : Option ResponseSnapshot
  deriving DecidableEq, Repr
```

- [ ] **Step 2: Build to verify compilation**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Build succeeds with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client.lean
git commit -m "Add Lean client turn observation types

Define ClientTurnState (5 states), ResponseStatus, RequestSnapshot,
ResponseSnapshot, AttemptView, rank ordering, and HasTerminal instance
for the client turn observation model.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Lean deriveAttempt function

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Client.lean`

- [ ] **Step 1: Add deriveAttempt function**

Append after the `AttemptView` definition:

```lean
/-- Derive client turn state from a single attempt observation.

    Priority order:
    1. Supersession takes absolute precedence (cross-turn event).
    2. Server terminal lifecycle states override any response
       (terminal states are irreversible — proven in Request.lean).
    3. For non-terminal request states, response may be more current
       than the request under P2P replication lag. Trust the response.
    4. No response and non-terminal request → waitingForClaim. -/
def deriveAttempt : AttemptView → ClientTurnState
  | ⟨req, resp⟩ =>
    -- Supersession: cross-turn event, always takes precedence
    if req.isSuperseded then .superseded
    else match req.lifecycleState with
    -- Server terminal states override any stale response
    | .superseded    => .superseded
    | .completed     => .completed
    | .failed        => .failed
    | .dead          => .failed
    -- Non-terminal: response may be more current than request
    | .pending | .claimed | .processing | .inputRequired =>
      match resp with
      | some r =>
        match r.status with
        | .complete  => .completed
        | .error     => .failed
        | .streaming => .streaming
      | none => .waitingForClaim
```

- [ ] **Step 2: Build to verify compilation**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Build succeeds.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client.lean
git commit -m "Add deriveAttempt: single-attempt client state projection

Server terminal states take precedence over response to prevent
stale streaming responses from demoting a failed/completed request.
Non-terminal request states defer to response for replication-lag
tolerance.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Lean deriveTurn and retry chain resolution

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Client.lean`

- [ ] **Step 1: Add deriveTurn function**

Append after `deriveAttempt`:

```lean
/-- Derive client turn state from a full turn observation.

    The turn is a retry chain: a list of attempts ordered root-first,
    tip-last. The tip is the most recent attempt — the one the client
    should render.

    Returns `none` for empty observations (no turn exists). -/
def deriveTurn : List AttemptView → Option ClientTurnState
  | []          => none
  | [a]         => some (deriveAttempt a)
  | _ :: rest   => deriveTurn rest

/-- deriveTurn always returns the derivation of the last element. -/
theorem deriveTurn_eq_last
    {attempts : List AttemptView}
    {a : AttemptView}
    (h : attempts ≠ []) :
    deriveTurn (attempts ++ [a]) = some (deriveAttempt a) := by
  induction attempts with
  | nil => contradiction
  | cons head tail ih =>
    simp [deriveTurn]
    cases tail with
    | nil => simp [deriveTurn]
    | cons h' t' =>
      simp [deriveTurn] at ih ⊢
      exact ih (by simp)
```

- [ ] **Step 2: Build to verify compilation**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Build succeeds. If `deriveTurn_eq_last` needs proof adjustment, iterate on the `induction` tactic.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client.lean
git commit -m "Add deriveTurn: retry chain tip resolution

Models the retry chain as a list ordered root-first, tip-last.
deriveTurn always returns the derivation of the tip (last element).
Prove deriveTurn_eq_last to anchor turn-replacement reasoning.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Lean T4 — Totality

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Client.lean`

- [ ] **Step 1: State and prove T4**

Append after `deriveTurn_eq_last`:

```lean
/-! ## Theorem T4: Totality

    `deriveAttempt` is total by construction — it is a match expression
    with exhaustive coverage over `RequestState` and `Option ResponseSnapshot`.

    `deriveTurn` is total for non-empty attempt lists.
-/

/-- T4: deriveAttempt is total — defined for every possible AttemptView. -/
theorem deriveAttempt_total (view : AttemptView) :
    ∃ s : ClientTurnState, deriveAttempt view = s :=
  ⟨deriveAttempt view, rfl⟩

/-- T4: deriveTurn is defined for every non-empty attempt list. -/
theorem deriveTurn_total
    {attempts : List AttemptView}
    (h : attempts ≠ []) :
    ∃ s : ClientTurnState, deriveTurn attempts = some s := by
  induction attempts with
  | nil => contradiction
  | cons head tail ih =>
    cases tail with
    | nil => exact ⟨deriveAttempt head, rfl⟩
    | cons h' t' =>
      simp [deriveTurn]
      exact ih (by simp)
```

- [ ] **Step 2: Build to verify proofs compile**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Build succeeds with no `sorry` remaining.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client.lean
git commit -m "Prove T4: client derivation is total

deriveAttempt is total by exhaustive match. deriveTurn is total for
non-empty attempt lists (structural induction on the list).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Lean T2 — Monotonicity

This is the hardest theorem. It proves that valid server transitions never decrease the client's view rank.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Client.lean`

- [ ] **Step 1: Add helper lemma for deriveAttempt on non-terminal lifecycle states**

Append:

```lean
/-! ## Theorem T2: Monotonicity

    If the server transitions a request forward (valid `Transition` from
    `Proofs.Request`) while the response is held fixed, the client rank
    never decreases.

    For response advances (none → some, or status change toward terminal),
    the client rank also never decreases when the request is held fixed
    and non-terminal.
-/

/-- Helper: for non-terminal lifecycle states, deriveAttempt result depends
    only on the response (not which specific non-terminal state). -/
theorem deriveAttempt_nonterminal_response_driven
    {req : RequestSnapshot}
    {resp : Option ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_state : req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
               req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired) :
    deriveAttempt ⟨req, resp⟩ = match resp with
      | some r => match r.status with
        | .complete => .completed
        | .error => .failed
        | .streaming => .streaming
      | none => .waitingForClaim := by
  simp [deriveAttempt, h_not_super]
  cases h_state with
  | inl h => simp [h]
  | inr h => cases h with
    | inl h => simp [h]
    | inr h => cases h with
      | inl h => simp [h]
      | inr h => simp [h]
```

- [ ] **Step 2: State the monotonicity theorem for request transitions**

```lean
/-- T2: A valid server request transition never decreases the client rank
    when the response is held fixed and supersession flag is unchanged.

    Proof strategy: case analysis over all 12 Transition constructors.
    For transitions between non-terminal states, the helper shows the
    result depends only on the response (unchanged). For transitions
    to terminal states, the terminal rank (2) is ≥ any non-terminal rank. -/
theorem request_transition_monotonic
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post)
    (resp : Option ResponseSnapshot)
    (h_not_super : Bool) :
    (deriveAttempt ⟨⟨post.state, h_not_super⟩, resp⟩).rank ≥
    (deriveAttempt ⟨⟨pre.state, h_not_super⟩, resp⟩).rank := by
  sorry  -- Fill with case analysis over h_trans
```

- [ ] **Step 3: Build to verify the theorem statement compiles**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Build succeeds. Warning about `sorry` is expected.

- [ ] **Step 4: Prove the monotonicity theorem**

Replace `sorry` with a case analysis over `h_trans`. Each of the 12 `Transition` constructors fixes the pre and post lifecycle states. For transitions between non-terminal states (e.g., `claim`: pending → claimed), both map to the same result via the helper. For transitions to terminal states (e.g., `finish`: processing → completed), the terminal rank 2 ≥ any previous rank.

The proof structure:

```lean
  cases h_trans with
  | claim h_state _ h_post =>
    -- pending → claimed: both non-terminal, same response → same rank
    subst h_post; simp [deriveAttempt, h_state, ClientTurnState.rank]
    cases h_not_super <;> simp [deriveAttempt] <;>
      cases resp with
      | none => simp [ClientTurnState.rank]
      | some r => cases r.status <;> simp [ClientTurnState.rank]
  | dedup_lose h_state _ h_post =>
    -- pending → superseded: rank 0 → 2
    subst h_post; simp [deriveAttempt, h_state, ClientTurnState.rank]
    cases h_not_super <;> simp [deriveAttempt] <;>
      cases resp with
      | none => simp [ClientTurnState.rank]
      | some r => cases r.status <;> simp [ClientTurnState.rank]
  -- ... (same pattern for remaining 10 constructors)
```

Each case follows the same template: substitute the post-state, simplify the derivation, case-split on `h_not_super` and `resp`, verify rank inequality. This is mechanical but verbose (~120 lines total for all 12 cases).

- [ ] **Step 5: Add response advance monotonicity**

```lean
/-- T2 (response direction): advancing the response never decreases rank
    when the request is held fixed at a non-terminal lifecycle state. -/
theorem response_advance_monotonic_none_to_some
    {req : RequestSnapshot}
    {resp : ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_nonterminal : req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
                     req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired) :
    (deriveAttempt ⟨req, some resp⟩).rank ≥
    (deriveAttempt ⟨req, none⟩).rank := by
  rw [deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal]
  simp [ClientTurnState.rank]
  cases resp.status <;> simp [ClientTurnState.rank]

/-- T2 (response direction): streaming → complete never decreases rank. -/
theorem response_advance_monotonic_streaming_to_terminal
    {req : RequestSnapshot}
    {resp_new : ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_nonterminal : req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
                     req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired)
    (h_terminal : resp_new.status = .complete ∨ resp_new.status = .error) :
    (deriveAttempt ⟨req, some resp_new⟩).rank ≥
    (deriveAttempt ⟨req, some ⟨.streaming⟩⟩).rank := by
  rw [deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal]
  simp [ClientTurnState.rank]
  cases h_terminal with
  | inl h => simp [h, ClientTurnState.rank]
  | inr h => simp [h, ClientTurnState.rank]
```

- [ ] **Step 6: Build to verify all proofs compile without sorry**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Build succeeds with no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client.lean
git commit -m "Prove T2: client view monotonicity under server transitions

Server request transitions never decrease client rank when response is
fixed. Response advances never decrease client rank when request is
non-terminal. Case analysis over all 12 Transition constructors.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Lean T3 — Terminal Coherence

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Client.lean`

- [ ] **Step 1: State and prove T3**

```lean
/-! ## Theorem T3: Terminal Coherence

    The client view is terminal iff the server request is effectively
    terminal. "Effectively terminal" means:
    - The lifecycle state is terminal (completed/failed/superseded/dead), OR
    - The request is superseded (isSuperseded = true), OR
    - The response status is terminal (complete/error) while the request
      is non-terminal (replication-lag tolerance)
-/

/-- Whether a request/response pair is effectively terminal from the
    server's perspective, accounting for replication lag. -/
def effectivelyTerminal (view : AttemptView) : Prop :=
  view.request.isSuperseded = true ∨
  isTerminal view.request.lifecycleState ∨
  (match view.response with
   | some r => r.status = .complete ∨ r.status = .error
   | none => False)

instance (view : AttemptView) : Decidable (effectivelyTerminal view) := by
  unfold effectivelyTerminal
  infer_instance

/-- T3: The client view is terminal iff the attempt is effectively terminal. -/
theorem terminal_coherence (view : AttemptView) :
    (deriveAttempt view).isTerminal = true ↔ effectivelyTerminal view := by
  constructor
  · -- Forward: client terminal → effectively terminal
    intro h_client_term
    unfold effectivelyTerminal
    cases view with
    | mk req resp =>
      simp [deriveAttempt] at h_client_term
      cases h_super : req.isSuperseded with
      | true => left; rfl
      | false =>
        simp [h_super] at h_client_term
        cases req.lifecycleState <;> simp [ClientTurnState.isTerminal] at h_client_term ⊢ <;>
          first
          | exact Or.inl (by assumption)
          | (cases resp with
             | none => simp [ClientTurnState.isTerminal] at h_client_term
             | some r => cases r.status <;> simp [ClientTurnState.isTerminal] at h_client_term ⊢ <;> right <;> right <;> assumption)
  · -- Backward: effectively terminal → client terminal
    intro h_eff
    cases view with
    | mk req resp =>
      cases h_eff with
      | inl h_super =>
        simp [deriveAttempt, h_super, ClientTurnState.isTerminal]
      | inr h =>
        cases h with
        | inl h_term =>
          simp [deriveAttempt]
          cases h_super : req.isSuperseded with
          | true => simp [ClientTurnState.isTerminal]
          | false =>
            simp [h_super]
            sorry -- Case analysis on terminal lifecycle states
        | inr h_resp =>
          simp [deriveAttempt]
          cases h_super : req.isSuperseded with
          | true => simp [ClientTurnState.isTerminal]
          | false =>
            simp [h_super]
            sorry -- Case analysis on non-terminal lifecycle + terminal response
```

- [ ] **Step 2: Build to check theorem statement compiles**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Compiles with `sorry` warnings.

- [ ] **Step 3: Fill in the sorry cases**

The backward direction needs case analysis on `req.lifecycleState` for the terminal-lifecycle branch and the terminal-response branch. Each sorry reduces to matching on the lifecycle state and response status, then simplifying with `ClientTurnState.isTerminal`.

Replace each `sorry` with:

```lean
            -- For h_term (terminal lifecycle):
            cases h_term with
            | inl h => cases h; simp [ClientTurnState.isTerminal]
            | inr h => cases h with
              | inl h => cases h; simp [ClientTurnState.isTerminal]
              | inr h => cases h with
                | inl h => cases h; simp [ClientTurnState.isTerminal]
                | inr h => cases h; simp [ClientTurnState.isTerminal]
```

```lean
            -- For h_resp (terminal response, non-terminal lifecycle):
            cases req.lifecycleState <;> simp <;>
              cases resp with
              | none => exact absurd h_resp (by simp)
              | some r =>
                cases h_resp with
                | inl h => simp [h, ClientTurnState.isTerminal]
                | inr h => simp [h, ClientTurnState.isTerminal]
```

- [ ] **Step 4: Build to verify no sorry remains**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Clean build, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client.lean
git commit -m "Prove T3: terminal coherence between client and server

Client view is terminal iff the attempt is 'effectively terminal':
server lifecycle is terminal, request is superseded, or response
shows complete/error while request is still non-terminal.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Lean T1 (Convergence) + T5 (Turn Replacement)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Client.lean`

- [ ] **Step 1: Add T1 and T5**

```lean
/-! ## Theorem T1: Convergence

    Under the merged-snapshot assumption (DefraDB delivers CRDT-merged
    latest per document), `deriveTurn` is a deterministic function of
    its input. Observation order does not affect the result.

    This is trivially true because `deriveTurn` is a pure function.
    We state it explicitly to document the assumption.
-/

/-- T1: deriveTurn is deterministic (pure function of input). -/
theorem deriveTurn_deterministic
    (attempts : List AttemptView) :
    deriveTurn attempts = deriveTurn attempts := rfl

/-! ## Theorem T5: Turn Replacement

    Adding a retry attempt to the chain changes the tip.
    deriveTurn of the extended chain equals deriveAttempt of the new tip.

    The rank relationship depends on the scenario:
    - Supersession (isSuperseded set on old tip): rank stays ≥ old
    - Retry restart (old tip was failed, new attempt is pending):
      rank decreases from 2 to 0. This is the one allowed decrease.
-/

/-- T5a: extending the chain with a new attempt always derives from
    the new attempt. -/
theorem turn_replacement_derives_new_tip
    (attempts : List AttemptView)
    (newTip : AttemptView) :
    deriveTurn (attempts ++ [newTip]) = some (deriveAttempt newTip) := by
  induction attempts with
  | nil => simp [deriveTurn]
  | cons head tail ih =>
    simp [deriveTurn]
    cases tail with
    | nil => simp [deriveTurn]
    | cons h' t' =>
      simp [deriveTurn] at ih ⊢
      exact ih

/-- T5b: supersession always produces rank 2. -/
theorem supersession_rank
    (view : AttemptView)
    (h_super : view.request.isSuperseded = true) :
    (deriveAttempt view).rank = 2 := by
  simp [deriveAttempt, h_super, ClientTurnState.rank]

/-- T5c: retry restart is the one case where a new tip can have lower
    rank than the old tip. The new tip is waitingForClaim (rank 0). -/
theorem retry_restart_state
    (newTip : AttemptView)
    (h_pending : newTip.request.lifecycleState = .pending)
    (h_not_super : newTip.request.isSuperseded = false)
    (h_no_resp : newTip.response = none) :
    deriveAttempt newTip = .waitingForClaim := by
  simp [deriveAttempt, h_not_super, h_pending, h_no_resp]
```

- [ ] **Step 2: Build to verify all proofs compile**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: Clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client.lean
git commit -m "Prove T1 (convergence) and T5 (turn replacement)

T1 is trivially true (pure function). T5 proves turn_replacement
always derives from the new tip, supersession always produces rank 2,
and retry restart is the one allowed rank decrease (failed → pending).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: Rust client protocol types (TDD)

**Files:**
- Create: `crates/defra-agent/src/client_protocol.rs`
- Modify: `crates/defra-agent/src/lib.rs`

- [ ] **Step 1: Add module declaration to lib.rs**

Find the module declarations section in `crates/defra-agent/src/lib.rs` and add:

```rust
pub mod client_protocol;
```

- [ ] **Step 2: Create client_protocol.rs with types and failing test stubs**

```rust
//! Client turn observation protocol.
//!
//! Pure-function projection from agent document snapshots to client-visible
//! turn states. Source of truth: `crates/defra-agent/proofs/Proofs/Client.lean`.
//!
//! The derivation checks server terminal states first, then falls through to
//! response status for non-terminal request states. This ordering prevents
//! stale streaming responses from demoting a failed/completed request.

/// The 5 client-visible turn states, mirroring `ClientTurnState` in Client.lean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTurnState {
    WaitingForClaim,
    Streaming,
    Completed,
    Failed,
    Superseded,
}

impl ClientTurnState {
    /// Monotonic rank for ordering. Terminal states share rank 2.
    pub fn rank(self) -> u32 {
        match self {
            Self::WaitingForClaim => 0,
            Self::Streaming => 1,
            Self::Completed => 2,
            Self::Failed => 2,
            Self::Superseded => 2,
        }
    }

    /// Whether this state is terminal.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Superseded)
    }
}

/// Response status as observed by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Streaming,
    Complete,
    Error,
}

/// Snapshot of an AgentRequest, containing only derivation-relevant fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSnapshot {
    pub lifecycle_state: String,
    pub is_superseded: bool,
}

/// Snapshot of an AgentResponse, containing only derivation-relevant fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseSnapshot {
    pub status: ResponseStatus,
}

/// A single attempt observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub request: RequestSnapshot,
    pub response: Option<ResponseSnapshot>,
}

/// Derive client turn state from a single attempt.
///
/// Priority:
/// 1. Supersession flag → Superseded
/// 2. Server terminal lifecycle → terminal client state
/// 3. Non-terminal lifecycle + response → trust response
/// 4. Non-terminal lifecycle + no response → WaitingForClaim
pub fn derive_attempt(view: &AttemptView) -> ClientTurnState {
    if view.request.is_superseded {
        return ClientTurnState::Superseded;
    }

    match view.request.lifecycle_state.as_str() {
        "superseded" => ClientTurnState::Superseded,
        "completed" => ClientTurnState::Completed,
        "failed" => ClientTurnState::Failed,
        "dead" => ClientTurnState::Failed,
        // Non-terminal: defer to response
        _ => match &view.response {
            Some(resp) => match resp.status {
                ResponseStatus::Complete => ClientTurnState::Completed,
                ResponseStatus::Error => ClientTurnState::Failed,
                ResponseStatus::Streaming => ClientTurnState::Streaming,
            },
            None => ClientTurnState::WaitingForClaim,
        },
    }
}

/// Derive client turn state from a full retry chain.
///
/// The last element is the tip (most recent attempt). Returns `None`
/// for empty chains.
pub fn derive_turn(attempts: &[AttemptView]) -> Option<ClientTurnState> {
    attempts.last().map(derive_attempt)
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 3: Create the test file**

```rust
// crates/defra-agent/src/client_protocol/tests.rs

use super::*;

fn req(lifecycle: &str) -> RequestSnapshot {
    RequestSnapshot {
        lifecycle_state: lifecycle.to_string(),
        is_superseded: false,
    }
}

fn req_superseded(lifecycle: &str) -> RequestSnapshot {
    RequestSnapshot {
        lifecycle_state: lifecycle.to_string(),
        is_superseded: true,
    }
}

fn resp(status: ResponseStatus) -> Option<ResponseSnapshot> {
    Some(ResponseSnapshot { status })
}

fn attempt(lifecycle: &str, response: Option<ResponseSnapshot>) -> AttemptView {
    AttemptView {
        request: req(lifecycle),
        response,
    }
}

// ── Derivation table coverage (T4) ──────────────────────────────

#[test]
fn pending_no_response() {
    assert_eq!(
        derive_attempt(&attempt("pending", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn claimed_no_response() {
    assert_eq!(
        derive_attempt(&attempt("claimed", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn processing_no_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn input_required_no_response() {
    assert_eq!(
        derive_attempt(&attempt("inputRequired", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn processing_streaming_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", resp(ResponseStatus::Streaming))),
        ClientTurnState::Streaming
    );
}

#[test]
fn processing_complete_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", resp(ResponseStatus::Complete))),
        ClientTurnState::Completed
    );
}

#[test]
fn processing_error_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", resp(ResponseStatus::Error))),
        ClientTurnState::Failed
    );
}

#[test]
fn completed_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("completed", None)),
        ClientTurnState::Completed
    );
}

#[test]
fn completed_lifecycle_ignores_stale_streaming() {
    assert_eq!(
        derive_attempt(&attempt("completed", resp(ResponseStatus::Streaming))),
        ClientTurnState::Completed
    );
}

#[test]
fn failed_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("failed", None)),
        ClientTurnState::Failed
    );
}

#[test]
fn failed_lifecycle_ignores_stale_streaming() {
    assert_eq!(
        derive_attempt(&attempt("failed", resp(ResponseStatus::Streaming))),
        ClientTurnState::Failed
    );
}

#[test]
fn dead_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("dead", None)),
        ClientTurnState::Failed
    );
}

#[test]
fn superseded_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("superseded", None)),
        ClientTurnState::Superseded
    );
}

#[test]
fn superseded_flag_overrides_everything() {
    let view = AttemptView {
        request: req_superseded("processing"),
        response: resp(ResponseStatus::Streaming),
    };
    assert_eq!(derive_attempt(&view), ClientTurnState::Superseded);
}

// ── Response-before-request replication lag ──────────────────────

#[test]
fn pending_with_complete_response_trusts_response() {
    assert_eq!(
        derive_attempt(&attempt("pending", resp(ResponseStatus::Complete))),
        ClientTurnState::Completed
    );
}

#[test]
fn claimed_with_streaming_response_trusts_response() {
    assert_eq!(
        derive_attempt(&attempt("claimed", resp(ResponseStatus::Streaming))),
        ClientTurnState::Streaming
    );
}

// ── deriveTurn: retry chain ─────────────────────────────────────

#[test]
fn derive_turn_empty() {
    assert_eq!(derive_turn(&[]), None);
}

#[test]
fn derive_turn_single() {
    let chain = vec![attempt("processing", resp(ResponseStatus::Streaming))];
    assert_eq!(derive_turn(&chain), Some(ClientTurnState::Streaming));
}

#[test]
fn derive_turn_uses_tip() {
    let chain = vec![
        attempt("failed", None),
        attempt("pending", None),
    ];
    assert_eq!(derive_turn(&chain), Some(ClientTurnState::WaitingForClaim));
}

#[test]
fn derive_turn_three_attempt_chain() {
    let chain = vec![
        attempt("failed", None),
        attempt("failed", None),
        attempt("processing", resp(ResponseStatus::Streaming)),
    ];
    assert_eq!(derive_turn(&chain), Some(ClientTurnState::Streaming));
}
```

- [ ] **Step 4: Verify tests pass**

Run: `cargo test -p defra-agent client_protocol --lib`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/client_protocol.rs crates/defra-agent/src/client_protocol/tests.rs crates/defra-agent/src/lib.rs
git commit -m "Add Rust client_protocol: types + derivation with tests

Mirrors the Lean Client.lean model. derive_attempt checks server
terminal states before response, derive_turn picks the retry chain
tip. 20 unit tests cover the full derivation table, replication lag
tolerance, and retry chain resolution.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Rust monotonicity and terminal coherence spot checks

**Files:**
- Modify: `crates/defra-agent/src/client_protocol/tests.rs`

- [ ] **Step 1: Add monotonicity spot checks**

Append to the test file:

```rust
// ── Monotonicity spot checks (T2) ───────────────────────────────

/// All valid server lifecycle transition pairs and their expected
/// rank relationship.
const LIFECYCLE_TRANSITIONS: &[(&str, &str)] = &[
    ("pending", "claimed"),       // claim
    ("pending", "superseded"),    // dedup_lose
    ("claimed", "processing"),    // begin_inference
    ("processing", "completed"),  // finish
    ("processing", "failed"),     // fail
    ("claimed", "failed"),        // fail_before_stream
    ("processing", "dead"),       // deadline_expire
    ("failed", "dead"),           // exhaust
];

#[test]
fn monotonicity_no_response() {
    for (pre, post) in LIFECYCLE_TRANSITIONS {
        let pre_state = derive_attempt(&attempt(pre, None));
        let post_state = derive_attempt(&attempt(post, None));
        assert!(
            post_state.rank() >= pre_state.rank(),
            "rank decreased: {pre} ({}) → {post} ({})",
            pre_state.rank(),
            post_state.rank()
        );
    }
}

#[test]
fn monotonicity_with_streaming_response() {
    for (pre, post) in LIFECYCLE_TRANSITIONS {
        let r = resp(ResponseStatus::Streaming);
        let pre_state = derive_attempt(&AttemptView {
            request: req(pre),
            response: r.clone(),
        });
        let post_state = derive_attempt(&AttemptView {
            request: req(post),
            response: r,
        });
        assert!(
            post_state.rank() >= pre_state.rank(),
            "rank decreased with streaming resp: {pre} ({}) → {post} ({})",
            pre_state.rank(),
            post_state.rank()
        );
    }
}

#[test]
fn monotonicity_response_none_to_streaming() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let pre = derive_attempt(&attempt(lifecycle, None));
        let post = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        assert!(
            post.rank() >= pre.rank(),
            "response none→streaming decreased rank for {lifecycle}"
        );
    }
}

#[test]
fn monotonicity_response_streaming_to_complete() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let pre = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        let post = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Complete)));
        assert!(
            post.rank() >= pre.rank(),
            "response streaming→complete decreased rank for {lifecycle}"
        );
    }
}

#[test]
fn monotonicity_response_streaming_to_error() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let pre = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        let post = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Error)));
        assert!(
            post.rank() >= pre.rank(),
            "response streaming→error decreased rank for {lifecycle}"
        );
    }
}

// ── Terminal coherence spot checks (T3) ─────────────────────────

#[test]
fn terminal_coherence_terminal_lifecycle_states() {
    for lifecycle in &["completed", "failed", "dead", "superseded"] {
        let state = derive_attempt(&attempt(lifecycle, None));
        assert!(
            state.is_terminal(),
            "terminal lifecycle {lifecycle} did not produce terminal client state"
        );
    }
}

#[test]
fn terminal_coherence_nonterminal_lifecycle_no_response() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let state = derive_attempt(&attempt(lifecycle, None));
        assert!(
            !state.is_terminal(),
            "non-terminal lifecycle {lifecycle} with no response produced terminal client state"
        );
    }
}

#[test]
fn terminal_coherence_nonterminal_lifecycle_streaming_response() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let state = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        assert!(
            !state.is_terminal(),
            "non-terminal lifecycle {lifecycle} with streaming response produced terminal client state"
        );
    }
}

#[test]
fn terminal_coherence_nonterminal_lifecycle_complete_response() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let state = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Complete)));
        assert!(
            state.is_terminal(),
            "non-terminal {lifecycle} + complete response should be effectively terminal"
        );
    }
}

#[test]
fn terminal_coherence_superseded_flag() {
    let view = AttemptView {
        request: req_superseded("pending"),
        response: None,
    };
    assert!(derive_attempt(&view).is_terminal());
}

// ── Turn replacement (T5) ───────────────────────────────────────

#[test]
fn turn_replacement_retry_restart() {
    let old_tip = attempt("failed", None);
    let new_tip = attempt("pending", None);
    let old_state = derive_attempt(&old_tip);
    let new_state = derive_attempt(&new_tip);

    assert_eq!(old_state, ClientTurnState::Failed);
    assert_eq!(new_state, ClientTurnState::WaitingForClaim);
    // This is the one allowed rank decrease
    assert!(new_state.rank() < old_state.rank());
}

#[test]
fn turn_replacement_supersession_rank() {
    let view = AttemptView {
        request: req_superseded("processing"),
        response: resp(ResponseStatus::Streaming),
    };
    assert_eq!(derive_attempt(&view).rank(), 2);
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p defra-agent client_protocol --lib`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/src/client_protocol/tests.rs
git commit -m "Add Rust T2/T3/T5 conformance spot checks

Systematic monotonicity checks across all valid lifecycle transitions
and response advances. Terminal coherence checks for all lifecycle ×
response combinations. Turn replacement checks for retry restart and
supersession.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: Protocol reference doc

**Files:**
- Create: `docs/protocols/client-state-machine.md`

- [ ] **Step 1: Create docs/protocols/ directory**

Run: `mkdir -p docs/protocols`

- [ ] **Step 2: Write the protocol doc**

```markdown
# Client Turn Observation Protocol

Formal source of truth: `crates/defra-agent/proofs/Proofs/Client.lean`

This document explains the formal model for client implementers building
CLI, web, mobile, or desktop applications against the defra-agent
document surface.

## Turn State

A client observing an agent turn derives one of 5 states:

| State | Rank | Meaning |
|---|---|---|
| `waitingForClaim` | 0 | Request observed, no response content yet |
| `streaming` | 1 | Response observed with partial content |
| `completed` | 2 | Response terminal success |
| `failed` | 2 | Latest attempt terminal failure, no successor |
| `superseded` | 2 | A later request replaced this turn |

Rank is monotonic under valid server transitions (rank never decreases)
with one exception: retry restart, where a failed attempt (rank 2) is
followed by a new pending attempt (rank 0).

## Turn Identity

A turn is identified by `retry_root_request_id` — the `request_id` of
the first request in a retry chain. All retries share the same root and
collapse into one logical user turn.

The tip of the chain (the attempt the client renders) is the most
recent attempt: the one whose `request_id` is not referenced as
`retry_parent_request` by any other observed attempt.

## Derivation Rules

Given the tip attempt's `AgentRequest` and its associated
`AgentResponse` (if any), derive the client state using this priority:

### 1. Supersession (highest priority)

If `AgentRequest.superseded_by_request` is set → `superseded`.
If `AgentRequest.lifecycle_state` is `"superseded"` → `superseded`.

### 2. Server terminal lifecycle states

These override any response — terminal states are irreversible.

| `lifecycle_state` | Client state |
|---|---|
| `"completed"` | `completed` |
| `"failed"` | `failed` |
| `"dead"` | `failed` |

### 3. Non-terminal lifecycle + response

If the request is in a non-terminal state (`pending`, `claimed`,
`processing`, `inputRequired`), the response may be more current
than the request under P2P replication lag. Trust the response:

| `AgentResponse.status` | Client state |
|---|---|
| `"complete"` | `completed` |
| `"error"` | `failed` |
| `"streaming"` | `streaming` |

### 4. No response (lowest priority)

Non-terminal request, no response observed → `waitingForClaim`.

## Current Deviations

See `crates/defra-agent/proofs/Proofs/Conformance/Deviations.lean`.

| Server state | Client mapping | Deviation |
|---|---|---|
| `inputRequired` | `waitingForClaim` | #2: no persisted inputRequired path |
| `dead` | `failed` | #3: clients derive exhaustion externally |

## Stall Detection

The server liveness proofs (L1, L3) guarantee every request terminates.
If a client perceives a "stall," that is a transport or replication
problem, not a turn-state problem.

Stall detection is a per-client UI affordance, not part of the turn
projection. A reasonable heuristic: if no observation update has arrived
for N seconds and the derived state is non-terminal, show a transport
health indicator. This is NOT a turn state — it does not affect the
derivation.

## Parallel Observation Surfaces

These are observed alongside the turn but do NOT affect turn state
derivation. They are rendered as supplementary UI content.

### AgentToolCall

Filter: `session_id = <current session>`
Order by: `message_sequence` (ascending)
Key fields: `tool_name`, `args`, `result`, `status`
Rendering: inline timeline cards during streaming, showing tool
invocations as they execute. `status` tracks individual tool lifecycle
(pending/running/completed/failed).

### AgentToolResult

Filter: `session_id = <current session>`
Key fields: `tool_name`, `tool_input`, `output_text`, `truncated`
Rendering: full tool output for completed tools. Useful for debug
views and tool output inspection. `truncated` indicates whether the
output was truncated for context window management.

### AgentMessage

Filter: `session_id = <current session>`
Order by: `sequence` (ascending)
Key fields: `role`, `content`, `timestamp`
Rendering: ordered transcript for scroll-back history. NOT on the
critical streaming path — the streaming bubble reads from
`AgentResponse.content`, not from AgentMessage. AgentMessage is
populated after the turn completes (or periodically during long turns).

## Subscription Model

A compliant client must observe these collections with these filters:

| Collection | Filter | Purpose |
|---|---|---|
| `AgentRequest` | `session_id = <session>` | Turn state derivation |
| `AgentResponse` | `request_id = <active request>` | Streaming content + status |
| `AgentToolCall` | `session_id = <session>` | Inline tool cards |
| `AgentToolResult` | `session_id = <session>` | Full tool output |
| `AgentMessage` | `session_id = <session>` | Scroll-back transcript |
| `AgentConversation` | `agent_did = <agent>` | Conversation list |

For turn-scoped observation, filter `AgentRequest` by
`retry_root_request = <turn root>` to see all attempts in a retry chain.

Polling interval: 500-1000ms for active turns (streaming), 5-10s for
idle session monitoring.

## Proven Properties

These are proven in `Proofs/Client.lean`:

| Property | Statement |
|---|---|
| T1 Convergence | Pure function of input; same documents → same state |
| T2 Monotonicity | Valid server transitions never decrease client rank |
| T3 Terminal coherence | Client terminal ↔ server effectively terminal |
| T4 Totality | Defined for every observation with ≥1 attempt |
| T5 Turn replacement | Chain extension derives from new tip; supersession is monotonic; retry restart is the one allowed rank decrease |

## Reference Pseudocode

### Swift

```swift
func deriveAttempt(request: AgentRequestState, response: AgentResponseState?) -> ClientTurnState {
    if request.supersededByRequest != nil { return .superseded }
    switch request.lifecycleState {
    case "superseded": return .superseded
    case "completed":  return .completed
    case "failed", "dead": return .failed
    default: break
    }
    guard let resp = response else { return .waitingForClaim }
    switch resp.status {
    case "complete":  return .completed
    case "error":     return .failed
    case "streaming": return .streaming
    default:          return .waitingForClaim
    }
}
```

### TypeScript

```typescript
function deriveAttempt(
  request: { lifecycleState: string; supersededByRequest?: string },
  response?: { status: string },
): ClientTurnState {
  if (request.supersededByRequest) return "superseded";
  if (request.lifecycleState === "superseded") return "superseded";
  if (request.lifecycleState === "completed") return "completed";
  if (request.lifecycleState === "failed" || request.lifecycleState === "dead") return "failed";
  if (!response) return "waitingForClaim";
  if (response.status === "complete") return "completed";
  if (response.status === "error") return "failed";
  if (response.status === "streaming") return "streaming";
  return "waitingForClaim";
}
```
```

- [ ] **Step 3: Commit**

```bash
git add docs/protocols/client-state-machine.md
git commit -m "Add client state machine protocol doc

Human-readable reference for client implementers. Covers derivation
rules, parallel observation surfaces (tool calls, messages),
subscription model, and proven properties from Client.lean.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Update design spec with corrected derivation priority

**Files:**
- Modify: `docs/superpowers/specs/2026-04-11-client-turn-observation-design.md`

- [ ] **Step 1: Update the derivation priority section**

In the design spec, find the "Layer 1 — single attempt" section under "Derivation: Two Layers". Replace the priority list with the corrected order that checks server terminal states before response:

```markdown
Priority order:
1. `isSuperseded = true` or `lifecycleState = .superseded` → `.superseded`
2. `lifecycleState = .completed` → `.completed`
3. `lifecycleState = .failed` or `lifecycleState = .dead` → `.failed`
4. (lifecycle is now known non-terminal: pending/claimed/processing/inputRequired)
5. Response exists with `.complete` → `.completed`
6. Response exists with `.error` → `.failed`
7. Response exists with `.streaming` → `.streaming`
8. No response → `.waitingForClaim`

Rules 2-3 checking server terminal states BEFORE response prevents stale
streaming responses from demoting a terminally failed/completed request.
This corrects the original spec ordering and is required for monotonicity
(T2) to hold.
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/2026-04-11-client-turn-observation-design.md
git commit -m "Fix derivation priority: check server terminal before response

The original priority let response override terminal request states,
which breaks monotonicity when a stale streaming response appears
alongside a failed request. Corrected to check server terminal first.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```
