import Proofs.Store.Identity.EvalRecords
import Proofs.Conformance.Family

namespace Conformance.OwnerRegistration

open Lean

structure ObservationInput where
  name : String
  visible : Bool
  isOwner : Bool
  ownerRegistered : Option Bool
  deriving ToJson

@[conformance_family]
def cases : Family ObservationInput (Option Bool) where
  inputs := [("owner", true, true, some true), ("reader", true, false, some true),
    ("unregistered", true, false, some false), ("hidden", false, false, some false),
    ("legacy-owner", true, true, none), ("legacy-reader", true, false, none),
    ("invalid-hidden-owner", false, true, some false),
    ("invalid-hidden-registered", false, false, some true),
    ("invalid-unregistered-owner", true, true, some false)].map
      fun (name, visible, isOwner, ownerRegistered) => { name, visible, isOwner, ownerRegistered }
  model := fun i => Identity.EvalRecords.registration i.visible i.isOwner i.ownerRegistered

end Conformance.OwnerRegistration
