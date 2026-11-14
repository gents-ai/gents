import Proofs.Clients.ClientShell.ObservationOrdering
import Proofs.Conformance.Family

namespace Conformance.ClientLiveDeltaContracts

open Lean

structure CursorInput where
  base : Option Nat
  current : Option Nat
  terminal : Bool
  deriving ToJson

structure CursorOutput where
  accepted : Bool
  deriving ToJson

@[conformance_family]
def cursor : Family CursorInput CursorOutput where
  inputs :=
    let cursors : List (Option Nat) := [none, some 0, some 1]
    cursors.flatMap fun base => cursors.flatMap fun current =>
      [false, true].map fun terminal => { base, current, terminal }
  model := fun i => { accepted := ClientLiveDelta.accepts i.base i.current i.terminal }

end Conformance.ClientLiveDeltaContracts
