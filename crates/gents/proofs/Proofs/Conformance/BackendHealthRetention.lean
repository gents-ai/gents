import Proofs.Runtime.BackendHealth.Scheduling
import Proofs.Conformance.Family

namespace Conformance.BackendHealthRetention

open Lean Proofs.BackendHealth

structure CycleInput where
  entries : List Nat
  observed : List Nat
  cancelled : Bool
  deriving ToJson

def idSets : List (List Nat) :=
  [[], [0], [1], [2], [0, 1], [0, 2], [1, 2], [0, 1, 2]]

@[conformance_family]
def cycle : Family CycleInput (List Nat) where
  inputs := idSets.flatMap fun entries =>
    idSets.flatMap fun observed =>
      [false, true].map fun cancelled => { entries, observed, cancelled }
  model := fun i => retainedEntries i.entries i.observed i.cancelled

end Conformance.BackendHealthRetention
