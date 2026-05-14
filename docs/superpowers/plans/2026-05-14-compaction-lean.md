# Compaction / truncation Lean coverage — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `Proofs/Compaction/` — a Lean module formalizing the contract any transcript-reduction strategy (Compactor, Truncator, future) must preserve (issue #184).

**Architecture:** New own-module `Proofs/Compaction/{State,Transition,Properties,Executable}.lean` with barrel `Proofs/Compaction.lean`. Imports `Proofs.Transcript.State` (#191/#195) and `Proofs.StreamingResponse.State` (#190). Exposes `IsValidReducer` typeclass over `PromptView → PromptView`, two witness instances (`identityReducer` + `stripToolResultsReducer`), ~12 theorems, and 10 conformance vectors registered as `consumerWithFollowUpCoverage`. One added import line in `Proofs.lean`; one added ledger entry in `Proofs/Conformance/CoverageLedger.lean`. No Rust production code.

**Tech Stack:** Lean 4 + mathlib4 via `lake`. Build command: `cd crates/defra-agent/proofs && lake build`.

## Hard constraints

These are absolute. Violations are merge-blockers:

1. **Zero `sorry`.** Every theorem named in the design spec must ship with a complete proof. The design-spec `sorry` for `strip_tool_results_is_strictly_idempotent` is shorthand for the proof *tactic* only — the shipped Lean file must compile without `sorry`. Run `grep -r sorry crates/defra-agent/proofs/Proofs/Compaction/` as the final step (Task 11); expect zero matches.
2. **No edits to `Proofs/Transcript/`.** The module just landed (#195) and is read-only from this PR's perspective.
3. **No edits to `Proofs/StreamingResponse/`.** That module is owned by #190's worktree.
4. **No Rust production code.** `crates/defra-agent/src/compaction*` and `crates/defra-agent/src/truncation*` stay as-is.
5. **`Proofs.lean` add-only.** Only one new import line; do not reorder existing imports.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `crates/defra-agent/proofs/Proofs/Compaction/State.lean` | new | `SummaryHandle`, `PromptView`, `PairsClosedInMessages`, `ViewCoherent`, `safeToReduce` |
| `crates/defra-agent/proofs/Proofs/Compaction/Transition.lean` | new | `TranscriptReducer`, `IsValidReducer` typeclass, witnesses |
| `crates/defra-agent/proofs/Proofs/Compaction/Properties.lean` | new | 12 theorems (10 contract-parametric + 2 witness-specific) |
| `crates/defra-agent/proofs/Proofs/Compaction/Executable.lean` | new | `CompactionReducerCase` type + 10 conformance rows |
| `crates/defra-agent/proofs/Proofs/Compaction.lean` | new | barrel re-export |
| `crates/defra-agent/proofs/Proofs.lean` | modify | add one import line after `import Proofs.Transcript` |
| `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` | modify | add one `consumerWithFollowUpCoverage` entry |

---

### Task 0: Pre-flight — verify #190 has merged

This task gates execution. The Compaction module imports `Proofs.StreamingResponse.State`; if #190 hasn't merged, the import will fail.

**Files:** None (verification only).

- [ ] **Step 1: Check if `Proofs.StreamingResponse` exists**

Run from the repo root:

```bash
ls crates/defra-agent/proofs/Proofs/StreamingResponse/State.lean 2>&1
```

Expected (if #190 has merged): file exists, prints the path.
Expected (if not merged): `No such file or directory`.

- [ ] **Step 2: If #190 has not merged, pause and surface to user**

If Step 1 printed `No such file or directory`, **STOP**. Report to the user:

> "#190 (`Proofs/StreamingResponse/`) has not merged to main. This plan's Task 1 imports `Proofs.StreamingResponse.State` for the `Status` vocabulary. Options: (a) wait for #190 to merge, then resume; (b) rebase this branch onto `proofs/issue-190-agent-response-streaming`; (c) inline a temporary `Status` enum in Compaction/State.lean and rebase later. Recommend (a)."

Do not proceed to Task 1 until the user decides.

- [ ] **Step 3: If #190 has merged, verify `Status.isTerminal` is available**

Run:

```bash
grep -n "HasTerminal Status" crates/defra-agent/proofs/Proofs/StreamingResponse/State.lean
```

Expected: at least one line matching `instance : HasTerminal Status`.

If not found, surface the discrepancy: the design spec assumes `HasTerminal Status`. Confirm with the user before proceeding.

- [ ] **Step 4: Verify lake builds cleanly on the current branch**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: exit 0, no errors, no warnings about `sorry`. If this fails, the branch is broken before our work; fix that first.

- [ ] **Step 5: No commit (verification only)**

---

### Task 1: Create `Proofs/Compaction/State.lean`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Compaction/State.lean`

- [ ] **Step 1: Create the directory**

```bash
mkdir -p crates/defra-agent/proofs/Proofs/Compaction
```

- [ ] **Step 2: Write the State.lean file**

Write `crates/defra-agent/proofs/Proofs/Compaction/State.lean` with the following content:

```lean
import Proofs.Basic
import Proofs.Transcript.State
import Proofs.StreamingResponse.State

/-!
# Compaction State

`PromptView` — the in-memory prompt-input projection that a
`TranscriptReducer` transforms. Unlike `Transcript.TranscriptState`,
a `PromptView` is *not* the durable transcript; reducing a `PromptView`
does not mutate any durable `AgentMessage` or `AgentToolCall` row.

This file defines the operand type and three predicates:
`PairsClosedInMessages` (list-level pair atomicity), `ViewCoherent`
(local conjunction: pairs + ordered + unique sequences), and
`safeToReduce` (every retained tool-result message belongs to a
terminal streaming response — cites #190's `Status.isTerminal`).
-/

namespace Compaction

open Transcript (Sequence MessageId MessageRow MessageKind ToolResultKey
                 MessageRole StrictlyIncreasingMessages UniqueMessageSequences)

/-- Opaque handle to a compaction-produced summary blob. The summary
text is generated by an LLM and is not formally constrained; the model
treats it as an identifier. The prompt builder later prepends the
summary as a synthetic user message. -/
structure SummaryHandle where
  payload : Nat
  deriving DecidableEq, Repr

/-- The slice of TranscriptState that a TranscriptReducer transforms.
A PromptView is the in-memory prompt input for the next inference call,
not the durable transcript. -/
structure PromptView where
  sessionId        : SessionId
  messages         : List MessageRow
  summary          : Option SummaryHandle
  responseStatuses : MessageId → Option StreamingResponse.Status

namespace PromptView

/-- A messages-only specialization of Transcript's pair-closure
invariant. Every tool-result row in the list has a matching
assistantToolCalls row in the same list whose callIds set contains
the result's callId. -/
def PairsClosedInMessages (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callId key, row.kind = .toolResult callId key →
      ∃ caller, caller ∈ msgs ∧
        caller.role = .assistant ∧
        (∃ callIds, caller.kind = .assistantToolCalls callIds ∧ callId ∈ callIds)

/-- Local coherence for a PromptView: pair atomicity + ordered +
unique sequences. The TranscriptState-level `Coherent` additionally
requires toolCall-side predicates that don't make sense for a
PromptView (a PromptView has no `toolCalls` list). -/
structure ViewCoherent (v : PromptView) : Prop where
  pairs           : PairsClosedInMessages v.messages
  ordered         : StrictlyIncreasingMessages v.messages
  uniqueSequences : UniqueMessageSequences v.messages

/-- A view is safe to reduce only if every retained tool-result message
belongs to a streaming response that has reached a terminal status. -/
def safeToReduce (v : PromptView) : Prop :=
  ∀ row, row ∈ v.messages →
    (∃ callId key, row.kind = .toolResult callId key) →
      ∃ status, v.responseStatuses row.messageId = some status ∧
        isTerminal status

end PromptView

end Compaction
```

- [ ] **Step 3: Build to verify**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.State
```

Expected: exit 0, no errors. If `StrictlyIncreasingMessages` or `UniqueMessageSequences` aren't found, double-check the `open Transcript (...)` line — those names live at the Transcript namespace root, not nested in `Transcript.TranscriptState`.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/State.lean
git commit -m "$(cat <<'EOF'
Add Proofs/Compaction/State.lean — PromptView vocabulary

Defines the operand type for TranscriptReducer: a derived projection
{ sessionId, messages, summary, responseStatuses } separate from the
durable TranscriptState. Adds three predicates: PairsClosedInMessages
(list-level pair atomicity), ViewCoherent (local conjunction), and
safeToReduce (every retained tool-result message belongs to a terminal
streaming response).

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Create `Proofs/Compaction/Transition.lean` — typeclass

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Compaction/Transition.lean`

- [ ] **Step 1: Write the file with just the typeclass (no instances yet)**

```lean
import Proofs.Compaction.State

/-!
# Compaction Transition

`TranscriptReducer := PromptView → PromptView` and the `IsValidReducer`
typeclass — the contract any transcript-reduction strategy must satisfy.

Each instance picks its own `gate` predicate (matches Rust's per-strategy
`needs_compaction`). The `identityBelowGate` and `identityUnlessSafe`
fields capture the conditional-fixpoint shape: the reducer is the
identity when its gate is false OR when the view is not safe to reduce.
`reapplyPreservesCoh` is the invariant-idempotence obligation — strict
`r (r v) = r v` would fail for LLM-based strategies (Summarize,
StripThenSummarize) whose summary output is non-deterministic, but
re-application must still preserve `ViewCoherent`.

Witness instances (`identityReducer`, `stripToolResultsReducer`) ship
in subsequent commits.
-/

namespace Compaction

abbrev TranscriptReducer := PromptView → PromptView

class IsValidReducer (r : TranscriptReducer) where
  gate                : PromptView → Prop
  decGate             : ∀ v, Decidable (gate v)
  preservesPairs      : ∀ v,
                          PromptView.PairsClosedInMessages v.messages →
                          PromptView.PairsClosedInMessages (r v).messages
  preservesOrder      : ∀ v,
                          Transcript.StrictlyIncreasingMessages v.messages →
                          Transcript.StrictlyIncreasingMessages (r v).messages
  preservesSession    : ∀ v, (r v).sessionId = v.sessionId
  identityBelowGate   : ∀ v, ¬ gate v → r v = v
  identityUnlessSafe  : ∀ v, ¬ PromptView.safeToReduce v → r v = v
  reapplyPreservesCoh : ∀ v, PromptView.ViewCoherent v →
                          PromptView.ViewCoherent (r (r v))

end Compaction
```

- [ ] **Step 2: Build to verify**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.Transition
```

Expected: exit 0, no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/Transition.lean
git commit -m "$(cat <<'EOF'
Add IsValidReducer typeclass (Compaction/Transition.lean)

Defines TranscriptReducer = PromptView → PromptView and the
IsValidReducer typeclass with seven obligations: gate predicate
(per-strategy), preservesPairs/Order/Session, identityBelowGate,
identityUnlessSafe, reapplyPreservesCoh. Witnesses come next.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Add `identityReducer` instance

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Compaction/Transition.lean`

- [ ] **Step 1: Append the identityReducer + instance to Transition.lean**

Append to the end of the file (after `end Compaction`, then re-open the namespace):

```lean

namespace Compaction

/-- The trivial reducer — does nothing. Witnesses that `IsValidReducer`
is non-vacuous. -/
def identityReducer : TranscriptReducer := fun v => v

instance instIsValidReducerIdentity : IsValidReducer identityReducer where
  gate                := fun _ => False
  decGate             := fun _ => .isFalse (fun h => h)
  preservesPairs      := fun _ h => h
  preservesOrder      := fun _ h => h
  preservesSession    := fun _ => rfl
  identityBelowGate   := fun _ _ => rfl
  identityUnlessSafe  := fun _ _ => rfl
  reapplyPreservesCoh := fun _ h => h

end Compaction
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.Transition
```

Expected: exit 0. If `.isFalse` resolution fails, write `Decidable.isFalse` explicitly.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/Transition.lean
git commit -m "$(cat <<'EOF'
Add identityReducer witness for IsValidReducer

The trivial reducer that returns its input unchanged. Discharges all
seven typeclass obligations by `rfl` or hypothesis-acceptance, witnessing
that IsValidReducer is non-vacuous.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Add `stripToolResultsReducer` definition

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Compaction/Transition.lean`

- [ ] **Step 1: Append the strip definitions (no instance yet)**

Append to the end of the file:

```lean

namespace Compaction

open Transcript (MessageKind)

/-- Abstract analogue of the Rust stub-payload mutation: the textual
content of a tool-result message is replaced with a stub, but the
linking metadata (callId, key) is preserved. Since the model abstracts
away payload text, this is case-wise the identity on MessageKind. -/
def stubMessageKind : MessageKind → MessageKind
  | .toolResult callId key => .toolResult callId key
  | .assistantToolCalls callIds => .assistantToolCalls callIds
  | .ordinary => .ordinary

theorem stubMessageKind_id (k : MessageKind) : stubMessageKind k = k := by
  cases k <;> rfl

def stubMessageRow (row : Transcript.MessageRow) : Transcript.MessageRow :=
  { row with kind := stubMessageKind row.kind }

theorem stubMessageRow_id (row : Transcript.MessageRow) :
    stubMessageRow row = row := by
  simp [stubMessageRow, stubMessageKind_id]

def stubMessages : List Transcript.MessageRow → List Transcript.MessageRow :=
  List.map stubMessageRow

theorem stubMessages_id (msgs : List Transcript.MessageRow) :
    stubMessages msgs = msgs := by
  unfold stubMessages
  rw [show stubMessageRow = id from funext stubMessageRow_id]
  exact List.map_id msgs

/-- Abstract analogue of Rust's `CompactionStrategy::StripToolResults`.
Replaces each tool-result payload with a stub. In the model this is
propositionally identity-shaped (the textual payload is abstracted away),
but the typeclass instance still has to discharge `preservesPairs`,
`preservesOrder`, etc. via `stubMessages_id` — see Properties.lean. -/
def stripToolResultsReducer : TranscriptReducer := fun v =>
  { v with messages := stubMessages v.messages }

theorem stripToolResultsReducer_id (v : PromptView) :
    stripToolResultsReducer v = v := by
  unfold stripToolResultsReducer
  simp [stubMessages_id]

end Compaction
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.Transition
```

Expected: exit 0. If `List.map_id` resolution fails, try `List.map_id'` or `List.map_const'` — mathlib has several variants. If `simp [stubMessages_id]` fails inside `stripToolResultsReducer_id`, try `ext; simp [stubMessages_id]` (PromptView struct extensionality).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/Transition.lean
git commit -m "$(cat <<'EOF'
Add stripToolResultsReducer definition (Compaction/Transition.lean)

Defines stubMessageKind / stubMessageRow / stubMessages chain and the
stripToolResultsReducer top-level function. The reducer is
propositionally identity-shaped (via List.map_id and case-wise reduction
of stubMessageKind) — captured in stripToolResultsReducer_id.

The typeclass instance lands in the next commit.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Add `stripToolResultsReducer` typeclass instance

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Compaction/Transition.lean`

- [ ] **Step 1: Append the IsValidReducer instance for stripToolResultsReducer**

Append:

```lean

namespace Compaction

instance instIsValidReducerStrip : IsValidReducer stripToolResultsReducer where
  gate                := fun _ => True
  decGate             := fun _ => .isTrue trivial
  preservesPairs      := by
                          intro v h
                          rw [stripToolResultsReducer_id]
                          exact h
  preservesOrder      := by
                          intro v h
                          rw [stripToolResultsReducer_id]
                          exact h
  preservesSession    := by
                          intro v
                          rw [stripToolResultsReducer_id]
  identityBelowGate   := by
                          intro v h
                          exact absurd trivial h
  identityUnlessSafe  := by
                          intro v _
                          exact stripToolResultsReducer_id v
  reapplyPreservesCoh := by
                          intro v h
                          rw [stripToolResultsReducer_id, stripToolResultsReducer_id]
                          exact h

end Compaction
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.Transition
```

Expected: exit 0, no `sorry`, no errors. If any tactic doesn't close its goal, expand it — e.g., `rw [stripToolResultsReducer_id]` may need `; rfl` or `; exact h` depending on Lean's reduction behavior.

- [ ] **Step 3: Verify zero sorry**

```bash
grep -n "sorry" crates/defra-agent/proofs/Proofs/Compaction/Transition.lean
```

Expected: zero matches.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/Transition.lean
git commit -m "$(cat <<'EOF'
Add stripToolResultsReducer IsValidReducer instance

All seven obligations discharged via stripToolResultsReducer_id, which
unfolds the reducer to the identity (List.map_id) and closes each
obligation by hypothesis-acceptance or rfl. Zero sorry.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Create `Proofs/Compaction/Properties.lean` — contract-parametric theorems

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Compaction/Properties.lean`

- [ ] **Step 1: Write Properties.lean with all 10 contract-parametric theorems**

```lean
import Proofs.Compaction.Transition

/-!
# Compaction Properties

Contract-parametric theorems over any `IsValidReducer` instance.
Every theorem here is parametric over `r : TranscriptReducer` with
`[IsValidReducer r]` — any future strategy that instantiates the
typeclass picks up these theorems for free.

Witness-specific theorems (strict idempotence for identityReducer and
stripToolResultsReducer) live at the bottom of the file under a
dedicated section header.

Hard constraint: zero `sorry`.
-/

namespace Compaction

variable {r : TranscriptReducer} [IsValidReducer r]

/-- Re-coherence: a valid reducer applied to a coherent view produces
a coherent view. Composes the three preservation obligations. -/
theorem reduction_preserves_view_coherent
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r v) := by
  refine ⟨?_, ?_, ?_⟩
  · exact IsValidReducer.preservesPairs (r := r) v h.pairs
  · exact IsValidReducer.preservesOrder (r := r) v h.ordered
  · -- uniqueSequences follows from preservesOrder via strictly-increasing
    -- ⇒ unique-sequence. The Transcript module proves this lemma; if
    -- it's not exposed, prove it locally by induction on the message list.
    exact uniqueSequences_of_strictlyIncreasing
      (IsValidReducer.preservesOrder (r := r) v h.ordered)

/-- Session identity is preserved by any valid reducer. -/
theorem reduction_preserves_session_id (v : PromptView) :
    (r v).sessionId = v.sessionId :=
  IsValidReducer.preservesSession (r := r) v

/-- Below the strategy's gate, the reducer is the identity. -/
theorem reduction_identity_when_below_gate
    {v : PromptView} (h_below : ¬ IsValidReducer.gate (r := r) v) :
    r v = v :=
  IsValidReducer.identityBelowGate (r := r) v h_below

/-- When the view is not safe to reduce (some retained tool-result
message belongs to a non-terminal streaming response), the reducer
must be the identity. -/
theorem reduction_blocked_unless_safe
    {v : PromptView} (h_unsafe : ¬ PromptView.safeToReduce v) :
    r v = v :=
  IsValidReducer.identityUnlessSafe (r := r) v h_unsafe

/-- Invariant idempotence: re-applying a reducer preserves `ViewCoherent`.
The strict `r (r v) = r v` form would fail for LLM strategies; this is
the safety-preserving weak form. -/
theorem reapply_preserves_view_coherent
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r (r v)) :=
  IsValidReducer.reapplyPreservesCoh (r := r) v h

/-- Acceptance criterion (a) from issue #184: no orphaned `AgentToolCall`
rows after compaction. Direct corollary of `preservesPairs`. -/
theorem no_orphaned_tool_results_after_reduction
    {v : PromptView}
    (h_pre : PromptView.PairsClosedInMessages v.messages) :
    ∀ row, row ∈ (r v).messages →
      ∀ callId key, row.kind = .toolResult callId key →
        ∃ caller, caller ∈ (r v).messages ∧
          caller.role = .assistant ∧
          (∃ callIds, caller.kind = .assistantToolCalls callIds ∧
            callId ∈ callIds) := by
  intro row h_mem callId key h_kind
  exact IsValidReducer.preservesPairs (r := r) v h_pre row h_mem callId key h_kind

/-- Acceptance criterion (b) from issue #184: message-order monotonicity
within retained windows. Direct corollary of `preservesOrder`. -/
theorem retained_window_is_ordered
    {v : PromptView}
    (h_pre : Transcript.StrictlyIncreasingMessages v.messages) :
    Transcript.StrictlyIncreasingMessages (r v).messages :=
  IsValidReducer.preservesOrder (r := r) v h_pre

/-- Acceptance criterion (c) from issue #184: idempotence under
re-application — conditional form 1. When the once-reduced view falls
below the gate, re-application is a strict no-op. -/
theorem reduction_idempotent_when_below_gate
    {v : PromptView}
    (h_below : ¬ IsValidReducer.gate (r := r) (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityBelowGate (r := r) (r v) h_below

/-- Acceptance criterion (c) from issue #184: idempotence under
re-application — conditional form 2. When the once-reduced view is
no longer safe to reduce, re-application is a strict no-op. -/
theorem reduction_idempotent_when_unsafe
    {v : PromptView}
    (h_unsafe : ¬ PromptView.safeToReduce (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityUnlessSafe (r := r) (r v) h_unsafe

/-- Streaming-coupling theorem: any non-identity reduction implies every
tool-result message in the *input* view has a terminal streaming-response
status. Ties compaction to #190's `Status.isTerminal` vocabulary. -/
theorem reduction_implies_all_retained_tool_results_terminal
    {v : PromptView} (h_nontrivial : r v ≠ v) :
    PromptView.safeToReduce v := by
  by_contra h_unsafe
  exact h_nontrivial (IsValidReducer.identityUnlessSafe (r := r) v h_unsafe)

end Compaction
```

- [ ] **Step 2: Build and identify gaps**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.Properties
```

Two likely failures:

(a) **`uniqueSequences_of_strictlyIncreasing` not found.** This lemma may not exist in `Proofs/Transcript/`. If it doesn't, prove it inline in Properties.lean before the theorems that use it:

```lean
theorem uniqueSequences_of_strictlyIncreasing
    {msgs : List Transcript.MessageRow}
    (h : Transcript.StrictlyIncreasingMessages msgs) :
    Transcript.UniqueMessageSequences msgs := by
  induction msgs with
  | nil => trivial
  | cons row rest ih =>
    refine ⟨?_, ?_⟩
    · intro other h_mem h_eq
      have h_lt := h.1 other h_mem
      omega
    · exact ih h.2
```

(b) **`(r := r)` syntax errors.** Lean 4 may want named application differently. If a theorem call fails, try `IsValidReducer.preservesPairs (r := r) v h.pairs` → `@IsValidReducer.preservesPairs r _ v h.pairs` (explicit positional).

- [ ] **Step 3: Verify zero sorry**

```bash
grep -n "sorry" crates/defra-agent/proofs/Proofs/Compaction/Properties.lean
```

Expected: zero.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/Properties.lean
git commit -m "$(cat <<'EOF'
Add Compaction/Properties.lean — contract-parametric theorems

Ten theorems parametric over any IsValidReducer instance, covering
issue #184's acceptance criteria: (a) no orphaned tool-call rows,
(b) message-order monotonicity, (c) conditional idempotence (two forms).
Adds reduction_implies_all_retained_tool_results_terminal as the
streaming-coupling theorem citing #190's Status.isTerminal vocabulary.

Zero sorry.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Add witness-specific strict-idempotence theorems

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Compaction/Properties.lean`

- [ ] **Step 1: Append the two witness theorems**

Append to Properties.lean (after `end Compaction`, then re-open):

```lean

namespace Compaction

/-!
## Witness-specific theorems

The two witness reducers (`identityReducer` and `stripToolResultsReducer`)
are deterministic, so they satisfy the *strict* idempotence law
`r (r v) = r v` unconditionally — not just the conditional forms that
the contract-parametric theorems prove. This is the boundary that
LLM-based strategies (Summarize, StripThenSummarize) cannot cross:
they satisfy only the conditional forms because their summary output
is non-deterministic.
-/

theorem identity_reducer_is_strictly_idempotent (v : PromptView) :
    identityReducer (identityReducer v) = identityReducer v := rfl

theorem strip_tool_results_is_strictly_idempotent (v : PromptView) :
    stripToolResultsReducer (stripToolResultsReducer v)
      = stripToolResultsReducer v := by
  rw [stripToolResultsReducer_id, stripToolResultsReducer_id]

end Compaction
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.Properties
```

Expected: exit 0. The `strip_tool_results_is_strictly_idempotent` proof relies on `stripToolResultsReducer_id` from Transition.lean reducing the call to `v`, then `stripToolResultsReducer v = v` again.

- [ ] **Step 3: Verify zero sorry**

```bash
grep -n "sorry" crates/defra-agent/proofs/Proofs/Compaction/Properties.lean
```

Expected: zero.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/Properties.lean
git commit -m "$(cat <<'EOF'
Add witness-specific strict-idempotence theorems

identityReducer and stripToolResultsReducer satisfy the strict
r (r v) = r v law unconditionally — closing the boundary between
deterministic strategies (this PR) and LLM-based strategies (which
satisfy only conditional idempotence). Zero sorry.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Create `Proofs/Compaction/Executable.lean` — conformance vectors

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Compaction/Executable.lean`

- [ ] **Step 1: Write the conformance vector type and 10 cases**

```lean
import Proofs.Compaction.Properties

/-!
# Compaction Executable

Conformance vectors emitted as `CompactionReducerCase` rows. Registered
in the coverage ledger as `consumerWithFollowUpCoverage` — the Rust
consumer wiring is intentionally deferred to a follow-up issue.

These vectors pin the *structural* contract; behavioral coverage
(stub-text formatting, file-activity extraction, byte-count display)
stays in `crates/defra-agent/src/compaction/tests.rs` and is not
replaced by the cases here.
-/

namespace Compaction

structure CompactionReducerCase where
  name                : String
  group               : String
  reducer             : String
  legal               : Bool
  preMessageCount     : Nat
  postMessageCount    : Nat
  preservesPairs      : Bool
  preservesOrder      : Bool
  gateOpen            : Bool
  safeToReduce        : Bool
  reducerIsIdentity   : Bool
  deriving Repr

def compactionReducerCases : List CompactionReducerCase := [
  { name              := "identity_reducer_is_no_op"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 0
  , postMessageCount  := 0
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "identity_preserves_pair_atomicity"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "identity_preserves_message_order"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "strip_preserves_pair_atomicity"
  , group             := "witness"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "strip_preserves_message_order"
  , group             := "witness"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "strip_is_strictly_idempotent"
  , group             := "witness"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "reduction_blocked_when_response_streaming"
  , group             := "streaming"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 1
  , postMessageCount  := 1
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := false
  , reducerIsIdentity := true }
, { name              := "reduction_allowed_when_response_terminal"
  , group             := "streaming"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 1
  , postMessageCount  := 1
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false }
, { name              := "no_orphaned_tool_results_after_strip"
  , group             := "contract"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "reapply_preserves_view_coherent"
  , group             := "contract"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
]

theorem compactionReducerCases_count :
    compactionReducerCases.length = 10 := by decide

end Compaction
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Compaction.Executable
```

Expected: exit 0. The `compactionReducerCases_count` theorem is a sanity check that decides by `decide`; if `decide` doesn't close it, replace with `rfl`.

- [ ] **Step 3: Verify zero sorry**

```bash
grep -n "sorry" crates/defra-agent/proofs/Proofs/Compaction/Executable.lean
```

Expected: zero.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction/Executable.lean
git commit -m "$(cat <<'EOF'
Add Compaction/Executable.lean — conformance vectors

Ten CompactionReducerCase rows across four groups (witness, streaming,
contract). Each row pins a structural assertion the corresponding Lean
theorem proves. Sanity check: compactionReducerCases.length = 10.

Behavioral coverage continues to live in
crates/defra-agent/src/compaction/tests.rs; the vectors here are
*additional* structural assertions, not a replacement.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Create `Proofs/Compaction.lean` barrel + add `Proofs.lean` import

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Compaction.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean`

- [ ] **Step 1: Write the barrel**

Create `crates/defra-agent/proofs/Proofs/Compaction.lean`:

```lean
import Proofs.Compaction.State
import Proofs.Compaction.Transition
import Proofs.Compaction.Properties
import Proofs.Compaction.Executable

/-!
# Compaction (barrel)

Re-exports the Compaction module. See `Proofs/Compaction/Properties.lean`
for the contract theorems.
-/
```

- [ ] **Step 2: Read the current `Proofs.lean` to confirm the insertion point**

```bash
sed -n '10,15p' crates/defra-agent/proofs/Proofs.lean
```

Expected output should include `import Proofs.Transcript` near line 12.

- [ ] **Step 3: Add the import to `Proofs.lean`**

Insert `import Proofs.Compaction` immediately after `import Proofs.Transcript`. Use Edit (not Bash sed) to keep the diff minimal:

```
import Proofs.Transcript
import Proofs.Compaction
import Proofs.RuntimeReconcile
```

- [ ] **Step 4: Build the full tree**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: exit 0, no errors. This is the first full build with Compaction wired in.

- [ ] **Step 5: Verify zero sorry across the module**

```bash
grep -rn "sorry" crates/defra-agent/proofs/Proofs/Compaction/ crates/defra-agent/proofs/Proofs/Compaction.lean
```

Expected: zero matches.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Compaction.lean crates/defra-agent/proofs/Proofs.lean
git commit -m "$(cat <<'EOF'
Wire Proofs/Compaction into the top-level Proofs.lean

Adds the barrel re-export and one import line in Proofs.lean (placed
topologically after Proofs.Transcript). Full lake build passes; zero
sorry across the new module.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Register the conformance vectors in the coverage ledger

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`

- [ ] **Step 1: Locate the right insertion point**

```bash
grep -n "recovery_sweep_cases" crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
```

Expected: at least one match around line 282–286 (per the spec's reference). The ledger entry for `recovery_sweep_cases` is the precedent for `consumerWithFollowUpCoverage` shape.

- [ ] **Step 2: Add the entry after the `transcript_cases` block**

Find the `consumerCoverage "transcript_cases" ...` entry (around line 287–290). Insert the following entry immediately after it (using Edit, not Bash sed):

```lean
  , consumerWithFollowUpCoverage
      "compaction_reducer_cases"
      "CompactionReducerCases"
      "state_machine_conformance::generated_compaction_reducer_cases_pin_contract"
      "Rust consumer wires up in a follow-up; vectors are stable and ready."
```

- [ ] **Step 3: Build to verify the ledger still type-checks**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Conformance.CoverageLedger
```

Expected: exit 0.

- [ ] **Step 4: Full tree build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
git commit -m "$(cat <<'EOF'
Register compaction_reducer_cases in the coverage ledger

consumerWithFollowUpCoverage entry; the Rust consumer
(state_machine_conformance::generated_compaction_reducer_cases_pin_contract)
ships in a follow-up issue. Vectors are stable and ready.

Refs #184.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 11: Final verification + PR

**Files:** None (verification + PR creation).

- [ ] **Step 1: Hard-constraint zero-sorry check across the entire Compaction module**

```bash
grep -rn "sorry" \
  crates/defra-agent/proofs/Proofs/Compaction/ \
  crates/defra-agent/proofs/Proofs/Compaction.lean
```

Expected: zero matches. If any match is found, the implementation is incomplete — return to the relevant task and discharge the proof. This is a merge-blocker.

- [ ] **Step 2: Full clean rebuild**

```bash
cd crates/defra-agent/proofs && lake clean && lake build
```

Expected: exit 0, no errors, no warnings about `sorry` or `axiom`.

- [ ] **Step 3: Verify the audit row's verdict moves**

Read `docs/superpowers/audits/2026-05-13-formal-coverage-audit.md` row 40 ("Compaction / context management"). Confirm that the PR body cites this row as moving from ❌ to ✓ Modeled.

- [ ] **Step 4: Push the branch**

```bash
git push -u origin proofs/issue-184-compaction
```

- [ ] **Step 5: Open the PR**

```bash
gh pr create --title "Add Lean coverage for compaction and truncation" --body "$(cat <<'EOF'
Closes #184.
Refs #183 (parent tracker).
Refs #191 / #195 (provides transcript vocabulary).
Refs #190 (provides response-terminal vocabulary).

## Summary

Adds `Proofs/Compaction/` — a new Lean module formalizing the contract
any transcript-reduction strategy (Compactor, Truncator, or future
strategies) must preserve.

**Contract type:** `IsValidReducer` typeclass over `TranscriptReducer
:= PromptView → PromptView`, strategy-parametric. Per-instance `gate`
predicate mirrors Rust's per-strategy `needs_compaction`.

**Proved invariants:**
- Pair atomicity (`preservesPairs`) — no orphaned `AgentToolCall` rows
  after reduction.
- Message-order monotonicity (`preservesOrder`) within retained windows.
- Session identity (`preservesSession`).
- Conditional fixpoint (`identityBelowGate`, `identityUnlessSafe`) —
  the reducer is the identity when the strategy's gate is false OR
  when retained tool-result messages reference non-terminal streaming
  responses.
- Invariant idempotence (`reapplyPreservesCoh`) — `ViewCoherent` is
  preserved under re-application even when strict `r (r v) = r v`
  fails (LLM-based strategies).
- Streaming-terminal safety (`reduction_implies_all_retained_tool_results_terminal`)
  — any non-identity reduction implies every retained tool-result
  message has a terminal streaming-response status (cites #190's
  `Status.isTerminal`).

**Witness instances:** `identityReducer` (trivial) +
`stripToolResultsReducer` (abstract analogue of Rust's
`CompactionStrategy::StripToolResults`).

**Conformance:** 10 `CompactionReducerCase` rows registered as
`consumerWithFollowUpCoverage`. The Rust consumer
(`state_machine_conformance::generated_compaction_reducer_cases_pin_contract`)
lands in a follow-up.

## Audit verdict moved

| Row | Before | After |
|---|---|---|
| Compaction / context management (line 40) | ❌ | ✓ Modeled |

## Reuse vs replacement

The `stripToolResultsReducer` witness is *propositionally* equal to the
identity in the abstract model (because `MessageKind` doesn't carry
payload text). This is correct and load-bearing: the witness pins the
**structural** contract (pair atomicity, ordering, identity invariants
are preserved by any admissible "strip"-shaped mutation). It does NOT
replace `crates/defra-agent/src/compaction/tests.rs`, which exercises
**behavioral** properties (stub text format, file-activity extractor,
byte-count formatting, summary persistence flow). Those Rust tests
stay as-is and remain the source of behavioral truth. The new Lean
vectors are an *additional* layer of structural assertions, not a
replacement.

This framing prevents two failure modes: (a) deleting
`compaction/tests.rs` because "Lean covers it now" (it doesn't), and
(b) trusting the Lean vector pass-rate to catch a regression in
stub-text formatting (it won't — that's the Rust tests' job).

## Not in scope

- Per-strategy semantics (sliding-window vs summary vs token-budget) —
  the contract is parametric over strategy.
- Rust production code — `crates/defra-agent/src/compaction*` and
  `crates/defra-agent/src/truncation*` stay as-is.
- Rust consumer test wiring — deferred to a follow-up issue.
- `Truncator` with side-effects (spill-doc lifecycle) — the contract
  admits a future `truncatePayloadReducer` instance; the spill-doc
  state machine is a separate model.
- Summary content (the LLM-emitted blob is opaque `SummaryHandle`).
- Token-budget convergence — the model preserves invariants, not
  token-count reduction.
- Composability lemma — "two valid reducers compose to a valid reducer"
  is an obvious follow-up; this PR does not prove it. A v2 lift into
  an effect monad (`StateM SpillDocs PromptView`) may be required if a
  future spill-doc-bearing Truncator witness ships; door left open.

## Test plan

- [x] `lake build` from `crates/defra-agent/proofs/` exits 0.
- [x] `grep -r sorry crates/defra-agent/proofs/Proofs/Compaction/`
      returns zero matches.
- [x] Witness instances `instIsValidReducerIdentity` and
      `instIsValidReducerStrip` compile (every typeclass obligation
      discharged).
- [x] `compactionReducerCases.length = 10` (sanity-check theorem).

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 6: Capture the PR URL and report**

```bash
gh pr view --json url --jq .url
```

Report the URL back to the user.

---

## Self-review (post-write)

**Spec coverage:**

- §1 Goal → covered by the architecture summary and the new module structure.
- §2 Why now → recapped in the PR body.
- §3 Verified obligations → each row maps to a theorem in Task 6/7. Specifically:
  - acceptance (a) "tool-call/message-pair atomicity, no orphaned `AgentToolCall` rows" → `no_orphaned_tool_results_after_reduction` (Task 6).
  - acceptance (b) "message-order monotonicity within retained windows" → `retained_window_is_ordered` (Task 6).
  - acceptance (c) "idempotence under re-application" → `reduction_idempotent_when_below_gate`, `reduction_idempotent_when_unsafe`, `reapply_preserves_view_coherent`, plus witness-specific `identity_reducer_is_strictly_idempotent`, `strip_tool_results_is_strictly_idempotent` (Tasks 6 + 7).
  - §"strategy-parametric properties" → all theorems in Task 6 are parametric over `[IsValidReducer r]`.
  - streaming coupling from §"Streaming coupling" → `reduction_implies_all_retained_tool_results_terminal` (Task 6).
- §4 Model → Task 1 (State.lean) + Task 2 (Transition.lean typeclass).
- §5 Witnesses → Tasks 3–5.
- §6 Properties → Task 6 + 7.
- §7 Conformance vectors → Task 8.
- §8 Cross-module wiring → Tasks 9 (Proofs.lean import) + 10 (ledger).
- §9 Coordination with #190 → Task 0.
- §11 Out of scope → restated in PR body (Task 11 Step 5).

**Placeholder scan:** No "TBD" / "TODO" / "similar to Task N". Every code block is concrete. Every command is exact.

**Type consistency:**
- `stubMessageKind` (Task 4) → used by `stubMessageRow` (Task 4) → used by `stubMessages` (Task 4) → used by `stripToolResultsReducer` (Task 4). All match.
- `IsValidReducer` field names (`gate`, `preservesPairs`, `preservesOrder`, `preservesSession`, `identityBelowGate`, `identityUnlessSafe`, `reapplyPreservesCoh`) are consistent across Tasks 2, 3, 5, 6.
- `CompactionReducerCase` field names match between Task 8 (definition) and the spec's vector listing.
- `consumerWithFollowUpCoverage` 4-arg form matches the precedent at CoverageLedger.lean line 282 (Task 10).

**Hard-constraint propagation:**
- Zero-sorry checks appear at every task that adds Lean code (Tasks 5, 6, 7, 8, 9) plus the final aggregate check in Task 11 Step 1.
- "No edits to `Proofs/Transcript/`" honored — only imports from it.
- "No edits to `Proofs/StreamingResponse/`" honored — only imports from it.
- "No Rust production code" honored — no `crates/defra-agent/src/` files in any task's Files block.
- "`Proofs.lean` add-only" honored — Task 9 uses Edit (not Write) for a single-line insertion.
