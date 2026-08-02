# Compaction Model + Production Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the vacuous identity-function compaction proofs with a `summarize` reducer over the production drop/replace policy, define one canonical `providerView` reduction that both the compaction writer and the request reader index, and fix the pair-splitting, prefix-accounting, missing-gate, and tool-stripping defects the model exposes.

**Architecture:** Lean first. `Proofs/Compaction` gains a real `strip`, a `providerView = sanitize ∘ strip` reduction with commutation and idempotence, prefix stability under append, and a `summarize` reducer parameterised over the token-budget split policy whose boundary is retreated to a turn boundary. Rust then exposes `compaction::provider_view` as the single narrowing both call sites use, makes `strip_tool_results` idempotent, makes `split_messages_for_summary` pair-safe, and gives `safeToReduce` a runtime counterpart.

**Tech Stack:** Lean 4 + Mathlib (`crates/gents/proofs`, `lake`), Rust (`crates/gents`), DefraDB via `defra_node::EmbeddedNode`, generated Lean→Rust conformance contract JSON.

**Spec:** `docs/superpowers/specs/2026-08-01-compaction-model-design.md`

## Global Constraints

- **Zero `sorry`s.** `cd crates/gents/proofs && lake build` must be clean. If a proof obligation cannot be discharged, stop and say so — do not ship a `sorry` or a plausible-looking reorder. This is provider-input assembly; a wrong fix corrupts context silently instead of failing loudly.
- **Gate with the full package suite**, not `--lib`: `cargo test -p gents`.
- **Compile the whole workspace before pushing**: `cargo check --workspace --all-targets`.
- **Always `graphql::escape_graphql_string()`** for anything interpolated into a GraphQL string.
- **Never emit `[]` in a DefraDB mutation** — emit `null`.
- `tracing`, never `println`.
- Lean proof *bodies* are developed against `lake build`; this plan pins the exact **statements** and names later tasks depend on, plus the existing lemma toolkit each proof draws from. Statements and names are contractual; tactic scripts are not.
- Every new Lean contract group needs a `CoverageLedger.lean` row **and** a matching entry in `crates/gents/tests/support/conformance_consumers.rs` in the same commit, or `lean_contract_coverage_ledger_accounts_for_every_emitted_domain` fails.

---

## File Structure

**Lean — created**

| File | Responsibility |
|------|----------------|
| `crates/gents/proofs/Proofs/Compaction/Strip.lean` | The real payload-stubbing `strip` and its shape lemmas |
| `crates/gents/proofs/Proofs/Compaction/ProviderView.lean` | `providerView = sanitize ∘ strip`, commutation, idempotence, soundness |
| `crates/gents/proofs/Proofs/Compaction/Prefix.lean` | `pendingAfter`, append/prefix stability, the compacted-prefix correspondence |
| `crates/gents/proofs/Proofs/Compaction/Summarize.lean` | `SplitPolicy`, `pairSafeBoundary`, the `summarize` reducer, its `IsValidReducer` instance, `raw_split_can_orphan` |

**Lean — modified**

| File | Change |
|------|--------|
| `Proofs/Compaction.lean` | Import the four new modules |
| `Proofs/Compaction/Transition.lean` | Retire `stubMessageKind`/`stripToolResultsReducer` and their identity instance; keep `IsValidReducer` and `identityReducer` |
| `Proofs/Compaction/Properties.lean` | Drop `strip_tool_results_is_strictly_idempotent` (moves to `Strip.lean` as a real theorem) |
| `Proofs/Compaction/Executable.lean` | Extend `CompactionReducerCase`; replace `strip_tool_results` rows; add `summarize` and `provider_view_prefix` rows |
| `Proofs/Conformance/Contracts/Json/ClientRuntime.lean` | Emit the new case fields |
| `Proofs/Conformance/CoverageLedger.lean` | Ledger rows for the new groups + the `safeToReduce` boundary |
| `Proofs/Conformance/Boundaries.lean` | `boundary.compaction.safe-to-reduce-session-scope` |

**Rust — modified**

| File | Change |
|------|--------|
| `crates/gents/src/compaction/history.rs` | Idempotent strip + stub format, tool classification, UTF-8 fix, scoped call map, `pair_safe_boundary`, pair-safe split |
| `crates/gents/src/compaction.rs` | `provider_view`, `safe_to_reduce`, `ResponseStatus`/`ResponseStatusIndex`, `compact()` normalizes its input |
| `crates/gents/src/agent/daemon/request.rs` | View-then-drop; session response-status resolver; gate the compaction call |
| `crates/gents/src/compaction/tests.rs` | New unit tests; update `integration_compaction_persists_entry_and_prompt_builder_uses_it` to the new space |
| `crates/gents/tests/conformance/streaming_compaction.rs` | Drive `summarize` + `provider_view_prefix` cases; call `safe_to_reduce` |
| `crates/gents/tests/support/conformance_consumers.rs` | Registry entries for the new consumers |

---

## Task 1: Lean — the real `strip`

Today `stubMessageKind` is `| .toolResult callId key => .toolResult callId key` and `stubMessageRow_id` proves it is `id`. That is why every compaction theorem is vacuous. Replace it with a strip that genuinely rewrites the payload while preserving everything `sanitize` inspects.

**Files:**
- Create: `crates/gents/proofs/Proofs/Compaction/Strip.lean`
- Modify: `crates/gents/proofs/Proofs/Compaction.lean`

**Interfaces:**
- Consumes: `Transcript.MessageRow`, `Transcript.MessageKind`, `Transcript.ToolResultKey`, `PromptAssembly.callsIn`, `PromptAssembly.resolvedIn`
- Produces: `Compaction.stubKey`, `Compaction.stripKind`, `Compaction.stripRow`, `Compaction.strip`, `Compaction.strip_idempotent`, `Compaction.strip_length`, `Compaction.callsIn_strip`, `Compaction.resolvedIn_strip`, `Compaction.strip_kind_ordinary`, `Compaction.strip_kind_assistant`, `Compaction.strip_kind_result`, `Compaction.strip_sequence`

- [ ] **Step 1: Write the module with statements**

```lean
import Proofs.Compaction.State
import Proofs.PromptAssembly.State

namespace Compaction

open Transcript (MessageRow MessageKind ToolResultKey)

/-- The canonical pointer payload a stripped tool result carries. Production
writes `[tool: NAME(ARG), call_id: ID, N bytes — see DefraDB AgentToolCall for
full output]`; the model abstracts that to a single canonical payload hash. -/
def stubKey (key : ToolResultKey) : ToolResultKey := { key with payloadHash := 0 }

@[simp] theorem stubKey_idempotent (key : ToolResultKey) :
    stubKey (stubKey key) = stubKey key := rfl

/-- Strip rewrites the *payload* of a tool result and nothing else. It never
changes a constructor and never changes a call id, which is exactly why it
commutes with `sanitize` (see `Proofs/Compaction/ProviderView.lean`). -/
def stripKind : MessageKind → MessageKind
  | .toolResult callId key => .toolResult callId (stubKey key)
  | k => k

def stripRow (row : MessageRow) : MessageRow := { row with kind := stripKind row.kind }

def strip (msgs : List MessageRow) : List MessageRow := msgs.map stripRow

theorem stripKind_idempotent (k : MessageKind) : stripKind (stripKind k) = stripKind k
theorem stripRow_idempotent (row : MessageRow) : stripRow (stripRow row) = stripRow row
theorem strip_idempotent (msgs : List MessageRow) : strip (strip msgs) = strip msgs
@[simp] theorem strip_length (msgs : List MessageRow) : (strip msgs).length = msgs.length
@[simp] theorem strip_nil : strip [] = []
@[simp] theorem strip_cons (row : MessageRow) (rest : List MessageRow) :
    strip (row :: rest) = stripRow row :: strip rest
theorem strip_append (a b : List MessageRow) : strip (a ++ b) = strip a ++ strip b
@[simp] theorem strip_sequence (row : MessageRow) : (stripRow row).sequence = row.sequence
@[simp] theorem strip_role (row : MessageRow) : (stripRow row).role = row.role

theorem strip_kind_ordinary (row : MessageRow) (h : row.kind = .ordinary) :
    (stripRow row).kind = .ordinary
theorem strip_kind_assistant (row : MessageRow) (callIds : Finset ToolExecution.ToolCallId)
    (h : row.kind = .assistantToolCalls callIds) :
    (stripRow row).kind = .assistantToolCalls callIds
theorem strip_kind_result (row : MessageRow) (callId : ToolExecution.ToolCallId)
    (key : ToolResultKey) (h : row.kind = .toolResult callId key) :
    (stripRow row).kind = .toolResult callId (stubKey key)

theorem callsIn_strip (l : List MessageRow) :
    PromptAssembly.callsIn (strip l) = PromptAssembly.callsIn l
theorem resolvedIn_strip (l : List MessageRow) :
    PromptAssembly.resolvedIn (strip l) = PromptAssembly.resolvedIn l

theorem strip_preserves_uniqueCallIds {l : List MessageRow} :
    PromptAssembly.UniqueCallIds l → PromptAssembly.UniqueCallIds (strip l)
theorem strip_preserves_strictlyIncreasing {l : List MessageRow} :
    Transcript.StrictlyIncreasingMessages l → Transcript.StrictlyIncreasingMessages (strip l)

end Compaction
```

- [ ] **Step 2: Add the import**

In `crates/gents/proofs/Proofs/Compaction.lean`, add `import Proofs.Compaction.Strip` as the second line (after `Proofs.Compaction.State`).

- [ ] **Step 3: Build and iterate the proof bodies**

Run: `cd crates/gents/proofs && lake build Proofs.Compaction.Strip`
Expected: clean, zero `sorry`.

Proof toolkit: everything is `List.map` over a `cases hk : row.kind` split. `callsIn_strip` / `resolvedIn_strip` mirror the induction in `PromptAssembly.Properties.callsIn_dropOrphanedFrom` using `callsIn_cons_*` / `resolvedIn_cons_*` rewritten through `strip_kind_*`.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/proofs/Proofs/Compaction/Strip.lean crates/gents/proofs/Proofs/Compaction.lean
git commit -m "proofs(compaction): model the real payload-stubbing strip

stubMessageKind was literally identity, which is why every compaction
theorem quantified over id. strip now rewrites the tool-result payload
while preserving constructors and call ids — the shape sanitize inspects.

Refs #993"
```

---

## Task 2: Lean — `providerView`, commutation, idempotence

Issue #993 flagged `strip ∘ sanitize = sanitize ∘ strip` as unproven and therefore blocking the obvious reorder. Prove it, then define the single reduction both production call sites will use.

**Files:**
- Create: `crates/gents/proofs/Proofs/Compaction/ProviderView.lean`
- Modify: `crates/gents/proofs/Proofs/Compaction.lean`

**Interfaces:**
- Consumes: Task 1's `strip` and shape lemmas; `PromptAssembly.sanitize`, `sanitize_sound`, `sanitize_idempotent`, `dropOrphanedFrom`, `filterCallsBy`, `withKind`
- Produces: `Compaction.providerView`, `Compaction.strip_dropOrphanedFrom`, `Compaction.strip_filterCallsBy`, `Compaction.strip_sanitize_commute`, `Compaction.providerView_idempotent`, `Compaction.providerView_sound`, `Compaction.providerView_nonempty_announcements`

- [ ] **Step 1: Write the module**

```lean
import Proofs.Compaction.Strip
import Proofs.PromptAssembly.Properties

namespace Compaction

open Transcript (MessageRow MessageKind)
open PromptAssembly (sanitize dropOrphanedFrom filterCallsBy resolvedIn callsIn
                     UniqueCallIds ProviderValid)

/-- `strip` commutes with the two sanitize stages because both branch only on
`row.kind`'s constructor and its call ids, which `strip` fixes. -/
theorem strip_dropOrphanedFrom (l : List MessageRow) :
    ∀ pending, strip (dropOrphanedFrom pending l) = dropOrphanedFrom pending (strip l)

theorem strip_filterCallsBy (l : List MessageRow) :
    ∀ resolved, strip (filterCallsBy resolved l) = filterCallsBy resolved (strip l)

/-- The theorem issue #993 named as the blocker for reordering the compacted
prefix drop past sanitization. -/
theorem strip_sanitize_commute (msgs : List MessageRow) :
    strip (sanitize msgs) = sanitize (strip msgs)

/-- The one canonical narrowing from the durable transcript to the provider
view. Both the compaction writer (`messages_compacted`) and the request reader
(`drop_compacted_prefix`) index *this* list. -/
def providerView (msgs : List MessageRow) : List MessageRow := sanitize (strip msgs)

theorem providerView_sound (msgs : List MessageRow) (huniq : UniqueCallIds msgs) :
    ProviderValid (providerView msgs)

theorem providerView_idempotent (msgs : List MessageRow) (huniq : UniqueCallIds msgs) :
    providerView (providerView msgs) = providerView msgs

theorem providerView_nonempty_announcements (msgs : List MessageRow) :
    PromptAssembly.NonemptyAnnouncements (providerView msgs)

end Compaction
```

- [ ] **Step 2: Add the import to the barrel, build, iterate**

Run: `cd crates/gents/proofs && lake build Proofs.Compaction.ProviderView`

Proof toolkit for `strip_filterCallsBy`: the assistant branch produces `withKind row (.assistantToolCalls (callIds ∩ resolved))`; discharge with `stripRow (withKind row (.assistantToolCalls S)) = withKind (stripRow row) (.assistantToolCalls S)` — `stripKind` fixes assistant kinds, so both sides are `{ row with kind := .assistantToolCalls S }`. `providerView_idempotent` chains `strip_sanitize_commute`, `strip_idempotent`, and the existing `PromptAssembly.sanitize_idempotent` (which needs `UniqueCallIds`, supplied by `strip_preserves_uniqueCallIds`).

- [ ] **Step 3: Commit**

```bash
git add crates/gents/proofs/Proofs/Compaction/ProviderView.lean crates/gents/proofs/Proofs/Compaction.lean
git commit -m "proofs(compaction): prove strip and sanitize commute; define providerView

Settles the question #993 raised as unproven, and settles it
affirmatively, so moving the compacted-prefix drop past sanitization is
licensed rather than assumed.

Refs #993"
```

---

## Task 3: Lean — prefix stability and the compacted-prefix correspondence

This is defect 3: the count is measured in one list and applied to another. The obligation is that `providerView` extends by append, so a count recorded against `providerView H` still names the same rows in `providerView (H ++ new)`.

**Files:**
- Create: `crates/gents/proofs/Proofs/Compaction/Prefix.lean`
- Modify: `crates/gents/proofs/Proofs/Compaction.lean`

**Interfaces:**
- Consumes: Task 2's `providerView`, `strip_sanitize_commute`; `PromptAssembly.filterCallsBy_irrelevant`, `callsIn_append`, `UniqueCallIds.of_append_right`
- Produces: `Compaction.pendingAfter`, `Compaction.dropOrphanedFrom_append`, `Compaction.providerView_append`, `Compaction.providerView_append_of_turn_boundary`, `Compaction.compacted_prefix_correspondence`, `Compaction.drop_preserves_providerValid`

- [ ] **Step 1: Write the module**

```lean
import Proofs.Compaction.ProviderView

namespace Compaction

open Transcript (MessageRow)
open PromptAssembly (sanitize dropOrphanedFrom filterCallsBy resolvedIn callsIn
                     UniqueCallIds ProviderValid ActiveBlockValidFrom)

/-- The pending-call set `dropOrphanedFrom` threads: an assistant announcement
replaces it, a matching tool result erases one id, anything else clears it. -/
def pendingAfter (pending : Finset ToolExecution.ToolCallId) :
    List MessageRow → Finset ToolExecution.ToolCallId
  | [] => pending
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ =>
        if callId ∈ pending then pendingAfter (pending.erase callId) rest
        else pendingAfter pending rest
    | .assistantToolCalls callIds => pendingAfter callIds rest
    | .ordinary => pendingAfter ∅ rest

theorem dropOrphanedFrom_append (a b : List MessageRow) (pending : Finset ToolExecution.ToolCallId) :
    dropOrphanedFrom pending (a ++ b) =
      dropOrphanedFrom pending a ++ dropOrphanedFrom (pendingAfter pending a) b

/-- General form: `providerView` extends by append whenever the suffix
contributes no tool result for a call announced in the prefix. -/
theorem providerView_append (a b : List MessageRow)
    (huniq : UniqueCallIds (strip a ++ strip b))
    (hclean : Disjoint
        (resolvedIn (dropOrphanedFrom (pendingAfter ∅ (strip a)) (strip b)))
        (callsIn a)) :
    providerView (a ++ b) =
      providerView a ++
        filterCallsBy (resolvedIn (dropOrphanedFrom (pendingAfter ∅ (strip a)) (strip b)))
          (dropOrphanedFrom (pendingAfter ∅ (strip a)) (strip b))

/-- Checkable sufficient condition: the prefix ends at a turn boundary. This is
what production satisfies — every new request appends its user prompt (an
ordinary row) before anything else, so no result in the suffix can attach to a
call in the prefix. -/
theorem providerView_append_of_turn_boundary (a b : List MessageRow)
    (huniq : UniqueCallIds (strip a ++ strip b))
    (hb : pendingAfter ∅ (strip a) = ∅) :
    ∃ tail, providerView (a ++ b) = providerView a ++ tail

/-- Dropping at a pending-empty index of a provider view leaves a provider
view. This is what makes it safe for `agent/daemon/request.rs` to drop the
compacted prefix *after* sanitization without re-sanitizing. -/
theorem drop_preserves_providerValid (msgs : List MessageRow) (n : Nat)
    (hvalid : ProviderValid msgs)
    (hboundary : pendingAfter ∅ (msgs.take n) = ∅) :
    ProviderValid (msgs.drop n)

/-- The correspondence the production fix rests on: the count the compaction
writer records against `providerView H` names exactly the rows the next
request's reader drops from `providerView (H ++ new)`. -/
theorem compacted_prefix_correspondence
    {H new dropped old recent tail : List MessageRow}
    (hstable : providerView (H ++ new) = providerView H ++ tail)
    (hsplit : providerView H = dropped ++ old ++ recent) :
    (providerView (H ++ new)).drop (dropped.length + old.length) = recent ++ tail

end Compaction
```

- [ ] **Step 2: Add the import, build, iterate**

Run: `cd crates/gents/proofs && lake build Proofs.Compaction.Prefix`

Proof toolkit: `dropOrphanedFrom_append` is a straight induction on `a` mirroring `dropOrphanedFrom_cons_*`. `providerView_append` then needs `filterCallsBy` to distribute over `++` (a one-line induction) plus the existing `PromptAssembly.filterCallsBy_irrelevant` to discharge `filterCallsBy (Rsuffix ∪ Rprefix) prefix = filterCallsBy Rprefix prefix` under `hclean`. `providerView_append_of_turn_boundary` derives `hclean` from `hb`: starting from `∅`, `dropOrphanedFrom` in the suffix keeps only results whose calls were announced inside the suffix, so `resolvedIn ⊆ callsIn b`, and `UniqueCallIds (a ++ b)` gives `Disjoint (callsIn a) (callsIn b)`. `compacted_prefix_correspondence` is `List.drop_append` arithmetic once `hstable` and `hsplit` are rewritten.

**If `providerView_append` resists:** stop, report which sub-obligation failed, and do not proceed to Task 9's reorder. The whole production fix is licensed by this theorem.

- [ ] **Step 3: Commit**

```bash
git add crates/gents/proofs/Proofs/Compaction/Prefix.lean crates/gents/proofs/Proofs/Compaction.lean
git commit -m "proofs(compaction): prove the compacted-prefix index correspondence

providerView extends by append at a turn boundary, so a count recorded
against providerView H names the same rows in providerView (H ++ new).
Defect 3 in #993 is that production measured in one space and dropped in
another; this is the theorem that makes measuring and dropping in one
space correct.

Refs #993"
```

---

## Task 4: Lean — `summarize` over the real policy

This is defects 1 and the modelled half of 2. `summarize` is parameterised over the token-budget split policy — the thing production actually computes — and retreats its boundary to a turn boundary so pair-closure holds.

**Files:**
- Create: `crates/gents/proofs/Proofs/Compaction/Summarize.lean`
- Modify: `crates/gents/proofs/Proofs/Compaction.lean`, `Proofs/Compaction/Transition.lean`, `Proofs/Compaction/Properties.lean`

**Interfaces:**
- Consumes: Task 3's `pendingAfter`, `drop_preserves_providerValid`; `Compaction.IsValidReducer`, `PromptView`, `PromptView.safeToReduce`, `PromptView.PairsClosedInMessages`
- Produces: `Compaction.SplitPolicy`, `Compaction.pairSafeBoundary`, `Compaction.pairSafeBoundary_le`, `Compaction.pairSafeBoundary_pending_empty`, `Compaction.summarize`, `Compaction.instIsValidReducerSummarize`, `Compaction.summarize_messages_suffix`, `Compaction.raw_split_can_orphan`

- [ ] **Step 1: Write the module**

```lean
import Proofs.Compaction.Prefix
import Proofs.Compaction.Transition

namespace Compaction

open Transcript (MessageRow)

/-- The token-budget index production computes in `split_messages_for_summary`.
The model is parameterised over it rather than pinning a token function: what
must be proven is that *whatever* index the budget picks, the reducer is sound. -/
abbrev SplitPolicy := List MessageRow → Nat

/-- The greatest `j ≤ limit` at which no tool call is awaiting its result.
Production mirrors this in `compaction::history::pair_safe_boundary`. -/
def pairSafeBoundary (msgs : List MessageRow) (limit : Nat) : Nat :=
  ((List.range (min limit msgs.length + 1)).filter
    (fun j => pendingAfter ∅ (msgs.take j) = ∅)).foldr Nat.max 0

theorem pairSafeBoundary_le (msgs : List MessageRow) (limit : Nat) :
    pairSafeBoundary msgs limit ≤ limit

theorem pairSafeBoundary_pending_empty (msgs : List MessageRow) (limit : Nat) :
    pendingAfter ∅ (msgs.take (pairSafeBoundary msgs limit)) = ∅

/-- The production reducer: retain the tail from the pair-safe boundary and
record a summary handle for everything dropped. -/
def summarize (policy : SplitPolicy) (handle : SummaryHandle) : TranscriptReducer :=
  fun v =>
    if PromptView.safeToReduce v ∧ 0 < pairSafeBoundary v.messages (policy v.messages) then
      { v with
        messages := v.messages.drop (pairSafeBoundary v.messages (policy v.messages))
        summary  := some handle }
    else v

theorem summarize_messages_suffix (policy : SplitPolicy) (handle : SummaryHandle)
    (v : PromptView) : (summarize policy handle v).messages <:+ v.messages

/-- Pair closure over the *real* reducer. The `ActiveBlockValid` hypothesis is
discharged in production by the input being a `providerView` — which is why
defect 1's fix depends on defect 3's. -/
theorem summarize_preserves_pairs (policy : SplitPolicy) (handle : SummaryHandle)
    (v : PromptView) (hblock : PromptAssembly.ActiveBlockValid v.messages) :
    PromptView.PairsClosedInMessages v.messages →
      PromptView.PairsClosedInMessages (summarize policy handle v).messages

instance instIsValidReducerSummarize (policy : SplitPolicy) (handle : SummaryHandle)
    (hblock : ∀ v : PromptView, PromptAssembly.ActiveBlockValid v.messages) :
    IsValidReducer (summarize policy handle)

/-- Without the boundary retreat the reducer is unsound: an unadjusted budget
index can land between an assistant tool-call row and its result row, leaving
the result orphaned in the retained tail. This is the counterexample that makes
`pairSafeBoundary` load-bearing rather than decoration. -/
theorem raw_split_can_orphan :
    ∃ (msgs : List MessageRow) (k : Nat),
      PromptAssembly.ActiveBlockValid msgs ∧
        ¬ PromptView.PairsClosedInMessages (msgs.drop k)

end Compaction
```

- [ ] **Step 2: Retire the vacuous reducer**

Delete from `Proofs/Compaction/Transition.lean` (lines 24–85): `stubMessageKind`, `stubMessageKind_id`, `stubMessageRow`, `stubMessageRow_id`, `stubMessages`, `stubMessages_id`, `stripToolResultsReducer`, `stripToolResultsReducer_id`, `instIsValidReducerStrip`. Keep the `IsValidReducer` class and the `identityReducer` namespace — `identityReducer` remains the legitimate degenerate below-gate reducer.

Delete from `Proofs/Compaction/Properties.lean` (lines 92–96): `strip_tool_results_is_strictly_idempotent`. Its honest replacement is `Compaction.strip_idempotent` from Task 1.

- [ ] **Step 3: Write the counterexample witness**

`raw_split_can_orphan` is discharged by a concrete two-row list — an assistant announcement followed by its result — split at `k = 1`:

```lean
theorem raw_split_can_orphan :
    ∃ (msgs : List MessageRow) (k : Nat),
      PromptAssembly.ActiveBlockValid msgs ∧
        ¬ PromptView.PairsClosedInMessages (msgs.drop k) := by
  refine ⟨[⟨0, 0, 0, .assistant, .assistantToolCalls {1}⟩,
           ⟨1, 0, 1, .user, .toolResult 1 ⟨0, 0, 0⟩⟩], 1, ?_, ?_⟩
  · -- ActiveBlockValid: announcement then its result
    simp [PromptAssembly.ActiveBlockValid, PromptAssembly.ActiveBlockValidFrom]
  · -- the retained tail holds the result with no caller
    intro h
    obtain ⟨caller, hmem, hrole, _⟩ := h _ (by simp) 1 ⟨0, 0, 0⟩ rfl
    simp at hmem
    subst hmem
    exact absurd hrole (by decide)
```

- [ ] **Step 4: Build and iterate**

Run: `cd crates/gents/proofs && lake build`
Expected: clean, zero `sorry`, and the whole `Proofs` library still builds after the Transition/Properties deletions.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/proofs/Proofs/Compaction/
git commit -m "proofs(compaction): model the real summarize reducer

Replaces the identity stripToolResultsReducer with a summarize reducer
parameterised over the production token-budget split policy, and proves
pair closure over it. raw_split_can_orphan is the counterexample showing
the unadjusted budget index is unsound, so the boundary retreat is
load-bearing.

Refs #993"
```

---

## Task 5: Lean — contract cases, ledger, boundary

**Files:**
- Modify: `crates/gents/proofs/Proofs/Compaction/Executable.lean`, `Proofs/Conformance/Contracts/Json/ClientRuntime.lean`, `Proofs/Conformance/CoverageLedger.lean`, `Proofs/Conformance/Boundaries.lean`

**Interfaces:**
- Produces: `CompactionReducerCase` with three new fields; case names `summarize_retains_straddling_turn`, `summarize_drops_whole_turns`, `summarize_blocked_when_response_streaming`, `provider_view_is_idempotent`, `provider_view_prefix_is_stable`; JSON keys `split_index`, `safe_boundary`, `retained_count`

- [ ] **Step 1: Extend the case structure**

In `Proofs/Compaction/Executable.lean`, add to `CompactionReducerCase` after `reducerIsIdentity`:

```lean
  splitIndex          : Nat
  safeBoundary        : Nat
  retainedCount       : Nat
```

Set `splitIndex := 0`, `safeBoundary := 0`, `retainedCount := postMessageCount` on the existing rows that do not exercise a split. Rename the three `strip_tool_results` rows' reducer string from `"strip_tool_results"` to `"strip"` and keep their names.

- [ ] **Step 2: Add the new rows**

Add to `compactionReducerCases`, computed from the model rather than hand-asserted:

```lean
, { name              := "summarize_retains_straddling_turn"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false
  , splitIndex        := 2
  , safeBoundary      := 1
  , retainedCount     := 2 }
, { name              := "summarize_drops_whole_turns"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false
  , splitIndex        := 1
  , safeBoundary      := 1
  , retainedCount     := 2 }
, { name              := "summarize_blocked_when_response_streaming"
  , group             := "summarize"
  , reducer           := "summarize"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := false
  , reducerIsIdentity := true
  , splitIndex        := 2
  , safeBoundary      := 1
  , retainedCount     := 3 }
, { name              := "provider_view_is_idempotent"
  , group             := "provider_view"
  , reducer           := "provider_view"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 0
  , retainedCount     := 3 }
, { name              := "provider_view_prefix_is_stable"
  , group             := "provider_view"
  , reducer           := "provider_view"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true
  , splitIndex        := 0
  , safeBoundary      := 1
  , retainedCount     := 3 }
```

Update `compactionReducerCases_count` to `= 15`.

- [ ] **Step 3: Emit the new fields**

In `Proofs/Conformance/Contracts/Json/ClientRuntime.lean`, in `compactionReducerCaseJson`, replace the trailing `reducer_is_identity` line with:

```lean
    ++ "\"reducer_is_identity\":" ++ boolString witness.reducerIsIdentity ++ ","
    ++ "\"split_index\":" ++ toString witness.splitIndex ++ ","
    ++ "\"safe_boundary\":" ++ toString witness.safeBoundary ++ ","
    ++ "\"retained_count\":" ++ toString witness.retainedCount
    ++ "}"
```

- [ ] **Step 4: Add the boundary**

In `Proofs/Conformance/Boundaries.lean`, add the id near the other boundary ids:

```lean
def boundaryCompactionSafeToReduceSessionScopeId : String :=
  "boundary.compaction.safe-to-reduce-session-scope"
```

and the entry in `boundaries`:

```lean
  , { id := boundaryCompactionSafeToReduceSessionScopeId
    , domain := "Compaction"
    , subject := "safeToReduce resolver scope"
    , statement :=
        "PromptView.safeToReduce requires every retained tool-result row to carry a known terminal response status. Rust resolves this at session scope: compaction::safe_to_reduce is the modelled predicate, and agent/daemon/request.rs backs it with a single non-terminal-AgentResponse query rather than per-message request_id linkage. All-terminal at session scope implies terminal for every row, so the refinement can only err toward unsafe, whose cost is a skipped compaction retried on the next request."
    , acceptedFailureMode :=
        some "A row whose request has no AgentResponse row at all (crashed run) reads unsafe in the model but safe under the session check. sanitize_history_for_provider already removes half-turns from crashed runs — an unpaired call or an orphaned result never reaches compaction's input — so what survives is a complete turn, which is safe to summarize."
    }
```

- [ ] **Step 5: Ledger rows**

In `Proofs/Conformance/CoverageLedger.lean`, next to the existing `compaction_reducer_cases` row, add:

```lean
  , tagged (boundaryCoverage
      "follow_up_hook"
      "Compaction.safeToReduce.sessionScopeResolver"
      boundaryCompactionSafeToReduceSessionScopeId)
      "compaction" [Surface.agentFacing]
```

- [ ] **Step 6: Build and verify JSON**

```bash
cd crates/gents/proofs && lake build Proofs.Conformance.Contracts \
  && lake env lean --run Proofs/Conformance/Contracts.lean | grep -o '"compaction_reducer_cases":\[[^]]*\]' | head -c 600
```
Expected: 15 objects, each carrying `split_index`, `safe_boundary`, `retained_count`.

- [ ] **Step 7: Commit**

```bash
git add crates/gents/proofs/Proofs/
git commit -m "proofs(compaction): emit summarize and provider-view contract cases

Refs #993"
```

---

## Task 6: Rust — idempotent strip with an argument-bearing stub

**Files:**
- Modify: `crates/gents/src/compaction/history.rs:169-306`
- Test: `crates/gents/src/compaction/tests.rs`

**Interfaces:**
- Produces: `history::tool_result_is_stub`, and the stub format `[tool: NAME(ARG), call_id: ID, N bytes — see DefraDB AgentToolCall for full output]`

- [ ] **Step 1: Write the failing tests**

Add to `crates/gents/src/compaction/tests.rs`:

```rust
#[test]
fn strip_is_idempotent_and_preserves_the_original_byte_count() {
    let long_result = "x".repeat(5000);
    let messages = vec![
        tool_call_msg("read_file", r#"{"path": "/tmp/test.rs"}"#),
        tool_result_msg("call-1", &long_result),
    ];

    let (once, _) = strip_tool_results(messages);
    let (twice, _) = strip_tool_results(once.clone());

    assert_eq!(once, twice, "strip must be idempotent");
    let stub = sole_tool_result_text(&twice[1]);
    assert!(
        stub.contains("5000 bytes"),
        "reapplying strip must not re-measure the stub: {stub}"
    );
}

#[test]
fn strip_stub_carries_the_primary_argument() {
    let messages = vec![
        tool_call_msg("read_file", r#"{"path": "/src/compaction/history.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];

    let (stripped, _) = strip_tool_results(messages);
    assert_eq!(
        sole_tool_result_text(&stripped[1]),
        "[tool: read_file(/src/compaction/history.rs), call_id: call-1, 12 bytes \
         — see DefraDB AgentToolCall for full output]"
    );
}

#[test]
fn strip_marks_already_truncated_output_without_sniffing_the_word() {
    let messages = vec![
        tool_call_msg("bash", r#"{"command": "echo hi"}"#),
        tool_result_msg("call-1", "the build log says truncated somewhere"),
    ];
    let (stripped, _) = strip_tool_results(messages);
    assert!(
        !sole_tool_result_text(&stripped[1]).contains(", truncated"),
        "ordinary output mentioning the word must not be flagged as truncated"
    );

    let messages = vec![
        tool_call_msg("bash", r#"{"command": "echo hi"}"#),
        tool_result_msg("call-1", "output\n[Full output: DefraDB doc bafy123]"),
    ];
    let (stripped, _) = strip_tool_results(messages);
    assert!(sole_tool_result_text(&stripped[1]).contains(", truncated"));
}

fn sole_tool_result_text(message: &Message) -> String {
    let Message::User { content } = message else {
        panic!("expected user message");
    };
    let UserContent::ToolResult(tool_result) = first_content(content) else {
        panic!("expected tool result");
    };
    let ToolResultContent::Text(text) = first_content(&tool_result.content) else {
        panic!("expected text content");
    };
    text.text.clone()
}
```

Also update the existing `strip_rewrites_tool_results_into_stubs` (line 612) expected string to the new argument-bearing format, and change its `tool_call_msg("read", ...)` to `tool_call_msg("read_file", r#"{"path": "/tmp/test.rs"}"#)`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents --lib compaction::tests::strip_ -- --nocapture`
Expected: FAIL — idempotence test shows the stub re-measured, argument test shows the old format.

- [ ] **Step 3: Implement**

In `crates/gents/src/compaction/history.rs`, replace `strip_tool_result`, `tool_result_was_truncated`, and `truncate_tool_result_content`:

```rust
/// Tail shared by every stub this module writes. Used to recognize a stub on
/// reapplication so `strip_tool_results` is idempotent: without it, a second
/// pass re-measures the stub and reports *its* length instead of the tool's,
/// which is what the model sees after a compaction.
const STUB_TAIL: &str = "see DefraDB AgentToolCall for full output]";
const STUB_HEAD: &str = "[tool: ";

/// Markers the truncation layer itself writes (`truncation::logic`,
/// `truncation::spill`). Matching these exactly replaces a `contains("truncated")`
/// sniff that fired on any tool output mentioning the word.
const TRUNCATION_MARKERS: [&str; 2] = ["[Full output: DefraDB doc ", "[Showing lines "];

fn tool_result_is_stub(tool_result: &ToolResult) -> bool {
    matches!(
        tool_result.content.as_slice(),
        [ToolResultContent::Text(text)]
            if text.text.starts_with(STUB_HEAD) && text.text.ends_with(STUB_TAIL)
    )
}

fn strip_tool_result(
    mut tool_result: ToolResult,
    tool_calls: &HashMap<String, ToolCallInfo>,
) -> ToolResult {
    if tool_result_is_stub(&tool_result) {
        return tool_result;
    }

    let call_id = tool_result_key(&tool_result);
    let info = tool_calls.get(&call_id);
    let tool_name = info.map_or("unknown", |info| info.name.as_str());
    let argument = info
        .and_then(|info| info.file_path.as_deref())
        .map(|path| format!("({path})"))
        .unwrap_or_default();
    let byte_count = tool_result_byte_count(&tool_result);
    let truncated = if tool_result_was_truncated(&tool_result) {
        ", truncated"
    } else {
        ""
    };

    let stub = format!(
        "[tool: {tool_name}{argument}, call_id: {call_id}, {byte_count} bytes{truncated} — {STUB_TAIL}"
    );
    tool_result.content = vec![ToolResultContent::Text(Text { text: stub })];
    tool_result
}

fn tool_result_was_truncated(tool_result: &ToolResult) -> bool {
    tool_result.content.iter().any(|content| match content {
        ToolResultContent::Text(text) => TRUNCATION_MARKERS
            .iter()
            .any(|marker| text.text.contains(marker)),
        _ => false,
    })
}

fn truncate_tool_result_content(content: ToolResultContent, max_chars: usize) -> ToolResultContent {
    match content {
        ToolResultContent::Text(text) if text.text.len() > max_chars => {
            // `max_chars` is a byte budget; slicing at a byte index that is not
            // a char boundary panics. Floor to the nearest boundary the way
            // toolset::edit_match does.
            let cut = floor_char_boundary(&text.text, max_chars);
            let truncated = format!(
                "{}… [pre-truncated {}/{} chars for compaction]",
                &text.text[..cut],
                cut,
                text.text.len()
            );
            ToolResultContent::Text(Text { text: truncated })
        }
        other => other,
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p gents --lib compaction::tests -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/src/compaction/history.rs crates/gents/src/compaction/tests.rs
git commit -m "fix(compaction): make strip idempotent and carry the tool argument

The path already stripped twice (request.rs, then again inside compact),
and the second pass re-stubbed the stub — so after a compaction the model
was told the stub's byte count, not the tool's. Recognize a stub and pass
it through. The stub now names the file it read, and the truncated flag
matches the truncation layer's own markers instead of sniffing for the
word 'truncated' in arbitrary output.

Refs #993"
```

---

## Task 7: Rust — tool classification and scoped call map

`ToolCallInfo::from` classifies against `"write" | "edit" | "replace" | "apply_patch"`; the real tools are `write_file` and `edit_file`, so `files_modified` has always been empty. `is_read` matches exactly one real tool, `grep`.

**Files:**
- Modify: `crates/gents/src/compaction/history.rs:10-51,340-361`
- Test: `crates/gents/src/compaction/tests.rs`

**Interfaces:**
- Produces: `history::is_read_tool`, `history::is_write_tool`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn file_activity_classifies_the_registered_file_tools() {
    let messages = vec![
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
        tool_call_msg("write_file", r#"{"path": "/src/lib.rs"}"#),
        tool_result_msg("call-1", "ok"),
        tool_call_msg("edit_file", r#"{"path": "/src/edit.rs"}"#),
        tool_result_msg("call-1", "ok"),
        tool_call_msg("grep", r#"{"path": "/src/grep.rs"}"#),
        tool_result_msg("call-1", "hit"),
        tool_call_msg("glob", r#"{"path": "/src/glob.rs"}"#),
        tool_result_msg("call-1", "hit"),
        tool_call_msg("list_files", r#"{"path": "/src/list.rs"}"#),
        tool_result_msg("call-1", "hit"),
    ];

    let (_, files) = strip_tool_results(messages);
    assert_eq!(
        files.files_read,
        vec!["/src/glob.rs", "/src/grep.rs", "/src/list.rs", "/src/main.rs"]
    );
    assert_eq!(files.files_modified, vec!["/src/edit.rs", "/src/lib.rs"]);
}

#[test]
fn every_registered_file_tool_is_classified() {
    // Guards against a file tool being added to toolset::file_tools without a
    // matching classification here, which would silently empty the compaction
    // summary's file lists — the defect this test exists to prevent recurring.
    for name in ["read_file", "list_files", "glob", "grep"] {
        assert!(super::history::is_read_tool(name), "{name} unclassified");
    }
    for name in ["write_file", "edit_file"] {
        assert!(super::history::is_write_tool(name), "{name} unclassified");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents --lib compaction::tests::file_activity_classifies -- --nocapture`
Expected: FAIL — `files_modified` is empty, `files_read` holds only `/src/grep.rs`.

- [ ] **Step 3: Implement**

Replace the `From<&ToolCall> for ToolCallInfo` impl at the bottom of `history.rs`:

```rust
/// Tools whose calls mean "this path was read". The first group is the
/// registered native file tools (`toolset/file_tools.rs`); the second is the
/// generic names MCP servers commonly use.
pub(super) fn is_read_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_files"
            | "glob"
            | "grep"
            | "read"
            | "cat"
            | "search"
            | "find"
            | "query"
    )
}

/// Tools whose calls mean "this path was modified".
pub(super) fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "write" | "edit" | "replace" | "apply_patch"
    )
}

impl From<&ToolCall> for ToolCallInfo {
    fn from(tool_call: &ToolCall) -> Self {
        let file_path = tool_call
            .function
            .arguments
            .get("file_path")
            .or_else(|| tool_call.function.arguments.get("path"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        let name = tool_call.function.name.clone();

        Self {
            is_read: is_read_tool(&name),
            is_write: is_write_tool(&name),
            name,
            file_path,
        }
    }
}
```

Then scope the call map in `strip_tool_results` so a reused call id from an earlier turn cannot label a later stub with the wrong tool name — clear it when an assistant message opens a new turn:

```rust
            Message::Assistant { id, content } => {
                // Scope the lookup to the turn being opened: call ids are
                // provider-generated and can repeat across turns, and a stale
                // entry would label a later stub with the wrong tool.
                tool_calls.clear();
                for item in content.iter() {
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p gents --lib compaction::tests -- --nocapture`
Expected: PASS. The pre-existing `strip_extracts_read_and_modified_files` (line 645) uses `"read"`/`"write"`, which the legacy aliases still cover, so it stays green.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/src/compaction/history.rs crates/gents/src/compaction/tests.rs
git commit -m "fix(compaction): classify the tools that actually exist

ToolCallInfo classified writes against write/edit/replace/apply_patch; the
registered tools are write_file and edit_file, so files_modified has always
been empty and only grep ever landed in files_read. Masked because the
summarizing model's own file lists were merged on top. Also scope the
call-id lookup per turn so a reused id cannot mislabel a later stub.

Refs #993"
```

---

## Task 8: Rust — pair-safe split boundary

**Files:**
- Modify: `crates/gents/src/compaction/history.rs:197-226`
- Test: `crates/gents/src/compaction/tests.rs`

**Interfaces:**
- Produces: `history::pair_safe_boundary(messages: &[Message], limit: usize) -> usize`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn split_never_separates_a_tool_call_from_its_result() {
    // A budget that retains roughly the last message would land the boundary
    // between the assistant tool call and the user tool result.
    let messages = vec![
        text_msg("user", &"a".repeat(4000)),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];

    let (old, recent) = super::history::split_messages_for_summary(messages, 40);

    assert_eq!(old.len(), 1, "only the bulky user turn should be summarized");
    assert_eq!(recent.len(), 2, "the assistant turn and its result stay together");
    assert!(
        matches!(&recent[0], Message::Assistant { .. }),
        "the retained tail must start at the assistant announcement"
    );
}

#[test]
fn pair_safe_boundary_retreats_to_the_turn_start() {
    let messages = vec![
        text_msg("user", "go"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];

    assert_eq!(super::history::pair_safe_boundary(&messages, 2), 1);
    assert_eq!(super::history::pair_safe_boundary(&messages, 3), 3);
    assert_eq!(super::history::pair_safe_boundary(&messages, 1), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents --lib compaction::tests::split_never_separates -- --nocapture`
Expected: FAIL — `recent.len()` is 1; the assistant call was summarized while its result stayed behind.

- [ ] **Step 3: Implement**

Add to `history.rs` and use it in `split_messages_for_summary`:

```rust
/// Greatest `j <= limit` at which no tool call is awaiting its result.
///
/// Mirrors `Compaction.pairSafeBoundary` and the pending-set discipline in
/// `drop_orphaned_tool_results`: an assistant message replaces the pending set
/// with its own call ids, a tool result erases one, and anything else clears it.
pub(super) fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    let limit = limit.min(messages.len());
    let mut pending: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut boundary = 0usize;

    for (index, message) in messages.iter().take(limit).enumerate() {
        if pending.is_empty() {
            boundary = index;
        }
        match message {
            Message::Assistant { content, .. } => {
                pending = content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(tool_call) => Some(tool_call_key(tool_call)),
                        _ => None,
                    })
                    .collect();
            }
            Message::User { content } => {
                let has_plain_content = content
                    .iter()
                    .any(|item| !matches!(item, UserContent::ToolResult(_)));
                for item in content.iter() {
                    if let UserContent::ToolResult(tool_result) = item {
                        pending.remove(&tool_result_key(tool_result));
                    }
                }
                if has_plain_content {
                    pending.clear();
                }
            }
            Message::System { .. } => pending.clear(),
        }
    }

    if pending.is_empty() {
        limit
    } else {
        boundary
    }
}
```

and in `split_messages_for_summary`, between the budget loop and the `split_index == 0` check:

```rust
    // The token budget can land the boundary between an assistant message
    // carrying a ToolCall and the user message carrying its ToolResult. Left
    // alone, the call is summarized away while the result stays in the retained
    // tail, and sanitize_history_for_provider then drops the orphaned result at
    // loop entry — the tool's output is lost from the provider view entirely
    // while the summary describes only the call.
    //
    // Retreat to the nearest turn boundary. Moving *earlier* over-retains by at
    // most one turn and never loses context; moving later would summarize a turn
    // the budget wanted kept. For provider-input assembly, over-retaining is the
    // correct failure direction. Modelled as `Compaction.pairSafeBoundary`, with
    // `Compaction.raw_split_can_orphan` witnessing that the unadjusted index is
    // unsound.
    let split_index = pair_safe_boundary(&messages, split_index);
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p gents --lib compaction::tests -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/src/compaction/history.rs crates/gents/src/compaction/tests.rs
git commit -m "fix(compaction): never split a tool call from its result

split_messages_for_summary cut on a token budget at an arbitrary index, so
the boundary could land between an assistant ToolCall and its ToolResult.
The call was summarized away, the result stayed in the retained tail, and
sanitize_history_for_provider dropped the orphan at loop entry — the tool
output vanished from the provider view while the summary described only
the call. Retreat the boundary to the nearest turn start.

Refs #993"
```

---

## Task 9: Rust — `provider_view` and the prefix-accounting fix

**Files:**
- Modify: `crates/gents/src/compaction.rs:92-204`, `crates/gents/src/agent/daemon/request.rs:49-76`
- Test: `crates/gents/src/compaction/tests.rs`

**Interfaces:**
- Consumes: Task 6's idempotent `strip_tool_results`, Task 8's `pair_safe_boundary`
- Produces: `compaction::provider_view(Vec<Message>) -> (Vec<Message>, FileActivity)`

- [ ] **Step 1: Write the failing regression test**

```rust
#[test]
fn compacted_prefix_is_counted_and_dropped_in_the_same_space() {
    // An orphaned tool result at the head: sanitize removes it, so the
    // unsanitized and sanitized indexings of the compacted prefix diverge.
    // Under the old order (strip -> drop -> sanitize) the count measured in the
    // sanitized space was applied to the unsanitized one, shifting the boundary.
    let history = vec![
        tool_result_msg("orphan-1", "result with no call"),
        text_msg("user", "first real turn"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
        text_msg("assistant", "done"),
        text_msg("user", "second turn"),
    ];

    let (view, _) = provider_view(history.clone());
    assert_eq!(
        view.len(),
        5,
        "sanitize must remove the orphaned result from the view"
    );

    // Compaction summarized the first two rows *of the view*.
    let compacted = 2usize;
    let retained = view.iter().skip(compacted).cloned().collect::<Vec<_>>();

    // The next request rebuilds the view from the same durable history and
    // drops the same count. It must land on exactly the retained rows.
    let (reread, _) = provider_view(history);
    assert_eq!(reread.into_iter().skip(compacted).collect::<Vec<_>>(), retained);
}

#[test]
fn provider_view_is_idempotent() {
    let history = vec![
        tool_result_msg("orphan-1", "result with no call"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];
    let (once, _) = provider_view(history);
    let (twice, _) = provider_view(once.clone());
    assert_eq!(once, twice);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents --lib compaction::tests::provider_view -- --nocapture`
Expected: FAIL with "cannot find function `provider_view`".

- [ ] **Step 3: Implement `provider_view`**

In `crates/gents/src/compaction.rs`, below `sanitize_history_for_provider`:

```rust
/// The single canonical narrowing from the durable transcript to the provider
/// view: stub tool-result payloads, then drop unpaired calls and orphaned
/// results and normalize assistant content order.
///
/// Both sides of compaction's prefix accounting index *this* list. The
/// compaction writer records `messages_compacted` against it and the request
/// reader drops that many rows from it; measuring in one space and dropping in
/// another is defect 3 of #993. Modelled as `Compaction.providerView`, proven
/// idempotent by `Compaction.providerView_idempotent` — which is what lets
/// `compact()` re-normalize its own input for free.
pub fn provider_view(messages: Vec<Message>) -> (Vec<Message>, FileActivity) {
    let (stripped, activity) = strip_tool_results(messages);
    (sanitize_history_for_provider(stripped), activity)
}
```

- [ ] **Step 4: Normalize `compact()`'s input**

Replace the strategy match at `compaction.rs:100-108` with:

```rust
        // Normalize to the canonical provider view so `messages_compacted`
        // indexes the same list `drop_compacted_prefix` will later index,
        // whoever the caller is. Idempotent, so this is a no-op when the caller
        // already passed a provider view — which the daemon always does.
        //
        // This makes `CompactionStrategy::Summarize` and `StripThenSummarize`
        // behave identically. They already did in the daemon path, which strips
        // unconditionally before calling here; the variant is retained for
        // config compatibility.
        let (stripped_messages, stripped_activity) = provider_view(messages);
```

- [ ] **Step 5: Rewire the daemon**

In `crates/gents/src/agent/daemon/request.rs`, replace lines 49-50 with:

```rust
                let (provider_history, file_activity) =
                    compaction::provider_view(full_history);
```

replace lines 72-76 with:

```rust
                // Drop in the same space the count was measured in. The drop
                // lands on a turn boundary (the writer's boundary is always
                // `pair_safe_boundary`), so the tail is still provider-valid
                // and needs no re-sanitizing —
                // `Compaction.drop_preserves_providerValid`.
                let mut history = drop_compacted_prefix(
                    provider_history,
                    total_compacted_messages(&compaction_entries),
                );
```

- [ ] **Step 6: Update the existing integration test**

`integration_compaction_persists_entry_and_prompt_builder_uses_it` (`compaction/tests.rs:678`) currently round-trips through `strip_tool_results` on both sides, which is the old space. Change both `let (…, _) = strip_tool_results(history);` sites to `let (…, _) = provider_view(history);`.

- [ ] **Step 7: Run the full package suite**

Run: `cargo test -p gents`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/gents/src/compaction.rs crates/gents/src/compaction/tests.rs crates/gents/src/agent/daemon/request.rs
git commit -m "fix(compaction): count and drop the compacted prefix in one space

messages_compacted indexed strip(sanitize(strip(H))) while
drop_compacted_prefix applied it to strip(H). Whenever sanitize removed
anything at or before the boundary the two diverged: either summarized
messages survived alongside their own summary, or messages that were
never summarized were silently dropped from the provider view.

Both call sites now go through compaction::provider_view. The reorder is
licensed by Compaction.strip_sanitize_commute and the drop needs no
re-sanitizing by Compaction.drop_preserves_providerValid.

Refs #993"
```

---

## Task 10: Rust — `safe_to_reduce` and its daemon resolver

**Files:**
- Modify: `crates/gents/src/compaction.rs`, `crates/gents/src/agent/daemon/request.rs`
- Test: `crates/gents/src/compaction/tests.rs`

**Interfaces:**
- Produces: `compaction::ResponseStatus` (`Streaming`/`Complete`/`Error`), `compaction::ResponseStatusIndex` trait with `fn status_of(&self, message: &Message) -> Option<ResponseStatus>`, `compaction::safe_to_reduce(&[Message], &impl ResponseStatusIndex) -> bool`, `compaction::AllTerminal`, `compaction::NoneKnown`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn safe_to_reduce_requires_every_retained_tool_result_to_be_terminal() {
    let messages = vec![
        text_msg("user", "go"),
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];

    assert!(safe_to_reduce(&messages, &AllTerminal));
    assert!(!safe_to_reduce(&messages, &NoneKnown));

    // No tool results at all: nothing to gate on.
    let plain = vec![text_msg("user", "go"), text_msg("assistant", "ok")];
    assert!(safe_to_reduce(&plain, &NoneKnown));
}

struct StreamingIndex;
impl ResponseStatusIndex for StreamingIndex {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        Some(ResponseStatus::Streaming)
    }
}

#[test]
fn safe_to_reduce_is_closed_while_a_response_is_streaming() {
    let messages = vec![
        tool_call_msg("read_file", r#"{"path": "/src/main.rs"}"#),
        tool_result_msg("call-1", "fn main() {}"),
    ];
    assert!(!safe_to_reduce(&messages, &StreamingIndex));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p gents --lib compaction::tests::safe_to_reduce -- --nocapture`
Expected: FAIL with "cannot find function `safe_to_reduce`".

- [ ] **Step 3: Implement the predicate**

In `crates/gents/src/compaction.rs`:

```rust
/// Mirror of Lean `StreamingResponse.Status`, with the same terminal partition,
/// so generated conformance cases can be fed straight in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    Streaming,
    Complete,
    Error,
}

impl ResponseStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Error)
    }

    pub fn from_defra(value: &str) -> Option<Self> {
        match value {
            "streaming" => Some(Self::Streaming),
            "complete" => Some(Self::Complete),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Resolves the streaming status of the response that produced a message.
pub trait ResponseStatusIndex {
    fn status_of(&self, message: &Message) -> Option<ResponseStatus>;
}

/// Every response in scope is terminal.
pub struct AllTerminal;
impl ResponseStatusIndex for AllTerminal {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        Some(ResponseStatus::Complete)
    }
}

/// No status is known — the conservative resolution when anything in scope is
/// still streaming.
pub struct NoneKnown;
impl ResponseStatusIndex for NoneKnown {
    fn status_of(&self, _message: &Message) -> Option<ResponseStatus> {
        None
    }
}

/// Runtime counterpart of Lean `PromptView.safeToReduce`: a transcript may only
/// be reduced when every tool result it retains belongs to a response whose
/// status is known and terminal. Reducing under a live response can summarize
/// away a turn that is still being written.
///
/// See `boundary.compaction.safe-to-reduce-session-scope` for how the daemon
/// resolves statuses at session scope rather than per message.
pub fn safe_to_reduce(messages: &[Message], statuses: &impl ResponseStatusIndex) -> bool {
    messages.iter().all(|message| {
        let carries_tool_result = matches!(message, Message::User { content }
            if content.iter().any(|item| matches!(item, crate::llm::message::UserContent::ToolResult(_))));
        if !carries_tool_result {
            return true;
        }
        statuses
            .status_of(message)
            .is_some_and(ResponseStatus::is_terminal)
    })
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p gents --lib compaction::tests::safe_to_reduce -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Wire the daemon resolver**

In `crates/gents/src/agent/daemon/request.rs`, add below `total_compacted_messages`:

```rust
/// Session-scope resolution of the modelled `safeToReduce` gate: if any
/// response in this session is still non-terminal, no status is treated as
/// known, so the gate closes. All-terminal at session scope implies terminal
/// for every row, so this can only err toward skipping a compaction — which the
/// next request retries. See `boundary.compaction.safe-to-reduce-session-scope`.
async fn session_has_live_response(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
) -> Result<bool> {
    let escaped_session_id = crate::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    status: {{ _eq: "streaming" }}
                }}
            ) {{
                response_key
            }}
        }}"#
    );
    let resp = crate::session::execute_query_timed(node, &query, "session_live_responses").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading live responses for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }
    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(|value| value.as_array())
        .is_some_and(|rows| !rows.is_empty()))
}
```

and gate the compaction call in `handle_request` — replace the `if prompt_exceeds_compaction_threshold(...) {` condition body's opening with a pre-check:

```rust
                if prompt_exceeds_compaction_threshold(
                    built.estimated_tokens,
                    &request.content,
                    self.behavior.context_window,
                    self.behavior.compaction_threshold,
                ) {
                    let live_response =
                        session_has_live_response(&self.node, &request.session_id).await?;
                    let gate_open = if live_response {
                        compaction::safe_to_reduce(&history, &compaction::NoneKnown)
                    } else {
                        compaction::safe_to_reduce(&history, &compaction::AllTerminal)
                    };
                    if !gate_open {
                        tracing::info!(
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            behavior_id = %behavior_name,
                            "compaction skipped: a response in this session is still streaming"
                        );
                    }
                    if gate_open {
```

with a matching closing brace before the existing `built = self.prompt_builder...` re-build (the re-build must still run so the prompt reflects whatever `history` currently is).

- [ ] **Step 6: Verify `execute_query_timed` visibility**

Run: `cargo check -p gents`
If `session::execute_query_timed` is not visible from `agent::daemon::request`, widen it to `pub(crate)` in `crates/gents/src/session/query.rs` and re-export from `session`.

- [ ] **Step 7: Run the full suite**

Run: `cargo test -p gents`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/gents/src/compaction.rs crates/gents/src/compaction/tests.rs crates/gents/src/agent/daemon/request.rs crates/gents/src/session/query.rs
git commit -m "feat(compaction): give safeToReduce a runtime counterpart

The gate existed only in Lean, and the conformance case reimplemented it
inside the test, so the test could not detect its absence from production.
compaction::safe_to_reduce is the modelled predicate; the daemon resolves
statuses at session scope and skips compaction while any response in the
session is still streaming.

Refs #993"
```

---

## Task 11: Rust — conformance test drives the new contract

**Files:**
- Modify: `crates/gents/tests/conformance/streaming_compaction.rs:1109-1246`, `crates/gents/src/lean_vocab_test/background_transcript.rs:326-339`, `crates/gents/tests/support/conformance_consumers.rs`

**Interfaces:**
- Consumes: Task 5's contract fields; Tasks 8/9/10's production functions

- [ ] **Step 1: Extend the case struct**

In `crates/gents/src/lean_vocab_test/background_transcript.rs`, add to `LeanCompactionReducerCase`:

```rust
    pub(crate) split_index: usize,
    pub(crate) safe_boundary: usize,
    pub(crate) retained_count: usize,
```

- [ ] **Step 2: Update the expected-name set and count**

In `streaming_compaction.rs`, change `assert_eq!(cases.len(), 10);` to `15` and add to `expected_names`:

```rust
        "summarize_retains_straddling_turn",
        "summarize_drops_whole_turns",
        "summarize_blocked_when_response_streaming",
        "provider_view_is_idempotent",
        "provider_view_prefix_is_stable",
```

- [ ] **Step 3: Replace the in-test gate with a production call**

Replace `apply_compaction_reducer` so no case branches on `case.safe_to_reduce` itself — the gate is now production's:

```rust
fn apply_compaction_reducer(
    case: &lean_vocab_test::LeanCompactionReducerCase,
    input: Vec<Message>,
) -> Vec<Message> {
    match case.reducer.as_str() {
        "identity" => input,
        "strip" => gents::compaction::strip_tool_results(input).0,
        "provider_view" => gents::compaction::provider_view(input).0,
        "summarize" => {
            // The gate is production's, not the test's: safe_to_reduce is the
            // function under test, fed the Lean case's status.
            let gate_open = if case.safe_to_reduce {
                gents::compaction::safe_to_reduce(&input, &gents::compaction::AllTerminal)
            } else {
                gents::compaction::safe_to_reduce(&input, &gents::compaction::NoneKnown)
            };
            assert_eq!(
                gate_open, case.safe_to_reduce,
                "{}: production safe_to_reduce must agree with the modelled gate",
                case.name
            );
            if !gate_open {
                return input;
            }
            let boundary = gents::compaction::pair_safe_boundary(&input, case.split_index);
            assert_eq!(
                boundary, case.safe_boundary,
                "{}: production pair_safe_boundary must match the modelled boundary",
                case.name
            );
            input.into_iter().skip(boundary).collect()
        }
        "any_valid" if case.safe_to_reduce => gents::compaction::strip_tool_results(input).0,
        "any_valid" => input,
        other => panic!("unsupported compaction reducer {other:?} for {}", case.name),
    }
}
```

Add a `retained_count` assertion in `drive_compaction_reducer_case` after the `post_message_count` check:

```rust
    assert_eq!(
        reduced.len(),
        case.retained_count,
        "{}: retained_count",
        case.name
    );
```

- [ ] **Step 4: Export `pair_safe_boundary`**

In `crates/gents/src/compaction.rs`, add:

```rust
/// Re-exported for the generated conformance cases, which check the production
/// boundary against `Compaction.pairSafeBoundary`.
pub fn pair_safe_boundary(messages: &[Message], limit: usize) -> usize {
    history::pair_safe_boundary(messages, limit)
}
```

- [ ] **Step 5: Run**

Run: `cargo test -p gents --test conformance generated_compaction_reducer_cases_pin_contract -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Run the ledger check**

Run: `cargo test -p gents lean_contract_coverage_ledger_accounts_for_every_emitted_domain -- --nocapture`
Expected: PASS. If it fails naming an unaccounted domain, add the matching `consumerCoverage` row in `CoverageLedger.lean` and the registry entry in `conformance_consumers.rs`.

- [ ] **Step 7: Commit**

```bash
git add crates/gents/tests/ crates/gents/src/lean_vocab_test/ crates/gents/src/compaction.rs
git commit -m "test(compaction): drive the summarize and provider-view contract from production

The streaming_compaction case implemented safeToReduce inside the test, so
it could not detect the gate's absence. It now calls
gents::compaction::safe_to_reduce and gents::compaction::pair_safe_boundary
and asserts they agree with the model.

Refs #993"
```

---

## Task 12: `run_timeline` round-trip after compaction

Compaction is a projection — it writes an `AgentCompactionEntry` and never mutates `AgentMessage` rows. Prove the timeline is unperturbed.

**Files:**
- Modify: `crates/gents/src/compaction/tests.rs`

- [ ] **Step 1: Write the test**

Append to `integration_compaction_persists_entry_and_prompt_builder_uses_it`, or add as a sibling using the same fixture setup:

```rust
#[tokio::test]
async fn compaction_leaves_the_persisted_timeline_untouched() {
    // Compaction is a projection: it writes a summary entry and drops a prefix
    // from the *provider view*. The durable AgentMessage rows the run timeline
    // reconstructs from must be byte-identical before and after.
    let (node, compactor) = compaction_fixture().await;
    seed_compactable_session(&node).await;

    let before = session::load_history(&node, "session-1").await.unwrap();

    let (view, _) = provider_view(before.clone());
    let result = compactor
        .compact(
            view,
            2000,
            &CompactionOptions {
                threshold: 0.50,
                keep_recent_tokens: 200,
                strategy: CompactionStrategy::StripThenSummarize,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    session::save_compaction_entry(
        &node,
        "session-1",
        "did:test:test",
        &result.summary.clone().unwrap(),
        &result.files_read,
        &result.files_modified,
        result.messages_compacted,
        result.original_token_estimate,
        result.compacted_token_estimate,
    )
    .await
    .unwrap();

    let after = session::load_history(&node, "session-1").await.unwrap();
    assert_eq!(
        before, after,
        "compaction must not mutate the durable transcript the timeline is built from"
    );
}
```

Extract `compaction_fixture()` and `seed_compactable_session()` from the existing integration test body (lines 678-760) so both tests share them rather than duplicating the seeding loop.

- [ ] **Step 2: Run**

Run: `cargo test -p gents --lib compaction::tests::compaction_leaves_the_persisted_timeline -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/gents/src/compaction/tests.rs
git commit -m "test(compaction): assert compaction leaves the durable transcript untouched

Refs #993"
```

---

## Task 13: Acceptance demonstration, gates, PR

- [ ] **Step 1: Demonstrate the pair-splitting regression fails**

Temporarily replace the boundary retreat in `history.rs` with `let split_index = split_index;` and comment out the `pairSafeBoundary` call inside Lean's `summarize`.

Run: `cargo test -p gents --lib compaction::tests::split_never_separates -- --nocapture`
Expected: FAIL.

Run: `cargo test -p gents --test conformance generated_compaction_reducer_cases_pin_contract -- --nocapture`
Expected: FAIL on `summarize_retains_straddling_turn`.

Run: `cd crates/gents/proofs && lake build`
Expected: FAIL — `summarize_preserves_pairs` no longer holds.

Record the three failure messages for the PR body, then `git checkout -- .` to restore.

- [ ] **Step 2: Run the gates**

```bash
cd crates/gents/proofs && lake build && cd -
grep -rn "sorry" crates/gents/proofs/Proofs/ | grep -v "sorry-free" || echo "no sorries"
cargo test -p gents
cargo check --workspace --all-targets
```
All must pass.

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin compaction-model
gh pr create --base main --title "Model the real compaction reducer and fix what it exposes (#993)" --body "..."
```

The PR body must carry: the four defects and their fixes; the split-direction decision and its rationale; the `safeToReduce` session-scope boundary and its accepted failure mode; the five tool-stripping fixes; the acceptance demonstration output from Step 1; and the deployment note that pre-existing `AgentCompactionEntry` counts were measured in `sanitize(drop(strip H))` space and are applied in `drop(sanitize(strip H))` space, which coincide unless sanitize removed a row before the boundary — the case already broken today.

---

## Self-Review

**Spec coverage**

| Spec section | Task |
|---|---|
| 1.1 real `strip` | 1 |
| 1.2 `providerView` + commutation + idempotence | 2 |
| 1.3 prefix stability + correspondence + `drop_preserves_providerValid` | 3 |
| 1.4 `summarize` + `pairSafeBoundary` + `IsValidReducer` + `raw_split_can_orphan` + retiring the vacuous reducer | 4 |
| 1.5 contract JSON + ledger + boundary | 5 |
| 2.1 `provider_view` + call-site rewiring | 9 |
| 2.2 idempotent strip | 6 |
| 2.3 pair-safe split | 8 |
| 2.4 `safe_to_reduce` + resolver | 10 |
| 2.5 (a) classification, (e) map scoping | 7 |
| 2.5 (b) UTF-8, (c) truncation markers, (d) stub argument | 6 |
| 3.1 pair-closure regression | 8 |
| 3.2 prefix regression | 9 |
| 3.3 strip idempotence | 6 |
| 3.4 `safe_to_reduce` tests + conformance calls production | 10, 11 |
| 3.5 tool classification test | 7 |
| 3.6 UTF-8 pre-truncation test | 6 |
| 3.7 `run_timeline` round-trip | 12 |
| Acceptance demonstration | 13 |
| Deployment note | 13 |

No gaps.

**Note on Task 6**: the UTF-8 test named in spec 3.6 is not written out in Task 6's test block. Adding it there now:

```rust
#[test]
fn pretruncation_does_not_panic_on_a_multibyte_boundary() {
    // "é" is two bytes; a 2001-byte payload puts byte 2000 inside a codepoint.
    let payload = format!("{}é{}", "a".repeat(1999), "b".repeat(500));
    let messages = vec![
        tool_call_msg("bash", r#"{"command": "cat notes"}"#),
        tool_result_msg("call-1", &payload),
    ];
    let truncated = super::history::pretruncate_tool_results(messages, 2000);
    assert!(sole_tool_result_text(&truncated[1]).contains("pre-truncated"));
}
```

**Type consistency**: `pair_safe_boundary` is `pub(super)` in `history.rs` (Task 8) and re-exported as `compaction::pair_safe_boundary` (Task 11) — the conformance test calls the re-export, `compaction/tests.rs` calls `super::history::pair_safe_boundary`. `provider_view` returns `(Vec<Message>, FileActivity)` in Task 9 and is destructured as such in Tasks 9, 11, 12. `ResponseStatusIndex::status_of` takes `&Message` and returns `Option<ResponseStatus>` consistently in Task 10 and Task 11. Lean `pendingAfter` takes the pending set first and the list second in Tasks 3 and 4.

**Placeholder scan**: no TBDs. Lean proof bodies are deliberately given as statements + toolkit, flagged in Global Constraints; every Rust step carries the actual code.
