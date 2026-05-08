# ToolCall Lifecycle Lean Spec — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the Lean 4 spec defined in `docs/superpowers/specs/2026-05-08-toolcall-lifecycle-spec-design.md` — a daemon-visible `ToolCallContext` lifecycle state machine with five single-machine theorems and four composition theorems that close issue #149's request-liveness gap at the spec layer.

**Architecture:** Restructure `Proofs/ToolExecution.lean` as a folder (mirroring `Proofs/Request/` and `Proofs/InferenceCall/`); move existing policy contents into `ToolExecution/Policy.lean`; add new `State.lean`, `Transition.lean`, `Properties.lean`, `Executable.lean`; extend `Composed.lean` with a `tool_step` transition variant and the four 149-closing theorems.

**Tech Stack:** Lean 4 (toolchain pinned via `lean-toolchain`), Lake build system, Mathlib4 v4.18.0. All build/verify via `lake build` from `crates/defra-agent/proofs/`.

---

## What's NOT in this plan (deferred)

- **Conformance JSON emission** for the new `ToolCallState` vocabulary in `Proofs/Conformance/Contracts/Machines.lean`. Adding a new entry to `vocabularies` or `stateMachines` would change the JSON snapshot consumed by Rust conformance tests; that crosses the "Lean-only, no Rust work" boundary set for B1. Tracked as a follow-up alongside B2 (runtime work) when the Rust side gains a consumer for these states.
- **Rust runtime / test changes.** All B2..B6 work (runtime subprocess supervisor, native-tool migration, sandbox tiers, schema migration, observability). Tracked in separate specs.

---

## Conventions

- **Build/verify command:** `cd crates/defra-agent/proofs && lake build` from repo root, or `lake build` from inside the proofs directory. Treat any non-zero exit or any `sorry`/`unsolved goals`/`error:` line in output as failure.
- **TDD in Lean:** the "failing test" is a `theorem` declaration with `sorry` (Lean reports `sorry` as a warning but continues compiling). The "passing test" is the same `theorem` with a complete proof and no `sorry`. Verify with `lake build` after each.
- **Commit cadence:** one commit per task. Commit messages are imperative, scoped to the change, and end with the Co-Authored-By trailer used elsewhere in the repo. See Task 1 for the exact format.
- **Working directory:** all paths in this plan are relative to the repo root (`/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-issue-149-native-glob-deadline`). The git working directory should remain at repo root throughout.
- **Branch:** `bug/issue-149-native-glob-deadline` (already current).

---

## Task 1: Restructure `Proofs/ToolExecution.lean` as a folder (move existing contents)

The existing `Proofs/ToolExecution.lean` becomes `Proofs/ToolExecution/Policy.lean` with content unchanged. The top-level file becomes a barrel re-export, mirroring `Proofs/Request.lean` and `Proofs/InferenceCall.lean`.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolExecution/Policy.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution.lean`

- [ ] **Step 1: Create the folder and move contents into Policy.lean**

```bash
mkdir -p crates/defra-agent/proofs/Proofs/ToolExecution
git mv crates/defra-agent/proofs/Proofs/ToolExecution.lean crates/defra-agent/proofs/Proofs/ToolExecution/Policy.lean
```

- [ ] **Step 2: Recreate `Proofs/ToolExecution.lean` as a re-export stub**

Write the file `crates/defra-agent/proofs/Proofs/ToolExecution.lean` with this exact content:

```lean
import Proofs.ToolExecution.Policy

/-!
# Tool Execution

Barrel import for tool-execution policy (preflight, retry disposition).
Lifecycle modules are added in subsequent tasks.
-/
```

- [ ] **Step 3: Build and confirm nothing regressed**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build (exit 0, no errors, no `sorry`). The pre-existing theorems in `Policy.lean` (`unreachable_blocks_dispatch`, `mcp_call_transport_retry_requires_idempotency`, etc.) must still typecheck. Any importer of `Proofs.ToolExecution` (e.g. `Proofs/Conformance/Contracts/Machines.lean`) should still work via the re-export.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution.lean crates/defra-agent/proofs/Proofs/ToolExecution/Policy.lean
git commit -m "$(cat <<'EOF'
Restructure ToolExecution as a folder with Policy barrel re-export

Mirrors the Proofs/Request/ and Proofs/InferenceCall/ layout in preparation
for adding ToolExecution/{State,Transition,Properties,Executable}.lean.
No behavioral change; existing policy content unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `ToolCallState` enum and persisted vocabulary

Create the new state vocabulary file with the six states from the spec. Mirrors `Proofs/Request/State.lean:14-67` (state inductive + `toDefraDB` + `fromDefraDB?` + `HasTerminal` instance).

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolExecution/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution.lean` (add import)

- [ ] **Step 1: Write the new file with state enum, persisted vocabulary, and `HasTerminal` instance**

Write `crates/defra-agent/proofs/Proofs/ToolExecution/State.lean`:

```lean
import Proofs.Basic
import Proofs.Persistence
import Proofs.ToolExecution.Policy

/-!
# Tool Call State

Daemon-visible lifecycle vocabulary for an individual tool dispatch. The
lifecycle picks up after `Policy.preflight = .dispatch`; a `.block` decision
skips the lifecycle entirely and persists `failed` at the request level via
the existing `tool_failure_class` field. That gating is enforced in Rust at
the dispatch site and is documented here as a structural assumption rather
than a Lean theorem.
-/

namespace ToolExecution

/-- The 6 persisted states of the tool-call lifecycle. -/
inductive ToolCallState where
  | pending
  | running
  | completed
  | failed
  | timedOut
  | cancelled
  deriving DecidableEq, Repr

namespace ToolCallState

/-- String vocabulary persisted in `AgentToolCall.lifecycle_state`. -/
def toDefraDB : ToolCallState → String
  | .pending => "pending"
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .timedOut => "timedOut"
  | .cancelled => "cancelled"

/-- Parse the persisted vocabulary. -/
def fromDefraDB? : String → Option ToolCallState
  | "pending" => some .pending
  | "running" => some .running
  | "completed" => some .completed
  | "failed" => some .failed
  | "timedOut" => some .timedOut
  | "cancelled" => some .cancelled
  | _ => none

theorem fromDefraDB_toDefraDB (s : ToolCallState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

/-- Exhaustive constructor list for Rust conformance vocabulary generation. -/
def all : List ToolCallState :=
  [ .pending, .running, .completed, .failed, .timedOut, .cancelled ]

theorem all_complete (s : ToolCallState) : s ∈ all := by
  cases s <;> simp [all]

instance : HasTerminal ToolCallState where
  isTerminal s :=
    s = .completed ∨ s = .failed ∨ s = .timedOut ∨ s = .cancelled
  isTerminal_dec s :=
    match s with
    | .completed => isTrue (Or.inl rfl)
    | .failed => isTrue (Or.inr (Or.inl rfl))
    | .timedOut => isTrue (Or.inr (Or.inr (Or.inl rfl)))
    | .cancelled => isTrue (Or.inr (Or.inr (Or.inr rfl)))
    | .pending => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))
    | .running => isFalse (by intro h; rcases h with h | h | h | h <;> exact absurd h (by decide))

end ToolCallState

end ToolExecution
```

- [ ] **Step 2: Add import to the barrel file**

Edit `crates/defra-agent/proofs/Proofs/ToolExecution.lean` so it reads:

```lean
import Proofs.ToolExecution.Policy
import Proofs.ToolExecution.State

/-!
# Tool Execution

Barrel import for tool-execution policy (preflight, retry disposition) and
lifecycle (state vocabulary, transitions, properties, executable semantics).
-/
```

- [ ] **Step 3: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. All six new theorems (`fromDefraDB_toDefraDB`, `all_complete`, the `HasTerminal` instance) should typecheck.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution.lean crates/defra-agent/proofs/Proofs/ToolExecution/State.lean
git commit -m "$(cat <<'EOF'
Add ToolCallState enum and persisted vocabulary

Six states: pending, running, completed, failed, timedOut, cancelled.
Defines toDefraDB / fromDefraDB? round-trip and HasTerminal instance over
the four terminal constructors.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `ToolCallContext` structure and predicates

Extend `State.lean` with the per-call context, mirroring `RequestContext` (`Proofs/Request/State.lean:108-126`).

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/State.lean`

- [ ] **Step 1: Append the context structure to State.lean**

After the closing `end ToolCallState` and `end ToolExecution` lines, but inside the `ToolExecution` namespace, add a new namespace block. The full file should now end with this addition (re-open `namespace ToolExecution` after the existing `end ToolExecution`):

Add after the existing `end ToolExecution`:

```lean

namespace ToolExecution

/-- Identifier for an individual tool-call row. -/
abbrev ToolCallId := Nat

/-- Mutable per-tool-call context that transitions carry along. -/
structure ToolCallContext where
  callId       : ToolCallId
  requestId    : RequestId
  state        : ToolCallState
  operation    : ToolOperation
  deadline     : Time
  startedAt    : Option Time := none
  currentTime  : Time
  failureClass : Option FailureClass := none
  persistence  : PersistenceState
  deriving Repr

namespace ToolCallContext

/-- Whether the tool's deadline has been exceeded. -/
def deadlineExceeded (c : ToolCallContext) : Prop :=
  c.currentTime > c.deadline

instance (c : ToolCallContext) : Decidable c.deadlineExceeded :=
  Nat.decLt c.deadline c.currentTime

/-- A call is cancellable iff it is in a non-terminal pre-state. -/
def cancellable (c : ToolCallContext) : Prop :=
  c.state = .pending ∨ c.state = .running

instance (c : ToolCallContext) : Decidable c.cancellable := by
  unfold cancellable; infer_instance

/-- Linkage to a parent request. -/
def linkedTo (c : ToolCallContext) (rid : RequestId) : Prop :=
  c.requestId = rid

instance (c : ToolCallContext) (rid : RequestId) : Decidable (c.linkedTo rid) := by
  unfold linkedTo; infer_instance

end ToolCallContext

end ToolExecution
```

- [ ] **Step 2: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. `ToolCallContext` typechecks; `deadlineExceeded`, `cancellable`, `linkedTo` predicates have decidable instances.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution/State.lean
git commit -m "$(cat <<'EOF'
Add ToolCallContext structure with deadline/cancellable/linkedTo predicates

Mirrors RequestContext: state + failure metadata + currentTime + deadline +
persistence. Composition guards (callId/deadline tied to parent request) are
enforced at the Composed layer in a later task, not here.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `Transition` relational definition with all 9 constructors and `Trace`

Mirrors `Proofs/Request/Transition.lean`. Defines the relational transition and the reflexive-transitive-closure trace.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolExecution/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution.lean` (add import)

- [ ] **Step 1: Write Transition.lean**

Write `crates/defra-agent/proofs/Proofs/ToolExecution/Transition.lean`:

```lean
import Proofs.ToolExecution.State

/-!
# Tool Call Transitions

Relational transition system for `ToolCallContext`. Seven state-changing
constructors plus two non-state constructors (`timeAdvance`, `persistenceStep`).
-/

namespace ToolExecution
namespace ToolCallContext

inductive Transition : ToolCallContext → ToolCallContext → Prop where

  | dispatch {pre post : ToolCallContext}
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .running
                                 , startedAt := some pre.currentTime })
      : Transition pre post

  | spawnFailed {pre post : ToolCallContext} (failure : FailureClass)
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .failed
                                 , failureClass := some failure })
      : Transition pre post

  | complete {pre post : ToolCallContext}
      (h_state   : pre.state = .running)
      (h_persist : pre.persistence = .committed)
      (h_post    : post = { pre with state := .completed })
      : Transition pre post

  | fail {pre post : ToolCallContext} (failure : FailureClass)
      (h_state : pre.state = .running)
      (h_post  : post = { pre with state := .failed
                                 , failureClass := some failure })
      : Transition pre post

  | timeout {pre post : ToolCallContext}
      (h_state    : pre.state = .running)
      (h_deadline : pre.deadlineExceeded)
      (h_post     : post = { pre with state := .timedOut })
      : Transition pre post

  | cancelBeforeDispatch {pre post : ToolCallContext}
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .cancelled })
      : Transition pre post

  | cancelDuringRun {pre post : ToolCallContext}
      (h_state : pre.state = .running)
      (h_post  : post = { pre with state := .cancelled })
      : Transition pre post

  | timeAdvance {pre post : ToolCallContext} (t : Time)
      (h_le   : pre.currentTime ≤ t)
      (h_post : post = { pre with currentTime := t })
      : Transition pre post

  | persistenceStep {pre post : ToolCallContext}
      (policy : PersistenceState.FailurePolicy)
      (next : PersistenceState)
      (h_p_step : PersistenceState.Transition policy pre.persistence next)
      (h_post   : post = { pre with persistence := next })
      : Transition pre post

/-- A trace is a sequence of valid tool-call transitions. -/
inductive Trace : ToolCallContext → ToolCallContext → Prop where
  | refl {c : ToolCallContext} : Trace c c
  | step {c₁ c₂ c₃ : ToolCallContext} :
      Transition c₁ c₂ → Trace c₂ c₃ → Trace c₁ c₃

end ToolCallContext
end ToolExecution
```

- [ ] **Step 2: Add import to the barrel file**

Edit `crates/defra-agent/proofs/Proofs/ToolExecution.lean`:

```lean
import Proofs.ToolExecution.Policy
import Proofs.ToolExecution.State
import Proofs.ToolExecution.Transition

/-!
# Tool Execution

Barrel import for tool-execution policy (preflight, retry disposition) and
lifecycle (state vocabulary, transitions, properties, executable semantics).
-/
```

- [ ] **Step 3: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. The 9 `Transition` constructors and the 2 `Trace` constructors all typecheck. If a `Persistence.Transition` import error appears, double-check that `Proofs.Persistence` is transitively reachable through `Proofs.ToolExecution.State`'s import of `Proofs.Persistence`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution.lean crates/defra-agent/proofs/Proofs/ToolExecution/Transition.lean
git commit -m "$(cat <<'EOF'
Add ToolCallContext.Transition relational system and Trace closure

Seven state-changing constructors (dispatch, spawnFailed, complete, fail,
timeout, cancelBeforeDispatch, cancelDuringRun) plus two non-state
constructors (timeAdvance, persistenceStep). Trace is the reflexive-
transitive closure used in liveness theorems.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Prove T1 (terminal_irreversible)

Single-machine theorem. Mirrors Request S1: once in any of `completed | failed | timedOut | cancelled`, no transition leaves the state. Proof is exhaustive case analysis over the 9 `Transition` constructors; every state-changing constructor has a `pre.state = .pending` or `pre.state = .running` guard, contradicting `isTerminal pre.state`.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution.lean` (add import)

- [ ] **Step 1: Write the theorem statement with `sorry`**

Write `crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean`:

```lean
import Proofs.ToolExecution.Transition

/-!
# Tool Call Single-Machine Properties

T1..T5 — daemon-visible invariants over `ToolCallContext.Transition`.
Composition theorems C1, C1', C2, C3 live in `Proofs/Composed.lean`.
-/

namespace ToolExecution
namespace ToolCallContext

/-- T1: Terminal irreversibility. Once in completed/failed/timedOut/cancelled,
    no transition leaves the state or mutates the failureClass. -/
theorem terminal_irreversible
    {pre post : ToolCallContext}
    (h_terminal : isTerminal pre.state)
    (h_step : Transition pre post) :
    pre.state = post.state ∧ pre.failureClass = post.failureClass := by
  sorry

end ToolCallContext
end ToolExecution
```

Add the import to `crates/defra-agent/proofs/Proofs/ToolExecution.lean`:

```lean
import Proofs.ToolExecution.Policy
import Proofs.ToolExecution.State
import Proofs.ToolExecution.Transition
import Proofs.ToolExecution.Properties

/-!
# Tool Execution
…
-/
```

- [ ] **Step 2: Build and verify it compiles with a `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: build succeeds with `declaration uses 'sorry'` warning on `terminal_irreversible`.

- [ ] **Step 3: Replace `sorry` with the proof**

In `Properties.lean`, replace the `sorry` line with this proof:

```lean
  cases h_step with
  | dispatch h_state _              => simp_all [isTerminal]
  | spawnFailed _ h_state _         => simp_all [isTerminal]
  | complete h_state _ _            => simp_all [isTerminal]
  | fail _ h_state _                => simp_all [isTerminal]
  | timeout h_state _ _             => simp_all [isTerminal]
  | cancelBeforeDispatch h_state _  => simp_all [isTerminal]
  | cancelDuringRun h_state _       => simp_all [isTerminal]
  | timeAdvance _ _ h_post          => simp_all
  | persistenceStep _ _ _ h_post    => simp_all
```

The two non-state constructors (`timeAdvance`, `persistenceStep`) leave state and failureClass untouched, so `simp_all` on `h_post` closes those goals. The seven state-changing constructors all guard on `pre.state = .pending` or `pre.state = .running`; combined with `h_terminal : isTerminal pre.state`, `simp_all [isTerminal]` derives a contradiction.

- [ ] **Step 4: Build and verify the proof passes**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build, no `sorry` warnings on `terminal_irreversible`. If a specific case doesn't close cleanly, expand `simp_all` to `simp_all [isTerminal, ToolCallState.HasTerminal]` or split into explicit `obtain ⟨_, _, _, _⟩ := h_terminal` over the four terminal disjunctions.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution.lean crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean
git commit -m "$(cat <<'EOF'
Prove T1 (terminal_irreversible) for ToolCallContext

Mirror of Request S1. Establishes that the four terminal states
(completed, failed, timedOut, cancelled) have no outgoing transitions
that change the state or failureClass. Underpins the composition
theorems by ruling out reverse transitions in trace constructions.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Prove T4 (cancellable_iff_non_terminal)

Easiest after T1; pure case analysis over the six `ToolCallState` constructors.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

In `Properties.lean`, before `end ToolCallContext`, add:

```lean
/-- T4: A call is cancellable iff its state is non-terminal. Operational
    meaning: any in-flight call accepts a cancel transition. -/
theorem cancellable_iff_non_terminal (c : ToolCallContext) :
    c.cancellable ↔ ¬ isTerminal c.state := by
  sorry
```

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: `sorry` warning on `cancellable_iff_non_terminal`.

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  unfold cancellable
  cases c.state <;> simp [isTerminal]
```

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean
git commit -m "$(cat <<'EOF'
Prove T4 (cancellable_iff_non_terminal) for ToolCallContext

Establishes the equivalence between the cancellable predicate and the
negation of isTerminal. Used by composition theorem C2 to discharge the
cancellability hypothesis from the request-state side.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Prove T2 (timedOut_requires_deadline_exceeded)

The headline single-machine theorem for issue #149. Establishes that the only way to reach `.timedOut` is via the `timeout` constructor, which guards on `pre.deadlineExceeded`.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

In `Properties.lean`, before `end ToolCallContext`, add:

```lean
/-- T2: TimedOut is reachable only when deadline is exceeded.
    The property whose absence in the runtime caused issue #149. -/
theorem timedOut_requires_deadline_exceeded
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_post : post.state = .timedOut) :
    pre.deadlineExceeded := by
  sorry
```

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  cases h_step with
  | dispatch _ h_post'              => simp_all
  | spawnFailed _ _ h_post'         => simp_all
  | complete _ _ h_post'            => simp_all
  | fail _ _ h_post'                => simp_all
  | timeout _ h_deadline _          => exact h_deadline
  | cancelBeforeDispatch _ h_post'  => simp_all
  | cancelDuringRun _ h_post'       => simp_all
  | timeAdvance _ _ h_post'         => simp_all
  | persistenceStep _ _ _ h_post'   => simp_all
```

Every constructor except `timeout` produces a `post.state ≠ .timedOut`, contradicting `h_post`. The `timeout` case discharges directly with the `h_deadline` precondition.

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean
git commit -m "$(cat <<'EOF'
Prove T2 (timedOut_requires_deadline_exceeded) — closes issue #149 at the spec layer

Establishes that no transition reaches state=.timedOut without
pre.deadlineExceeded as a precondition. This is the formal statement of the
liveness property whose absence in the runtime caused issue #149: a tool
call cannot be marked timed-out unless time has passed beyond the deadline.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Prove T3 (completed_implies_committed)

Mirror of Request S6. The `complete` constructor guards on `pre.persistence = .committed`, and the post-state preserves persistence.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

```lean
/-- T3: Persistence before completion. Mirror of Request S6. -/
theorem completed_implies_committed
    {pre post : ToolCallContext}
    (h_step : Transition pre post)
    (h_post : post.state = .completed) :
    post.persistence = .committed := by
  sorry
```

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  cases h_step with
  | dispatch _ h_post'              => simp_all
  | spawnFailed _ _ h_post'         => simp_all
  | complete _ h_persist h_post'    => simp_all
  | fail _ _ h_post'                => simp_all
  | timeout _ _ h_post'             => simp_all
  | cancelBeforeDispatch _ h_post'  => simp_all
  | cancelDuringRun _ h_post'       => simp_all
  | timeAdvance _ _ h_post'         => simp_all
  | persistenceStep _ _ _ h_post'   => simp_all
```

The `complete` case discharges via `h_persist` (which says `pre.persistence = .committed`) and `h_post'` (which says `post = { pre with state := .completed }`, so `post.persistence = pre.persistence`). All other cases produce `post.state ≠ .completed`, contradicting `h_post`.

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean
git commit -m "$(cat <<'EOF'
Prove T3 (completed_implies_committed) for ToolCallContext

Mirror of Request S6 (persistence before completion). The complete
constructor guards on pre.persistence = .committed, and persistence is
preserved across that transition.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Prove T5 (live_call_reaches_terminal)

The liveness theorem. Most complex of the single-machine proofs because it constructs a `Trace` of length 1 or 2 with a `timeAdvance` step. Two cases by `pre.state`:

- `.pending`: one step using `cancelBeforeDispatch` (no time needed) → `.cancelled` (terminal).
- `.running`: two steps. First `timeAdvance` to a time `t = c.deadline + 1` (so `deadlineExceeded` holds in the next state), then `timeout` → `.timedOut` (terminal).

(The other non-terminal states do not exist — `pending | running` exhaust the non-terminal cases.)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

```lean
/-- T5: Bounded reachability to terminal (liveness). Any non-terminal call
    has a 1- or 2-step trace to a terminal state, given a sufficient time
    advance. Daemon-side liveness underlying issue #149's fix. -/
theorem live_call_reaches_terminal
    (c : ToolCallContext)
    (h_live : ¬ isTerminal c.state) :
    ∃ post, Trace c post ∧ isTerminal post.state := by
  sorry
```

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  -- Two non-terminal states: .pending and .running.
  -- .pending  → cancelBeforeDispatch → .cancelled (terminal)
  -- .running  → timeAdvance(deadline+1); timeout → .timedOut (terminal)
  match h_state : c.state with
  | .pending =>
      let post : ToolCallContext := { c with state := .cancelled }
      have h_trans : Transition c post :=
        Transition.cancelBeforeDispatch (h_state := h_state) (h_post := rfl)
      refine ⟨post, Trace.step h_trans Trace.refl, ?_⟩
      simp [isTerminal]
  | .running =>
      let mid : ToolCallContext := { c with currentTime := c.deadline + 1 }
      have h_le : c.currentTime ≤ c.deadline + 1 := by omega
      have h_step1 : Transition c mid :=
        Transition.timeAdvance (t := c.deadline + 1) (h_le := h_le) (h_post := rfl)
      let post : ToolCallContext := { mid with state := .timedOut }
      have h_mid_running : mid.state = .running := by
        change c.state = .running; exact h_state
      have h_mid_deadline : mid.deadlineExceeded := by
        unfold deadlineExceeded
        change c.deadline + 1 > c.deadline
        omega
      have h_step2 : Transition mid post :=
        Transition.timeout (h_state := h_mid_running) (h_deadline := h_mid_deadline) (h_post := rfl)
      refine ⟨post, Trace.step h_step1 (Trace.step h_step2 Trace.refl), ?_⟩
      simp [isTerminal]
  | .completed => exact absurd (Or.inl h_state) h_live
  | .failed    => exact absurd (Or.inr (Or.inl h_state)) h_live
  | .timedOut  => exact absurd (Or.inr (Or.inr (Or.inl h_state))) h_live
  | .cancelled => exact absurd (Or.inr (Or.inr (Or.inr h_state))) h_live
```

The `omega` tactic handles arithmetic on `Nat` (since `Time := Nat`). Lean may complain that `mid.deadlineExceeded` doesn't reduce; if so, replace `change c.deadline + 1 > c.deadline` with `show c.deadline + 1 > c.deadline` or with the explicit definitional unfold `simp [deadlineExceeded, mid]`.

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. If the `match h_state : c.state` syntax errors out, try `cases h_state : c.state` — Lean 4 sometimes prefers `cases`/`rcases` for term-level pattern matching with hypothesis tracking. The structure of the proof is unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean
git commit -m "$(cat <<'EOF'
Prove T5 (live_call_reaches_terminal) for ToolCallContext

Liveness theorem: any non-terminal call has a 1- or 2-step trace to a
terminal state. Pending → cancelBeforeDispatch → cancelled; Running →
timeAdvance(deadline+1) → timeout → timedOut. This is the daemon-side
liveness guarantee underlying the fix for issue #149.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Add Executable.lean (Action enum, step? function, refinement theorem)

Mirrors `Proofs/Request/Executable.lean`. Defines an `Action` enum that names each state-changing transition (the two non-state transitions are not exposed as conformance actions — they're internal trace primitives), a `step?` function that pattern-matches on `Action` and returns `Option ToolCallContext`, and a refinement theorem proving each successful `step?` invocation corresponds to a `Transition` derivation.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/ToolExecution/Executable.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution.lean` (add import)

- [ ] **Step 1: Write Executable.lean**

Write `crates/defra-agent/proofs/Proofs/ToolExecution/Executable.lean`:

```lean
import Proofs.ToolExecution.Transition

/-!
# Executable Tool-Call Semantics

Executable actions, step function, and refinement theorem connecting
`step?` to the relational `Transition`. Mirrors `Proofs/Request/Executable.lean`.
-/

namespace ToolExecution
namespace ToolCallContext

/-- Executable tool-call actions mirroring the state-changing constructors of
    `Transition`. The two non-state constructors (`timeAdvance`,
    `persistenceStep`) are not exposed here; they are internal to trace
    construction in liveness proofs. -/
inductive Action where
  | dispatch
  | spawnFailed (failure : FailureClass)
  | complete
  | fail (failure : FailureClass)
  | timeout
  | cancelBeforeDispatch
  | cancelDuringRun
  deriving DecidableEq, Repr

/-- Executable transition function for the tool-call layer. -/
def step? (pre : ToolCallContext) : Action → Option ToolCallContext
  | .dispatch =>
      if pre.state = .pending then
        some { pre with state := .running, startedAt := some pre.currentTime }
      else
        none
  | .spawnFailed failure =>
      if pre.state = .pending then
        some { pre with state := .failed, failureClass := some failure }
      else
        none
  | .complete =>
      if pre.state = .running ∧ pre.persistence = .committed then
        some { pre with state := .completed }
      else
        none
  | .fail failure =>
      if pre.state = .running then
        some { pre with state := .failed, failureClass := some failure }
      else
        none
  | .timeout =>
      if pre.state = .running ∧ pre.deadlineExceeded then
        some { pre with state := .timedOut }
      else
        none
  | .cancelBeforeDispatch =>
      if pre.state = .pending then
        some { pre with state := .cancelled }
      else
        none
  | .cancelDuringRun =>
      if pre.state = .running then
        some { pre with state := .cancelled }
      else
        none

/-- Refinement: every successful `step?` corresponds to a relational `Transition`. -/
theorem step_refines_transition
    (pre : ToolCallContext) (a : Action) (post : ToolCallContext) :
    step? pre a = some post → Transition pre post := by
  intro h_step
  cases a with
  | dispatch =>
      simp [step?] at h_step
      split at h_step
      · case _ h_state =>
          rw [← h_step]
          exact Transition.dispatch (h_state := h_state) (h_post := rfl)
      · contradiction
  | spawnFailed failure =>
      simp [step?] at h_step
      split at h_step
      · case _ h_state =>
          rw [← h_step]
          exact Transition.spawnFailed failure (h_state := h_state) (h_post := rfl)
      · contradiction
  | complete =>
      simp [step?] at h_step
      split at h_step
      · case _ h =>
          obtain ⟨h_state, h_persist⟩ := h
          rw [← h_step]
          exact Transition.complete (h_state := h_state) (h_persist := h_persist) (h_post := rfl)
      · contradiction
  | fail failure =>
      simp [step?] at h_step
      split at h_step
      · case _ h_state =>
          rw [← h_step]
          exact Transition.fail failure (h_state := h_state) (h_post := rfl)
      · contradiction
  | timeout =>
      simp [step?] at h_step
      split at h_step
      · case _ h =>
          obtain ⟨h_state, h_deadline⟩ := h
          rw [← h_step]
          exact Transition.timeout (h_state := h_state) (h_deadline := h_deadline) (h_post := rfl)
      · contradiction
  | cancelBeforeDispatch =>
      simp [step?] at h_step
      split at h_step
      · case _ h_state =>
          rw [← h_step]
          exact Transition.cancelBeforeDispatch (h_state := h_state) (h_post := rfl)
      · contradiction
  | cancelDuringRun =>
      simp [step?] at h_step
      split at h_step
      · case _ h_state =>
          rw [← h_step]
          exact Transition.cancelDuringRun (h_state := h_state) (h_post := rfl)
      · contradiction

end ToolCallContext
end ToolExecution
```

- [ ] **Step 2: Add import to barrel**

Edit `crates/defra-agent/proofs/Proofs/ToolExecution.lean`:

```lean
import Proofs.ToolExecution.Policy
import Proofs.ToolExecution.State
import Proofs.ToolExecution.Transition
import Proofs.ToolExecution.Properties
import Proofs.ToolExecution.Executable

/-!
# Tool Execution
…
-/
```

- [ ] **Step 3: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. If a specific case in `step_refines_transition` fails (the `split` tactic shapes can be sensitive to Lean's elaboration), the fallback pattern is to use `simp only [step?]` followed by `if_pos`/`if_neg` lemmas; consult `Proofs/Request/Executable.lean` lines 60-160 for the exact pattern used in this codebase.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution.lean crates/defra-agent/proofs/Proofs/ToolExecution/Executable.lean
git commit -m "$(cat <<'EOF'
Add ToolCallContext.Executable with Action, step?, and refinement theorem

Seven-constructor Action enum mirroring the state-changing transitions.
step? is partial (returns none when guards fail). step_refines_transition
proves every successful step? produces a valid relational Transition.
Foundation for future Rust conformance trace generation.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Extend `ComposedState` with `tool : Option ToolCallContext`

Modify `Proofs/Composed.lean`. Add the new field, update the `initial` value, and verify all existing theorems still build.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Add the import and extend the structure**

Edit `Proofs/Composed.lean`. At the top, add:

```lean
import Proofs.ToolExecution
```

Then replace the `ComposedState` definition (currently lines 14-19) with:

```lean
/-- The composed state of all single-execution layers, including the
    optional in-flight tool call. -/
structure ComposedState where
  requestId : RequestId
  process : ProcessState
  request : RequestContext
  call : InferenceCall
  tool : Option ToolExecution.ToolCallContext := none
  deriving Repr
```

The `:= none` default keeps existing call sites that build a `ComposedState` without a tool field source-compatible (anonymous constructor will work as long as fields are positional or the field is omitted).

- [ ] **Step 2: Update `initial` to set `tool := none` explicitly**

In the `def initial : ComposedState` block (currently at lines 60-84), append `, tool := none` to the structure literal — i.e. change the closing of the structure literal so the final fields read:

```lean
  , call :=
    { callId := 0
    , requestId := 0
    , backend := { val := "initial-backend" }
    , state := .queued
    }
  , tool := none
  }
```

- [ ] **Step 3: Build and verify nothing regressed**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. The existing `interrupted_request_cancels_live_linked_call` theorem should still typecheck — it doesn't reference the `tool` field. If any existing call site builds a `ComposedState` without naming fields explicitly, the `:= none` default will keep it compiling; if Lean complains about a missing field on a positional constructor, locate that site and add `, none` at the end (e.g. via `grep -rn "ComposedState.mk\|⟨0, .uninitialized" crates/defra-agent/proofs`).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Extend ComposedState with tool : Option ToolCallContext

Single-flight model matches the daemon's current single-active-tool
execution path and the max_concurrent=1 deployment in issue #149's
evidence. Multi-flight (Array) is a future extension when codex-style
persistent processes (B4) land.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Add `tool_step` variant and amend existing constructors to preserve `tool`

Two coupled changes that must land together:

1. Add the new `tool_step` cross-layer constructor.
2. Amend the four existing constructors (`process_step`, `request_step`, `persistence_step`, `call_step`) to add a `post.tool = pre.tool` preservation clause. Without this, the inductive silently allows non-tool transitions to mutate the tool field — a soundness gap.

The amendment ripples into the existing `interrupted_request_cancels_live_linked_call` proof, which currently invokes `Transition.call_step h_call_step rfl rfl rfl` (4 args). Adding a tool-preservation clause makes it 5 args, requiring one more `rfl`.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Replace the existing `Transition` inductive with the amended version**

In `Proofs/Composed.lean`, replace the entire `inductive Transition : ComposedState → ComposedState → Prop where ...` block with:

```lean
/-- A composed transition is valid only when cross-layer guards hold.
    Each constructor lifts a single-layer transition; the other layers must
    be unchanged across the composed step. -/
inductive Transition : ComposedState → ComposedState → Prop where
  | process_step {pre post : ComposedState} :
      ProcessState.Transition pre.process post.process →
      post.request = pre.request →
      post.call = pre.call →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      Transition pre post
  | request_step {pre post : ComposedState} :
      RequestContext.Transition pre.request post.request →
      post.process = pre.process →
      post.call = pre.call →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      (pre.request.state = .pending → pre.process.acceptsWork) →
      Transition pre post
  | persistence_step {pre post : ComposedState} (policy : PersistenceState.FailurePolicy)
      (nextPersistence : PersistenceState) :
      PersistenceState.Transition policy pre.request.persistence nextPersistence →
      post.request = { pre.request with persistence := nextPersistence } →
      post.process = pre.process →
      post.call = pre.call →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      Transition pre post
  | call_step {pre post : ComposedState} :
      InferenceCall.Transition pre.call post.call →
      post.request = pre.request →
      post.process = pre.process →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      Transition pre post
  | tool_step {pre post : ComposedState} {toolPre toolPost : ToolExecution.ToolCallContext} :
      pre.tool = some toolPre →
      ToolExecution.ToolCallContext.Transition toolPre toolPost →
      post.tool = some toolPost →
      post.request = pre.request →
      post.process = pre.process →
      post.call = pre.call →
      post.requestId = pre.requestId →
      -- structural composition guards: tool tracks the parent request
      toolPre.requestId = pre.requestId →
      toolPre.deadline = pre.request.deadline →
      toolPre.currentTime = pre.request.currentTime →
      Transition pre post
```

- [ ] **Step 2: Update the existing `interrupted_request_cancels_live_linked_call` proof**

The existing theorem's proof has this line (around the bottom of the proof body, after constructing `h_call_step`):

```lean
  have h_step : Transition pre post := by
    exact Transition.call_step h_call_step rfl rfl rfl
```

Replace it with the 5-arg form (the new `rfl` is for `post.tool = pre.tool`, which holds because `post : ComposedState := { pre with call := postCall }` preserves all unmentioned fields including `tool`):

```lean
  have h_step : Transition pre post := by
    exact Transition.call_step h_call_step rfl rfl rfl rfl
```

- [ ] **Step 3: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. The amended constructors take one more `=` proof each; the only existing proof that uses any of them is `interrupted_request_cancels_live_linked_call` (updated above). If the build reports any other call site of `process_step`/`request_step`/`persistence_step`/`call_step`, it's a hidden user — locate via `grep -rn "Transition\.\(process_step\|request_step\|persistence_step\|call_step\)" crates/defra-agent/proofs` and add the missing `rfl`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Add tool_step variant and require tool-preservation in other layer steps

Adds the cross-layer tool_step constructor lifting ToolCallContext.Transition
into ComposedState. Also amends process_step / request_step / persistence_step /
call_step to add post.tool = pre.tool, closing the soundness gap where a
non-tool transition could silently mutate the tool field. Updates the
existing interrupted_request_cancels_live_linked_call proof to pass the
extra rfl for tool preservation.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Prove C2 (interrupted_request_cancels_live_linked_tool)

Direct mirror of the existing `interrupted_request_cancels_live_linked_call` theorem (`Proofs/Composed.lean:96-126`). Doing C2 first builds confidence in the composition machinery before tackling C1's two-step `timeAdvance + timeout` trace.

Case-split on `toolPre.state`:
- `.pending` → use `cancelBeforeDispatch` → `.cancelled` (1 step)
- `.running` → use `cancelDuringRun` → `.cancelled` (1 step)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

In `Proofs/Composed.lean`, after the existing `interrupted_request_cancels_live_linked_call` theorem (and before `end ComposedState`), add:

```lean
/-- C2: An interrupted request cancels every live linked tool call.
    Mirror of `interrupted_request_cancels_live_linked_call`. -/
theorem interrupted_request_cancels_live_linked_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool        : pre.tool = some toolPre)
    (h_interrupted : pre.request.state = .interrupted)
    (h_linked      : toolPre.linkedTo pre.requestId)
    (h_live        : toolPre.cancellable)
    (h_synced      : toolPre.requestId = pre.requestId ∧
                     toolPre.deadline = pre.request.deadline ∧
                     toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId := by
  sorry
```

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  obtain ⟨h_sync_id, h_sync_deadline, h_sync_time⟩ := h_synced
  -- Case-split on toolPre.state via h_live (which is .pending ∨ .running).
  rcases h_live with h_pending | h_running
  · -- Pending → cancelBeforeDispatch → Cancelled
    let toolPost : ToolExecution.ToolCallContext :=
      { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tool := some toolPost }
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
      ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch
        (h_state := h_pending) (h_post := rfl)
    have h_step : Transition pre post :=
      Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl
        h_sync_id h_sync_deadline h_sync_time
    refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
    unfold ToolExecution.ToolCallContext.linkedTo at *
    exact h_linked
  · -- Running → cancelDuringRun → Cancelled
    let toolPost : ToolExecution.ToolCallContext :=
      { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tool := some toolPost }
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
      ToolExecution.ToolCallContext.Transition.cancelDuringRun
        (h_state := h_running) (h_post := rfl)
    have h_step : Transition pre post :=
      Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl
        h_sync_id h_sync_deadline h_sync_time
    refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
    unfold ToolExecution.ToolCallContext.linkedTo at *
    exact h_linked
```

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. If Lean complains about `Transition.tool_step` argument count, double-check: it takes the `h_tool`, the inner transition, `post.tool` proof, three `post.X = pre.X` proofs, the `requestId` equality, and three sync guards = 10 arguments total.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Prove C2 (interrupted_request_cancels_live_linked_tool)

Direct mirror of interrupted_request_cancels_live_linked_call for tool
calls. Case-splits on toolPre.state via the cancellable hypothesis;
both Pending and Running pre-states reach .cancelled in one composed
step (cancelBeforeDispatch / cancelDuringRun respectively).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Prove C1 (deadline_exceeded_request_timesOut_running_tool)

The headline composition theorem — closes issue #149 at the spec layer. A `Running` tool whose parent request's deadline is exceeded reaches `.timedOut` in one composed step using the `timeout` transition.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

```lean
/-- C1: A request whose deadline is exceeded times out a Running linked
    tool call via the timeout transition. The composition theorem whose
    absence in the runtime caused issue #149. -/
theorem deadline_exceeded_request_timesOut_running_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tool = some toolPre)
    (h_running  : toolPre.state = .running)
    (h_linked   : toolPre.linkedTo pre.requestId)
    (h_deadline : pre.request.deadlineExceeded)
    (h_synced   : toolPre.requestId = pre.requestId ∧
                  toolPre.deadline = pre.request.deadline ∧
                  toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.linkedTo pre.requestId := by
  sorry
```

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  obtain ⟨h_sync_id, h_sync_deadline, h_sync_time⟩ := h_synced
  -- Tool deadline is exceeded because the request deadline is exceeded
  -- and they're synced.
  have h_tool_deadline : toolPre.deadlineExceeded := by
    unfold ToolExecution.ToolCallContext.deadlineExceeded
    rw [h_sync_time, h_sync_deadline]
    exact h_deadline
  let toolPost : ToolExecution.ToolCallContext :=
    { toolPre with state := .timedOut }
  let post : ComposedState := { pre with tool := some toolPost }
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
    ToolExecution.ToolCallContext.Transition.timeout
      (h_state := h_running) (h_deadline := h_tool_deadline) (h_post := rfl)
  have h_step : Transition pre post :=
    Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl
      h_sync_id h_sync_deadline h_sync_time
  refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
  unfold ToolExecution.ToolCallContext.linkedTo at *
  exact h_linked
```

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Prove C1 (deadline_exceeded_request_timesOut_running_tool)

The composition theorem closing issue #149 at the spec layer. A Running
tool linked to a request whose deadline is exceeded reaches state .timedOut
in one composed step. The structural sync guards on tool_step let us
transport request.deadlineExceeded into tool.deadlineExceeded directly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Prove C1' (deadline_exceeded_request_cancels_pending_tool)

Companion theorem to C1. A `Pending` tool whose parent request's deadline is exceeded reaches `.cancelled` (not `.timedOut` — it never ran). Uses `cancelBeforeDispatch`.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

```lean
/-- C1': A request whose deadline is exceeded cancels a Pending linked tool
    call. Companion to C1 — a Pending tool never ran, so it reaches
    .cancelled rather than .timedOut. -/
theorem deadline_exceeded_request_cancels_pending_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tool = some toolPre)
    (h_pending  : toolPre.state = .pending)
    (h_linked   : toolPre.linkedTo pre.requestId)
    (h_deadline : pre.request.deadlineExceeded)
    (h_synced   : toolPre.requestId = pre.requestId ∧
                  toolPre.deadline = pre.request.deadline ∧
                  toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId := by
  sorry
```

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  obtain ⟨h_sync_id, h_sync_deadline, h_sync_time⟩ := h_synced
  let toolPost : ToolExecution.ToolCallContext :=
    { toolPre with state := .cancelled }
  let post : ComposedState := { pre with tool := some toolPost }
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
    ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch
      (h_state := h_pending) (h_post := rfl)
  have h_step : Transition pre post :=
    Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl
      h_sync_id h_sync_deadline h_sync_time
  refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
  unfold ToolExecution.ToolCallContext.linkedTo at *
  exact h_linked
```

Note: `h_deadline` is unused in this proof — we don't need it because `cancelBeforeDispatch` has no deadline guard. It remains in the theorem signature to make the operator semantics explicit (the theorem describes the "deadline-exceeded → cancel" path even though the cancel itself doesn't require deadline expiry).

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build, possibly with a `unused variable: h_deadline` warning. The warning is intentional and documented in the theorem comment.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Prove C1' (deadline_exceeded_request_cancels_pending_tool)

Companion to C1 covering the Pending case. A Pending tool never ran, so
deadline expiry resolves it to .cancelled (via cancelBeforeDispatch) rather
than .timedOut. h_deadline appears in the signature for operator-semantics
clarity but the cancel transition itself has no deadline precondition.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Prove C3 (terminal_tool_unblocks_request_progress)

The liveness counterpart of C1/C1'/C2. If the linked tool is in any terminal state and the request is `.processing`, then there exists a request-side transition the daemon can take. This formalizes "terminal tool ⇒ no daemon-side blockage" — the operational complement of issue #149.

The proof uses the existing `RequestContext.Transition` constructors. From `.processing`, the request can transition to `.completed` (via `finish`) or `.failed` (via `fail`). We pick `fail` since it's always available from `.processing` and doesn't require additional persistence-state preconditions; the existential statement only requires *some* transition.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Append the theorem statement with `sorry`**

```lean
/-- C3: A request whose linked tool is terminal can resume making progress.
    Semantic complement of issue #149: terminal tool ⇒ no daemon-side
    blockage at the request layer. -/
theorem terminal_tool_unblocks_request_progress
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tool = some toolPre)
    (h_terminal : isTerminal toolPre.state)
    (h_proc     : pre.request.state = .processing)
    (h_admit    : pre.request.admission = .executing) :
    ∃ post : ComposedState,
      Transition pre post ∧
      post.request.state = .failed := by
  sorry
```

Note: the original spec sketch said `RequestContext.Transition pre.request post.request` — strengthened here to `post.request.state = .failed` because that's the most direct demonstration of "progress." `h_admit` is added because `RequestContext.Transition.fail` requires `admission = .executing` to fire from `.processing`; this is structurally implied by the existing `coherentStateAdmission` invariant on `.processing` (`Proofs/Request/State.lean:131-141`) but stating it as a hypothesis avoids a side-derivation.

- [ ] **Step 2: Build with `sorry` warning**

```bash
cd crates/defra-agent/proofs && lake build
```

- [ ] **Step 3: Replace `sorry` with the proof**

```lean
  -- Use a request_step transition: processing → failed via the existing
  -- RequestContext.Transition.fail constructor.
  let postReq : RequestContext :=
    { pre.request with state := .failed, admission := .released }
  let post : ComposedState := { pre with request := postReq }
  have h_req_step : RequestContext.Transition pre.request postReq := by
    -- The exact constructor name and argument shape come from
    -- Proofs/Request/Transition.lean. Adjust the constructor invocation
    -- below if Lean reports a mismatch — the relevant constructor is the
    -- one that fires .processing/.executing → .failed/.released.
    exact RequestContext.Transition.fail
      (h_state := h_proc) (h_admit := h_admit) (h_post := rfl)
  have h_pending_guard : pre.request.state = .pending → pre.process.acceptsWork := by
    intro h_eq; rw [h_proc] at h_eq; cases h_eq
  have h_step : Transition pre post :=
    Transition.request_step h_req_step rfl rfl rfl h_pending_guard
  exact ⟨post, h_step, rfl⟩
```

If `RequestContext.Transition.fail` has a different argument shape than assumed (the codebase may use named arguments differently), inspect `Proofs/Request/Transition.lean` lines 30-50 to find the exact `fail` constructor and adjust the named-argument invocation. The conceptual move is: `processing/executing → failed/released`.

- [ ] **Step 4: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. If `RequestContext.Transition.fail` doesn't accept `h_admit` as a named arg, check the actual constructor signature in `Proofs/Request/Transition.lean` and adjust.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Prove C3 (terminal_tool_unblocks_request_progress)

Liveness counterpart to C1/C1'/C2. A request whose linked tool is in any
terminal state can take a request-side transition (specifically processing →
failed). This formalizes the operational complement of issue #149:
terminal tool ⇒ daemon is not blocked at the request layer.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Final verification — full lake build clean, Proofs.lean barrel intact

A wrap-up task. No new code; just confirm the entire proof set builds cleanly with no `sorry`s and the top-level barrel still imports everything.

**Files:**
- (no modifications expected; if anything needs touching, it's `crates/defra-agent/proofs/Proofs.lean`)

- [ ] **Step 1: Full clean build from scratch**

```bash
cd crates/defra-agent/proofs && lake clean && lake build
```

Expected: clean build, exit 0, no `sorry` warnings, no `unsolved goals` errors. Build time should be on the order of minutes (Mathlib v4.18.0 is a large dependency; the cache should already be warm from earlier tasks).

- [ ] **Step 2: Confirm `Proofs.ToolExecution` is reachable from `Proofs.lean`**

The top-level `Proofs.lean` already imports `Proofs.ToolExecution` (verified in plan preamble). The barrel re-export pattern from Task 1 means the new `State.lean`, `Transition.lean`, `Properties.lean`, `Executable.lean` modules are reachable through the existing `Proofs.ToolExecution` import — no change needed.

Sanity check:
```bash
grep -n "Proofs.ToolExecution" crates/defra-agent/proofs/Proofs.lean
```

Expected output: a single line `import Proofs.ToolExecution`. If for any reason this line is missing, add it back in alphabetical position with the other imports.

- [ ] **Step 3: Sanity-check theorem count**

```bash
grep -c "^theorem\|^  theorem" crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean
grep -c "^theorem\|^  theorem" crates/defra-agent/proofs/Proofs/Composed.lean
```

Expected: 5 theorems in `ToolExecution/Properties.lean` (T1..T5) and at least 5 theorems in `Composed.lean` (the original `interrupted_request_cancels_live_linked_call` plus C1, C1', C2, C3).

- [ ] **Step 4: No commit needed if no code changed**

If neither `lake clean && lake build` nor the sanity checks required edits, this task does not commit. If `Proofs.lean` needed re-adding the `Proofs.ToolExecution` import, commit it as:

```bash
git add crates/defra-agent/proofs/Proofs.lean
git commit -m "$(cat <<'EOF'
Restore Proofs.ToolExecution import in top-level barrel

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Plan completion checklist

After Task 17 passes:

- [ ] `lake build` clean from scratch
- [ ] Five new files created: `Proofs/ToolExecution/{State, Transition, Properties, Executable}.lean` + the relocated `Proofs/ToolExecution/Policy.lean`
- [ ] `Proofs/ToolExecution.lean` is a 5-line barrel re-export
- [ ] `Proofs/Composed.lean` has the new `tool` field, the `tool_step` constructor, and theorems C1, C1', C2, C3
- [ ] No new `sorry`s anywhere in the spec
- [ ] Each task committed individually with the canonical Co-Authored-By trailer
- [ ] Branch `bug/issue-149-native-glob-deadline` is ahead of main by 17 commits (16 implementation + 1 prior spec commit `dc5930d`)

The spec is now ready for the implementation plans for B2 (ManagedExec runtime), B3 (tool-handler refactor), B6 (schema/observability) to consume — each of those will produce Rust changes that must conform to the lifecycle, transitions, and theorems landed here.
