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
