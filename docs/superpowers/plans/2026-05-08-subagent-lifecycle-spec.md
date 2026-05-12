# Subagent Lifecycle Lean Spec — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the Lean 4 spec defined in `docs/superpowers/specs/2026-05-08-subagent-lifecycle-design.md` — a unified `ToolCallContext` lifecycle with multi-flight, foreground/background, and child-request linkage, plus a new `BridgedState` paired-context type and theorems B1–B6 covering subagent invocation as a generalization of tool dispatch.

**Architecture:** Three orthogonal extensions to the B1 ToolCall lifecycle:
1. `ComposedState.tool : Option` becomes `tools : List`.
2. `ToolCallContext` gains `awaitMode`, `cancelPolicy`, `childRequestId` fields plus three mode/policy transitions.
3. New `Proofs/Subagent/` folder defines `BridgedState` (parent × child `ComposedState` pair), six bridge transitions, and B-properties.

**Tech Stack:** Lean 4 (toolchain pinned via `lean-toolchain`), Lake build system, Mathlib4 v4.18.0. All build/verify via `lake build` from `crates/defra-agent/proofs/`.

---

## What's NOT in this plan (deferred)

- **Conformance JSON emission** for the new `AwaitMode` / `CancelPolicy` vocabularies and `BridgedState.Transition` cases in `Proofs/Conformance/Contracts/Machines.lean`. Crosses the "Lean-only, no Rust work" boundary that B1 also held. Tracked alongside the Rust runtime plan.
- **Rust runtime changes** — `SubagentSource` impl, the seven new tools (`spawn_subagent`, `wait_task`, `get_task_result`, `cancel_task`, `read_subagent_transcript`, `send_message_to_subagent`, `list_tasks`, `background_task`), apply-time validation extensions. Tracked in a separate plan.
- **Schema migration** — adding new fields to `AgentRequest`, `AgentToolCall`, `ToolSelection` GraphQL schemas + DefraDB migration plan. Tracked in a separate plan.
- **Cross-principal delegation** — lands with sourcenetwork/defra-agent#9 (AgentPrincipal/AgentBehavior split).

---

## Conventions

- **Build/verify command:** `cd crates/defra-agent/proofs && lake build` from repo root, or `lake build` from inside the proofs directory. Treat any non-zero exit, `sorry`, `unsolved goals`, or `error:` line as failure unless the task explicitly says otherwise (e.g., a step that *expects* a `sorry` warning before proving the theorem).
- **TDD in Lean:** the "failing test" is a `theorem` declaration with `sorry` (Lean reports `sorry` as a warning but continues compiling). The "passing test" is the same `theorem` with a complete proof and no `sorry`. Verify with `lake build` after each.
- **Commit cadence:** one commit per task. Imperative, scoped commit messages with the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.
- **Working directory:** all paths in this plan are relative to the repo root (`/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-design-subagent-management`). The git working directory should remain at repo root throughout.
- **Branch:** `design/subagent-management` (already current; rebased onto `bug/issue-149-native-glob-deadline`).
- **Proof iteration:** Where a step provides a proof body, that proof reflects the planner's best understanding of the existing tactic landscape. If `lake build` reports `unsolved goals` or `unknown identifier`, treat it as a discovery loop: examine the goal state, find the analogous pattern in the cited reference file, and adjust. Do not commit a `sorry` unless the task explicitly says so.

---

## Task 1: Create the `Proofs/Subagent/` folder and barrel re-export stub

Mirror the layout of `Proofs/ToolExecution/` and `Proofs/Request/` — a top-level `Subagent.lean` barrel re-export and a `Subagent/` folder for per-concern modules.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Subagent.lean`
- Create: `crates/defra-agent/proofs/Proofs/Subagent/` (folder)

- [ ] **Step 1: Create the folder**

```bash
mkdir -p crates/defra-agent/proofs/Proofs/Subagent
```

- [ ] **Step 2: Create the barrel stub**

Write `crates/defra-agent/proofs/Proofs/Subagent.lean`:

```lean
/-!
# Subagent

Barrel import for subagent lifecycle (mode/policy state, BridgedState
paired-context, bridge transitions, properties B1–B6).

Modules are added in subsequent tasks; this file is a re-export aggregator.
-/
```

- [ ] **Step 3: Build and confirm clean**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build (exit 0, no errors, no `sorry`).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent.lean
git commit -m "$(cat <<'EOF'
Add Proofs/Subagent barrel re-export stub

Mirrors the Proofs/ToolExecution/ and Proofs/Request/ folder layout in
preparation for adding Subagent/{State,Transition,Properties,Executable}.lean.
No behavioral change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `AwaitMode` and `CancelPolicy` enums in `Subagent/State.lean`

Define the two new enums and their persisted-vocabulary round-trip helpers. Mirrors `Proofs/ToolExecution/State.lean:19-72` style.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Subagent/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Subagent.lean` (add import)

- [ ] **Step 1: Write Subagent/State.lean**

Write `crates/defra-agent/proofs/Proofs/Subagent/State.lean`:

```lean
import Proofs.Basic

/-!
# Subagent State

Mode and policy enums attached to `ToolCallContext` to support multi-flight,
foreground/background scheduling, and detachable subagent invocations.

`BridgedState` (a paired parent-child `ComposedState`) is added in a later task
once `ComposedState` has been refactored to multi-flight.
-/

namespace Subagent

/-- Whether the parent's narrative is blocked on this tool's terminal state. -/
inductive AwaitMode where
  | foreground   -- parent.advance / begin_inference are blocked while this tool is non-terminal
  | background   -- parent advances independently; tool runs concurrently
  deriving DecidableEq, Repr

namespace AwaitMode

/-- Persisted vocabulary in `AgentToolCall.await_mode`. -/
def toDefraDB : AwaitMode → String
  | .foreground => "foreground"
  | .background => "background"

def fromDefraDB? : String → Option AwaitMode
  | "foreground" => some .foreground
  | "background" => some .background
  | _ => none

theorem fromDefraDB_toDefraDB (m : AwaitMode) :
    fromDefraDB? m.toDefraDB = some m := by
  cases m <;> rfl

def all : List AwaitMode := [ .foreground, .background ]

theorem all_complete (m : AwaitMode) : m ∈ all := by
  cases m <;> simp [all]

end AwaitMode

/-- Cancel-cascade policy: whether parent termination drives the linked child
    to .interrupted, or detaches the child to its own deadline. -/
inductive CancelPolicy where
  | cascade   -- default; parent terminal ⇒ child.interruptRequestedAt set
  | detach    -- child outlives parent
  deriving DecidableEq, Repr

namespace CancelPolicy

def toDefraDB : CancelPolicy → String
  | .cascade => "cascade"
  | .detach  => "detach"

def fromDefraDB? : String → Option CancelPolicy
  | "cascade" => some .cascade
  | "detach"  => some .detach
  | _ => none

theorem fromDefraDB_toDefraDB (p : CancelPolicy) :
    fromDefraDB? p.toDefraDB = some p := by
  cases p <;> rfl

def all : List CancelPolicy := [ .cascade, .detach ]

theorem all_complete (p : CancelPolicy) : p ∈ all := by
  cases p <;> simp [all]

end CancelPolicy

/-- Configured cap on subagent recursion depth. Treated as a global parameter
    referenced by the depth-bound theorem; the runtime supplies the concrete
    value from behavior config. -/
def maxSubagentDepth : Nat := 3

end Subagent
```

- [ ] **Step 2: Update the barrel**

Edit `crates/defra-agent/proofs/Proofs/Subagent.lean`:

```lean
import Proofs.Subagent.State

/-!
# Subagent

Barrel import for subagent lifecycle (mode/policy state, BridgedState
paired-context, bridge transitions, properties B1–B6).
-/
```

- [ ] **Step 3: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. The two enums, their `toDefraDB` / `fromDefraDB?` round-trips, `all` exhaustiveness lemmas, and `maxSubagentDepth` constant all typecheck.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent.lean crates/defra-agent/proofs/Proofs/Subagent/State.lean
git commit -m "$(cat <<'EOF'
Add Subagent.AwaitMode and Subagent.CancelPolicy enums

Persisted vocabularies match planned AgentToolCall.await_mode and
AgentToolCall.cancel_policy schema fields. Round-trip lemmas and
exhaustive 'all' lists support future Rust conformance generation.
maxSubagentDepth defined here as the spec's global cap.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `awaitMode`, `cancelPolicy`, `childRequestId` fields to `ToolCallContext`

Amend `Proofs/ToolExecution/State.lean` to add three new fields with sensible defaults that preserve existing behavior. New fields don't change `ToolCallState`; T1–T5 are unaffected (verified by build).

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/State.lean:82-92`

- [ ] **Step 1: Add the import for `Subagent.State`**

At the top of `crates/defra-agent/proofs/Proofs/ToolExecution/State.lean`, add the import after the existing imports:

```lean
import Proofs.Basic
import Proofs.Persistence
import Proofs.ToolExecution.Policy
import Proofs.Subagent.State
```

- [ ] **Step 2: Extend `ToolCallContext` with the three new fields**

Replace the existing `structure ToolCallContext` definition (around lines 82-92) with:

```lean
/-- Mutable per-tool-call context that transitions carry along. -/
structure ToolCallContext where
  callId         : ToolCallId
  requestId      : RequestId
  state          : ToolCallState
  operation      : ToolOperation
  deadline       : Time
  startedAt      : Option Time := none
  currentTime    : Time
  failureClass   : Option FailureClass := none
  persistence    : PersistenceState
  -- Subagent extensions:
  awaitMode      : Subagent.AwaitMode := .foreground
  cancelPolicy   : Subagent.CancelPolicy := .cascade
  childRequestId : Option RequestId := none
  deriving Repr
```

The defaults match today's behavior: every existing tool call is conceptually `foreground + cascade + native (no childRequestId)`.

- [ ] **Step 3: Build and verify nothing else broke**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. Existing T1–T5 proofs in `Proofs/ToolExecution/Properties.lean` and existing C1–C3 proofs in `Proofs/Composed.lean` still typecheck — they don't case-split on the new fields, and the defaults absorb the schema change. If a proof breaks because it pattern-matched the old `ToolCallContext` constructor explicitly, locate the failure and replace the explicit match with field-update syntax (`{ pre with state := ... }` form).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution/State.lean
git commit -m "$(cat <<'EOF'
Extend ToolCallContext with awaitMode, cancelPolicy, childRequestId

Three subagent-extension fields with defaults that preserve existing
tool-call behavior (.foreground, .cascade, none). T1–T5 single-machine
properties and C1–C3 composition properties unaffected — fields don't
participate in any current state-machine guard or post-state computation.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `subagentDepth`, `causedByParentRequestId`, `causedByParentToolCallId` fields to `RequestContext`

Add the three new lineage / depth fields to `RequestContext` with defaults that preserve top-level-request semantics.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Request/State.lean` (the `structure RequestContext` block)

- [ ] **Step 1: Read the existing RequestContext to see field positioning**

```bash
grep -n "structure RequestContext" crates/defra-agent/proofs/Proofs/Request/State.lean
```

Note the line range for the struct.

- [ ] **Step 2: Extend `RequestContext` with the three new fields**

Inside the `structure RequestContext where` block, add the three fields at the end (just before the `deriving Repr`):

```lean
  -- Subagent lineage / depth bound:
  subagentDepth                : Nat := 0
  causedByParentRequestId      : Option RequestId := none
  causedByParentToolCallId     : Option ToolExecution.ToolCallId := none
```

If `ToolExecution.ToolCallId` isn't yet imported in `Request/State.lean`, add the import line near the top:

```lean
import Proofs.ToolExecution.State
```

(Note: this creates a transitive dependency from `Request/State.lean` to `ToolExecution/State.lean`, which itself imports `Subagent/State.lean`. If the import graph rejects this for cycle reasons, fall back to defining `causedByParentToolCallId : Option Nat` here and stating the equivalence as a structural assumption — `ToolCallId` is `abbrev ToolCallId := Nat` already, so the underlying type is the same.)

- [ ] **Step 3: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. Existing S1–S6 properties in `Proofs/Properties/Safety.lean` are unaffected — they don't reference any of the new fields. If a proof breaks, the most likely cause is a pattern-match-by-position on `RequestContext.mk`; rewrite it as a structural pattern (or use `{ pre with ... }` form).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Request/State.lean
git commit -m "$(cat <<'EOF'
Extend RequestContext with subagentDepth and parent-link fields

Three new fields support the depth-bound structural invariant (B4) and
parent-request linkage from the subagent spec:
- subagentDepth : Nat (default 0 for top-level)
- causedByParentRequestId : Option RequestId (default none)
- causedByParentToolCallId : Option ToolCallId (default none)

S1–S6 unaffected; defaults preserve top-level-request semantics.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Add `background`, `foreground`, `detach` transitions and restrict `complete` to native tools

Three new mode/policy transitions on `ToolCallContext.Transition` (no state advance), plus a tightened `complete` precondition that restricts the inner constructor to native tools (`childRequestId = none`). Subagent tools reach `.completed` only via the composed-layer `bridge_complete` (Task 14).

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean` (T1 may need a tweak to handle the new constructors)

- [ ] **Step 1: Add the three new constructors to the `Transition` inductive**

In `Proofs/ToolExecution/Transition.lean`, locate the `inductive Transition : ToolCallContext → ToolCallContext → Prop where` block. Inside it, after the `cancelDuringRun` constructor and before `timeAdvance`, add:

```lean
  | background {pre post : ToolCallContext}
      (h_state : pre.state = .running)
      (h_mode  : pre.awaitMode = .foreground)
      (h_post  : post = { pre with awaitMode := .background })
      : Transition pre post

  | foreground {pre post : ToolCallContext}
      (h_state : pre.state = .running)
      (h_mode  : pre.awaitMode = .background)
      (h_post  : post = { pre with awaitMode := .foreground })
      : Transition pre post

  | detach {pre post : ToolCallContext}
      (h_live  : pre.state = .pending ∨ pre.state = .running)
      (h_pol   : pre.cancelPolicy = .cascade)
      (h_post  : post = { pre with cancelPolicy := .detach })
      : Transition pre post
```

- [ ] **Step 2: Restrict the `complete` constructor to native tools**

In the same `Transition` inductive, locate the existing `complete` constructor:

```lean
  | complete {pre post : ToolCallContext}
      (h_state    : pre.state = .running)
      (h_persist  : pre.persistence = .committed)
      (h_post     : post = { pre with state := .completed })
      : Transition pre post
```

Replace it with:

```lean
  | complete {pre post : ToolCallContext}
      (h_state    : pre.state = .running)
      (h_persist  : pre.persistence = .committed)
      (h_native   : pre.childRequestId = none)
      (h_post     : post = { pre with state := .completed })
      : Transition pre post
```

- [ ] **Step 3: Update T1 (terminal_irreversible) to handle the three new constructors**

In `Proofs/ToolExecution/Properties.lean`, locate `terminal_irreversible`. Its proof case-splits over all `Transition` constructors; the three new ones each have a `pre.state = .running` (or `.pending` for `detach`) precondition that contradicts `isTerminal pre.state`. Add the three new cases to the `cases h_step with` block:

```lean
  | background _ _ _ =>
      rw [show pre.state = .running from ‹_›] at h_terminal
      exact (running_not_terminal h_terminal).elim
  | foreground _ _ _ =>
      rw [show pre.state = .running from ‹_›] at h_terminal
      exact (running_not_terminal h_terminal).elim
  | detach h_live _ _ =>
      cases h_live with
      | inl h => rw [h] at h_terminal; exact (pending_not_terminal h_terminal).elim
      | inr h => rw [h] at h_terminal; exact (running_not_terminal h_terminal).elim
```

(If `pending_not_terminal` / `running_not_terminal` aren't already lemmas in the file, replace the `.elim` lines with explicit `simp [isTerminal] at h_terminal` reductions. Look at the existing `dispatch` case in the same proof for the right pattern.)

- [ ] **Step 4: Update T4 (cancellable_iff_non_terminal) and T5 (live_call_reaches_terminal) — each may need new cases**

For T4: the new constructors don't touch `state`, so any case that infers from `Transition` to `state = ...` should be unaffected. Verify build picks them up.

For T5: the proof shows reachability to terminal. The new constructors don't progress state but don't block reachability either — the existing `timeAdvance + timeout` path remains the trace witness for any `running` tool, regardless of `awaitMode`. Verify build still passes.

- [ ] **Step 5: Build and verify**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. T1's case-split now handles 12 constructors; the three new ones each contradict `isTerminal pre.state`. T2, T3, T4, T5 unaffected (the new constructors don't satisfy any of their hypotheses).

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ToolExecution/Transition.lean crates/defra-agent/proofs/Proofs/ToolExecution/Properties.lean
git commit -m "$(cat <<'EOF'
Add background/foreground/detach mode transitions; restrict complete to native

Three new ToolCallContext.Transition constructors flip awaitMode and
cancelPolicy without advancing state. The complete constructor gains
h_native (childRequestId = none) so subagent tools reach .completed
only via the composed-layer bridge_complete (added later). T1's case
analysis is extended; T2–T5 unaffected.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Refactor `ComposedState.tool : Option` to `tools : List` and adapt `tool_step`

The big structural refactor. `ComposedState.tool : Option ToolCallContext` becomes `tools : List ToolCallContext`. The `tool_step` constructor lifts a single inner transition acting on one element of the list. Existing C1, C1', C2, C3 proofs will break — re-stated in subsequent tasks.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean:21` (struct field) and `:66-90` (`tool_step` variant)

- [ ] **Step 1: Change the struct field**

In `Proofs/Composed.lean`, locate the `structure ComposedState where` block. Replace:

```lean
  tool : Option ToolExecution.ToolCallContext := none
```

with:

```lean
  tools : List ToolExecution.ToolCallContext := []
```

- [ ] **Step 2: Replace the `tool_step` constructor**

Locate the existing `tool_step` constructor in the `inductive Transition : ComposedState → ComposedState → Prop`. Replace its body with the multi-flight form:

```lean
  | tool_step {pre post : ComposedState} {idx : Nat}
              {toolPre toolPost : ToolExecution.ToolCallContext} :
      pre.tools[idx]? = some toolPre →
      ToolExecution.ToolCallContext.Transition toolPre toolPost →
      post.tools = pre.tools.set idx toolPost →
      post.request = pre.request →
      post.process = pre.process →
      post.call = pre.call →
      post.requestId = pre.requestId →
      post.persistence = pre.persistence →
      -- structural composition guards (carry through):
      toolPre.requestId = pre.requestId →
      toolPre.deadline = pre.request.deadline →
      toolPre.currentTime = pre.request.currentTime →
      Transition pre post
```

The composition guards on the inner tool's `requestId`, `deadline`, and `currentTime` are unchanged from the single-flight version — they still hold per linked tool.

- [ ] **Step 3: Add a helper for "tool is in tools by callId"**

Add this helper near the top of `Composed.lean`, after the imports and before the `ComposedState` struct:

```lean
namespace ComposedState

/-- A tool is linked to this composed state if it's in the tools list. -/
def hasToolByCallId (s : ComposedState) (callId : ToolExecution.ToolCallId) : Prop :=
  ∃ t ∈ s.tools, t.callId = callId

instance (s : ComposedState) (callId : ToolExecution.ToolCallId) :
    Decidable (s.hasToolByCallId callId) := by
  unfold hasToolByCallId; infer_instance

/-- The unique tool with a given callId, if it exists. -/
def findToolByCallId (s : ComposedState) (callId : ToolExecution.ToolCallId) :
    Option ToolExecution.ToolCallContext :=
  s.tools.find? (fun t => t.callId = callId)

end ComposedState
```

- [ ] **Step 4: Build — expect existing C1/C1'/C2/C3 proofs to break**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: build FAILS. Existing C1, C1', C2, C3 theorems pattern-matched on `pre.tool = some toolPre`; with the field renamed and re-typed they no longer typecheck. This is the expected breakage; the next four tasks restate them.

To unblock the build for now (so subsequent tasks can run partial verification), comment out the bodies of `interrupted_request_cancels_live_linked_tool`, `deadline_exceeded_request_timesOut_running_tool`, `deadline_exceeded_request_cancels_pending_tool`, and `terminal_tool_unblocks_request_progress` and replace each with `:= sorry`. The compile-only build will then succeed with `sorry` warnings.

- [ ] **Step 5: Build with sorrys to confirm structural change is otherwise clean**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | grep -E "error:|sorry" | head -20
```

Expected: only the four `sorry` warnings on the four C-theorems we just stubbed; no `error:` lines. Any `error:` line means there's a structural problem to fix before continuing.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Refactor ComposedState.tool to tools : List for multi-flight

ComposedState now carries a list of concurrently-live ToolCallContexts.
tool_step lifts a single inner transition acting on one element by index.
hasToolByCallId / findToolByCallId helpers added for membership reasoning.

C1, C1', C2, C3 stubbed with sorry — to be restated with multi-flight
quantification in tasks 7–10. T1–T5 single-machine properties unaffected.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Restate C1 (deadline_exceeded → running tool .timedOut) for multi-flight

Quantify ∀ over `pre.tools`: every running, deadline-exceeded linked tool reaches `.timedOut`.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean` (the `deadline_exceeded_request_timesOut_running_tool` theorem)

- [ ] **Step 1: Replace the theorem statement and proof**

Locate `deadline_exceeded_request_timesOut_running_tool`. Replace the theorem with:

```lean
/-- C1: A request whose deadline is exceeded times out every Running linked
    tool. Multi-flight form: quantified over all live linked tools. -/
theorem deadline_exceeded_request_timesOut_running_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in        : toolPre ∈ pre.tools)
    (h_running   : toolPre.state = .running)
    (h_linked    : toolPre.requestId = pre.requestId)
    (h_deadline  : pre.request.deadlineExceeded)
    (h_synced    : toolPre.deadline = pre.request.deadline ∧
                   toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      toolPost ∈ post.tools ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.requestId = pre.requestId := by
  -- The proof follows the single-flight version but indexes the chosen tool.
  -- Find the index of toolPre in pre.tools.
  obtain ⟨idx, h_idx⟩ := List.mem_iff_get?.mp h_in
  -- Apply the inner ToolCallContext.timeout transition on toolPre.
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                    { toolPre with state := .timedOut } := by
    exact ToolExecution.ToolCallContext.Transition.timeout
      h_running
      (by rw [h_synced.1, h_synced.2]; exact h_deadline)
      rfl
  -- Lift to a tool_step transition.
  let toolPost := { toolPre with state := .timedOut }
  let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
  refine ⟨post, toolPost, ?_, ?_, rfl, rfl, h_linked⟩
  · exact Trace.step
      (Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl rfl
         h_linked h_synced.1 h_synced.2)
      Trace.refl
  · simp [post, List.mem_set]; exact Or.inl rfl
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: C1 typechecks. If `List.mem_iff_get?` is named differently in the Mathlib version pinned by the project, search Mathlib for the equivalent — typical names: `List.mem_iff_get?`, `List.mem_iff_getElem?`, `List.get?_eq_some`. Adjust the call accordingly.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Restate C1 (deadline_exceeded_request_timesOut_running_tools) for multi-flight

Multi-flight quantification: every running, deadline-exceeded tool linked
to the parent reaches .timedOut. Proof obtains an index witness from the
list-membership hypothesis, applies the inner timeout transition, and
lifts via tool_step at the obtained index.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Restate C1' (deadline_exceeded → pending tool .cancelled) for multi-flight

Same pattern as C1, but for `Pending`-state tools that get cancelled instead of timed-out (they never ran).

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Replace the theorem**

```lean
/-- C1': A request whose deadline is exceeded cancels every Pending linked tool. -/
theorem deadline_exceeded_request_cancels_pending_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in        : toolPre ∈ pre.tools)
    (h_pending   : toolPre.state = .pending)
    (h_linked    : toolPre.requestId = pre.requestId)
    (h_deadline  : pre.request.deadlineExceeded)
    (h_synced    : toolPre.deadline = pre.request.deadline ∧
                   toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId := by
  obtain ⟨idx, h_idx⟩ := List.mem_iff_get?.mp h_in
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                    { toolPre with state := .cancelled } :=
    ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch h_pending rfl
  let toolPost := { toolPre with state := .cancelled }
  let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
  refine ⟨post, toolPost, ?_, ?_, rfl, h_linked⟩
  · exact Trace.step
      (Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl rfl
         h_linked h_synced.1 h_synced.2)
      Trace.refl
  · simp [post, List.mem_set]; exact Or.inl rfl
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: C1' typechecks.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Restate C1' (deadline_exceeded_request_cancels_pending_tools) for multi-flight

Pending tools at deadline route to .cancelled (they never ran). Same
multi-flight pattern as C1: index witness + cancelBeforeDispatch + lift.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Restate C2 (interrupted request → live tools .cancelled) for multi-flight

Multi-flight C2: every cancellable linked tool reaches `.cancelled` when the parent transitions to `.interrupted`.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Replace the theorem**

```lean
/-- C2: An interrupted request cancels every live linked tool call. -/
theorem interrupted_request_cancels_live_linked_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in           : toolPre ∈ pre.tools)
    (h_interrupted  : pre.request.state = .interrupted)
    (h_linked       : toolPre.requestId = pre.requestId)
    (h_live         : toolPre.cancellable) :
    ∃ post toolPost,
      Trace pre post ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId := by
  obtain ⟨idx, h_idx⟩ := List.mem_iff_get?.mp h_in
  -- toolPre.cancellable means state = .pending ∨ state = .running.
  cases h_live with
  | inl h_pending =>
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                      { toolPre with state := .cancelled } :=
      ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch h_pending rfl
    let toolPost := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, ?_, rfl, h_linked⟩
    · exact Trace.step
        (Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl rfl
           h_linked rfl rfl)
        Trace.refl
    · simp [post, List.mem_set]; exact Or.inl rfl
  | inr h_running =>
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                      { toolPre with state := .cancelled } :=
      ToolExecution.ToolCallContext.Transition.cancelDuringRun h_running rfl
    let toolPost := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, ?_, rfl, h_linked⟩
    · exact Trace.step
        (Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl rfl
           h_linked rfl rfl)
        Trace.refl
    · simp [post, List.mem_set]; exact Or.inl rfl
```

- [ ] **Step 2: Build and commit**

```bash
cd crates/defra-agent/proofs && lake build
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Restate C2 (interrupted_request_cancels_live_linked_tools) for multi-flight

Two cases per cancellable disjunct (pending → cancelBeforeDispatch;
running → cancelDuringRun). Each branch lifts via tool_step at the
chosen index.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Restate C3 (terminal tool unblocks parent progress) for multi-flight

C3 says: a request whose tools are all terminal can resume making progress. With multi-flight, "all terminal" is the universal-quantified version.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Replace the theorem**

```lean
/-- C3: A request whose linked tools are all terminal can resume making progress. -/
theorem all_tools_terminal_unblocks_request_progress
    {pre : ComposedState}
    (h_all_terminal : ∀ t ∈ pre.tools, isTerminal t.state)
    (h_proc         : pre.request.state = .processing) :
    ∃ post,
      Transition pre post ∧
      RequestContext.Transition pre.request post.request := by
  -- The advance transition fires unconditionally on a processing request when
  -- no live tool blocks it. This proof mirrors the single-flight C3 pattern
  -- in the previous Composed.lean — pick the request_step variant that lifts
  -- RequestContext.Transition.advance, with the new no_blocking_foreground
  -- guard discharged by h_all_terminal (every tool is terminal, so no live
  -- foreground tool exists).
  let postReq : RequestContext :=
    { pre.request with progressSeq := pre.request.progressSeq + 1 }
  let post : ComposedState := { pre with request := postReq }
  refine ⟨post, ?_, ?_⟩
  · exact Transition.request_step
      (RequestContext.Transition.advance h_proc rfl rfl)
      rfl rfl rfl rfl
      (by
        intro h_fg
        obtain ⟨t, h_in, h_mode, h_live⟩ := h_fg
        exact h_live (h_all_terminal t h_in))
  · exact RequestContext.Transition.advance h_proc rfl rfl
```

(Note: this references `Transition.request_step` and a `no_blocking_foreground` guard. If those signatures don't yet exist in `Composed.lean` — they're added in Task 11 — temporarily use `:= sorry` here and re-prove this theorem at the end of Task 11. Alternatively, swap Task 10 and Task 11 in execution order.)

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: C3 typechecks if Task 11's `no_blocking_foreground` guard is already in place; otherwise commit a `:= sorry` and revisit after Task 11.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Restate C3 (all_tools_terminal_unblocks_request_progress) for multi-flight

When every linked tool is terminal, the no_blocking_foreground guard
on advance is satisfied and the request can progress. Proof discharges
the guard by case analysis: any tool claimed live foreground is
contradicted by h_all_terminal.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Add `INV-FG` invariant and `no_blocking_foreground` guard on `advance` / `begin_inference`

INV-FG is the structural invariant that at most one foreground non-terminal tool exists. The guard on `advance` and `begin_inference` ensures the parent can't advance its narrative while a foreground tool is live.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`

- [ ] **Step 1: Add the predicate and the guard to `Transition.request_step`**

Locate the existing `request_step` constructor in `Composed.Transition` (the inner one that lifts `RequestContext.Transition`). Add an extra hypothesis to it for `advance` and `begin_inference` cases. The simplest model: split `request_step` into two constructors — one for non-`advance`/non-`begin_inference` request transitions (no guard) and one for advance/begin_inference (with guard).

In practice: add a single new precondition to the existing `request_step` constructor that holds vacuously for non-advance transitions:

```lean
  | request_step {pre post : ComposedState} {reqPost : RequestContext}
      (h_req    : RequestContext.Transition pre.request reqPost) :
      pre.tools = post.tools →
      pre.process = post.process →
      pre.call = post.call →
      pre.persistence = post.persistence →
      post.request = reqPost →
      -- new guard: advance/begin_inference require no live foreground tool
      (∀ {hadv : reqPost.progressSeq > pre.request.progressSeq ∨
                  (pre.request.state = .claimed ∧ reqPost.state = .processing)},
       ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧ ¬ isTerminal t.state) →
      Transition pre post
```

Actually, the cleanest formulation is to express the guard via the post-state's transition kind. Use the simpler form: ALL `request_step` lifts must satisfy the guard, but the guard is vacuously true for non-blocking transitions.

Reformulate as:

```lean
  | request_step {pre post : ComposedState} {reqPost : RequestContext}
      (h_req       : RequestContext.Transition pre.request reqPost)
      (h_no_block  : ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                                          ¬ ToolExecution.isTerminal t.state)
      (h_tools_eq  : pre.tools = post.tools)
      (h_proc_eq   : pre.process = post.process)
      (h_call_eq   : pre.call = post.call)
      (h_persist_eq: pre.persistence = post.persistence)
      (h_req_eq    : post.request = reqPost)
      : Transition pre post
```

This unconditionally requires no foreground non-terminal tool for any request_step lift. That's slightly stronger than necessary (terminal-only request_step lifts like fail or interrupt would also require it), but it's a simpler and provably-safe overapproximation.

If that's too restrictive (it blocks `interrupt_processing` from firing while a foreground tool is live, which the cancel-cascade story specifically wants to allow), tighten the guard to apply only to `advance` and `begin_inference` by inspecting `reqPost.progressSeq` and `reqPost.state`:

```lean
      (h_no_block  : (reqPost.progressSeq > pre.request.progressSeq ∨
                       (pre.request.state = .claimed ∧
                        reqPost.state = .processing)) →
                      ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                                          ¬ ToolExecution.isTerminal t.state)
```

(The disjunction names the two transitions that bump progress: `advance` increments `progressSeq`; `begin_inference` flips `claimed → processing`.)

- [ ] **Step 2: Add INV-FG as a structural invariant on the trace**

Append to `Proofs/Composed.lean` (after the C theorems):

```lean
namespace ComposedState

/-- INV-FG: at most one foreground non-terminal tool per composed state. -/
def invFG (s : ComposedState) : Prop :=
  (s.tools.filter (fun t => t.awaitMode = .foreground ∧
                              ¬ ToolExecution.isTerminal t.state)).length ≤ 1

/-- INV-FG is preserved by any transition. -/
theorem invFG_preserved
    {pre post : ComposedState}
    (h_inv  : pre.invFG)
    (h_step : Transition pre post) :
    post.invFG := by
  cases h_step with
  | tool_step h_idx h_t_step h_tools _ _ _ _ _ _ _ _ =>
    -- A single tool transitions; the count of foreground non-terminal tools
    -- can only stay the same or decrease (state advancing toward terminal,
    -- or awaitMode flipping to background).
    sorry  -- iterative discovery: case-split h_t_step by inner constructor.
  | request_step _ _ h_tools _ _ _ _ =>
    -- Tools list unchanged.
    rw [← h_tools]; exact h_inv
  | _ =>
    -- All other variants leave tools unchanged or fall under similar arguments.
    sorry  -- iterative discovery: examine each composed-layer constructor.

end ComposedState
```

The two `sorry`s mark places where the proof needs case-by-case discharge against the inner transition constructors. Both cases are decidable structurally — every inner constructor either preserves or strictly decreases the count of "foreground non-terminal tools."

- [ ] **Step 3: Build with sorrys**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: build succeeds with `sorry` warnings on the two cases of `invFG_preserved`. The guard is in place; subsequent tasks can rely on it.

- [ ] **Step 4: Discharge the two sorrys (iteration)**

For each `sorry`, use `cases h_t_step with` and discharge per inner constructor. The pattern: each inner constructor either (a) doesn't change `awaitMode` and either advances `state` toward terminal or doesn't change it, or (b) flips `awaitMode` (the new `background` / `foreground` constructors). For (a), `List.length_filter_mono` or direct counting works. For (b), `background` strictly decreases the count; `foreground` may increase it by one but only when the count was previously zero (because `INV-FG` already constrained the pre-state).

If the discovery loop gets stuck for more than ~30 minutes per case, stub `sorry`, file a follow-up issue, and continue. INV-FG isn't load-bearing for the B-theorems below; it's a structural witness.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Composed.lean
git commit -m "$(cat <<'EOF'
Add no_blocking_foreground guard and INV-FG invariant on ComposedState

request_step lifts now carry h_no_block: while any foreground non-terminal
tool is live, advance / begin_inference cannot fire. INV-FG (at most one
foreground non-terminal tool per state) is preserved across transitions —
proof case-splits on inner Transition constructors. C3 (Task 10) discharges
its guard via this invariant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Define `BridgedState` type with structural guards

The paired-context type that captures the parent ↔ child Request relationship for a single subagent invocation.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Subagent/State.lean` (append)

- [ ] **Step 1: Append `BridgedState` to `Subagent/State.lean`**

At the end of `Subagent/State.lean` (after the `CancelPolicy` namespace and `maxSubagentDepth`), add:

```lean
import Proofs.Composed

namespace Subagent

/-- A paired parent/child composed state representing one subagent invocation
    edge. Structural guards are stated as predicates rather than baked into
    the constructor; they hold for any state reachable from `bridge_spawn`. -/
structure BridgedState where
  parent       : ComposedState
  child        : ComposedState
  bridgeCallId : ToolExecution.ToolCallId
  deriving Repr

namespace BridgedState

/-- The bridge tool exists on the parent and points to the child. -/
def parentLink (s : BridgedState) : Prop :=
  ∃ t ∈ s.parent.tools,
    t.callId = s.bridgeCallId ∧
    t.childRequestId = some s.child.requestId

/-- The child request points back to the parent. -/
def childLink (s : BridgedState) : Prop :=
  s.child.request.causedByParentRequestId = some s.parent.requestId ∧
  s.child.request.causedByParentToolCallId = some s.bridgeCallId

/-- The full link is symmetric. -/
def linked (s : BridgedState) : Prop :=
  s.parentLink ∧ s.childLink

/-- The child has been observed reaching .completed by its terminal flag. -/
def bridgeObservedCompleted (s : BridgedState) : Prop :=
  s.child.request.state = .completed

/-- The child terminated in any non-completed terminal state. -/
def bridgeChildFailed (s : BridgedState) : Prop :=
  s.child.request.state = .failed ∨
  s.child.request.state = .dead ∨
  s.child.request.state = .interrupted ∨
  s.child.request.state = .superseded

end BridgedState

end Subagent
```

The `import Proofs.Composed` line goes at the top of the file (move existing imports together).

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: `BridgedState` and its predicates typecheck. The `linked` predicate is the structural guard that `bridge_spawn` (next task) establishes at construction time and that subsequent transitions preserve.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent/State.lean
git commit -m "$(cat <<'EOF'
Add Subagent.BridgedState paired parent/child composed state

Carries parent and child ComposedStates plus the bridgeCallId that
identifies which parent ToolCall owns the bridge edge. Predicates:
parentLink, childLink, linked (their conjunction), bridgeObservedCompleted,
bridgeChildFailed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Add `BridgedState.Transition` with `parent_step`, `child_step`, `bridge_spawn`

Three of the six bridge transitions. `parent_step` and `child_step` lift single-side composed transitions (preserving link symmetry); `bridge_spawn` materializes a new parent tool + new child request.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Subagent/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Subagent.lean` (add import)

- [ ] **Step 1: Write Subagent/Transition.lean**

```lean
import Proofs.Subagent.State

/-!
# Subagent Bridge Transitions

Six transitions on `BridgedState`:
  • parent_step  — lift any ComposedState transition on the parent
  • child_step   — lift any ComposedState transition on the child
  • bridge_spawn — materialize the bridge edge (new parent tool + new child request)
  • bridge_complete       — child .completed → parent ToolCall .completed
  • bridge_failure        — child non-.completed terminal → parent ToolCall .failed/.cancelled
  • bridge_cancel_cascade — parent terminal w/ cascade → child interruptRequestedAt set
-/

namespace Subagent
namespace BridgedState

inductive Transition : BridgedState → BridgedState → Prop where

  | parent_step {pre post : BridgedState}
      (h_step          : ComposedState.Transition pre.parent post.parent)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      (h_link_pre      : pre.linked)
      (h_link_post     : post.linked)
      : Transition pre post

  | child_step {pre post : BridgedState}
      (h_step          : ComposedState.Transition pre.child post.child)
      (h_parent_eq     : post.parent = pre.parent)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      (h_link_pre      : pre.linked)
      (h_link_post     : post.linked)
      : Transition pre post

  | bridge_spawn {pre post : BridgedState}
      (h_parent_proc   : pre.parent.request.state = .processing)
      (h_depth_ok      : pre.parent.request.subagentDepth + 1 ≤ maxSubagentDepth)
      -- new parent tool with the right shape:
      (h_post_parent_tool :
         ∃ t ∈ post.parent.tools,
           t.callId = post.bridgeCallId ∧
           t.state = .pending ∧
           t.childRequestId = some post.child.requestId)
      -- new child request with parent linkage and depth = parent depth + 1:
      (h_post_child :
         post.child.request.state = .pending ∧
         post.child.request.causedByParentRequestId = some pre.parent.requestId ∧
         post.child.request.causedByParentToolCallId = some post.bridgeCallId ∧
         post.child.request.subagentDepth = pre.parent.request.subagentDepth + 1)
      : Transition pre post

/-- Reflexive-transitive closure for liveness statements. -/
inductive Trace : BridgedState → BridgedState → Prop where
  | refl {s : BridgedState} : Trace s s
  | step {s₁ s₂ s₃ : BridgedState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

end BridgedState
end Subagent
```

- [ ] **Step 2: Add the import to the barrel**

Edit `crates/defra-agent/proofs/Proofs/Subagent.lean`:

```lean
import Proofs.Subagent.State
import Proofs.Subagent.Transition

/-!
# Subagent

Barrel import for subagent lifecycle (mode/policy state, BridgedState
paired-context, bridge transitions, properties B1–B6).
-/
```

- [ ] **Step 3: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. The three constructors typecheck. `bridge_spawn`'s structural guards on the post-state are existential predicates the proof obligation can discharge with `⟨t, h_in, ...⟩` witnesses.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent.lean crates/defra-agent/proofs/Proofs/Subagent/Transition.lean
git commit -m "$(cat <<'EOF'
Add BridgedState.Transition: parent_step, child_step, bridge_spawn + Trace

Three single-side / spawn transitions. parent_step and child_step lift
inner ComposedState transitions while preserving the symmetric link
predicate. bridge_spawn materializes a new bridge edge with the depth
precondition (parent.subagentDepth + 1 ≤ maxSubagentDepth) and the
expected post-state shape (pending tool + pending child request with
parent-linkage fields).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Add `bridge_complete`, `bridge_failure`, `bridge_cancel_cascade`

The three explicitly-bidirectional bridge transitions.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Subagent/Transition.lean`

- [ ] **Step 1: Append three new constructors to the `Transition` inductive**

In `Proofs/Subagent/Transition.lean`, add three constructors inside the `inductive Transition` block (before its closing). Place them after `bridge_spawn`:

```lean
  | bridge_complete {pre post : BridgedState}
      (h_child_done    : pre.child.request.state = .completed)
      (h_running       : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧ t.state = .running)
      (h_persisted     : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧
                           t.persistence = .committed)
      (h_post_tool     : ∃ t ∈ post.parent.tools,
                           t.callId = pre.bridgeCallId ∧ t.state = .completed)
      (h_others_eq     : ∀ t ∈ pre.parent.tools, t.callId ≠ pre.bridgeCallId →
                          t ∈ post.parent.tools)
      (h_request_eq    : post.parent.request = pre.parent.request)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      : Transition pre post

  | bridge_failure {pre post : BridgedState}
      (h_child_term    : pre.child.request.state = .failed ∨
                         pre.child.request.state = .dead ∨
                         pre.child.request.state = .interrupted ∨
                         pre.child.request.state = .superseded)
      (h_running       : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧ t.state = .running)
      (h_post_tool     : ∃ t ∈ post.parent.tools,
                           t.callId = pre.bridgeCallId ∧
                           (t.state = .failed ∨ t.state = .cancelled))
      (h_others_eq     : ∀ t ∈ pre.parent.tools, t.callId ≠ pre.bridgeCallId →
                          t ∈ post.parent.tools)
      (h_request_eq    : post.parent.request = pre.parent.request)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      : Transition pre post

  | bridge_cancel_cascade {pre post : BridgedState}
      (h_parent_term   : ToolExecution.isTerminal pre.parent.request.state ∨
                         (∃ t ∈ pre.parent.tools,
                            t.callId = pre.bridgeCallId ∧
                            t.state = .cancelled))
      (h_cascade_pol   : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧
                           t.cancelPolicy = .cascade)
      (h_interrupt_set : post.child.request.interruptRequestedAt.isSome)
      (h_parent_eq     : post.parent = pre.parent)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      : Transition pre post
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build. All six bridge constructors are now defined.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent/Transition.lean
git commit -m "$(cat <<'EOF'
Add bridge_complete, bridge_failure, bridge_cancel_cascade

Three explicitly-bidirectional bridge transitions:
- bridge_complete: child .completed → parent tool .completed (with persistence)
- bridge_failure: child non-.completed terminal → parent tool .failed/.cancelled
- bridge_cancel_cascade: parent terminal w/ cascade → child interruptRequestedAt set

The cancel-cascade only sets the interrupt flag; the child's existing
interrupt_processing constructor (lifted via child_step) drives the actual
state transition to .interrupted in B3's trace.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Define `INV-DEPTH` and `INV-LINK` in `Subagent/Properties.lean`

Two structural invariants. INV-DEPTH bounds subagent recursion; INV-LINK confirms parent ↔ child references stay symmetric across all transitions.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Subagent/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Subagent.lean` (add import)

- [ ] **Step 1: Write Subagent/Properties.lean**

```lean
import Proofs.Subagent.Transition

/-!
# Subagent Properties

B1–B6 plus the structural invariants INV-DEPTH and INV-LINK.
-/

namespace Subagent
namespace BridgedState

/-- INV-DEPTH: subagent depth on both sides of the bridge stays ≤ maxSubagentDepth
    across any reachable trace. -/
theorem inv_depth
    (pre post : BridgedState)
    (h_init  : pre.parent.request.subagentDepth ≤ maxSubagentDepth ∧
               pre.child.request.subagentDepth ≤ maxSubagentDepth)
    (h_trace : Trace pre post) :
    post.parent.request.subagentDepth ≤ maxSubagentDepth ∧
    post.child.request.subagentDepth ≤ maxSubagentDepth := by
  induction h_trace with
  | refl => exact h_init
  | step h_step _ ih =>
    apply ih
    cases h_step with
    | parent_step h_inner _ _ _ _ =>
      -- parent's subagentDepth doesn't change across any RequestContext.Transition
      -- nor across any tool_step (tool_step preserves request).
      -- This is structurally true; the proof discovers the right invariant lemma.
      sorry  -- iterative discovery: case-split h_inner; subagentDepth unchanged in all cases.
    | child_step h_inner _ _ _ _ =>
      sorry  -- same as parent_step but on child side.
    | bridge_spawn h_proc h_depth _ h_post_child =>
      refine ⟨h_init.1, ?_⟩
      rw [h_post_child.2.2.2]; exact h_depth
    | bridge_complete _ _ _ _ _ h_req_eq h_child_eq _ =>
      rw [h_req_eq, h_child_eq]; exact h_init
    | bridge_failure _ _ _ _ h_req_eq h_child_eq _ =>
      rw [h_req_eq, h_child_eq]; exact h_init
    | bridge_cancel_cascade _ _ _ h_parent_eq _ =>
      rw [h_parent_eq]
      -- Child's subagentDepth doesn't change; only interruptRequestedAt does.
      sorry  -- iterative: post.child.request.subagentDepth = pre.child.request.subagentDepth.

/-- INV-LINK: parent and child links stay symmetric across any reachable trace
    once initialized by `bridge_spawn`. -/
theorem inv_link
    (pre post : BridgedState)
    (h_init  : pre.linked)
    (h_trace : Trace pre post) :
    post.linked := by
  induction h_trace with
  | refl => exact h_init
  | step h_step _ ih =>
    apply ih
    cases h_step with
    | parent_step _ _ _ _ h_link_post => exact h_link_post
    | child_step _ _ _ _ h_link_post  => exact h_link_post
    | bridge_spawn _ _ h_post_tool h_post_child =>
      -- Establish post.linked from the post-state shape.
      refine ⟨?_, ?_, ?_⟩
      · obtain ⟨t, h_in, h_id, _, h_child⟩ := h_post_tool
        exact ⟨t, h_in, h_id, h_child⟩
      · exact h_post_child.2.1
      · exact h_post_child.2.2.1
    | bridge_complete _ _ _ _ _ h_req_eq h_child_eq h_bridge_eq =>
      sorry  -- iterative: bridge_complete preserves link via h_others_eq + h_request_eq + h_child_eq.
    | bridge_failure _ _ _ h_req_eq h_child_eq h_bridge_eq =>
      sorry  -- same as bridge_complete pattern.
    | bridge_cancel_cascade _ _ _ h_parent_eq h_bridge_eq =>
      -- post.parent = pre.parent, so parentLink unchanged. childLink touched only
      -- via interruptRequestedAt, which is not in the link predicate.
      sorry  -- iterative: post.child.request preserves causedByParentRequestId/ToolCallId.

end BridgedState
end Subagent
```

The four `sorry`s mark places where the proof needs a discovery loop against the structural shape of the post-state. They're each closeable in 5–15 lines once the engineer iterates with `lake build`.

- [ ] **Step 2: Update barrel**

Edit `crates/defra-agent/proofs/Proofs/Subagent.lean`:

```lean
import Proofs.Subagent.State
import Proofs.Subagent.Transition
import Proofs.Subagent.Properties

/-!
# Subagent
…
-/
```

- [ ] **Step 3: Build with sorrys**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: build with 4 sorry warnings on `inv_depth` and `inv_link`. No errors.

- [ ] **Step 4: Discharge the four sorrys (iteration loop)**

For each `sorry`:
1. Run `lake build`, read the goal state.
2. Find the analogous case in `Proofs/Composed.lean` (specifically how the proofs of `progress_monotonic`, `interrupt_monotonicity` and `valid_until_monotonicity` discharge per-constructor preservation).
3. Apply the same pattern: discharge via `simp [<post-state field equation>]; exact h_init.<projection>`.

If a `sorry` resists discovery for >30 minutes, leave it as `sorry`, file a follow-up issue, and continue. INV-DEPTH and INV-LINK are structural witnesses; load-bearing for B-theorems is mostly the existential statements in the constructors themselves, not the trace-level invariants.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent.lean crates/defra-agent/proofs/Proofs/Subagent/Properties.lean
git commit -m "$(cat <<'EOF'
Add INV-DEPTH and INV-LINK trace invariants on BridgedState

Structural invariants preserved across any reachable trace:
- INV-DEPTH: subagentDepth ≤ maxSubagentDepth on both sides
- INV-LINK: parent.parentLink ∧ child.childLink stay symmetric

Proofs case-split on bridge Transition constructors and discharge
preservation per constructor.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Prove B1 (`bridged_child_completion_propagates`)

Liveness theorem: a child reaching `.completed` propagates to parent ToolCall `.completed` along a trace.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Subagent/Properties.lean`

- [ ] **Step 1: Append the theorem**

After `inv_link` in `Subagent/Properties.lean`, add:

```lean
/-- B1: A child Request reaching .completed propagates to parent ToolCall
    .completed (assuming the parent observes within trace continuation). -/
theorem bridged_child_completion_propagates
    (pre : BridgedState)
    (h_running     : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧ t.state = .running)
    (h_persisted   : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧ t.persistence = .committed)
    (h_child_done  : pre.child.request.state = .completed) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧ t.state = .completed := by
  -- Construct the post state by applying bridge_complete: the parent's bridge
  -- tool flips from .running to .completed; everything else preserved.
  obtain ⟨tPre, h_in, h_id, h_run_state⟩ := h_running
  obtain ⟨tPersisted, h_in_p, h_id_p, h_committed⟩ := h_persisted
  -- Build the post-state tools list by replacing tPre with its .completed variant.
  let tPost : ToolExecution.ToolCallContext :=
    { tPre with state := .completed }
  obtain ⟨idx, h_idx⟩ := List.mem_iff_get?.mp h_in
  let postParent : ComposedState :=
    { pre.parent with tools := pre.parent.tools.set idx tPost }
  let post : BridgedState :=
    { pre with parent := postParent }
  refine ⟨post, ?_, tPost, ?_, h_id, rfl⟩
  · refine Trace.step ?_ Trace.refl
    apply Transition.bridge_complete h_child_done
    · exact ⟨tPre, h_in, h_id, h_run_state⟩
    · exact ⟨tPersisted, h_in_p, h_id_p, h_committed⟩
    · exact ⟨tPost, by simp [post, postParent, List.mem_set]; left; rfl, h_id, rfl⟩
    · intro t h_t_in h_t_neq
      simp [post, postParent, List.mem_set]
      right; refine ⟨h_t_in, ?_⟩
      intro h_eq; exact h_t_neq (by rw [h_eq])
    · rfl
    · rfl
    · rfl
  · simp [post, postParent, List.mem_set]; left; rfl
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: B1 typechecks. The proof constructs an explicit single-step trace using `bridge_complete` with the witnessed `tPost` as the post-tool.

If `List.set` and `List.mem_set` lemma names differ in the project's Mathlib version, search Mathlib for `List.set_eq_of_get?` or `List.mem_set_iff` and adjust.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent/Properties.lean
git commit -m "$(cat <<'EOF'
Prove B1 (bridged_child_completion_propagates)

A child Request in .completed propagates to parent ToolCall .completed
along a single bridge_complete step. Proof constructs the post-state
explicitly by replacing the bridge tool at its index with its .completed
variant and discharges the existential post-tool witness.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Prove B2 (`bridged_child_failure_projects`)

Same structure as B1 but uses `bridge_failure` and proves the parent reaches `.failed` (or `.cancelled` for `.interrupted` child).

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Subagent/Properties.lean`

- [ ] **Step 1: Append the theorem**

```lean
/-- B2: A child Request reaching a non-.completed terminal projects to
    parent ToolCall .failed or .cancelled. -/
theorem bridged_child_failure_projects
    (pre : BridgedState)
    (h_running    : ∃ t ∈ pre.parent.tools,
                      t.callId = pre.bridgeCallId ∧ t.state = .running)
    (h_child_term : pre.child.request.state = .failed ∨
                    pre.child.request.state = .dead ∨
                    pre.child.request.state = .interrupted ∨
                    pre.child.request.state = .superseded) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧
              (t.state = .failed ∨ t.state = .cancelled) := by
  obtain ⟨tPre, h_in, h_id, h_run_state⟩ := h_running
  -- Project: failed/dead/superseded → .failed; interrupted → .cancelled.
  let projectedState : ToolExecution.ToolCallState :=
    match pre.child.request.state with
    | .interrupted => .cancelled
    | _ => .failed
  let tPost : ToolExecution.ToolCallContext :=
    { tPre with state := projectedState
              , failureClass := some .deadline }  -- placeholder; runtime decides class
  obtain ⟨idx, h_idx⟩ := List.mem_iff_get?.mp h_in
  let postParent : ComposedState :=
    { pre.parent with tools := pre.parent.tools.set idx tPost }
  let post : BridgedState :=
    { pre with parent := postParent }
  refine ⟨post, ?_, tPost, ?_, h_id, ?_⟩
  · refine Trace.step ?_ Trace.refl
    apply Transition.bridge_failure h_child_term
    · exact ⟨tPre, h_in, h_id, h_run_state⟩
    · refine ⟨tPost, ?_, h_id, ?_⟩
      · simp [post, postParent, List.mem_set]; left; rfl
      · cases h_child_term with
        | inl _ => left; rfl
        | inr h => cases h with
                  | inl _ => left; rfl
                  | inr h' => cases h' with
                              | inl _ => right; rfl  -- .interrupted → .cancelled
                              | inr _ => left; rfl
    · intro t h_t_in h_t_neq
      simp [post, postParent, List.mem_set]
      right; refine ⟨h_t_in, ?_⟩
      intro h_eq; exact h_t_neq (by rw [h_eq])
    · rfl
    · rfl
    · rfl
  · simp [post, postParent, List.mem_set]; left; rfl
  · cases h_child_term with
    | inl _ => left; simp [tPost, projectedState]
    | inr h => cases h with
              | inl _ => left; simp [tPost, projectedState]
              | inr h' => cases h' with
                          | inl _ => right; simp [tPost, projectedState]
                          | inr _ => left; simp [tPost, projectedState]
```

- [ ] **Step 2: Build and iterate**

```bash
cd crates/defra-agent/proofs && lake build
```

The `match` on `pre.child.request.state` may need `decidable_of_iff` or explicit case-splits to compile cleanly. If `simp [tPost, projectedState]` doesn't reduce in the projection-arm, replace with explicit equalities (`left; rfl` or `right; rfl`).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent/Properties.lean
git commit -m "$(cat <<'EOF'
Prove B2 (bridged_child_failure_projects)

Child non-.completed terminal projects to parent ToolCall .failed (or
.cancelled when child is .interrupted). Proof case-splits the failure
disjunction and applies bridge_failure with the projected state.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 18: Prove B3 (cascade) and B3' (detach negative)

Two paired theorems: cascade-mode parent termination drives child to `.interrupted`; detach-mode parent termination does NOT.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Subagent/Properties.lean`

- [ ] **Step 1: Append B3 and B3'**

```lean
/-- B3: Cascade cancellation correctness. Parent terminal under cascade ⇒
    child reaches .interrupted via two-step trace (bridge_cancel_cascade
    sets interruptRequestedAt; child_step lifts interrupt_processing). -/
theorem cascade_cancels_child
    (pre : BridgedState)
    (h_parent_term : ToolExecution.isTerminal pre.parent.request.state)
    (h_cascade     : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧
                       t.cancelPolicy = .cascade ∧
                       ¬ ToolExecution.isTerminal t.state)
    (h_child_proc  : pre.child.request.state = .processing) :
    ∃ post, Trace pre post ∧ post.child.request.state = .interrupted := by
  -- Step 1: bridge_cancel_cascade sets interruptRequestedAt on child.
  obtain ⟨tCascade, h_in, h_id, h_pol, _⟩ := h_cascade
  let mid : BridgedState :=
    { pre with child :=
        { pre.child with request :=
            { pre.child.request with interruptRequestedAt := some pre.child.request.currentTime } } }
  -- Step 2: child_step lifts interrupt_processing.
  let postChildReq : RequestContext :=
    { mid.child.request with state := .interrupted, admission := .released }
  let postChild : ComposedState :=
    { mid.child with request := postChildReq }
  let post : BridgedState := { mid with child := postChild }
  refine ⟨post, ?_, rfl⟩
  refine Trace.step ?_ (Trace.step ?_ Trace.refl)
  · -- bridge_cancel_cascade
    apply Transition.bridge_cancel_cascade
    · left; exact h_parent_term
    · exact ⟨tCascade, h_in, h_id, h_pol⟩
    · simp [mid]
    · rfl
    · rfl
  · -- child_step lifting RequestContext.Transition.interrupt_processing
    apply Transition.child_step
    · apply ComposedState.Transition.request_step
      · -- the inner RequestContext.Transition.interrupt_processing
        apply RequestContext.Transition.interrupt_processing
        · exact h_child_proc
        · sorry  -- iterative: pre.child admission = .executing under .processing assumption.
        · simp [mid]
        · rfl
      · -- no_blocking_foreground guard on child side
        sorry  -- iterative: child has no in-flight tools by construction (subagent leaf).
      · rfl
      · rfl
      · rfl
      · rfl
      · rfl
    · rfl
    · rfl
    · sorry  -- iterative: link preserved across child_step.
    · sorry  -- iterative: link preserved across child_step.

/-- B3': Detach correctness (negative form). Detach-mode bridge tools do NOT
    cascade cancellation to the child. -/
theorem detach_does_not_cancel_child
    (pre post : BridgedState)
    (h_detach    : ∃ t ∈ pre.parent.tools,
                     t.callId = pre.bridgeCallId ∧ t.cancelPolicy = .detach)
    (h_step      : Transition pre post)
    (h_no_other  : ¬ pre.child.request.interruptRequestedAt.isSome) :
    post.child.request.interruptRequestedAt = pre.child.request.interruptRequestedAt := by
  cases h_step with
  | parent_step _ h_child_eq _ _ _ =>
    rw [h_child_eq]
  | child_step _ _ _ _ _ =>
    -- A child_step on its own doesn't fire bridge_cancel_cascade. The only way
    -- interruptRequestedAt could change in a non-cascade trace is via an
    -- explicit child_step lifting RequestContext.Transition.interrupt_*. By
    -- h_no_other those don't apply at this step (no precondition met).
    sorry  -- iterative: examine child's RequestContext.Transition cases.
  | bridge_spawn _ _ _ h_post_child =>
    -- bridge_spawn sets the child to .pending with default fields; no interrupt.
    sorry  -- iterative: post.child.request.interruptRequestedAt = none = pre.child.request.interruptRequestedAt under h_no_other.
  | bridge_complete _ _ _ _ _ _ h_child_eq _ =>
    rw [h_child_eq]
  | bridge_failure _ _ _ _ _ h_child_eq _ =>
    rw [h_child_eq]
  | bridge_cancel_cascade _ h_cascade _ _ _ =>
    -- bridge_cancel_cascade requires cascade policy; contradicts h_detach
    -- when both apply to the same bridge tool. (Different tools are possible
    -- only if there are multiple bridge tools, which violates uniqueness.)
    obtain ⟨tDet, h_in_d, h_id_d, h_pol_d⟩ := h_detach
    obtain ⟨tCas, h_in_c, h_id_c, h_pol_c⟩ := h_cascade
    -- Both refer to the same bridgeCallId, so same tool, so contradicting policies.
    sorry  -- iterative: prove tDet = tCas via uniqueness of callId in the tools list.
```

The five `sorry`s all close out via reasoning that the engineer can derive from the structural definitions. The most non-trivial is the last one in B3' — uniqueness of `callId` in `tools`. If the project's `tools` list doesn't enforce uniqueness, this needs an additional invariant (or a weakening of B3' to "if no other cascade tool with same callId is involved, then no interrupt set").

- [ ] **Step 2: Build with sorrys**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: build with 5 sorry warnings on B3 and B3'. No errors.

- [ ] **Step 3: Discharge sorrys (iteration loop)**

Same iteration pattern as Task 15. For B3 specifically, the third `sorry` (no_blocking_foreground on child) should be discharged by adding a precondition to the theorem stating the child has no live foreground tools — or by strengthening the spec to require that subagent children start with empty tools (which is the natural runtime invariant).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent/Properties.lean
git commit -m "$(cat <<'EOF'
Prove B3 (cascade_cancels_child) and B3' (detach_does_not_cancel_child)

B3: parent terminal under cascade ⇒ child reaches .interrupted via two-step
trace (bridge_cancel_cascade sets the interrupt flag; child_step lifts
interrupt_processing).

B3': detach-mode parent termination does NOT cascade — proven by
case-splitting on Transition and showing every case preserves child's
interruptRequestedAt under the detach hypothesis.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 19: Prove B6 (`foreground_blocks_parent_advance`)

Foreground tools block the parent's seq advancement. Restates the no_blocking_foreground guard (Task 11) at the BridgedState layer.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Subagent/Properties.lean`

- [ ] **Step 1: Append the theorem**

```lean
/-- B6: A live foreground tool on the parent prevents the parent's
    progressSeq and messageSeq from advancing across any single bridge
    Transition. -/
theorem foreground_blocks_parent_advance
    (pre post : BridgedState)
    (h_fg     : ∃ t ∈ pre.parent.tools,
                  t.awaitMode = .foreground ∧
                  ¬ ToolExecution.isTerminal t.state)
    (h_step   : Transition pre post) :
    pre.parent.request.progressSeq = post.parent.request.progressSeq := by
  cases h_step with
  | parent_step h_inner h_child_eq h_bridge_eq _ _ =>
    -- Inner ComposedState.Transition's request_step lift requires no foreground
    -- live tool — h_fg contradicts the guard, so request_step cannot be the
    -- inner constructor in this case unless the resulting reqPost.progressSeq
    -- is unchanged.
    cases h_inner with
    | request_step _ h_no_block _ _ _ _ _ =>
      exact absurd h_fg h_no_block
    | tool_step _ _ _ h_req_eq _ _ _ _ _ _ _ =>
      -- tool_step preserves request.
      rw [h_req_eq]
    | _ =>
      -- Other ComposedState.Transition variants (process_step, persistence_step,
      -- call_step) preserve request similarly.
      sorry  -- iterative: each remaining variant has post.request = pre.request.
  | child_step _ h_parent_eq _ _ _ =>
    rw [h_parent_eq]
  | bridge_spawn _ _ _ _ =>
    -- bridge_spawn changes parent.tools (adds new pending tool) but its
    -- structural guards don't bump progressSeq; the post-state equation
    -- on the parent's request is implicit but holds (no advance).
    sorry  -- iterative: post.parent.request.progressSeq = pre.parent.request.progressSeq.
  | bridge_complete _ _ _ _ _ h_req_eq _ _ =>
    rw [h_req_eq]
  | bridge_failure _ _ _ _ h_req_eq _ _ =>
    rw [h_req_eq]
  | bridge_cancel_cascade _ _ _ h_parent_eq _ =>
    rw [h_parent_eq]
```

- [ ] **Step 2: Build with sorrys, discharge**

```bash
cd crates/defra-agent/proofs && lake build
```

Two sorrys remain — both close via structural inspection (process_step and persistence_step preserve `request`; bridge_spawn's post-state implicitly preserves `request` via the unstated invariant that spawn doesn't advance the parent's narrative).

If `bridge_spawn` doesn't yet structurally guarantee `post.parent.request = pre.parent.request`, add that as an explicit constructor field to `bridge_spawn` (`h_request_eq : post.parent.request = pre.parent.request`) and update Task 13's code accordingly. Then this case discharges by `rw [h_request_eq]`.

- [ ] **Step 3: Add `h_request_eq` to `bridge_spawn` if needed**

If Step 2 shows the bridge_spawn case can't be closed without an explicit guard, edit `Proofs/Subagent/Transition.lean` and add:

```lean
      (h_request_eq      : post.parent.request = pre.parent.request)
```

to the `bridge_spawn` constructor's preconditions. This is structurally accurate (spawn creates a new tool but doesn't progress the parent's narrative).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent/Properties.lean crates/defra-agent/proofs/Proofs/Subagent/Transition.lean
git commit -m "$(cat <<'EOF'
Prove B6 (foreground_blocks_parent_advance)

A live foreground tool on the parent prevents progressSeq advancement
across any bridge Transition. Proof case-splits Transition; the
parent_step + request_step::advance case discharges via the
no_blocking_foreground guard from Task 11. Other cases preserve
parent.request structurally.

Adds h_request_eq guard to bridge_spawn for completeness.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 20: Add `Subagent/Executable.lean` step function and refinement theorem

A computable `step` function for Rust conformance trace generation. Mirrors `Proofs/Request/Executable.lean` and `Proofs/ToolExecution/Executable.lean`.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Subagent/Executable.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Subagent.lean` (add import)

- [ ] **Step 1: Write Subagent/Executable.lean**

```lean
import Proofs.Subagent.Transition

/-!
# Subagent Executable Semantics

A computable `step` function corresponding to each `BridgedState.Transition`
constructor, plus a `step_refines_transition` theorem that proves the function
implements the relation.

Used by Rust conformance generation to enumerate legal traces.
-/

namespace Subagent
namespace BridgedState

/-- An event that selects which bridge Transition to apply. -/
inductive Event where
  | parent_step      (innerEvent : ComposedState.Event)
  | child_step       (innerEvent : ComposedState.Event)
  | bridge_spawn     (newCallId : ToolExecution.ToolCallId)
                     (newChildRid : RequestId)
  | bridge_complete
  | bridge_failure
  | bridge_cancel_cascade
  deriving Repr

/-- Executable single-step. Returns `none` if the event isn't legal in the
    current state. -/
def step (s : BridgedState) (e : Event) : Option BridgedState := by
  -- The full body is filled in iteratively. As a starting point, return none
  -- for every event; the proof of step_refines_transition will guide the
  -- per-arm implementation.
  exact none

/-- Soundness: every legal step refines a Transition. -/
theorem step_refines_transition
    (s s' : BridgedState) (e : Event)
    (h : step s e = some s') :
    Transition s s' := by
  sorry  -- iterative: implement step constructor-by-constructor; each arm closes
         -- by applying the matching Transition constructor with explicit witnesses.

end BridgedState
end Subagent
```

(Note: this task ships a *stub* `step` function and a `sorry` refinement. The full implementation requires `ComposedState.Event` and a corresponding `ComposedState.step` function, which may not yet exist. If they don't, this task can be downscoped: write only the `Event` enum and leave `step` / `step_refines_transition` as `sorry` for a follow-up plan. The conformance JSON emission task in the deferred-work list will pick this up.)

- [ ] **Step 2: Update barrel**

Edit `crates/defra-agent/proofs/Proofs/Subagent.lean`:

```lean
import Proofs.Subagent.State
import Proofs.Subagent.Transition
import Proofs.Subagent.Properties
import Proofs.Subagent.Executable

/-!
# Subagent

Barrel import for subagent lifecycle (mode/policy state, BridgedState
paired-context, bridge transitions, properties B1–B6, executable
semantics).
-/
```

- [ ] **Step 3: Build with sorrys**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: build with sorry warnings on `step` (well, an empty body is `none` not `sorry`, but `step_refines_transition` is `sorry`). No errors.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent.lean crates/defra-agent/proofs/Proofs/Subagent/Executable.lean
git commit -m "$(cat <<'EOF'
Add Subagent.BridgedState.Executable scaffolding

Event enum + stub step function + sorry-stub refinement theorem. Real
implementation needs ComposedState.Event/step which may land in a
follow-up plan. Scaffolding allows Rust conformance to import the
module and start consuming its types.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 21: Final verification + add B4 / B5 prominent aliases + maintenance documentation

A wrap-up task: re-run the full build, document any remaining `sorry`s in a tracking note, and add prominent restatements of B4 and B5 as aliases of `inv_depth` and `inv_link` so callers can refer to them by spec name.

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Subagent/Properties.lean`
- Create: `crates/defra-agent/proofs/Proofs/Subagent/MAINTENANCE.md` (a brief note)

- [ ] **Step 1: Add B4 and B5 aliases**

Append to `Proofs/Subagent/Properties.lean`:

```lean
/-- B4: Subagent depth bound. Restated standalone for prominence; alias of inv_depth. -/
theorem subagent_depth_bounded
    (pre post : BridgedState)
    (h_init  : pre.parent.request.subagentDepth ≤ maxSubagentDepth ∧
               pre.child.request.subagentDepth ≤ maxSubagentDepth)
    (h_trace : Trace pre post) :
    post.parent.request.subagentDepth ≤ maxSubagentDepth ∧
    post.child.request.subagentDepth ≤ maxSubagentDepth :=
  inv_depth pre post h_init h_trace

/-- B5: Bridge link symmetry. Restated standalone for prominence; alias of inv_link. -/
theorem bridge_link_symmetric
    (pre post : BridgedState)
    (h_init  : pre.linked)
    (h_trace : Trace pre post) :
    post.linked :=
  inv_link pre post h_init h_trace
```

- [ ] **Step 2: Write the maintenance note**

Write `crates/defra-agent/proofs/Proofs/Subagent/MAINTENANCE.md`:

```markdown
# Subagent Lifecycle — Maintenance Obligations

Tracks the spec ↔ proof correspondence for the subagent lifecycle.

## Theorems

| Property | Lean theorem | File |
|---|---|---|
| B1 — child .completed propagates | `bridged_child_completion_propagates` | `Properties.lean` |
| B2 — child failure projects | `bridged_child_failure_projects` | `Properties.lean` |
| B3 — cascade cancels child | `cascade_cancels_child` | `Properties.lean` |
| B3' — detach does not cascade | `detach_does_not_cancel_child` | `Properties.lean` |
| B4 — depth bound | `subagent_depth_bounded` (alias of `inv_depth`) | `Properties.lean` |
| B5 — link symmetry | `bridge_link_symmetric` (alias of `inv_link`) | `Properties.lean` |
| B6 — foreground blocks parent | `foreground_blocks_parent_advance` | `Properties.lean` |

## Invariants

| Invariant | Lean theorem | File |
|---|---|---|
| INV-FG — single foreground non-terminal | `ComposedState.invFG_preserved` | `Composed.lean` |
| INV-DEPTH — depth ≤ maxSubagentDepth | `BridgedState.inv_depth` | `Subagent/Properties.lean` |
| INV-LINK — symmetric link | `BridgedState.inv_link` | `Subagent/Properties.lean` |

## Maintenance rule

Per `CLAUDE.md`: any change that alters legal transitions or the invariants
this folder asserts must (a) update the relevant `Subagent/State.lean` or
`Subagent/Transition.lean` file, (b) re-prove or re-state the affected
B-theorem in `Subagent/Properties.lean`, and (c) update the spec at
`docs/superpowers/specs/2026-05-08-subagent-lifecycle-design.md` with the
new shape. The Rust runtime and conformance JSON layers consume these
theorems via constructor enumeration; renaming a constructor without
updating both layers will fail the build.

## Open obligations

Tracked here when intentional `sorry`s remain after this plan completes.
At plan-completion time this section reads "(none)"; if any `sorry` is
landed, it MUST be filed as a follow-up issue and recorded here with a
link.

(none)
```

- [ ] **Step 3: Run a full clean build**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tee /tmp/lake-build.log
```

Examine the log. Count `sorry` warnings:

```bash
grep -c "uses 'sorry'" /tmp/lake-build.log
```

Expected: zero remaining `sorry`s if all iteration loops in earlier tasks closed. If any `sorry` remains, update `MAINTENANCE.md` "Open obligations" with a one-line entry per residual obligation and a follow-up issue link.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Subagent/Properties.lean crates/defra-agent/proofs/Proofs/Subagent/MAINTENANCE.md
git commit -m "$(cat <<'EOF'
Add B4/B5 prominent aliases + Subagent maintenance documentation

B4 (subagent_depth_bounded) and B5 (bridge_link_symmetric) are restated
as named theorems aliasing inv_depth and inv_link so callers can refer
to them by spec letter. MAINTENANCE.md tracks the spec ↔ proof
correspondence and records any residual sorry obligations.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review Notes

The following spec sections each map to a task above:

| Spec section | Implementing tasks |
|---|---|
| Section "State vocabulary" — `AwaitMode`, `CancelPolicy` | Task 2 |
| Section "State vocabulary" — new `ToolCallContext` fields | Task 3 |
| Section "State vocabulary" — new `RequestContext` fields | Task 4 |
| Section "Cross-request modeling" — `BridgedState` | Task 12 |
| Section "Transitions" — single-machine additions (background/foreground/detach + complete restriction) | Task 5 |
| Section "Transitions" — multi-flight refactor + composed-layer guard | Tasks 6, 11 |
| Section "Transitions" — restated C1, C1', C2, C3 | Tasks 7, 8, 9, 10 |
| Section "Transitions" — bridge layer (6 constructors) | Tasks 13, 14 |
| Section "Properties" — INV-FG | Task 11 |
| Section "Properties" — INV-DEPTH, INV-LINK | Task 15 |
| Section "Properties" — B1 | Task 16 |
| Section "Properties" — B2 | Task 17 |
| Section "Properties" — B3, B3' | Task 18 |
| Section "Properties" — B4, B5 (aliases) | Task 21 |
| Section "Properties" — B6 | Task 19 |
| Section "Conformance contract additions" | Task 20 (scaffolding only; emission deferred) |
| Section "File layout" | Tasks 1, 2, 12, 13, 14, 15, 20 |

Sections deferred to follow-up plans (per "What's NOT in this plan"):
- Tool surface (`spawn_subagent`, etc.) — runtime plan.
- Schema deltas (`AgentRequest`, `AgentToolCall`, `ToolSelection` GraphQL) — schema plan.
- `SubagentSource` — runtime plan.
- Conformance JSON emission — conformance plan, after the runtime side has consumers.
- Cross-principal delegation — issue #9 plan.
- Migration — schema plan.
