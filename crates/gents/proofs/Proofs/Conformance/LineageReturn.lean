import Proofs.ToolCalls.Background.LineageReturn
import Proofs.Conformance.Family

namespace LineageReturn

open Lean

attribute [conformance_function] quiet

deriving instance ToJson for Node

structure SettleInput where
  nodes : List Node
  deriving ToJson

/-- The rank that answers, or none while the lineage is not quiet. -/
def answerRank (nodes : List Node) : Option Nat :=
  match decide nodes with
  | .wait => none
  | .answer rank => some rank

/-- A caused request alone, live, finished, and finished with a helper still
running or settled but undelivered; one and two delivered helpers with their
wakes pending, running and answered, listed in the order the settler finds
them, which need not be the order they ran. -/
@[conformance_family]
def settle : Conformance.Family SettleInput (Option Nat) where
  inputs :=
    [{ nodes := [⟨false, false, 0⟩] },
     { nodes := [⟨true, false, 0⟩] },
     { nodes := [⟨true, true, 0⟩] },
     { nodes := [⟨true, false, 0⟩, ⟨false, false, 1⟩] },
     { nodes := [⟨true, false, 0⟩, ⟨true, true, 1⟩] },
     { nodes := [⟨true, false, 0⟩, ⟨true, false, 1⟩] },
     { nodes := [⟨true, false, 0⟩, ⟨true, false, 2⟩, ⟨true, false, 1⟩] },
     { nodes := [⟨true, false, 0⟩, ⟨true, false, 2⟩, ⟨false, false, 1⟩] }]
  model := fun i => answerRank i.nodes

end LineageReturn
