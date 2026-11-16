import Proofs.Clients.ClientShell.ObservationScope
import Lean

/-! Executes `ClientWindowScope` on the window traces a native test serializes.
After every operation it reports, for every window, the node a command acts on
with no node named and with each node named, and which nodes a node-scoped
delivery reaches it for. -/
namespace Conformance.ClientWindowScope
open Lean
open _root_.ClientWindowScope

structure Op where
  op : String
  window : Nat
  node : Option Nat := none
  deriving FromJson

structure Trace where
  windows : Nat
  nodes : Nat
  ops : List Op
  deriving FromJson

def apply (s : Selections) (op : Op) : Selections :=
  match op.op with
  | "register" => register s op.window
  | "select" => select s op.window op.node
  | "destroy" => destroy s op.window
  | _ => s

def windowJson (trace : Trace) (s : Selections) (window : Nat) : Json :=
  let nodes := List.range trace.nodes
  Json.mkObj [
    ("command", toJson (commandNode s window none)),
    ("named", toJson (nodes.map fun node => commandNode s window (some node))),
    ("delivers", toJson (nodes.filter fun node => delivers s window node))]

def execute (trace : Trace) : Json := Id.run do
  let mut s : Selections := fun _ => none
  let mut steps : Array Json := #[]
  for op in trace.ops do
    s := apply s op
    steps := steps.push (Json.arr ((List.range trace.windows).map (windowJson trace s)).toArray)
  return Json.arr steps

end Conformance.ClientWindowScope

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  match Lean.Json.parse input >>= Lean.fromJson? (α := List Conformance.ClientWindowScope.Trace) with
  | .error reason => throw (IO.userError reason)
  | .ok traces =>
      IO.println (Lean.toJson (traces.map Conformance.ClientWindowScope.execute)).compress
