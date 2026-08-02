import Proofs.Compaction.State

/-!
# What makes a transcript reducer valid

`preservesCoherent` replaced a pair of narrower fields (`preservesPairs`,
`reapplyPreservesCoh`) when the real `summarize` reducer was modelled (#993).
Pair closure of a compacted transcript is not preserved by an arbitrary drop —
it needs the provider-validity and role structure the runtime maintains, which
now live in `ViewCoherent`. Carrying whole-view coherence through the reducer is
both stronger and simpler than carrying pair closure alone, and re-application
coherence falls out of it rather than being asserted separately.

`Proofs/Compaction/Properties.lean` re-derives the old theorem names from it.
-/

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

/-- The degenerate reducer. Legitimate: it is what every valid reducer becomes
below its gate. -/
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
