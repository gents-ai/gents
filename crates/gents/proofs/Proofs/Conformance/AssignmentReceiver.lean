import Proofs.Store.AssignmentReceiver
import Proofs.Conformance.Family

namespace AssignmentReceiver

deriving instance Lean.ToJson for Position, State

attribute [conformance_function] permits retry

end AssignmentReceiver

namespace Conformance.AssignmentReceiver

open Lean _root_.AssignmentReceiver

structure Input where
  state : State
  kind : String
  position : _root_.AssignmentReceiver.Position
  assigned : List String
  deriving ToJson

def position (term height : Nat) (hash : String) : _root_.AssignmentReceiver.Position :=
  ⟨term, height, hash⟩

@[conformance_family]
def observe : Family Input State where
  inputs :=
    let first := position 1 10 "a"
    let state : State := ⟨some first, ["B"]⟩
    [ ⟨state, "unavailable", first, []⟩
    , ⟨state, "invalid", first, []⟩
    , ⟨state, "complete", position 1 9 "old", ["B"]⟩
    , ⟨state, "complete", position 1 10 "wrong", ["B"]⟩
    , ⟨state, "complete", first, ["C"]⟩
    , ⟨state, "complete", position 1 11 "next", ["C"]⟩
    , ⟨state, "complete", position 2 1 "term", []⟩
    , ⟨⟨none, []⟩, "complete", first, ["B"]⟩ ]
  model := fun input => _root_.AssignmentReceiver.observe input.state
    (if input.kind == "complete" then .complete input.position input.assigned
      else if input.kind == "unavailable" then .unavailable else .invalid)

structure RunInput where
  state : State
  id : String
  bodyVerified : Bool
  deriving ToJson

@[conformance_family]
def mayRun : Family RunInput Bool where
  inputs :=
    [ ⟨⟨some (position 1 11 "new"), ["C"]⟩, "B", true⟩
    , ⟨⟨some (position 1 11 "new"), ["C"]⟩, "C", false⟩
    , ⟨⟨some (position 1 11 "new"), ["C"]⟩, "C", true⟩ ]
  model := fun input => _root_.AssignmentReceiver.mayRun input.state input.id input.bodyVerified

end Conformance.AssignmentReceiver
