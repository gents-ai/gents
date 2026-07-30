import Proofs.Compaction.State

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

def stripToolResultsReducer : TranscriptReducer := fun v =>
  { v with messages := stubMessages v.messages }

theorem stripToolResultsReducer_id (v : PromptView) :
    stripToolResultsReducer v = v := by
  unfold stripToolResultsReducer
  simp [stubMessages_id]

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

namespace Compaction

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
