import Proofs.Shared.ToolExecution.Parallel
import Lean

namespace Conformance.ScriptEffectBatch
open Lean ToolExecution.Parallel

structure Call where
  key : Nat
  read : Bool
  deriving FromJson

structure Input where
  limit : Nat
  calls : List Call
  deriving FromJson

def execute (input : Input) : Nat :=
  effectWidth input.limit (input.calls.map fun c =>
    ⟨c.key, if c.read then .read else .barrier⟩)

end Conformance.ScriptEffectBatch

def main : IO Unit := do
  let line ← (← IO.getStdin).getLine
  match Lean.Json.parse line >>= Lean.fromJson? with
  | .error reason => throw (IO.userError reason)
  | .ok inputs => IO.println ((Lean.toJson
      ((inputs : List Conformance.ScriptEffectBatch.Input).map
        Conformance.ScriptEffectBatch.execute)).compress)
