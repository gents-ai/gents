import Proofs.Automation.EventDelivery.Group
import Proofs.Conformance.Family

namespace Conformance.EventGroupClock

open Lean EventDelivery.Group

deriving instance ToJson for QuiesceReason, StepOutcome, ClockObservation

structure TraceInput where
  members : List Int
  deriving ToJson

@[conformance_family]
def scenario : Family TraceInput ClockObservation where
  inputs := ([[2, 3], [2, 2, 2], [0], [3], [2, 3, 2]] : List (List Int)).map fun members =>
    { members }
  model := fun i => clockObservation i.members

end Conformance.EventGroupClock
