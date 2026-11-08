import Proofs.ClientShell.ObservationOrdering
import Lean

/-! Executes `ObserverResync` and `ClientDataPublication` on the observer
traces a native test serializes, and reports each finished load's settlement,
the patch fence's decision for that read, and every node's final slot. -/
namespace Conformance.ObserverResync
open Lean
open _root_.ClientDataPublication
open _root_.ObserverResync
open _root_.RuntimeConnectionObservation (Status)

structure Op where
  op : String
  node : Nat := 0
  result : String := "rows"
  deriving FromJson

structure Trace where
  nodes : Nat
  ops : List Op
  deriving FromJson

structure Machine where
  pub : State
  slots : Nat → Slot
  reads : Nat → Option Read

def initialSlot : Slot := { pending := false, waiting := false, loading := false, status := .connecting }

def initial : Machine :=
  { pub := { dataEpoch := 0, provenanceEpoch := 0, requestEventEpoch := 0, transcriptEpoch := 0 },
    slots := fun _ => initialSlot, reads := fun _ => none }

def update {α : Type} (f : Nat → α) (node : Nat) (value : α) : Nat → α :=
  fun other => if other = node then value else f other

def loadResult : String → LoadResult
  | "transport_failed" => .transportFailed
  | "denied" => .denied
  | _ => .rows

def settlementName : Settlement → String
  | .published => "published"
  | .superseded => "superseded"
  | .transportFailed => "transport_failed"
  | .denied => "denied"

def statusName : Status → String
  | .connecting => "connecting"
  | .connected => "connected"
  | .disconnected => "disconnected"
  | .denied => "denied"

def presentationName : Presentation → String
  | .catchingUp => "catching_up"
  | .current => "current"
  | .retrying => "retrying"
  | .denied => "denied"
  | .stalled => "stalled"

def step (m : Machine) (op : Op) : Machine × Option Json :=
  let n := op.node
  let slot := m.slots n
  match op.op with
  | "armed" => ({ m with slots := update m.slots n (armed slot) }, none)
  | "start" =>
      if startable slot then
        ({ m with slots := update m.slots n (start slot),
                  reads := update m.reads n (some (beginRead m.pub)) }, none)
      else (m, none)
  | "deliver" =>
      ({ m with pub := observerEvent m.pub n, slots := update m.slots n (queue slot) }, none)
  | "reconnect_event" => ({ m with pub := requestEvent m.pub }, none)
  | "publish" => ({ m with pub := dataChange m.pub (.nodes [n]) }, none)
  | "timer" => ({ m with slots := update m.slots n (timer slot) }, none)
  | "finish" =>
      match slot.loading, m.reads n with
      | true, some read =>
          let outcome := settlement m.pub read n (loadResult op.result)
          let patch := acceptsFor m.pub read (.nodes [n])
          let pub := if outcome = .published then dataChange m.pub (.nodes [n]) else m.pub
          ({ pub, slots := update m.slots n (settle slot outcome), reads := update m.reads n none },
            some (Json.mkObj [("settlement", toJson (settlementName outcome)),
              ("patch_accepted", toJson patch)]))
      | _, _ => (m, some Json.null)
  | _ => (m, none)

def slotJson (slot : Slot) : Json :=
  Json.mkObj [("pending", toJson slot.pending), ("waiting", toJson slot.waiting),
    ("loading", toJson slot.loading), ("status", toJson (statusName slot.status)),
    ("presentation", toJson (presentationName (present slot)))]

def execute (trace : Trace) : Json := Id.run do
  let mut machine := initial
  let mut results : Array Json := #[]
  for op in trace.ops do
    let (next, result) := step machine op
    machine := next
    if let some result := result then
      results := results.push result
  return Json.mkObj [("results", Json.arr results),
    ("slots", Json.arr ((List.range trace.nodes).map (fun n => slotJson (machine.slots n))).toArray)]

end Conformance.ObserverResync

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  match Lean.Json.parse input >>= Lean.fromJson? (α := List Conformance.ObserverResync.Trace) with
  | .error reason => throw (IO.userError reason)
  | .ok traces =>
      IO.println (Lean.toJson (traces.map Conformance.ObserverResync.execute)).compress
