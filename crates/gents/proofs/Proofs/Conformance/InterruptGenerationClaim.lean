import Proofs.Session.SessionQueue.InterruptGeneration
import Proofs.Conformance.Family

namespace Conformance.InterruptGenerationClaim

open Lean SessionQueue.InterruptGeneration

inductive Read where
  | found
  | noSession
  | unreadable
  deriving ToJson

structure Input where
  read : Read
  generation : Nat
  createsSession : Bool
  deriving ToJson

structure Output where
  recorded : Option Nat
  deriving ToJson

def markerRead (i : Input) : MarkerRead :=
  match i.read with
  | .found => .found i.generation
  | .noSession => .noSession
  | .unreadable => .unreadable

/-- The generation a claim records for each marker read. -/
@[conformance_family]
def claim : Family Input Output where
  inputs :=
    ([(Read.found, 0), (.found, 3), (.noSession, 0), (.unreadable, 0)] : List (Read × Nat)).flatMap
      fun (read, generation) => [false, true].map fun createsSession =>
        ({ read, generation, createsSession } : Input)
  model := fun i => { recorded := claimRecord (markerRead i) i.createsSession }

end Conformance.InterruptGenerationClaim
