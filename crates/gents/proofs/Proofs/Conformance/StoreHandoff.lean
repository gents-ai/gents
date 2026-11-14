import Proofs.Store.StoreHandoff
import Proofs.Conformance.Family
import Lean

namespace Conformance.StoreHandoff
open Lean _root_.StoreHandoff

deriving instance ToJson for Decision

/-- `held` with `releasing` names the holder's declaration in its lock file;
`free` carries `releasing := false`. -/
structure ObservationWire where
  held : Bool
  releasing : Bool
  deriving ToJson

def ObservationWire.toObservation (wire : ObservationWire) : Observation :=
  if wire.held then .held wire.releasing else .free

@[conformance_family]
def decisions : Conformance.Family ObservationWire Decision where
  inputs := [{ held := false, releasing := false }, { held := true, releasing := false },
    { held := true, releasing := true }]
  model := fun i => decide i.toObservation

end Conformance.StoreHandoff
