import Proofs.ToolExecution.Parallel
import Lean

namespace Conformance.ParallelToolBatch
open Lean ToolExecution.Parallel

structure InputCall where
  key : Nat
  read : Bool
  deriving FromJson

structure Input where
  limit : Nat
  calls : List InputCall
  accepted : List Nat
  settled : List (Nat × Nat)
  stop : String
  actions : List String
  deriving FromJson

structure Output where
  length : Nat
  complete : Bool
  admitted : List Nat
  targets : List Nat
  ordered : Option (List (Nat × Option Nat))
  deriving ToJson

def execute (input : Input) : Output :=
  let batch : Batch := ⟨input.accepted, input.settled,
    if input.stop = "cancelled" then .cancelled
    else if input.stop = "terminated" then .terminated else .none⟩
  ⟨(takeReads input.limit [] (input.calls.map fun c =>
    ⟨c.key, if c.read then .read else .barrier⟩)).length,
    complete batch, accepted (input.actions.map fun action =>
      if action = "run" then .run else if action = "skip" then .skip else .terminate),
    cancellationTargets batch, finish batch⟩

end Conformance.ParallelToolBatch

def main : IO Unit := do
  let line ← (← IO.getStdin).getLine
  match Lean.Json.parse line >>= Lean.fromJson? with
  | .error reason => throw (IO.userError reason)
  | .ok inputs => IO.println ((Lean.toJson ((inputs : List Conformance.ParallelToolBatch.Input).map Conformance.ParallelToolBatch.execute)).compress)
