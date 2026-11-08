import Proofs.ClientShell.ObservationOrdering
import Lean

/-! Executes `ObserverResync` and `ClientDataPublication` on the observer
traces a native test serializes. After every operation it reports the slot of
the operation's node; a finished load also reports its settlement and the
patch fence's decision for its read. The final report adds every node's slot
and the store's rows. -/
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
  rows : List FootprintRow

def initialMachine : Machine :=
  { pub := { dataEpoch := 0, provenanceEpoch := 0, requestEventEpoch := 0, transcriptEpoch := 0 },
    slots := fun _ => initial, reads := fun _ => none, rows := [] }

def update {α : Type} (f : Nat → α) (node : Nat) (value : α) : Nat → α :=
  fun other => if other = node then value else f other

/-- The node's own row, the other node's row and the row shared by key with an
unowned row the trace may seed. -/
def ownRow (node : Nat) : FootprintRow := { key := s!"session-{node}", owner := some node }

def sharedKey : String := "shared"

/-- The rows a load or direct reload of `node` reads: `rows`, `foreign` (with a
row the runtime returns for another node) or `shared` (with the node's row
under the shared key). -/
def snapshot (node : Nat) : String → List FootprintRow
  | "foreign" => [ownRow node, ownRow (1 - node)]
  | "shared" => [ownRow node, { key := sharedKey, owner := some node }]
  | _ => [ownRow node]

/-- A reload publishing `incoming` through the fence, captured at `read`. -/
def reload (m : Machine) (read : Read) (node : Nat) (incoming : List FootprintRow) :
    Settlement × Machine :=
  let outcome := settlement m.pub read (reloadCheck node m.rows incoming) .rows
  if outcome = .published then
    (outcome, { m with pub := dataChange m.pub (reloadFootprint node m.rows incoming),
                       rows := reloadRows node m.rows incoming })
  else (outcome, m)

/-- The observer clears a denied node with a fresh capture. -/
def clear (m : Machine) (node : Nat) : Machine :=
  (reload m (beginRead m.pub) node []).2

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

def withSlot (m : Machine) (node : Nat) (slot : Slot) : Machine :=
  { m with slots := update m.slots node slot }

def dropRead (m : Machine) (node : Nat) : Machine :=
  { m with reads := update m.reads node none }

def step (m : Machine) (op : Op) : Machine × Option Json :=
  let n := op.node
  let slot := m.slots n
  match op.op with
  | "source_started" => (dropRead (withSlot m n (sourceStarted slot)) n, none)
  | "armed" => (withSlot m n (armed slot), none)
  | "start" =>
      if startable slot then
        ({ withSlot m n (start slot) with reads := update m.reads n (some (beginRead m.pub)) },
          none)
      else (m, none)
  | "deliver" => (withSlot { m with pub := observerEvent m.pub n } n (queue slot), none)
  | "tool_call_event" => ({ m with pub := provenanceInvalidation m.pub }, none)
  | "transcript_event" => ({ m with pub := transcriptInvalidation m.pub }, none)
  | "stream_failed" =>
      (dropRead (withSlot { m with pub := requestEvent m.pub } n (streamFailed slot)) n, none)
  | "stream_denied" =>
      let m := clear { m with pub := requestEvent m.pub } n
      (dropRead (withSlot m n (streamDenied slot)) n, none)
  | "publish" => ((reload m (beginRead m.pub) n (snapshot n op.result)).2, none)
  | "seed_unowned" =>
      ({ m with pub := dataChange m.pub .all,
                rows := m.rows.filter (fun row => row.key != sharedKey) ++
                  [{ key := sharedKey, owner := none }] }, none)
  | "timer" => (withSlot m n (timer slot), none)
  | "finish" =>
      match slot.loading, m.reads n with
      | true, some read =>
          let patch := acceptsFor m.pub read (.nodes [n])
          let (outcome, next) := match loadResult op.result with
            | .rows => reload m read n (snapshot n op.result)
            | .transportFailed => (.transportFailed, m)
            | .denied => (.denied, clear m n)
          (dropRead (withSlot next n (settle slot outcome)) n,
            some (Json.mkObj [("settlement", toJson (settlementName outcome)),
              ("patch_accepted", toJson patch)]))
      | _, _ => (m, some Json.null)
  | _ => (m, none)

def slotJson (slot : Slot) : Json :=
  Json.mkObj [("pending", toJson slot.pending), ("waiting", toJson slot.waiting),
    ("loading", toJson slot.loading), ("armed", toJson slot.armed),
    ("status", toJson (statusName slot.status)),
    ("presentation", toJson (presentationName (present slot)))]

def rowJson (row : FootprintRow) : Json :=
  Json.mkObj [("key", toJson row.key), ("owner", toJson row.owner)]

def execute (trace : Trace) : Json := Id.run do
  let mut machine := initialMachine
  let mut steps : Array Json := #[]
  for op in trace.ops do
    let (next, result) := step machine op
    machine := next
    steps := steps.push (Json.mkObj [("slot", slotJson (machine.slots op.node)),
      ("result", result.getD Json.null)])
  return Json.mkObj [("steps", Json.arr steps),
    ("slots", Json.arr ((List.range trace.nodes).map (fun n => slotJson (machine.slots n))).toArray),
    ("rows", Json.arr (machine.rows.map rowJson).toArray)]

end Conformance.ObserverResync

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  match Lean.Json.parse input >>= Lean.fromJson? (α := List Conformance.ObserverResync.Trace) with
  | .error reason => throw (IO.userError reason)
  | .ok traces =>
      IO.println (Lean.toJson (traces.map Conformance.ObserverResync.execute)).compress
