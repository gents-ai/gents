import Proofs.Store.Identity.PackPlacementReaders
import Proofs.Conformance.Family

namespace Conformance.PackPlacementReaders

open Lean Identity Identity.PackPlacementReaders

inductive PlacementState where
  | installed | reinstalledAfterRevoke | removed
  deriving ToJson

/-- `declared` is the reader list the fixture pack declares on its placement. -/
structure ReaderInput where
  state : PlacementState
  declared : List String
  deriving ToJson

structure ReaderObservation where
  actor : String
  read : Bool
  deriving ToJson

def stateOf : PlacementState → Household.State
  | .installed => installed
  | .reinstalledAfterRevoke => reinstalledAfterRevoke
  | .removed => removed

/-- The install, the reinstall after a reader was revoked and the removal
happen in this order on one installation. -/
@[conformance_family]
def readers : Family ReaderInput (List ReaderObservation) where
  inputs := [PlacementState.installed, .reinstalledAfterRevoke, .removed].map fun state => { state, declared }
  model := fun i => ["user", "writer", "reviewer", "integrator", "other"].map fun actor =>
    { actor, read := readable (stateOf i.state) actor }

end Conformance.PackPlacementReaders
