import Proofs.Compaction.State

namespace Compaction

abbrev TranscriptReducer := PromptView → PromptView

class IsValidReducer (r : TranscriptReducer) where
  gate              : PromptView → Prop
  decGate           : ∀ v, Decidable (gate v)
  preservesCoherent : ∀ v, PromptView.ViewCoherent v → PromptView.ViewCoherent (r v)
  preservesOrder    : ∀ v,
                        Transcript.StrictlyIncreasingMessages v.messages →
                        Transcript.StrictlyIncreasingMessages (r v).messages
  preservesSession  : ∀ v, (r v).sessionId = v.sessionId
  identityBelowGate : ∀ v, ¬ gate v → r v = v
  identityUnlessSafe : ∀ v, ¬ PromptView.safeToReduce v → r v = v

end Compaction

namespace Compaction

def identityReducer : TranscriptReducer := fun v => v

instance instIsValidReducerIdentity : IsValidReducer identityReducer where
  gate               := fun _ => False
  decGate            := fun _ => .isFalse (fun h => h)
  preservesCoherent  := fun _ h => h
  preservesOrder     := fun _ h => h
  preservesSession   := fun _ => rfl
  identityBelowGate  := fun _ _ => rfl
  identityUnlessSafe := fun _ _ => rfl

end Compaction
