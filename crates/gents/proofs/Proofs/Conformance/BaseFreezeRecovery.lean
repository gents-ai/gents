import Proofs.Session.Workspace.BaseFreezeRecovery
import Proofs.Conformance.Family

namespace Conformance.BaseFreezeRecovery

open Lean Workspace.BaseFreezeRecovery

deriving instance ToJson for Journal

/-- Each freeze runs with a fresh journal; `faults` says whether its placement
write fails after the workspace row was written. -/
structure TraceInput where
  placementCleared : Bool
  faults : List Bool
  deriving ToJson

structure StepOutput where
  ok : Bool
  returnedPlacementAtBase : Option Bool
  journal : Journal
  workspaceState : String
  sealedAtBase : Bool
  placementAtBase : Bool
  workspaceWrites : Nat
  placementWrites : Nat
  receipts : Nat
  trunkMoved : Bool
  deriving ToJson

def observe (next : Docs) (r : Result) : StepOutput :=
  { ok := r.ok, returnedPlacementAtBase := r.returnedPlacementAtBase, journal := r.journal
    workspaceState := next.state.toDefraDB, sealedAtBase := next.sealedAtBase
    placementAtBase := next.placementAtBase, workspaceWrites := next.workspaceWrites
    placementWrites := next.placementWrites, receipts := next.receipts
    trunkMoved := next.trunkMoved }

def run (i : TraceInput) : List StepOutput :=
  (i.faults.foldl (fun (acc : Docs × List StepOutput) fault =>
    let (next, r) := freeze acc.1 fault
    (next, acc.2 ++ [observe next r]))
    ({ state := .ready, sealedAtBase := false, placementAtBase := !i.placementCleared }, [])).2

@[conformance_family]
def steps : Family TraceInput (List StepOutput) where
  inputs :=
    [ ⟨true, [true, false, false]⟩
    , ⟨true, [false, false]⟩
    , ⟨false, [false, false]⟩
    , ⟨true, [true, true, false]⟩
    , ⟨false, [true, false]⟩ ]
  model := run

end Conformance.BaseFreezeRecovery
