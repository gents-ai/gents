import Proofs.Clients.ClientShell.ObservationOrdering
import Lean

/-! Executes `ObserverResync` and `ClientDataPublication` on the observer
traces a native test serializes. Each node has its own store. After every
operation it reports the slot of the operation's node; a finished load also
reports its settlement and the patch fence's decision for its read, and a
finished collection reload its settlement. The final report adds every node's
slot and every node's store rows: its sessions, which only a full load reads,
and its backend observations, which a collection reload reads alone. -/
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
  pubs : Nat → State
  slots : Nat → Slot
  reads : Nat → Option Read
  rows : Nat → List String
  observations : Nat → List String

def emptyStore : State :=
  { dataEpoch := 0, provenanceEpoch := 0, requestEventEpoch := 0, transcriptEpoch := 0 }

def initialMachine : Machine :=
  { pubs := fun _ => emptyStore, slots := fun _ => initial, reads := fun _ => none,
    rows := fun _ => [], observations := fun _ => [] }

def update {α : Type} (f : Nat → α) (node : Nat) (value : α) : Nat → α :=
  fun other => if other = node then value else f other

/-- The rows a load or direct reload of `node` reads from its runtime. It
reads no backend observation: a published load replaces every collection. -/
def snapshot (node : Nat) : List String := [s!"session-{node}"]

/-- The backend observations a collection reload of `node` reads. -/
def observationRows (node : Nat) : List String := [s!"observation-{node}"]

def onStore (m : Machine) (node : Nat) (step : State → State) : Machine :=
  { m with pubs := Fleet.on m.pubs node step }

/-- A reload of `node`'s store publishing `incoming`, captured at `read`. -/
def reloadNode (m : Machine) (read : Read) (node : Nat) (incoming : List String) :
    Settlement × Machine :=
  let outcome := settlement (m.pubs node) read .rows
  if outcome = .published then
    (outcome, { onStore m node dataChange with rows := update m.rows node incoming,
                                                observations := update m.observations node [] })
  else (outcome, m)

/-- A collection reload of `node`'s backend observations, captured at `read`. -/
def reloadCollections (m : Machine) (read : Read) (node : Nat) (result : LoadResult) :
    CollectionSettlement × Machine :=
  let outcome := collectionSettlement (m.pubs node) read result
  if outcome = .published then
    (outcome, { onStore m node dataChange with
                observations := update m.observations node (observationRows node) })
  else (outcome, m)

/-- The observer clears a refused node's store with a fresh capture. -/
def clear (m : Machine) (node : Nat) : Machine :=
  (reloadNode m (beginRead (m.pubs node)) node []).2

def loadResult : String → LoadResult
  | "transport_failed" => .transportFailed
  | "refused" => .refused
  | _ => .rows

def settlementName : Settlement → String
  | .published => "published"
  | .superseded => "superseded"
  | .transportFailed => "transport_failed"
  | .refused => "refused"

def collectionSettlementName : CollectionSettlement → String
  | .published => "published"
  | .superseded => "superseded"
  | .failed => "failed"

def statusName : Status → String
  | .connecting => "connecting"
  | .connected => "connected"
  | .disconnected => "disconnected"
  | .refused => "refused"
  | .noEndpoint => "noEndpoint"

def presentationName : Presentation → String
  | .catchingUp => "catching_up"
  | .current => "current"
  | .retrying => "retrying"
  | .refused => "refused"
  | .noEndpoint => "no_endpoint"
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
        ({ withSlot m n (start slot) with
            reads := update m.reads n (some (beginRead (m.pubs n))) }, none)
      else (m, none)
  | "deliver" => (withSlot (onStore m n requestEvent) n (queue slot), none)
  | "deliver_collection" =>
      (withSlot (onStore m n requestEvent) n (queueCollection slot), none)
  | "start_collections" =>
      if collectionStartable slot then
        ({ withSlot m n (startCollections slot) with
            reads := update m.reads n (some (beginRead (m.pubs n))) }, none)
      else (m, none)
  | "finish_collections" =>
      match slot.reloadingCollections, m.reads n with
      | true, some read =>
          let (outcome, next) := reloadCollections m read n (loadResult op.result)
          (dropRead (withSlot next n (settleCollections slot outcome)) n,
            some (Json.mkObj [("collection_settlement",
              toJson (collectionSettlementName outcome))]))
      | _, _ => (m, some Json.null)
  | "tool_call_event" => (onStore m n provenanceInvalidation, none)
  | "transcript_event" => (onStore m n transcriptInvalidation, none)
  | "stream_failed" =>
      (dropRead (withSlot (onStore m n requestEvent) n (streamFailed slot)) n, none)
  | "stream_refused" =>
      let m := clear (onStore m n requestEvent) n
      (dropRead (withSlot m n (streamRefused slot)) n, none)
  | "publish" => ((reloadNode m (beginRead (m.pubs n)) n (snapshot n)).2, none)
  | "timer" => (withSlot m n (timer slot), none)
  | "retry_now" =>
      if retryNowRestarts slot then
        (dropRead (withSlot (onStore m n requestEvent) n (retryNow slot)) n, none)
      else (m, none)
  | "finish" =>
      match slot.loading, m.reads n with
      | true, some read =>
          let patch := accepts (m.pubs n) read
          let (outcome, next) := match loadResult op.result with
            | .rows => reloadNode m read n (snapshot n)
            | .transportFailed => (.transportFailed, m)
            | .refused => (.refused, clear m n)
          (dropRead (withSlot next n (settle slot outcome)) n,
            some (Json.mkObj [("settlement", toJson (settlementName outcome)),
              ("patch_accepted", toJson patch)]))
      | _, _ => (m, some Json.null)
  | _ => (m, none)

def slotJson (slot : Slot) : Json :=
  Json.mkObj [("pending", toJson slot.pending), ("waiting", toJson slot.waiting),
    ("loading", toJson slot.loading), ("armed", toJson slot.armed),
    ("collections", toJson slot.collections),
    ("reloading_collections", toJson slot.reloadingCollections),
    ("status", toJson (statusName slot.status)),
    ("presentation", toJson (presentationName (present slot)))]

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
    ("rows", Json.arr ((List.range trace.nodes).map (fun n => toJson (machine.rows n))).toArray),
    ("observations",
      Json.arr ((List.range trace.nodes).map (fun n => toJson (machine.observations n))).toArray)]

end Conformance.ObserverResync

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  match Lean.Json.parse input >>= Lean.fromJson? (α := List Conformance.ObserverResync.Trace) with
  | .error reason => throw (IO.userError reason)
  | .ok traces =>
      IO.println (Lean.toJson (traces.map Conformance.ObserverResync.execute)).compress
