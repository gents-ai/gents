import Proofs.Inference.InferenceCall.UsageRefusal
import Proofs.Conformance.Family

namespace Conformance.ProviderUsageRefusal
open InferenceCall.UsageRefusal

/-- A refusal queued at `queuedAt`, the latest completion on its backend and
model, and the reader's clock. Times are seconds. -/
structure StandingInput where
  now : Nat
  queuedAt : Nat
  resetsAt : Option Nat
  lastCompletion : Option Nat
  deriving Lean.ToJson

@[conformance_family]
def standing : Conformance.Family StandingInput Bool where
  inputs :=
    [100, 149, 150, 200].flatMap fun now =>
      [none, some 100, some 150].flatMap fun resetsAt =>
        [none, some 50, some 100, some 101].map fun lastCompletion =>
          { now, queuedAt := 100, resetsAt, lastCompletion }
  model := fun i => stands i.now ⟨i.queuedAt, i.resetsAt⟩ i.lastCompletion

end Conformance.ProviderUsageRefusal
