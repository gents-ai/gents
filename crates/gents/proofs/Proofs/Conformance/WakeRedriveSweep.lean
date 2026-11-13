import Proofs.ToolCalls.Background.CompletionContinuation
import Proofs.Conformance.Family

namespace Conformance.WakeRedriveSweep

open Lean BackgroundCompletion

deriving instance ToJson for RedriveDecision

structure Input where
  retryCount : Nat
  maxRetries : Nat
  elapsedSeconds : Nat
  deriving ToJson

structure Output where
  decision : RedriveDecision
  rowsUnchanged : Bool
  deriving ToJson

@[conformance_family]
def sweep : Family Input Output where
  inputs := [⟨4, 6, 0⟩, ⟨4, 6, 3600⟩, ⟨6, 6, 3600⟩]
  model := fun i =>
    let decision := redriveSweepDecision ⟨i.retryCount, i.maxRetries, i.elapsedSeconds, false⟩
    { decision, rowsUnchanged := decision != .redriven }

end Conformance.WakeRedriveSweep
