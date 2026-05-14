# Compaction / truncation Lean design

Date: 2026-05-14
Issue: #184 (Refs #183 parent tracker, #191 / #195 provide transcript vocabulary, #190 provides response-terminal vocabulary)
Status: design — pending implementation plan

## Why

Gap #3 from the 2026-05-13 formal coverage audit (`docs/superpowers/audits/2026-05-13-formal-coverage-audit.md`, row line 40, "Compaction / context management"). The `Compactor` trait in `crates/defra-agent/src/compaction.rs` and the `Truncator` trait in `crates/defra-agent/src/truncation.rs` are explicit extension points; their behavior is asserted today only by `crates/defra-agent/src/compaction/tests.rs`.

Compaction is the change that ships most often, has the highest blast radius (transcript corruption — orphaned tool calls, reordered messages, dropped tool results), and currently has no formal backstop. Closing this gap lets new transcript-reduction strategies (sliding window, summary, token-budget-driven, structured, summary-with-citation, truncation-then-summarize) ship under proof: any new strategy that instantiates the contract is admissible by construction.

## What

A new Lean module `Proofs/Compaction/` (own module, not an extension of `Proofs/Transcript/`) containing:

- A `PromptView` projection type: the derived snapshot a `TranscriptReducer` consumes — `{ sessionId, messages, summary, responseStatuses }`. The durable `TranscriptState` (with `toolCalls`, `inFlight`, `nextSeq`, `assistantTurn`) is upstream and unchanged by reduction.
- A unified `TranscriptReducer := PromptView → PromptView` abbreviation and an `IsValidReducer` typeclass carrying the contract: pair atomicity, ordering monotonicity, session identity, conditional fixpoint (identity below the strategy's reduction gate), streaming-terminal safety (identity unless every retained tool-result message belongs to a terminal response), and invariant idempotence under re-application.
- Two witness instances proving the typeclass is inhabitable: `identityReducer` (trivial) and `stripToolResultsReducer` (the abstract analogue of Rust's `CompactionStrategy::StripToolResults` — `map` over messages stubbing tool-result payloads while preserving `MessageKind` tags).
- Conformance vectors emitted as `CompactionReducerCase` rows registered in the coverage ledger.

Zero `sorry`. No Rust production code; `crates/defra-agent/src/compaction*` and `crates/defra-agent/src/truncation*` stay as-is. The brief's hard constraints are respected: `Proofs/Transcript/*`, `Proofs/StreamingResponse/*` (once #190 merges), `Proofs/Properties/Safety.lean`, and `Proofs/Properties/Liveness.lean` are read-only.

### Truncator under the same contract

`Truncator` (`crates/defra-agent/src/truncation.rs`) is text-level — it bounds a single tool-result payload to `(max_lines, max_bytes)` with optional spill-doc side-effect. From the transcript's perspective, truncating a tool-result message's payload is a `map` over messages that preserves `MessageKind` tags (call_id and message-key are unchanged), preserves sequence, and is idempotent (truncating an already-truncated string is a no-op modulo the marker). Truncator therefore lifts to the same `IsValidReducer` contract: a future `truncatePayloadReducer` instance would discharge `preservesPairs`, `preservesOrder`, `preservesSession`, and `identityBelowGate` by the same `map`-preservation lemmas the `stripToolResultsReducer` uses. We do not ship a Truncator witness in this PR (the spill-doc side-effect would require a separate state-machine and isn't on the issue's critical path), but the contract admits it and the design documents the path.

## Where it lives

```
crates/defra-agent/proofs/Proofs/Compaction/
  State.lean         -- PromptView, ViewCoherent, PairsClosedInMessages, safeToReduce
  Transition.lean    -- TranscriptReducer, IsValidReducer typeclass, identityReducer
                     -- and stripToolResultsReducer definitions
  Properties.lean    -- the theorems
  Executable.lean    -- CompactionReducerCase rows
crates/defra-agent/proofs/Proofs/Compaction.lean    -- barrel
```

Touches outside the module:

- `Proofs.lean` — adds one line: `import Proofs.Compaction`. Placed topologically right after `import Proofs.Transcript` (current line 12). This is downstream of `Transcript` and `StreamingResponse`, so the placement follows the file's existing dependency order rather than strict alphabetic order. **Note for the #190 agent**: their import lands between `MCPHealth` and `Subagent`. Our import lands after `Transcript`. No conflict.
- `Proofs/Conformance/CoverageLedger.lean` — one new `consumerWithFollowUpCoverage` entry registering `compaction_reducer_cases`. Wiring the Rust consumer (`tests/state_machine_conformance.rs`) is intentionally deferred to a follow-up issue so this PR ships independent of Rust test churn.

Read-only (do not modify):
- `Proofs/Transcript/*` — vocabulary import only (`Sequence`, `MessageRow`, `MessageKind`, `ToolResultKey`, `MessageRole`, `StrictlyIncreasingMessages`).
- `Proofs/StreamingResponse/*` (once #190 merges) — vocabulary import only (`Status`, `Status.isTerminal`).

## State vocabulary (Proofs/Compaction/State.lean)

```lean
import Proofs.Transcript.State
import Proofs.StreamingResponse.State  -- coordinated with #190 merge

namespace Compaction

open Transcript (Sequence MessageId MessageRow MessageKind ToolResultKey
                 StrictlyIncreasingMessages)
open StreamingResponse (Status)

/-- Abstract handle to a compaction-produced summary blob. The summary is
prepended to the prompt as a synthetic user message by the prompt builder;
the model treats it as opaque. -/
structure SummaryHandle where
  payload : Nat  -- abstract; actual blob is opaque to the proof
  deriving DecidableEq, Repr

/-- The slice of TranscriptState that a TranscriptReducer transforms.
A PromptView is the in-memory prompt input for the next inference call,
not the durable transcript: reducing a PromptView does not mutate any
durable AgentToolCall or AgentMessage row. -/
structure PromptView where
  sessionId        : Transcript.SessionId
  messages         : List MessageRow
  summary          : Option SummaryHandle
  responseStatuses : MessageId → Option Status
  deriving DecidableEq

namespace PromptView

/-- A messages-only specialization of Transcript's pair-closure invariant.
Every tool-result row in the list has a matching assistantToolCalls row
in the same list whose callIds set contains the result's callId. -/
def PairsClosedInMessages (msgs : List MessageRow) : Prop :=
  ∀ row, row ∈ msgs →
    ∀ callId key, row.kind = .toolResult callId key →
      ∃ caller, caller ∈ msgs ∧
        caller.role = .assistant ∧
        (∃ callIds, caller.kind = .assistantToolCalls callIds ∧ callId ∈ callIds)

/-- Local coherence: pair atomicity + ordered + unique sequences. The
TranscriptState-level Coherent additionally requires toolCall-side
predicates that don't make sense for a PromptView. -/
structure ViewCoherent (v : PromptView) : Prop where
  pairs           : PairsClosedInMessages v.messages
  ordered         : StrictlyIncreasingMessages v.messages
  uniqueSequences : Transcript.UniqueMessageSequences v.messages

/-- A view is safe to reduce only if every retained tool-result message
belongs to a streaming response that has reached a terminal status
(completed or error). Cites StreamingResponse.Status.isTerminal. -/
def safeToReduce (v : PromptView) : Prop :=
  ∀ row, row ∈ v.messages →
    (∃ callId key, row.kind = .toolResult callId key) →
      ∃ status, v.responseStatuses row.messageId = some status ∧
        Status.isTerminal status

end PromptView

end Compaction
```

Shape decisions:

- **`PromptView` is its own type, not a wrapper around `TranscriptState`.** A PromptView is "what the next inference sees" — a derived projection. Modeling reduction on `TranscriptState` directly would conflate two layers: durable rows (which never change under compaction) and prompt input (which is the actual artifact). The bridge — projecting a TranscriptState into a PromptView and checking that the projection preserves Coherent on the messages list — is a separate concern not modeled in this PR.
- **`responseStatuses` is a function, not a Finset of `(MessageId, Status)` pairs.** Reducers need to look up the status of a row by id; a function carrier is the natural shape. A `Finset`-based encoding would be equally valid but heavier in proof.
- **`PairsClosedInMessages` is defined in this module**, not lifted from `Transcript.CompletedToolCallsPaired`/`ToolResultMessagesPaired`, because those predicates reference the `toolCalls` list which a PromptView doesn't carry. The list-level predicate is strictly weaker than the TranscriptState-level one — sufficient for the prompt-input contract.
- **`SummaryHandle` is opaque.** The model treats summary blobs as identifiers; the actual text is generated by an LLM and is not formally constrained. Strategies that produce summaries (Summarize, StripThenSummarize) carry summary handles through the reducer; strategies that don't (StripToolResults, Truncator-lifted) leave `summary = none`.

## The contract — IsValidReducer (Proofs/Compaction/Transition.lean)

```lean
import Proofs.Compaction.State

namespace Compaction

abbrev TranscriptReducer := PromptView → PromptView

/-- The contract any transcript-reduction strategy must satisfy. The
`gate` field is per-strategy (Rust's `needs_compaction`): each strategy
knows its own trigger condition. `identityBelowGate` says the reducer
is the identity when its gate is false — formalizing the runtime's
early-return behavior. `identityUnlessSafe` says the reducer is the
identity unless `safeToReduce` holds — formalizing the
streaming-terminal precondition #190 vocabulary names.

`reapplyPreservesCoh` is the invariant-idempotence obligation: even
LLM-based strategies, whose strict `r (r v) = r v` would fail because
the LLM is nondeterministic, must preserve coherence under arbitrary
re-application. -/
class IsValidReducer (r : TranscriptReducer) where
  gate                : PromptView → Prop
  decGate             : ∀ v, Decidable (gate v)
  preservesPairs      : ∀ v,
                          PromptView.PairsClosedInMessages v.messages →
                          PromptView.PairsClosedInMessages (r v).messages
  preservesOrder      : ∀ v,
                          StrictlyIncreasingMessages v.messages →
                          StrictlyIncreasingMessages (r v).messages
  preservesSession    : ∀ v, (r v).sessionId = v.sessionId
  identityBelowGate   : ∀ v, ¬ gate v → r v = v
  identityUnlessSafe  : ∀ v, ¬ PromptView.safeToReduce v → r v = v
  reapplyPreservesCoh : ∀ v, PromptView.ViewCoherent v →
                          PromptView.ViewCoherent (r (r v))
```

### Witness 1: `identityReducer`

```lean
def identityReducer : TranscriptReducer := fun v => v

instance : IsValidReducer identityReducer where
  gate                := fun _ => False
  decGate             := fun _ => .isFalse (fun h => h)
  preservesPairs      := fun _ h => h
  preservesOrder      := fun _ h => h
  preservesSession    := fun _ => rfl
  identityBelowGate   := fun _ _ => rfl
  identityUnlessSafe  := fun _ _ => rfl
  reapplyPreservesCoh := fun _ h => h
```

Witnesses that the contract is non-vacuous. The `gate := False` choice means `identityReducer` never claims to reduce — every state is "below the gate."

### Witness 2: `stripToolResultsReducer`

Abstract analogue of Rust's `CompactionStrategy::StripToolResults`. Stubs each tool-result payload by tagging the row with a `stubbed` marker that preserves the `(callId, key)` pair structure.

```lean
def stubMessageKind : MessageKind → MessageKind
  | .toolResult callId key => .toolResult callId key  -- payload abstraction
  | other => other

/- In the model, message payloads aren't carried inside MessageRow, so the
stub is structurally the identity on MessageKind. The Rust stub replaces
the textual content with a 1-line marker; from the proof's perspective,
the call_id and tool-result key are what matter for pair atomicity, and
those are preserved. -/

def stubMessages : List MessageRow → List MessageRow :=
  List.map (fun row => { row with kind := stubMessageKind row.kind })

def stripToolResultsReducer : TranscriptReducer := fun v =>
  { v with messages := stubMessages v.messages }

instance : IsValidReducer stripToolResultsReducer where
  gate                := fun _ => True
  decGate             := fun _ => .isTrue trivial
  preservesPairs      := by /- stubMessageKind is the identity, so the
                              messages list is preserved up to definitional
                              equality. -/
  preservesOrder      := by /- map preserves the underlying sequence
                              numbers because stubMessageKind doesn't
                              touch `.sequence`. -/
  preservesSession    := fun _ => rfl
  identityBelowGate   := by /- gate = True is never false, so the
                              hypothesis is vacuous. -/
  identityUnlessSafe  := by /- when ¬ safeToReduce, stubMessages should
                              be the identity. Since stubMessageKind is
                              definitionally the identity in the model,
                              stripToolResultsReducer v = v
                              unconditionally; identityUnlessSafe is
                              vacuous. -/
  reapplyPreservesCoh := by /- two applications of the identity-shaped
                              stub preserve coherence trivially. -/
```

Note on the stub's modeling: in the model, `MessageKind.toolResult callId key` is the abstract carrier of pair structure; the textual payload is not represented. The Rust `StripToolResults` strategy replaces text content with a stub but preserves `call_id` and message key — exactly the structure the model abstracts over. So `stubMessageKind` is case-wise the identity on `MessageKind`, and `stripToolResultsReducer v = v` follows *propositionally* from `List.map_id` and case-wise reduction; it is not the judgmental identity. This is faithful: the model captures *what could go wrong* (orphaning a tool result, reordering, breaking the call_id link), and the Rust strip operation cannot break any of those because it never touches the linking metadata. The witness encodes that invariance.

**Why this witness still earns its keep even though it's propositionally identity-shaped:** the typeclass instance still has to *discharge* `preservesPairs`/`preservesOrder` explicitly — those proofs use the `List.map_id` chain rather than `rfl`, and they nail down the Rust-side contract: a future strip-style strategy is *only* admissible if its mutation is propositionally equal to identity on `MessageKind`. Any future strategy whose mutation alters `callId`, `key`, or `sequence` will fail `preservesPairs` or `preservesOrder` (no `List.map_id` discharge available) and will not instantiate the typeclass. The witness pins the boundary between "structural" mutations (admissible) and "linking" mutations (forbidden).

## Properties (Proofs/Compaction/Properties.lean)

### Contract-level theorems (parametric over any valid reducer)

```lean
theorem reduction_preserves_view_coherent
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r v) := by
  exact
    { pairs           := IsValidReducer.preservesPairs r v h.pairs
    , ordered         := IsValidReducer.preservesOrder r v h.ordered
    , uniqueSequences := -- derived from strictly-increasing
        StrictlyIncreasingMessages.uniqueSequences
          (IsValidReducer.preservesOrder r v h.ordered) }

theorem reduction_preserves_session_id
    {r : TranscriptReducer} [IsValidReducer r] (v : PromptView) :
    (r v).sessionId = v.sessionId :=
  IsValidReducer.preservesSession r v

theorem reduction_identity_when_below_gate
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView} (h_below : ¬ IsValidReducer.gate r v) :
    r v = v :=
  IsValidReducer.identityBelowGate r v h_below

theorem reduction_blocked_unless_safe
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView} (h_unsafe : ¬ PromptView.safeToReduce v) :
    r v = v :=
  IsValidReducer.identityUnlessSafe r v h_unsafe

theorem reapply_preserves_view_coherent
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView} (h : PromptView.ViewCoherent v) :
    PromptView.ViewCoherent (r (r v)) :=
  IsValidReducer.reapplyPreservesCoh r v h
```

### Pair-atomicity-specific theorems

```lean
theorem no_orphaned_tool_results_after_reduction
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView}
    (h_pre : PromptView.PairsClosedInMessages v.messages) :
    ∀ row, row ∈ (r v).messages →
      ∀ callId key, row.kind = .toolResult callId key →
        ∃ caller, caller ∈ (r v).messages ∧
          caller.role = .assistant ∧
          (∃ callIds, caller.kind = .assistantToolCalls callIds ∧
            callId ∈ callIds) := by
  intro row h_mem callId key h_kind
  exact IsValidReducer.preservesPairs r v h_pre row h_mem callId key h_kind
```

This is the "no orphaned `AgentToolCall` rows after compaction" theorem the issue acceptance criteria names explicitly. It's a direct corollary of `preservesPairs`.

### Message-order monotonicity

```lean
theorem retained_window_is_ordered
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView}
    (h_pre : StrictlyIncreasingMessages v.messages) :
    StrictlyIncreasingMessages (r v).messages :=
  IsValidReducer.preservesOrder r v h_pre
```

This is "message-order monotonicity within retained windows" from the issue acceptance criteria.

### Idempotence (weakened form)

```lean
theorem reduction_idempotent_when_below_gate
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView}
    (h_below : ¬ IsValidReducer.gate r (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityBelowGate r (r v) h_below

theorem reduction_idempotent_when_unsafe
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView}
    (h_unsafe : ¬ PromptView.safeToReduce (r v)) :
    r (r v) = r v :=
  IsValidReducer.identityUnlessSafe r (r v) h_unsafe
```

Two *conditional* idempotence theorems: re-application is a no-op (a) when the strategy's gate is false on the once-reduced view, and (b) when the once-reduced view is no longer safe to reduce. The unconditional `r (r v) = r v` would be false for LLM-based strategies; the conditional forms capture the actual runtime behavior.

### Streaming-coupling theorem

```lean
theorem reduction_implies_all_retained_tool_results_terminal
    {r : TranscriptReducer} [IsValidReducer r]
    {v : PromptView}
    (h_nontrivial : r v ≠ v) :
    PromptView.safeToReduce v := by
  by_contra h_unsafe
  exact h_nontrivial (IsValidReducer.identityUnlessSafe r v h_unsafe)
```

Reads as: any non-identity reduction implies every tool-result message in the *input* view has a terminal response status. This is the load-bearing safety claim that ties compaction to #190's response-state vocabulary.

### Witness theorems

```lean
theorem identity_reducer_is_strictly_idempotent (v : PromptView) :
    identityReducer (identityReducer v) = identityReducer v := rfl

theorem strip_tool_results_is_strictly_idempotent (v : PromptView) :
    stripToolResultsReducer (stripToolResultsReducer v)
      = stripToolResultsReducer v := by
  -- Implementation: discharge via `List.map_map` and the fact that
  -- `stubMessageKind ∘ stubMessageKind = stubMessageKind` (case-wise rfl).
  -- The shipped proof carries zero `sorry`; this snippet is design-spec
  -- shorthand only.
  sorry  -- DESIGN-SPEC PLACEHOLDER — implementation must discharge
```

Two extra theorems for the witnesses we ship: strict idempotence holds for both (neither uses an LLM). These do NOT generalize to the typeclass — the conditional forms above are what every strategy must satisfy.

## Conformance vectors (Proofs/Compaction/Executable.lean)

```lean
structure CompactionReducerCase where
  name                : String
  group               : String        -- "contract" | "witness" | "streaming"
  reducer             : String        -- "identity" | "strip_tool_results"
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
  /-  1 -/   identity_reducer_is_no_op
  /-  2 -/ , identity_preserves_pair_atomicity
  /-  3 -/ , identity_preserves_message_order
  /-  4 -/ , strip_preserves_pair_atomicity
  /-  5 -/ , strip_preserves_message_order
  /-  6 -/ , strip_is_strictly_idempotent
  /-  7 -/ , reduction_blocked_when_response_streaming
  /-  8 -/ , reduction_allowed_when_response_terminal
  /-  9 -/ , no_orphaned_tool_results_after_strip
  /- 10 -/ , reapply_preserves_view_coherent
]
```

| # | Name | Group | Headline assertion |
|---|---|---|---|
| 1 | `identity_reducer_is_no_op` | witness | `identityReducer v = v` for all v |
| 2 | `identity_preserves_pair_atomicity` | witness | identity is a valid reducer (pairs side) |
| 3 | `identity_preserves_message_order` | witness | identity is a valid reducer (order side) |
| 4 | `strip_preserves_pair_atomicity` | witness | `stripToolResultsReducer` preserves `PairsClosedInMessages` |
| 5 | `strip_preserves_message_order` | witness | `stripToolResultsReducer` preserves `StrictlyIncreasingMessages` |
| 6 | `strip_is_strictly_idempotent` | witness | `strip ∘ strip = strip` (witnesses the boundary between LLM and non-LLM strategies) |
| 7 | `reduction_blocked_when_response_streaming` | streaming | when `¬ safeToReduce v`, every valid reducer is identity on v |
| 8 | `reduction_allowed_when_response_terminal` | streaming | when `safeToReduce v`, reducers may be non-identity |
| 9 | `no_orphaned_tool_results_after_strip` | contract | concrete instance of the orphaning theorem for the strip witness |
| 10 | `reapply_preserves_view_coherent` | contract | re-application preserves `ViewCoherent` (invariant idempotence) |

Coverage ledger entry (in `Proofs/Conformance/CoverageLedger.lean`):

```lean
, consumerWithFollowUpCoverage
    "compaction_reducer_cases"
    "CompactionReducerCases"
    "state_machine_conformance::generated_compaction_reducer_cases_pin_contract"
    "Rust consumer wires up in a follow-up; vectors are stable and ready."
```

Modeled after the `queue_deadline_cases` and `recovery_sweep_cases` precedents (lines 277–286 of CoverageLedger.lean).

## Cross-module wiring summary

| File | Change | Net |
|---|---|---|
| `Proofs.lean` | + `import Proofs.Compaction` (after `import Proofs.Transcript`, line 13 placement — topological) | +1 line |
| `Proofs/Conformance/CoverageLedger.lean` | + one `consumerWithFollowUpCoverage` entry | +5 lines |
| New: `Proofs/Compaction/State.lean` | full | ~110 lines |
| New: `Proofs/Compaction/Transition.lean` | full | ~130 lines |
| New: `Proofs/Compaction/Properties.lean` | full | ~160 lines (~12 theorems) |
| New: `Proofs/Compaction/Executable.lean` | full | ~120 lines (10 vectors) |
| New: `Proofs/Compaction.lean` | barrel | ~10 lines |

Read-only (do not modify):
- `Proofs/Transcript/*` — vocabulary import only (`Sequence`, `MessageRow`, `MessageKind`, `ToolResultKey`, `MessageRole`, `StrictlyIncreasingMessages`, `UniqueMessageSequences`, `SessionId`).
- `Proofs/StreamingResponse/*` — vocabulary import only (`Status`, `Status.isTerminal`). Read after #190 merges.
- `Proofs/Properties/Safety.lean`, `Proofs/Properties/Liveness.lean` — cited in commentary; not imported directly.

## Coordination with #190

`Proofs/StreamingResponse/` is in implementation in a sibling worktree (`/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-issue-190-agent-response-streaming`) when this design is written. Three coordination points:

1. **Design phase is safe to run in parallel.** This spec cites #190's spec file (`docs/superpowers/specs/2026-05-14-agent-response-streaming-lean-design.md`) as the canonical source of `Status` vocabulary; no #190 proof file is read.
2. **Implementation pause if #190 hasn't merged.** When we reach the execution phase, check `git log main` for #190's merge. If unmerged, pause execution until #190 lands (or rebase against its branch); the `import Proofs.StreamingResponse.State` line is the dependency.
3. **One-rebase tolerance.** If #190 merges *during* implementation, expect at most one rebase to pick up canonical paths. The vocabulary names (`Status`, `Status.isTerminal`) are stable in #190's design spec, so the rebase should be mechanical.

## Verdicts moved by this PR

| Audit row | Before | After |
|---|---|---|
| Compaction / context management, line 40 | ❌ | ✓ Modeled |

## Out of scope

- **Per-strategy semantics.** Sliding-window vs summary vs token-budget — we model the contract any strategy must satisfy, not what any specific strategy *does*. The brief is explicit: "strategy-parametric, not strategy-specific."
- **Rust production code.** `crates/defra-agent/src/compaction*` and `crates/defra-agent/src/truncation*` stay as-is. No changes to the trait, the strategies, the LLM prompt, or the spill-doc machinery.
- **Rust consumer wiring.** Vectors are registered as `consumerWithFollowUpCoverage`; the actual `state_machine_conformance.rs` test that consumes them is a follow-up issue.
- **Truncator with side effects.** The spill-doc behavior (`TruncationResult.spill_doc_id`) is a side-effecting operation tied to a separate DefraDB document collection. The contract admits a pure `truncatePayloadReducer`, but modeling the spill-doc lifecycle is a separate state machine and not required for the issue's acceptance criteria.
- **Summary content modeling.** `SummaryHandle` is opaque. We do not constrain what the LLM emits inside the summary blob. Strategies that produce summaries must still satisfy `preservesPairs` etc. on the messages list; the summary field is structurally invisible to the contract.
- **Pre-compaction → durable transcript translation.** The bridge from `TranscriptState` to `PromptView` (which durable rows project into which prompt-input rows) is an apply-time concern owned upstream by the prompt builder. The model assumes a PromptView arrives well-formed; whether the projection is correct is not modeled here.
- **Token-budget verification.** `compacted_token_estimate` in Rust is an estimate that may diverge from actual tokenization. We do not prove the reducer reduces token count, only that it preserves invariants. Token convergence is a runtime concern.

## Risks and notes

- **`stripToolResultsReducer` is propositionally equal to the identity in the model, not judgmentally.** Because the model abstracts away message payload text, `stubMessageKind` is case-wise the identity on `MessageKind`, and `stripToolResultsReducer v = v` follows from `List.map_id`. The typeclass instance still has to *prove* `preservesPairs`/`preservesOrder` (no `rfl` discharge available without `List.map_id`), so the obligations are exercised. This is faithful: the model captures the structural contract (pair atomicity, ordering, identity invariants), and the Rust strip operation cannot break the structural contract because it never touches the linking metadata. The witness's load-bearing role is to pin the boundary: any future "strip"-style strategy whose Rust implementation alters `call_id`, `key`, or `sequence` will fail to instantiate the typeclass, and that failure is a build-time signal. Reviewers should not mistake the prop-identity for vacuity.
- **`responseStatuses` is a total function `MessageId → Option Status`**, not a partial map over only-tool-result-message ids. This is intentional — the function carrier makes lookups syntactically unconditional. The `safeToReduce` predicate uses the function only for tool-result rows, so non-tool-result ids' return values are unconstrained.
- **The conditional-idempotence shape ("identity below gate") depends on the strategy's gate predicate.** The typeclass carries the gate as a field; each instance picks its own. `identityReducer` picks `False` (never reduces); `stripToolResultsReducer` picks `True` (always strips). Future LLM strategies pick `exceedsThreshold` or analogues. The implementation plan must verify that every shipping instance discharges `identityBelowGate` for its chosen gate.
- **#190 merge timing.** If #190 hasn't merged when execution starts, pause and re-check; do not attempt to write proof files that import a nonexistent module. The design phase is independent; the execution phase has a hard dependency.
- **Composability lemma is deferred.** "Composition of two valid reducers is a valid reducer" (relevant if future strategies are built as `truncate ∘ summarize`) is *not* proved in this PR. It's an obvious follow-up and is flagged here.
- **v2 effect-monad lift (follow-up consideration, not a v1 blocker).** The v1 contract is `TranscriptReducer := PromptView → PromptView` — pure. A future Truncator witness whose spill-doc side-effect must be modeled (creating/updating the spill-doc collection row, threading a doc id back into the truncated payload) may require lifting the contract into an effect monad, e.g., `TranscriptReducerM := PromptView → StateM SpillDocs PromptView`. The composability lemma's prerequisites depend on whether v2 stays pure (`Reducer ∘ Reducer`) or moves to an effectful Kleisli composition (`Reducer >=> Reducer`). Flagged here so the v2 design knows the door is open; v1 stays pure.

## File-by-file build order (for the implementation plan)

1. `Proofs/Compaction/State.lean` — vocabulary first; `lake build` must pass.
2. `Proofs/Compaction/Transition.lean` — depends on State; both witnesses' typeclass instances must compile.
3. `Proofs/Compaction/Properties.lean` — depends on Transition. Build incrementally, one theorem at a time, to localize tactic-failure regressions.
4. `Proofs/Compaction/Executable.lean` — depends on Transition.
5. `Proofs/Compaction.lean` — barrel.
6. `Proofs.lean` — add the import; `lake build` must still pass.
7. `Proofs/Conformance/CoverageLedger.lean` — register the ledger entry.

Final `lake build` must complete without `sorry` and without errors.

## PR shape

Title: `Add Lean coverage for compaction and truncation`
Body should cite:
- `Closes #184`
- `Refs #183` (parent tracker)
- `Refs #191` / `Refs #195` (provides transcript vocabulary)
- `Refs #190` (provides response-terminal vocabulary)

PR body must call out:
- The contract type: `IsValidReducer` typeclass over `PromptView → PromptView`, strategy-parametric.
- Proved invariants: pair atomicity (`preservesPairs`), order monotonicity (`preservesOrder`), session identity (`preservesSession`), conditional fixpoint (`identityBelowGate`, `identityUnlessSafe`), invariant idempotence (`reapplyPreservesCoh`), and the streaming-terminal safety theorem (`reduction_implies_all_retained_tool_results_terminal`).
- Audit verdict moved: row 40 (Compaction / context management) ❌ → ✓ Modeled.
- Not in scope: per-strategy semantics, Rust production code, summary content, spill-doc lifecycle, token-budget convergence.

### Reuse-vs-replacement: what the new vectors do and don't replace

The PR body must state explicitly that the new `compaction_reducer_cases` are *structural* coverage, not *behavioral* coverage:

> The `stripToolResultsReducer` witness is propositionally equal to the identity in the abstract model (because `MessageKind` doesn't carry payload text — see §Risks). This is correct and load-bearing: the witness pins the *structural* contract (pair atomicity, ordering, identity invariants are preserved by any admissible "strip"-shaped mutation). It does NOT replace `crates/defra-agent/src/compaction/tests.rs`, which exercises behavioral properties (the stub text format, the file-activity extractor, the byte-count formatting, the summary persistence flow). Those Rust tests stay as-is and continue to be the source of behavioral truth. The new Lean vectors are an *additional* layer of structural assertions, not a replacement. Reviewers should not interpret 10/10 Lean vector pass as covering the behavioral surface — the two layers cover different things.

This framing prevents two failure modes: (a) deleting `compaction/tests.rs` because "Lean covers it now" (it doesn't), and (b) trusting the Lean witnesses to catch a regression in stub-text formatting (they won't — that's the Rust tests' job).
