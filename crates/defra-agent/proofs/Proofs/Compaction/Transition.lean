import Proofs.Compaction.State

/-!
# Compaction Transition

`TranscriptReducer := PromptView → PromptView` and the `IsValidReducer`
typeclass -- the contract any transcript-reduction strategy must satisfy.

Each instance picks its own `gate` predicate (matches Rust's per-strategy
`needs_compaction`). The `identityBelowGate` and `identityUnlessSafe`
fields capture the conditional-fixpoint shape: the reducer is the
identity when its gate is false OR when the view is not safe to reduce.
`reapplyPreservesCoh` is the invariant-idempotence obligation -- strict
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
`preservesOrder`, etc. via `stubMessages_id` -- see Properties.lean. -/
def stripToolResultsReducer : TranscriptReducer := fun v =>
  { v with messages := stubMessages v.messages }

theorem stripToolResultsReducer_id (v : PromptView) :
    stripToolResultsReducer v = v := by
  unfold stripToolResultsReducer
  simp [stubMessages_id]

end Compaction

namespace Compaction

/-- The trivial reducer -- does nothing. Witnesses that `IsValidReducer`
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
