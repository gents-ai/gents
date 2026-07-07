# Completion Retry (#631) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retry failed completion requests at the per-completion seam inside the owned loop — transport ladder, 400 resample/repair, mid-stream retract-or-continue — with a Lean-proved retry state machine, per-origin document config, and InferenceCall observability; delete the daemon whole-run retry (closes #638).

**Architecture:** A new Lean model (`Proofs/CompletionRetry.lean`) defines the legal retry transitions; a pure Rust mirror (`agent/completion_retry.rs`) makes retry decisions; `run_loop_stream` consumes those decisions around `model.stream()` and yields a native `LoopStreamItem` enum so the daemon's `StreamProcessor` can retract un-persisted turns. The daemon's outer attempt loop is removed.

**Tech Stack:** Lean 4 (lake), Rust (tokio, async-stream, rig Layer A), DefraDB SDL.

**Spec:** `docs/superpowers/specs/2026-07-07-completion-retry-631-design.md` — read it before starting any task.

## Global Constraints

- Worktree: `../defra-agent-completion-retry-631`, branch `issue-631-completion-retry`. All commands run from the worktree root.
- **Lean first, zero `sorry`s.** Tasks 1–3 land before any Rust behavior change. `lake build` must pass clean.
- **Gate with the full package suite:** `cargo test -p defra-agent` — never `--lib` (integration tests are separate compile units).
- **Never emit `[]` in a DefraDB mutation** — empty list literals corrupt nillable array columns; emit `null`.
- **Always `graphql::escape_graphql_string()`** for anything interpolated into GraphQL.
- `tracing`, never `println`.
- Retry never re-executes tools; retry never extends the claimed deadline; repair fires at most once per request.
- Default ladders: scheduled = 5s/30s/2m transport + 1 resample + 1 repair; interactive = 1×2s + repair.
- Lean proofs in this worktree: `lake exe cache get` crashes on macOS — symlink the parent repo's `.lake/packages/mathlib/.lake/build` into this worktree's proofs dir first (see memory: lean-worktree-mathlib-cache), then `lake build`.
- Commit after every task (and at marked mid-task points); commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: Lean CompletionRetry — State and Transition

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/CompletionRetry.lean` (barrel)
- Create: `crates/defra-agent/proofs/Proofs/CompletionRetry/State.lean`
- Create: `crates/defra-agent/proofs/Proofs/CompletionRetry/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean` (add `import Proofs.CompletionRetry` after `import Proofs.ToolExecution`)

**Interfaces:**
- Produces: `CompletionRetry.State`, `CompletionRetry.Phase`, `CompletionRetry.FailureClass`, `CompletionRetry.Budget`, `CompletionRetry.Transition` — consumed verbatim by Tasks 2 and 4, mirrored by Rust in Task 3.

- [ ] **Step 1: Write `State.lean`**

```lean
import Proofs.Basic

/-! # CompletionRetry: per-completion retry inside the owned loop (#631)

One request runs one owned loop; each turn issues completions. This model
governs how a single request's completion failures are retried. It is the
formal fence for: no tool re-execution on retry, retraction only before
effects, bounded budgets with at-most-once repair, backoff that fits the
claimed deadline, and at most one retained rendered turn per turn index. -/

namespace CompletionRetry

/-- Retry-relevant failure classification. Mirrors the Rust
`InferenceError` retry classes produced by `classify_completion_error`:
`transport` covers transient/timeout/rate-limit/5xx; `parseBadRequest` is
the vLLM tool-call json-parse 400 signature; `permanent` is everything
else. -/
inductive FailureClass
  | transport
  | parseBadRequest
  | permanent
  deriving DecidableEq, Repr

/-- Per-request retry budget, resolved from InferenceProfile + execution
origin before the loop starts. `transportRetries` is the ladder length. -/
structure Budget where
  transportRetries : Nat
  resampleRetries : Nat
  allowRepair : Bool
  deriving DecidableEq, Repr

/-- Where the current completion attempt stands. `backingOff until` holds
the wake time so deadline-fit is checkable at transition time. -/
inductive Phase
  | issuing
  | streaming
  | backingOff (wake : Time)
  | repairing
  | turnClosed      -- mid-stream failure after effects: partial turn + results durably threaded
  | turnDone        -- completion consumed to its end
  | exhausted       -- retry budget or deadline exhausted → terminal failed
  | failedPermanent -- non-retryable classification → terminal failed
  deriving DecidableEq, Repr

/-- Per-turn effect/render tracking. `turnIndex` identifies the current
turn; `effects` counts tool executions in the CURRENT turn; `rendered`
counts retained rendered instances of the current turn index in the
materialized response. Close-and-continue increments `turnIndex` (a new
turn with fresh counters); retraction keeps it (same turn, re-sampled). -/
structure TurnCtx where
  turnIndex : Nat
  effects : Nat
  rendered : Nat
  deriving DecidableEq, Repr

structure State where
  phase : Phase
  budget : Budget
  transportUsed : Nat
  resampleUsed : Nat
  repairUsed : Bool
  /-- Last parse-400 error text, for deterministic-400 detection. Opaque. -/
  lastParseError : Option String
  now : Time
  deadline : Option Time
  turn : TurnCtx
  deriving DecidableEq, Repr

/-- A wake time fits the deadline iff it does not pass it. No deadline
means the ladder itself is the ceiling. -/
def fitsDeadline (wake : Time) (deadline : Option Time) : Prop :=
  match deadline with
  | none => True
  | some d => wake ≤ d

def State.terminal (s : State) : Prop :=
  s.phase = Phase.turnDone ∨ s.phase = Phase.exhausted ∨ s.phase = Phase.failedPermanent

end CompletionRetry
```

- [ ] **Step 2: Write `Transition.lean`**

```lean
import Proofs.CompletionRetry.State

namespace CompletionRetry

/-- Legal transitions of the per-completion retry machine.

Failure transitions consume the observed `FailureClass`: transport
failures may only take the transport constructors, parse-400s the
resample/repair constructors, permanent classifications only
`failPermanent`. Design invariants are structural here:
- every entry into `backingOff`/`repairing` requires `s.turn.effects = 0`
  or increments the turn index (`continueAfterClose`), so re-issues never
  face open effects (proved as an invariant in `Properties.lean`);
- `retract` requires `s.turn.effects = 0`, zeroes `rendered`, and keeps
  the turn index — it is the only same-turn rendered decrease;
- every entry into `backingOff wake` carries `fitsDeadline wake s.deadline`;
- `repair` requires `¬ s.repairUsed`; `repairIssue` sets it. -/
inductive Transition : State → State → Prop
  /-- Issue the completion for the current turn. -/
  | issue (s : State) (h : s.phase = Phase.issuing) :
      Transition s { s with phase := Phase.streaming }

  /-- A tool executed during the current streaming turn. -/
  | toolEffect (s : State) (h : s.phase = Phase.streaming) :
      Transition s { s with turn := { s.turn with effects := s.turn.effects + 1 } }

  /-- The completion streamed to its end; its turn is retained once. -/
  | streamOk (s : State) (h : s.phase = Phase.streaming) :
      Transition s { s with phase := Phase.turnDone,
                            turn := { s.turn with rendered := 1 } }

  /-- Pre-stream / no-yield transport-class failure with ladder + deadline
  room. -/
  | transportBackoff (s : State) (c : FailureClass) (wake : Time)
      (hc : c = FailureClass.transport)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1 }

  /-- Transport-class failure with no ladder or no deadline room → terminal. -/
  | transportExhaust (s : State) (c : FailureClass)
      (hc : c = FailureClass.transport)
      (hp : s.phase = Phase.streaming)
      (h : s.transportUsed ≥ s.budget.transportRetries ∨
           ∀ wake, s.now ≤ wake → ¬ fitsDeadline wake s.deadline) :
      Transition s { s with phase := Phase.exhausted }

  /-- Fresh parse-400 (differs from the last seen) with resample room. -/
  | resampleBackoff (s : State) (c : FailureClass) (err : String) (wake : Time)
      (hc : c = FailureClass.parseBadRequest)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hfresh : s.lastParseError ≠ some err)
      (hbudget : s.resampleUsed < s.budget.resampleRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            resampleUsed := s.resampleUsed + 1,
                            lastParseError := some err }

  /-- Deterministic parse-400 (identical to last) or resample budget spent:
  go straight to repair — at most once per request. -/
  | repair (s : State) (c : FailureClass) (err : String)
      (hc : c = FailureClass.parseBadRequest)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hdet : s.lastParseError = some err ∨ s.resampleUsed ≥ s.budget.resampleRetries)
      (hallow : s.budget.allowRepair)
      (hunused : ¬ s.repairUsed) :
      Transition s { s with phase := Phase.repairing,
                            lastParseError := some err }

  /-- Parse-400 with no repair left → terminal. -/
  | parseExhaust (s : State) (c : FailureClass)
      (hc : c = FailureClass.parseBadRequest)
      (hp : s.phase = Phase.streaming)
      (h : ¬ s.budget.allowRepair ∨ s.repairUsed) :
      Transition s { s with phase := Phase.exhausted }

  /-- Permanent classification → terminal, immediately. -/
  | failPermanent (s : State) (c : FailureClass)
      (hc : c = FailureClass.permanent)
      (hp : s.phase = Phase.streaming) :
      Transition s { s with phase := Phase.failedPermanent }

  /-- Mid-stream failure with NO effects this turn: retract the partial
  render, then back off toward a resample of the same turn. -/
  | retract (s : State) (wake : Time)
      (hp : s.phase = Phase.streaming)
      (heff : s.turn.effects = 0)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1,
                            turn := { s.turn with rendered := 0 } }

  /-- Mid-stream failure WITH effects: close the turn durably (partial
  assistant turn + executed tool results threaded); rendered content is
  retained and frozen. -/
  | closeTurn (s : State)
      (hp : s.phase = Phase.streaming)
      (heff : 0 < s.turn.effects) :
      Transition s { s with phase := Phase.turnClosed,
                            turn := { s.turn with rendered := 1 } }

  /-- Continue after a closed turn: next completion begins a NEW turn —
  the turn index advances and effect/render counters reset (this is NOT a
  retraction of the closed turn, whose rendered content is frozen under
  its own index). Budget consumed like a transport retry. -/
  | continueAfterClose (s : State) (wake : Time)
      (hp : s.phase = Phase.turnClosed)
      (hbudget : s.transportUsed < s.budget.transportRetries)
      (hfit : fitsDeadline wake s.deadline)
      (hfwd : s.now ≤ wake) :
      Transition s { s with phase := Phase.backingOff wake,
                            transportUsed := s.transportUsed + 1,
                            turn := { turnIndex := s.turn.turnIndex + 1,
                                      effects := 0, rendered := 0 } }

  /-- A closed turn with no budget/deadline room → terminal. -/
  | closeExhaust (s : State)
      (hp : s.phase = Phase.turnClosed)
      (h : s.transportUsed ≥ s.budget.transportRetries ∨
           ∀ wake, s.now ≤ wake → ¬ fitsDeadline wake s.deadline) :
      Transition s { s with phase := Phase.exhausted }

  /-- Wake from backoff and re-issue. Clock moves to the wake time. -/
  | wake (s : State) (w : Time) (hp : s.phase = Phase.backingOff w) :
      Transition s { s with phase := Phase.issuing, now := w }

  /-- Repair mutates the assembled input (sanitizer pass), then re-issues.
  Marks repair used. -/
  | repairIssue (s : State)
      (hp : s.phase = Phase.repairing)
      (hunused : ¬ s.repairUsed) :
      Transition s { s with phase := Phase.issuing, repairUsed := true }

end CompletionRetry
```

- [ ] **Step 3: Write the barrel `CompletionRetry.lean`**

```lean
import Proofs.CompletionRetry.State
import Proofs.CompletionRetry.Transition
```

- [ ] **Step 4: Add `import Proofs.CompletionRetry` to `Proofs.lean`** (alphabetically near `Proofs.ToolExecution`), then build

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build, no errors, no `sorry` warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs
git commit -m "spec(lean): CompletionRetry state machine for #631 — states and legal transitions"
```

---

### Task 2: Lean CompletionRetry — Executable semantics and properties N1–N5

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/CompletionRetry/Executable.lean`
- Create: `crates/defra-agent/proofs/Proofs/CompletionRetry/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/CompletionRetry.lean` (import both)
- Modify: `crates/defra-agent/proofs/README.md` (structure table row + property summary)

**Interfaces:**
- Consumes: Task 1's `State`/`Transition`.
- Produces: `CompletionRetry.Action`, `CompletionRetry.step?`, theorems `n1_reissue_requires_no_open_effects`, `n2_retract_only_before_effects`, `n3_budget_monotone_bounded`, `n3_repair_at_most_once`, `n4_backoff_fits_deadline`, `n5_rendered_at_most_one`. Task 3 emits cases from `step?`.

- [ ] **Step 1: Write `Executable.lean`** — follow the house pattern (`Proofs/RuntimeReconcile/Executable.lean` is the closest template): an `Action` inductive naming each `Transition` constructor with its data, where every failure action carries the observed `FailureClass` so `step?` genuinely consumes the classification (`issue`, `toolEffect`, `streamOk`, `preStreamFail (c : FailureClass) (err : String) (wake : Time)` — dispatched by `step?` on `c` to the transport/resample/repair/permanent branches exactly as `Transition` guards them, `transportExhaust (c : FailureClass)`, `parseExhaust (c : FailureClass)`, `retract (wake : Time)`, `closeTurn`, `continueAfterClose (wake : Time)`, `closeExhaust`, `wake (w : Time)`, `repairIssue`), a total `step? : Action → State → Option State` that checks each guard with `decide`-able conditions (use `Bool` guards mirroring the `Prop` guards; for the universally-quantified deadline-exhaust guard, use the decidable equivalent `match s.deadline with | none => false | some d => d < s.now` — no wake at or after `now` can fit iff the deadline is already behind the clock), and the two bridge theorems `step_sound : step? a s = some s' → Transition s s'` and `transition_complete : Transition s s' → ∃ a, step? a s = some s'`. Prove by case analysis on the action/transition (`cases`, `simp [step?]`, `omega` for arithmetic guards). Wake-time/delay VALUES are chosen by the action's data, not the model — the model constrains only budget counts and deadline fit; record that in the Boundaries note (Task 4).

- [ ] **Step 2: Write `Properties.lean`**

```lean
import Proofs.CompletionRetry.Transition

namespace CompletionRetry

/-- Re-issue invariant: away from an active stream (and outside a closed
turn awaiting continuation), the current turn has no open effects. This is
the inductive carrier for N1 — `continueAfterClose` re-establishes it by
resetting the (new) turn's counters. -/
def ReissueInv (s : State) : Prop :=
  (s.phase = Phase.issuing ∨ (∃ w, s.phase = Phase.backingOff w) ∨
   s.phase = Phase.repairing) → s.turn.effects = 0

theorem reissue_inv_preserved
    {s s' : State} (t : Transition s s') (h : ReissueInv s) :
    ReissueInv s' := by
  cases t <;> simp_all [ReissueInv]

/-- N1: from any invariant-satisfying state, issuing a completion
(`issuing → streaming`) happens with zero open effects — a retried or
repaired completion can never face un-accounted tool executions, so retry
never re-executes tools. -/
theorem n1_reissue_requires_no_open_effects
    {s s' : State} (t : Transition s s')
    (hinv : ReissueInv s) (h : s.phase = Phase.issuing) :
    s.turn.effects = 0 := by
  exact hinv (Or.inl h)

/-- N2: a same-turn rendered decrease (retraction) requires zero effects
this turn. `continueAfterClose` increments the turn index, so its counter
reset is starting a new turn, not retracting the closed one. -/
theorem n2_retract_only_before_effects
    {s s' : State} (t : Transition s s')
    (hr : s'.turn.rendered < s.turn.rendered)
    (hsame : s'.turn.turnIndex = s.turn.turnIndex) :
    s.turn.effects = 0 := by
  cases t <;> simp_all

/-- N3a: budget counters never decrease and never exceed their budgets. -/
theorem n3_budget_monotone_bounded
    {s s' : State} (t : Transition s s')
    (hb : s.transportUsed ≤ s.budget.transportRetries ∧
          s.resampleUsed ≤ s.budget.resampleRetries) :
    s.transportUsed ≤ s'.transportUsed ∧
    s.resampleUsed ≤ s'.resampleUsed ∧
    s'.transportUsed ≤ s'.budget.transportRetries ∧
    s'.resampleUsed ≤ s'.budget.resampleRetries := by
  cases t <;> simp_all <;> omega

/-- N3b: repair happens at most once — `repairUsed` is monotone and the
`repair` transition requires it unset. -/
theorem n3_repair_at_most_once
    {s s' : State} (t : Transition s s') (h : s.repairUsed = true) :
    s'.repairUsed = true := by
  cases t <;> simp_all

/-- N4: every backoff wake time fits the deadline and never moves the
clock backwards; retry cannot extend the deadline (deadline is immutable
across all transitions). -/
theorem n4_backoff_fits_deadline
    {s s' : State} (t : Transition s s')
    (h : s'.phase = Phase.backingOff w) :
    fitsDeadline w s.deadline ∧ s.now ≤ w ∧ s'.deadline = s.deadline := by
  cases t <;> simp_all

/-- N5: the current turn retains at most one rendered instance. -/
theorem n5_rendered_at_most_one
    {s s' : State} (t : Transition s s') (h : s.turn.rendered ≤ 1) :
    s'.turn.rendered ≤ 1 := by
  cases t <;> simp_all
```

Adjust binders/hypotheses as needed to make the statements typecheck (e.g. `w` universally bound); the *content* of each obligation must not weaken. If a theorem fails, treat it as information about the transition definitions and fix the MODEL, not the theorem.

- [ ] **Step 3: Build**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean, zero `sorry`.

- [ ] **Step 4: Update `proofs/README.md`** — add `Proofs/CompletionRetry.lean` to the structure table ("Barrel for per-completion retry state, transitions, executable semantics, and budget/deadline/effects properties"), add the barrel's submodule row (`State`, `Transition`, `Executable`, `Properties`), and a short property-summary subsection "Completion Retry" listing N1–N5 in plain English with theorem names.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs
git commit -m "spec(lean): CompletionRetry executable semantics and N1-N5 obligations (#631)"
```

---

### Task 3: Rust decision mirror — `agent/completion_retry.rs`

**Files:**
- Create: `crates/defra-agent/src/agent/completion_retry.rs` (+ `pub mod completion_retry;` in `crates/defra-agent/src/agent.rs` near the other agent submodules — **`pub`, not `pub(crate)`**: Task 4's conformance consumer lives in `tests/`, a separate compile unit)
- Test: unit tests inline in the new module (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::error::InferenceError` (existing), `crate::lifecycle::ExecutionOrigin`.
- Produces (used by Tasks 4–9 verbatim; all types `pub` for the external conformance consumer):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass { Transport, ParseBadRequest, Permanent } // mirrors Lean CompletionRetry.FailureClass
pub fn failure_class(error: &crate::error::InferenceError, error_text: &str) -> FailureClass;
pub struct CompletionRetryPolicy {
    pub transport_backoff: Vec<std::time::Duration>, // ladder; len == transport retry budget
    pub max_resample: u32,
    pub allow_repair: bool,
}
pub struct CompletionRetryState { /* policy + used counters + last_parse_error */ }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind { Transport, Resample }
pub enum PreStreamDirective {
    RetryAfter { delay: std::time::Duration, kind: RetryKind },
    Repair,
    Fail { reason: String },
}
pub enum MidStreamDirective {
    RetractAndResample { delay: std::time::Duration },
    CloseAndContinue { delay: std::time::Duration },
    Fail { reason: String },
}
impl CompletionRetryPolicy {
    pub fn scheduled_default() -> Self;   // [5s, 30s, 120s], 1 resample, repair
    pub fn interactive_default() -> Self; // [2s], 0 resample, repair
}
impl CompletionRetryState {
    pub fn new(policy: CompletionRetryPolicy) -> Self;
    pub fn retry_count(&self) -> u32;
    pub fn on_pre_stream_failure(
        &mut self,
        error: &crate::error::InferenceError,
        error_text: &str,
        now: chrono::DateTime<chrono::Utc>,
        deadline: Option<chrono::DateTime<chrono::Utc>>,
    ) -> PreStreamDirective;
    pub fn on_mid_stream_failure(
        &mut self,
        effects_this_turn: bool,
        now: chrono::DateTime<chrono::Utc>,
        deadline: Option<chrono::DateTime<chrono::Utc>>,
    ) -> MidStreamDirective;
    pub fn mark_repair_used(&mut self);
}
```

`failure_class` is the pinned classification bridge: `ModelUnreachable`/`TransientFailure`/`Timeout`/`RateLimited` → `Transport`; parse-signature detection (`error.rs::provider_message_is_tool_call_json_parse_failure` on `error_text` — make that fn `pub(crate)`) → `ParseBadRequest`; `PermanentFailure`/`ContextLengthExceeded`/`RetriesExhausted` → `Permanent`. `on_pre_stream_failure` dispatches on `failure_class`, exactly as the Lean `step?` dispatches on its `FailureClass` argument.

Decision rules (must mirror Lean `step?` exactly):
- `failure_class == Permanent` → `Fail`.
- `failure_class == ParseBadRequest`: identical `error_text` to the stored last parse error, OR resample budget spent → `Repair` if `allow_repair && !repair_used`, else `Fail`; otherwise `RetryAfter { kind: Resample }` with the next ladder delay, record `error_text`.
- `failure_class == Transport`: next ladder entry; for `RateLimited { retry_after_secs }` use `max(ladder_delay, retry_after)`.
- **Deadline fail-fast:** apply ±25% jitter to the ladder delay first (reuse the arithmetic from `RetryPolicy::delay_for_attempt`), then if `now + jittered_delay > deadline` → `Fail { reason }` immediately (never sleep into certain death).
- Mid-stream: `effects_this_turn == false` → `RetractAndResample` (consumes a transport ladder entry, same deadline check); `true` → `CloseAndContinue` (same); budget spent → `Fail`.

- [ ] **Step 1: Write failing unit tests** covering: ladder progression 5s→30s→120s then Fail; deterministic-400 (same text twice) skips remaining resample and returns Repair; Repair only once; RateLimited uses provider hint when larger; deadline fail-fast (deadline in 10s, next delay 30s → Fail, and the reason mentions the deadline); mid-stream with effects → CloseAndContinue; mid-stream without effects → RetractAndResample; interactive default = single ~2s retry; `failure_class` mapping for every `InferenceError` variant including the parse-signature `error_text` override. For deterministic jitter in tests, make jitter a private fn taking an injected `&mut impl rand::Rng` and use a seeded `rand::rngs::StdRng` in tests; production callers pass `rand::rng()`.

Run: `cargo test -p defra-agent --lib agent::completion_retry -- --nocapture` (unit-level iteration only; full gate at task end)
Expected: FAIL (module does not exist).

- [ ] **Step 2: Implement the module to the interface above.**

Run: `cargo test -p defra-agent --lib agent::completion_retry`
Expected: PASS.

- [ ] **Step 3: Full gate:** `cargo test -p defra-agent`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src
git commit -m "feat(retry): CompletionRetryState decision mirror of the Lean model (#631)"
```

---

### Task 4: Lean contract emission + Rust conformance consumer

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/CompletionRetry/Contracts.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts.lean` (emit the new domain)
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` (ledger row)
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean` (boundary notes)
- Create: `crates/defra-agent/tests/conformance/completion_retry.rs` (follow the `tests/conformance/` mirror structure — check the existing module registration in that directory's `main`/mod file and register the new file the same way)
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs` (register consumer)

**Interfaces:**
- Consumes: Task 2's `step?`/`Action`; Task 3's public `CompletionRetryState`/`failure_class` (this task compiles against them — that is why the mirror lands first).
- Produces: JSON contract domain `completionRetry` (witness rows + `FailureClass` vocabulary); Rust test `completion_retry_lean_witness_cases_hold`.

- [ ] **Step 1: Write `Contracts.lean`** — generate finite witness rows from `step?` (house pattern: `Proofs/Conformance/ClientShell/Contracts.lean`), plus the `FailureClass` vocabulary (`toDefraDB`-style strings `transport` / `parse_bad_request` / `permanent`). Emit at minimum these named cases, each computed (not hand-written) by running `step?` on a concrete state:
  - `transport_ladder_progresses`: streaming + transport failure, budget 3, used 0, no deadline → `backingOff`, `transportUsed = 1`.
  - `transport_exhausts_after_budget`: used 3 of 3 → `exhausted`.
  - `deadline_behind_clock_fails_fast`: deadline < now → `exhausted` (fail-fast).
  - `deterministic_400_skips_to_repair`: `lastParseError = some e`, failure `e` again → `repairing`.
  - `repair_second_time_illegal`: `repairUsed = true`, repair action → `none`.
  - `retract_with_effects_illegal`: `effects = 1`, retract action → `none`.
  - `close_turn_with_effects_legal`: `effects = 1`, closeTurn → `turnClosed`, `rendered = 1`, turn index advances on the follow-up `continueAfterClose`.
  - `reissue_with_open_effects_illegal`: transport preStreamFail action with `effects = 1` → `none`.
  - `rendered_never_two`: streamOk from `rendered = 0` → `rendered = 1`.
  - `permanent_class_cannot_backoff`: preStreamFail with class `permanent` → `failedPermanent`, never `backingOff`.
- [ ] **Step 2: Emit the domain in `Conformance/Contracts.lean`** (mirror how `ToolExecution` cases are emitted), add the `consumerCoverage` ledger row in `CoverageLedger.lean` naming the consumer registered in Step 4, and add TWO Boundaries notes: (a) "Each completion retry attempt is its own InferenceCall row and admission permit; no backend slot is held during backoff sleeps — admission granularity is per provider call." (b) "Backoff delay values, jitter, and rate-limit hints are product policy: the model constrains budget counts and deadline fit, not the delay schedule."
- [ ] **Step 3: Build and print the contract**

Run: `cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts && lake env lean --run Proofs/Conformance/Contracts.lean | grep -c completionRetry`
Expected: non-zero — the JSON contains the `completionRetry` domain with the ten named cases and the `FailureClass` vocabulary.

- [ ] **Step 4: Write the Rust conformance test** in `tests/conformance/completion_retry.rs`: parse the emitted cases via the existing `lean_vocab_test` helper (see `src/lean_vocab_test.rs` usage in `tests/state_machine_conformance.rs`); for each witness row drive `defra_agent::agent::completion_retry::CompletionRetryState` through the corresponding decision and assert the outcome matches; assert the `FailureClass` vocabulary strings match `failure_class`'s variants for representative `InferenceError` inputs (transport = `TransientFailure`, parse = a `ProviderError` text with the prod 400 body, permanent = `PermanentFailure`). Register the test in `conformance_consumers.rs` with package/file/module/test-fn per the registry shape already in that file.
- [ ] **Step 5: Run the ledger check and the new test**

Run: `cargo test -p defra-agent lean_contract_coverage_ledger_accounts_for_every_emitted_domain && cargo test -p defra-agent completion_retry_lean_witness_cases_hold`
Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/proofs crates/defra-agent/tests
git commit -m "spec(lean): CompletionRetry conformance contract, ledger row, and Rust consumer fence (#631)"
```

---

### Task 5: Document config — InferenceProfile retry fields → LoopConfig

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/inference/inference_profile.graphql`
- Modify: `crates/defra-agent/src/document_config/inference_profile.rs`
- Modify: `crates/defra-agent/src/config.rs` (`AgentBehavior` gains `completion_retry: CompletionRetryProfileFields`)
- Modify: `crates/defra-agent/src/completion_factory.rs` (`loop_config_for_request` resolves policy)
- Modify: `crates/defra-agent/src/agent/loop_stream.rs` (`LoopConfig` gains `retry_policy: CompletionRetryPolicy`, `deadline: Option<DateTime<Utc>>` — used from Task 7 on; default `scheduled_default()` / `None`)
- Test: `crates/defra-agent/src/document_config/tests.rs` additions

**Interfaces:**
- Consumes: Task 3's `CompletionRetryPolicy`.
- Produces: `CompletionRetryProfileFields { retry_max_transport: Option<i64>, retry_backoff_ms: Option<Vec<i64>>, retry_max_resample: Option<i64>, retry_allow_repair: Option<bool>, retry_interactive_max: Option<i64> }` on the profile struct and `AgentBehavior`; `CompletionRetryPolicy::resolve(fields: &CompletionRetryProfileFields, origin: ExecutionOrigin) -> CompletionRetryPolicy` (add to `completion_retry.rs`).

- [ ] **Step 1: SDL** — add to `inference_profile.graphql`:

```graphql
    retry_max_transport: Int
    retry_backoff_ms: [Int]
    retry_max_resample: Int
    retry_allow_repair: Boolean
    retry_interactive_max: Int
```

- [ ] **Step 2: Failing tests** in `document_config/tests.rs`: round-trip upsert/load of a profile with retry fields set; a profile with `retry_backoff_ms` empty resolves to defaults AND the upsert mutation text contains `retry_backoff_ms: null` — **never `[]`** (assert on the rendered mutation string, following however the existing tests assert mutation shapes). Resolution tests: scheduled origin + fields unset → `scheduled_default()`; interactive origin + `retry_interactive_max: 2` → ladder `[2s, 2s]`; scheduled + explicit `retry_backoff_ms: [1000, 5000]` → 2-entry ladder.
- [ ] **Step 3: Implement** — struct fields (all `Option`), load/upsert plumbing in `inference_profile.rs` mirroring the existing optional-field handling (`escape_graphql_string` for anything string-typed), `AgentBehavior.completion_retry` populated wherever the profile currently feeds behavior fields (follow the `stream_liveness_timeout_secs` plumbing path end-to-end — reconcile resolves profile → behavior), `resolve()` in `completion_retry.rs`, and `loop_config_for_request` setting `config.retry_policy = CompletionRetryPolicy::resolve(&behavior.completion_retry, origin)` where `origin` comes from `request.execution_origin` parsed via the existing `lifecycle::ExecutionOrigin` parse path (unknown/absent → `Scheduled`).
- [ ] **Step 4:** `cargo test -p defra-agent document_config` → PASS, then full gate `cargo test -p defra-agent` → PASS.
- [ ] **Step 5: Commit** — `feat(config): InferenceProfile completion-retry fields resolved per execution origin (#631)`

---

### Task 6: Native `LoopStreamItem` (mechanical, no behavior change)

**Files:**
- Modify: `crates/defra-agent/src/agent/loop_stream.rs`
- Modify: `crates/defra-agent/src/agent/daemon/inference.rs` (consume the enum)
- Modify: `crates/defra-agent/src/agent/stream_processor.rs` (`process_item` takes the enum)
- Modify: `crates/defra-agent/src/agent/loop_stream/tests.rs`, `crates/defra-agent/src/agent/stream_processor/tests.rs` (mechanical updates)

**Interfaces:**
- Produces (Tasks 7–10 depend on these exact shapes):

```rust
pub(crate) enum LoopStreamItem<R> {
    Item(MultiTurnStreamItem<R>),
    TurnRetracted { turn: usize, attempt: u32 },
    AttemptFailed {
        turn: usize,
        attempt: u32,
        error: crate::error::InferenceError,
        will_retry: bool,
        backoff: std::time::Duration,
    },
}
```

`run_loop_stream` return type becomes `impl Stream<Item = Result<LoopStreamItem<M::StreamingResponse>, StreamingError>>`. The rendered-request sink signature becomes `Fn(usize /*turn*/, u32 /*attempt*/, CompletionRequest)`; this task passes `0` for attempt everywhere (Task 7 threads real attempts) and updates `rendered_completion_request` call sites in `inference.rs` accordingly (capture key becomes `(turn_index, attempt)` — extend `crate::rendered_request` context the same way its `turn_index` flows today).

- [ ] **Step 1:** Introduce the enum; wrap every existing `yield` in `LoopStreamItem::Item(...)`; update `run_loop_to_text` to unwrap `Item` and ignore the control variants (`TurnRetracted`/`AttemptFailed` → `continue`, remembering `AttemptFailed.error` as last-error context for its failure message); update `StreamProcessor::process_item` signature to `Result<LoopStreamItem<R>, StreamingError>` matching `Item(inner)` to the existing arms and returning `StreamAction::Continue` for the control variants (real handling arrives in Tasks 8/10).
- [ ] **Step 2:** Full gate: `cargo test -p defra-agent`
Expected: PASS — this task is behavior-neutral; any semantic diff is a bug in the task.
- [ ] **Step 3: Commit** — `refactor(loop): native LoopStreamItem envelope over rig stream items (#631)`

---

### Task 7: Pre-stream retry in the owned loop

**Files:**
- Modify: `crates/defra-agent/src/agent/loop_stream.rs`
- Test: `crates/defra-agent/src/agent/loop_stream/tests.rs`

**Interfaces:**
- Consumes: `LoopConfig.retry_policy`/`LoopConfig.deadline` (Task 5), `CompletionRetryState` (Task 3), `LoopStreamItem::AttemptFailed` (Task 6).
- Produces: retry semantics at the `model.stream(request)` seam; `repair_provider_input(history: &[Message], new_messages: &mut Vec<Message>)` helper that re-runs `normalize_tool_call_arguments` (`llm/tool.rs:360`, seam label `"repair"`) over every assistant tool call in `new_messages` and re-sanitizes via `crate::compaction::sanitize_history_for_provider`.

- [ ] **Step 1: Extend `ScriptedModel`** in `loop_stream/tests.rs` with scripted per-call failures:

```rust
enum ScriptedCall {
    Turn(Vec<RawStreamingChoice<()>>),
    FailStream(CompletionError),            // model.stream() itself errors
    TurnWithMidStreamError(Vec<RawStreamingChoice<()>>, CompletionError), // chunks then Err chunk
}
```

`stream()` pops a `ScriptedCall`; `FailStream` returns `Err(err)`; `TurnWithMidStreamError` yields the chunks then one `Err(err)` item. Keep the existing constructors as thin wrappers so current tests compile unchanged.

- [ ] **Step 2: Write failing tests** (drive with `tokio::time::pause()` + `tokio::time::advance`; the loop's sleep must use `tokio::time::sleep` so paused time works):
  - `pre_stream_transport_failure_retries_and_succeeds`: `[FailStream(connect-refused-shaped ProviderError), Turn(text)]` → final text; exactly one `AttemptFailed { will_retry: true }` yielded; `seen_histories` shows two identical requests.
  - `transport_ladder_exhaustion_fails_with_last_error`: 4× `FailStream` with budget 3 → stream ends with `Err`, error text contains `completion retry budget exhausted` and the last classified reason; three `AttemptFailed { will_retry: true }` then one `{ will_retry: false }`.
  - `three_minute_outage_recovers_within_ladder`: 3× `FailStream` then `Turn(text)`, assert total advanced sleep ≥ 5s+30s+120s neighborhood (jitter ±25%) and completion succeeds — the backend-restart acceptance shape at loop level.
  - `parse_400_resamples_once_then_repairs_on_identical_error`: two `FailStream(ProviderError(<exact prod payload>))` with the literal prod body `{"error":{"message":"Expecting value: line 1 column 28 (char 27)","type":"BadRequestError","param":null,"code":400}}` then `Turn(text)` → success; assert the third request's history had tool args re-normalized (seed `new_messages` via a prior scripted tool-call turn whose args are a JSON string, and assert `seen_histories[2]` carries object args) and that no fourth attempt happened.
  - `permanent_400_fails_immediately`: `FailStream` with a non-parse-signature 400 (`duplicate field max_tokens`) → immediate `Err`, no `AttemptFailed { will_retry: true }`.
  - `deadline_fail_fast_pre_sleep`: `LoopConfig.deadline = now + 10s`, first failure wants 30s → immediate `Err`, and **no sleep occurred** (assert elapsed paused-time is 0).
  - `retry_reissues_same_request`: assert `seen_histories[0] == seen_histories[1]` and `seen_tools[0] == seen_tools[1]` for a transport retry (same assembled input, constraint 1).

Run: `cargo test -p defra-agent --lib agent::loop_stream` → FAIL (new tests).

- [ ] **Step 3: Implement.** In `run_loop_stream`, per turn: build the request once, then an attempt loop:

```rust
let mut retry = CompletionRetryState::new(config.retry_policy.clone());
// ... inside `loop {` per turn, after build_request:
let mut attempt: u32 = 0;
let mut request = build_request(&model, current_prompt.clone(), &history, prior, tools.as_slice(), &config).await?;
let mut stream = loop {
    if let Some(on_rendered_request) = config.on_rendered_request.as_ref() {
        on_rendered_request(current_turn - 1, attempt, request.clone()).await.map_err(/* as today */)?;
    }
    match model.stream(request.clone()).await {
        Ok(stream) => break stream,
        Err(completion_error) => {
            let streaming_error = StreamingError::Completion(completion_error);
            let classified = crate::error::classify_completion_error(&streaming_error);
            let error_text = streaming_error.to_string();
            match retry.on_pre_stream_failure(&classified, &error_text, chrono::Utc::now(), config.deadline) {
                PreStreamDirective::RetryAfter { delay, kind } => {
                    yield LoopStreamItem::AttemptFailed { turn: current_turn - 1, attempt, error: classified, will_retry: true, backoff: delay };
                    tracing::warn!(turn = current_turn - 1, attempt, kind = ?kind, delay_ms = delay.as_millis() as u64, error = %error_text, "retrying completion after transient failure");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                PreStreamDirective::Repair => {
                    retry.mark_repair_used();
                    yield LoopStreamItem::AttemptFailed { turn: current_turn - 1, attempt, error: classified, will_retry: true, backoff: std::time::Duration::ZERO };
                    repair_provider_input(&history, &mut new_messages);
                    // rebuild from repaired inputs; prompt/prior are views into new_messages
                    let current_prompt = new_messages.last().cloned().expect("non-empty");
                    let prior = &new_messages[..new_messages.len() - 1];
                    request = build_request(&model, current_prompt, &history, prior, tools.as_slice(), &config).await?;
                    attempt += 1;
                }
                PreStreamDirective::Fail { reason } => {
                    yield LoopStreamItem::AttemptFailed { turn: current_turn - 1, attempt, error: classified, will_retry: false, backoff: std::time::Duration::ZERO };
                    Err(StreamingError::Completion(CompletionError::ProviderError(reason)))?;
                    unreachable!();
                }
            }
        }
    }
};
```

`retry` (the `CompletionRetryState`) lives OUTSIDE the turn loop — one budget per request. `repair_provider_input` maps every `Message::Assistant` tool call in `new_messages` through `normalize_tool_call_arguments("repair", name, &args)` and then replaces `new_messages` with `sanitize_history_for_provider(new_messages)` filtered as at entry. The terminal `Fail` error string format is `completion retry budget exhausted after {attempt+1} attempts: {reason}` when the budget drove the failure; a permanent classification keeps its own reason untouched.

- [ ] **Step 4:** `cargo test -p defra-agent --lib agent::loop_stream` → PASS, then the full gate `cargo test -p defra-agent` → PASS.
- [ ] **Step 5: Commit** — `feat(loop): pre-stream completion retry with ladder, resample, and repair (#631)`

---

### Task 8: Mid-stream retract-and-resample (no effects this turn)

**Files:**
- Modify: `crates/defra-agent/src/agent/loop_stream.rs`
- Modify: `crates/defra-agent/src/agent/stream_processor.rs`
- Test: `crates/defra-agent/src/agent/stream_processor/tests.rs` (durable fence — this is where #589/#590 says these fences live), plus loop-level tests in `loop_stream/tests.rs`

**Interfaces:**
- Consumes: `MidStreamDirective::RetractAndResample` (Task 3), `LoopStreamItem::TurnRetracted` (Task 6).
- Produces: `StreamProcessor` turn-retraction handling; internal `committed_text_len: usize` mark.

- [ ] **Step 1: Failing loop test** `mid_stream_decode_error_without_effects_retracts_and_resamples`: `[TurnWithMidStreamError(text-chunks "Hel", decode-shaped ProviderError), Turn(text "Hello world")]` → items contain the "Hel" deltas, then `TurnRetracted { turn: 0, attempt: 0 }`, then the fresh turn's deltas, then final response "Hello world".
- [ ] **Step 2: Failing processor fence** in `stream_processor/tests.rs` (use the existing hook/writer fixtures in that file): feed the processor `Item(Text "Hel")`, `TurnRetracted`, `Item(Text "Hello world")`, `Item(FinalResponse)`; assert (a) exactly one persisted assistant message whose text is "Hello world" (query the fixture's persisted messages as neighboring tests do), (b) the live-tail buffer content after retraction is empty then rebuilt (assert via the writer fixture), (c) `streamed_text` == "Hello world".
- [ ] **Step 3: Implement.** Loop side — in the mid-stream item loop, item `Err` no longer bubbles straight out: if `pending_results.is_empty()`, consult `retry.on_mid_stream_failure(false, now, deadline)`; on `RetractAndResample { delay }` yield `TurnRetracted { turn, attempt }`, discard `accumulator`/`turn_text`, sleep, `attempt += 1`, re-issue the SAME turn (jump back to the attempt loop from Task 7 — restructure the per-turn body so the attempt loop encloses both `model.stream` and the item-consumption loop); on `Fail` behave as today (bubble the error). Processor side — set `self.committed_text_len = self.streamed_text.len()` at both persist points (the ToolResult branch after `persist_message`, and the FinalResponse branch); handle `TurnRetracted` by `self.assistant_turn = AssistantTurnAccumulator::default(); self.streamed_text.truncate(self.committed_text_len); self.stream_writer.reset_tail(self.doc_id).await?;` — do NOT call `lifecycle.advance()` (retraction is not progress).
- [ ] **Step 4:** `cargo test -p defra-agent --lib agent::` → PASS; full gate → PASS.
- [ ] **Step 5: Commit** — `feat(loop): retract-and-resample for mid-stream failures before tool effects (#631)`

---

### Task 9: Mid-stream close-and-continue (tools already ran)

**Files:**
- Modify: `crates/defra-agent/src/agent/loop_stream.rs`
- Test: `crates/defra-agent/src/agent/loop_stream/tests.rs`

**Interfaces:**
- Consumes: `MidStreamDirective::CloseAndContinue` (Task 3).
- Produces: turn-closing on mid-stream failure with effects; the no-re-execution guarantee (N1/`closeTurn` path).

- [ ] **Step 1: Failing test** `mid_stream_failure_after_tool_ran_closes_turn_and_continues`: script `[TurnWithMidStreamError([tool-call chunk for echo tool], decode error), Turn(text "done")]` with a counting spy tool (wrap `EchoTool` with an `Arc<AtomicUsize>` dispatch counter): assert final text "done"; assert the tool dispatched **exactly once**; assert the second request's history (via `seen_histories[1]`) contains the assistant tool-call turn AND its tool-result message (the closed turn), and the loop yielded the `ToolResult` item before continuing. Also `mid_stream_failure_after_tool_budget_exhausted_fails`: same first call but zero remaining ladder → terminal `Err`, tool still dispatched exactly once.
- [ ] **Step 2: Implement.** In the mid-stream `Err` arm, when `pending_results` is non-empty: consult `on_mid_stream_failure(true, ...)`; on `CloseAndContinue { delay }` run the existing end-of-turn threading code (thread `accumulator.take_message()` into `new_messages`, then thread + `yield` each pending `ToolResult` exactly as the successful-turn path does — extract that block into a local helper `close_turn(...)` used by both paths), then yield `AttemptFailed { will_retry: true }`, sleep `delay`, `attempt += 1`, and `continue` the OUTER turn loop (the tool-result message is now the prompt; `current_turn` advances naturally). On `Fail` → bubble the error as today (daemon's partial-turn persistence handles the remains).
- [ ] **Step 3:** `cargo test -p defra-agent --lib agent::loop_stream` → PASS; full gate → PASS.
- [ ] **Step 4: Commit** — `feat(loop): close-and-continue for mid-stream failures after tool effects (#631)`

---

### Task 10: Retry observability — run_timeline rollup over per-attempt InferenceCall rows

**No schema change and no admission change.** The admission client acquires a permit per `model.stream()` call (`admission/client.rs:111`), so every retry attempt already persists its own `InferenceCall` row: a pre-stream failure is terminalized `failed` with `failure_reason` (`client.rs:129`) *before* the loop's backoff sleep, and the re-issue acquires a fresh permit/row. No slot is held during backoff. The observability work is purely a projection.

**Files:**
- Modify: `crates/defra-agent/src/run_timeline.rs` (group a request's `InferenceCall` rows into a retry summary, following how the timeline already loads/projects InferenceCall rows)
- Test: the run-timeline tests where InferenceCall projection is already covered, plus an admission-level assertion in `crates/defra-agent/src/admission/tests.rs`

**Interfaces:**
- Consumes: existing per-attempt `InferenceCall` rows (`attempt`, `call_seq`, `call_state`, `failure_reason`, timestamps).
- Produces: a `RetrySummary { retry_count: u32, last_transient_error: Option<String>, recovered: bool }` on the timeline's request projection. Rollup rule: order the request's inference-kind rows by `queued_at`/`call_seq`; `retry_count` = number of `failed` rows that are followed by a later row (a failed attempt that was retried); `last_transient_error` = `failure_reason` of the most recent such row; `recovered` = request terminal state is `completed` AND `retry_count > 0`. A `failed` row with no successor keeps today's meaning (the failure that killed the run).

- [ ] **Step 1: Failing tests:** (a) timeline test — seed a request with three InferenceCall rows (`failed` w/ reason, `failed` w/ reason, `completed`) and a completed request; assert `RetrySummary { retry_count: 2, last_transient_error: Some(<second reason>), recovered: true }`; (b) timeline test — rows (`failed`, `failed`) with a failed request → `retry_count: 1`, `recovered: false` (the last row is the killing failure, not a retry); (c) admission test — drive two consecutive `scope_call` inference failures then a success within one scope and assert three persisted rows exist for the request with the expected `call_state` sequence (this pins the per-attempt row granularity the rollup depends on).
- [ ] **Step 2: Implement** the rollup in `run_timeline.rs` and surface it wherever the timeline exposes request-level fields (follow the existing projection struct shape; CLI `trace timeline` output picks it up automatically through that struct).
- [ ] **Step 3:** targeted tests PASS → full gate PASS.
- [ ] **Step 4: Commit** — `feat(observability): retry rollup over per-attempt InferenceCall rows in run timeline (#631)`

---

### Task 11: Remove the daemon outer retry (closes #638)

**Files:**
- Modify: `crates/defra-agent/src/agent/daemon/inference.rs`
- Modify: `crates/defra-agent/src/agent/daemon.rs` (drop `BehaviorDaemon.retry_policy` field + constructor arg — **only** the daemon's copy). **Scope guard: `RetryPolicy` itself stays.** `agent/reconcile/slot.rs:239` and `:251` use `retry_policy.delay_for_attempt` for behavior-slot *restart* backoff — unrelated resilience that must not change. Keep `RetryPolicy` in `retry.rs`, keep it threaded through `agent.rs` options → `reconcile.rs` → `slot.rs`; the only removal is the daemon's inference-retry consumption.
- Modify: `crates/defra-agent/src/agent/daemon/inference.rs` LoopConfig population: set `loop_config.deadline = request_deadline` and confirm `retry_policy` resolution from Task 5 flows here
- Test: existing daemon tests updated; the deleted `retry_backoff_wait_is_cut_off_by_request_deadline` test's obligation is superseded by Task 3's deadline fail-fast unit test — note that in the commit message

**Interfaces:**
- Consumes: everything above; after this task the loop is the only retry seam.

- [ ] **Step 1:** Delete the `for attempt in 0..max_attempts` loop, `InferenceAttemptOutcome`, the `can_retry` gate, and the `Inference failed after N attempts` terminal block (the loop's `completion retry budget exhausted` error now arrives as an ordinary stream error through the existing terminal path at `inference.rs:496`). Keep: partial-turn persistence, `backfill_completed_tool_results`, liveness timeout, interrupt/shutdown select, the `inference.attempt` span (hardcode `attempt = 1`, drop `max_attempts`, or rename the span fields to reflect loop-owned retries — keep span name stable for dashboards, set `retry_attempt = false`).
- [ ] **Step 2:** Full gate: `cargo test -p defra-agent`
Expected: PASS. Pay attention to daemon tests that scripted multi-attempt behavior — they should now assert single-drive semantics.
- [ ] **Step 3: Commit** — `fix(daemon): remove whole-run inference retry; loop owns completion retry (#631, closes #638)`

---

### Task 12: Acceptance tape, docs, and gate

**Files:**
- Create: `crates/defra-agent/tests/completion_retry_tape.rs` (integration; register in the test-binary layout the same way sibling `tests/*.rs` files are — check `Cargo.toml` `[[test]]` sections given the 9-binary consolidation)
- Modify: `crates/defra-agent/proofs/README.md` (only if anything drifted since Task 2)
- Modify: `docs/superpowers/specs/2026-07-07-completion-retry-631-design.md` (status → implemented)

**Interfaces:** consumes everything; produces the 48h-tape regression suite at the daemon level (mock streaming backend from `tests/support/streaming_backend.rs` / `mock_endpoint.rs`).

- [ ] **Step 1: Write the tape tests** (daemon-level where the fixtures allow, loop-level otherwise — the loop-level variants from Tasks 7–9 already cover shapes (b)(c)(d); this file adds the daemon-visible ends):
  - `backend_restart_cluster_recovers`: 3 concurrent requests against a mock endpoint that refuses connections for ~3 simulated minutes then serves; all 3 complete; each request's `InferenceCall` rows include ≥1 `failed` attempt row followed by a `completed` row, and the run-timeline `RetrySummary` reports `retry_count > 0` with `recovered: true`; zero failed requests. (If the mock endpoint can't run under paused time because it uses real sockets, drive 3 concurrent `BehaviorDaemon` mock-stream fixtures instead — the point is N concurrent recoveries with persisted per-attempt rows.)
  - `deadline_tight_fails_cleanly`: request with a claimed deadline 10s out; backend down; terminal `failed` with today's error semantics; elapsed wall/simulated time well under the ladder total.
  - `interactive_budget_is_quick`: interactive-origin request, backend down; exactly 1 retry (~2s) then terminal failure.
  - `deterministic_400_tape`: daemon-level replay of the exact prod 400 body twice → repair → completes-or-fails cleanly with run-timeline `retry_count == 2` (two failed attempt rows, then the repair attempt's row) and no further attempts.
- [ ] **Step 2: Full gate + Lean:**

Run: `cd crates/defra-agent/proofs && lake build && cd ../../.. && cargo test -p defra-agent`
Expected: both clean. Any flake here is a defect — capture and fix, never shrug (house rule).

- [ ] **Step 3:** Update spec status line; check `proofs/README.md` matches what landed.
- [ ] **Step 4: Commit** — `test(retry): 48h failure-tape acceptance suite for completion retry (#631)`
- [ ] **Step 5:** Push branch, open PR titled `feat(retry): per-completion retry in the owned loop (#631)` with body: triage summary (deterministic 400 = pre-#601), the three mechanisms, Lean model + N1–N5, `Closes #631`, `Closes #638`. Then run the final branch review per house calibration (spec-compliance + one full-branch review; skip per-task quality reviewers).

---

## Self-Review Notes (already applied)

- Spec coverage: triage (spec §Triage → PR body), mechanisms 1/2/3 (Tasks 7/8+9/11), native enum (Task 6), config (Task 5), observability (Task 10), Lean N1–N5 + contracts (Tasks 1–2, 4), acceptance (a)–(e) (Tasks 7 (a-loop,b,c,e), 8 (d), 12 (a,e daemon-level + interactive)). Rendered-request capture keying: Task 6 changes the sink signature; Task 7 threads real attempts.
- Review round 1 (plan-level) applied: per-attempt InferenceCall rows instead of a mutable per-request row (admission acquires a permit per `model.stream()` — no admission refactor, no slot held during backoff); Task 3/4 order swapped so the conformance consumer compiles against an existing `pub` module; Lean model gained `turnIndex`, class-consuming failure transitions, and the `ReissueInv` reachability invariant (fixes the N2 counterexample via `continueAfterClose` and grounds N1 in actual re-issues); `RetryPolicy` retained for behavior-slot restart backoff (`slot.rs:239`), Task 11 removes only the daemon's inference-retry consumption; `FailureClass` is now consumed by `step?` and pinned by a vocabulary contract, with delay/jitter values documented as a product-policy boundary.
- Type consistency: `CompletionRetryPolicy`/`CompletionRetryState`/`FailureClass`/`PreStreamDirective`/`MidStreamDirective`/`LoopStreamItem` names match across Tasks 3–11; conformance test path `tests/conformance/completion_retry.rs` matches ledger registration in Task 4.
- Known judgment calls left to the implementer, deliberately: exact Lean binder spellings (statements must keep their stated content), the `tests/conformance/` module registration mechanics, and which existing daemon tests assert multi-attempt semantics (Task 11 Step 2 flags them).
