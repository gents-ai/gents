import Proofs.Automation.Triggers.NativeRoute
import Proofs.Conformance.Family

namespace Conformance.NativeRoute

open Lean Triggers.NativeRoute

deriving instance ToJson for Triggers.NativeRoute.Fields

structure DeliveryInput where
  name : String
  input : Fields
  emitOutcome : Bool
  deriving ToJson

structure DeliveryOutput where
  stamped : Fields
  admitted : Bool
  delivered : Fields
  deriving ToJson

/-- The producer stamps its invocation's route on the native document it
writes; delivery admits the fire and derives its route from that observed
document alone. -/
@[conformance_family]
def delivery : Family DeliveryInput DeliveryOutput where
  inputs := cases.map fun c => { name := c.name, input := c.input, emitOutcome := c.emitOutcome }
  model := fun i =>
    let stamped := stamp (projectRoute i.input)
    { stamped
      admitted := outcomeFireAdmitted i.emitOutcome stamped
      delivered := stamp (deliveredRoute stamped) }

end Conformance.NativeRoute
