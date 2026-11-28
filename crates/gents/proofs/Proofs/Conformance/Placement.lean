import Proofs.Runtime.RuntimeReconcile.Placement
import Proofs.Conformance.Family
import Proofs.Conformance.AssignmentReceiver

namespace RuntimeReconcile.Placement

deriving instance Lean.ToJson for Outcome

attribute [conformance_function] proposalOutcome

end RuntimeReconcile.Placement

namespace Conformance.Placement

open Lean RuntimeReconcile.Placement

structure Floor where
  scope : String
  position : AssignmentReceiver.Position
  deriving ToJson

structure AdmitInput where
  basis : List Floor
  fence : List Floor
  deriving ToJson

def floors (entries : List Floor) : Basis := entries.map fun entry => (entry.scope, entry.position)

def at' (scope : String) (term height : Nat) (hash : String) : Floor := ⟨scope, ⟨term, height, hash⟩⟩

@[conformance_family]
def admitsProposal : Family AdmitInput Bool where
  inputs :=
    [ ⟨[], []⟩
    , ⟨[at' "a" 1 4 "x"], []⟩
    , ⟨[], [at' "a" 1 4 "x"]⟩
    , ⟨[at' "a" 1 3 "w"], [at' "a" 1 4 "x"]⟩
    , ⟨[at' "a" 1 4 "x"], [at' "a" 1 4 "x"]⟩
    , ⟨[at' "a" 1 9 "y"], [at' "a" 1 4 "x"]⟩
    , ⟨[at' "a" 2 1 "z"], [at' "a" 1 4 "x"]⟩
    , ⟨[at' "a" 1 9 "y"], [at' "a" 2 1 "z"]⟩
    , ⟨[at' "a" 1 9 "y", at' "b" 3 2 "v"], [at' "a" 1 4 "x", at' "b" 3 3 "u"]⟩
    , ⟨[at' "a" 1 9 "y", at' "b" 3 3 "u"], [at' "a" 1 4 "x", at' "b" 3 3 "u"]⟩
    , ⟨[at' "b" 3 3 "u"], [at' "a" 1 4 "x"]⟩ ]
  model := fun input => RuntimeReconcile.Placement.admitsProposal (floors input.basis) (floors input.fence)

structure StoppedInput where
  basis : List Floor
  scope : String
  floor : AssignmentReceiver.Position
  ids : List String
  generation : List String
  retiring : List String
  deriving ToJson

@[conformance_family]
def stopped : Family StoppedInput Bool where
  inputs :=
    let floor : AssignmentReceiver.Position := ⟨1, 4, "x"⟩
    [ ⟨[], "a", floor, ["B"], [], []⟩
    , ⟨[at' "a" 1 3 "w"], "a", floor, ["B"], [], []⟩
    , ⟨[at' "b" 1 9 "y"], "a", floor, ["B"], [], []⟩
    , ⟨[at' "a" 1 4 "x"], "a", floor, ["B"], ["B", "C"], []⟩
    , ⟨[at' "a" 1 4 "x"], "a", floor, ["B"], ["C"], ["B"]⟩
    , ⟨[at' "a" 1 4 "x"], "a", floor, ["B"], ["C"], ["C"]⟩
    , ⟨[at' "a" 2 1 "z"], "a", floor, ["B", "D"], ["C"], []⟩
    , ⟨[at' "a" 1 4 "x"], "a", floor, [], ["B"], ["B"]⟩ ]
  model := fun input =>
    RuntimeReconcile.Placement.stopped (floors input.basis) input.scope input.floor input.ids
      input.generation input.retiring

end Conformance.Placement
