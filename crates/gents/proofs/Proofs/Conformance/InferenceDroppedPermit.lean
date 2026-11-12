import Proofs.Inference.InferenceCall.DroppedPermit
import Proofs.Conformance.Family

namespace Conformance.InferenceDroppedPermit
open InferenceCall.DroppedPermit

deriving instance Lean.ToJson for DropOutcome, RecordedReason

structure DropInput where
  cancelled : Bool
  observed : Option String
  deriving Lean.ToJson

structure DropOutput where
  outcome : DropOutcome
  callState : String
  failureReason : RecordedReason
  slotsWhileHeld : Nat
  slotsAfterDrop : Nat
  deriving Lean.ToJson

@[conformance_family]
def outcomes : Conformance.Family DropInput DropOutput where
  inputs :=
    [ { cancelled := true, observed := none }
    , { cancelled := true, observed := some "stream liveness timeout: no data received for 1800s" }
    , { cancelled := false, observed := none }
    , { cancelled := false, observed := some "stream liveness timeout: no data received for 1800s" }
    ]
  model := fun i =>
    let dropped := outcome i.cancelled i.observed
    { outcome := dropped
      callState := (recordedState dropped).toDefraDB
      failureReason := recordedReason dropped
      slotsWhileHeld := slotsHeld .running
      slotsAfterDrop := slotsHeld (recordedState dropped) }

end Conformance.InferenceDroppedPermit
