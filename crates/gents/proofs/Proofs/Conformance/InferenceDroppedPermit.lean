import Proofs.Inference.InferenceCall.DroppedPermit
import Proofs.Conformance.Family

namespace Conformance.InferenceDroppedPermit
open InferenceCall.DroppedPermit

deriving instance Lean.ToJson for DropOutcome

structure DropInput where
  cancelled : Bool
  observed : Option String
  deriving Lean.ToJson

structure DropOutput where
  outcome : DropOutcome
  deriving Lean.ToJson

@[conformance_family]
def outcomes : Conformance.Family DropInput DropOutput where
  inputs :=
    [ { cancelled := true, observed := none }
    , { cancelled := true, observed := some "stream liveness timeout: no data received for 1800s" }
    , { cancelled := false, observed := none }
    , { cancelled := false, observed := some "stream liveness timeout: no data received for 1800s" }
    ]
  model := fun i => { outcome := outcome i.cancelled i.observed }

end Conformance.InferenceDroppedPermit
