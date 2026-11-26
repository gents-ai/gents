import Proofs.Clients.ClientShell.ObservationOrdering
import Lean

/-! Executes `ObserverResync` and `ClientDataPublication` on the observer
traces a native test serializes. Each node has its own store. After every
operation it reports the slot of the operation's node; a finished load also
reports its settlement and the patch fence's decision for its read, and a
finished partial read its settlement. The final report adds every node's slot
and every node's store rows: its backend observations, which a collection
reload reads alone, and its sessions and requests, which a session scope
reload replaces for its scope. -/
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
  requests : Nat → List String
  /-- The node's queued partial read holds collections, session scopes. -/
  queued : Nat → Bool × Bool
  /-- The same for its partial read in flight. -/
  reading : Nat → Bool × Bool

def emptyStore : State :=
  { dataEpoch := 0, provenanceEpoch := 0, requestEventEpoch := 0, transcriptEpoch := 0 }

def initialMachine : Machine :=
  { pubs := fun _ => emptyStore, slots := fun _ => initial, reads := fun _ => none,
    rows := fun _ => [], observations := fun _ => [], requests := fun _ => [],
    queued := fun _ => (false, false), reading := fun _ => (false, false) }

def update {α : Type} (f : Nat → α) (node : Nat) (value : α) : Nat → α :=
  fun other => if other = node then value else f other

/-- The rows a load or direct reload of `node` reads from its runtime. It
reads no backend observation: a published load replaces every collection. -/
def snapshot (node : Nat) : List String := [s!"session-{node}"]

/-- The backend observations a collection reload of `node` reads. -/
def observationRows (node : Nat) : List String := [s!"observation-{node}"]

/-- The requests a read of `node`'s one session scope shows. -/
def scopeRequests (node : Nat) : List String := [s!"request-{node}"]

/-- The requests a full load of `node` shows: its scope's, and on node 1 a
trigger candidate, which no read of the scope returns. -/
def nodeRequests (node : Nat) : List String :=
  scopeRequests node ++ (if node = 1 then [s!"triggered-{node}"] else [])

/-- `SessionRequestReload.derivesAlone` of `node`'s scope against its store:
the scope read returns no trigger-caused request. -/
def derivable (m : Machine) (node : Nat) : Bool :=
  !(m.reading node).2 ||
    SessionRequestReload.derivesAlone ((m.requests node).any (·.startsWith "triggered")) false

def onStore (m : Machine) (node : Nat) (step : State → State) : Machine :=
  { m with pubs := Fleet.on m.pubs node step }

/-- A reload of `node`'s store publishing `incoming`, captured at `read`. -/
def reloadNode (m : Machine) (read : Read) (node : Nat) (incoming : List String) :
    Settlement × Machine :=
  let outcome := settlement (m.pubs node) read .rows
  if outcome = .published then
    (outcome, { onStore m node dataChange with
                rows := update m.rows node incoming,
                observations := update m.observations node [],
                requests := update m.requests node (if incoming.isEmpty then [] else nodeRequests node) })
  else (outcome, m)

/-- A partial read of `node`, captured at `read`: its backend observations
when it holds collections, its scope's session row and requests when it holds
session scopes. -/
def reloadCollections (m : Machine) (read : Read) (node : Nat)
    (result : LoadResult) : CollectionSettlement × Machine :=
  let outcome := collectionSettlement (m.pubs node) read (derivable m node) result
  let (roots, scopes) := m.reading node
  if outcome = .published then
    (outcome, { onStore m node dataChange with
                observations :=
                  if roots then update m.observations node (observationRows node)
                  else m.observations
                rows := if scopes then update m.rows node (snapshot node) else m.rows
                requests :=
                  if scopes then update m.requests node (scopeRequests node) else m.requests })
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
  | .deferred => "deferred"

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
  { m with reads := update m.reads node none, reading := update m.reading node (false, false) }

/-- The parts queued for `node`'s partial read follow the slot: none once the
slot's `collections` is clear. -/
def syncQueued (m : Machine) (node : Nat) : Machine :=
  if (m.slots node).collections then m
  else { m with queued := update m.queued node (false, false) }

def queuePart (m : Machine) (node : Nat) (roots scopes : Bool) : Machine :=
  let (r, s) := m.queued node
  { m with queued := update m.queued node (r || roots, s || scopes) }

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
      (queuePart (withSlot (onStore m n requestEvent) n (queueCollection slot)) n true false, none)
  | "deliver_session" =>
      (queuePart (withSlot (onStore m n requestEvent) n (queueCollection slot)) n false true, none)
  | "start_collections" =>
      if collectionStartable slot then
        ({ withSlot m n (startCollections slot) with
            reads := update m.reads n (some (beginRead (m.pubs n)))
            reading := update m.reading n (m.queued n)
            queued := update m.queued n (false, false) }, none)
      else (m, none)
  | "finish_collections" =>
      match slot.reloadingCollections, m.reads n with
      | true, some read =>
          let (outcome, next) := reloadCollections m read n (loadResult op.result)
          let (roots, scopes) := m.reading n
          let next := if outcome = .superseded then queuePart next n roots scopes else next
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
    machine := syncQueued next op.node
    steps := steps.push (Json.mkObj [("slot", slotJson (machine.slots op.node)),
      ("result", result.getD Json.null)])
  return Json.mkObj [("steps", Json.arr steps),
    ("slots", Json.arr ((List.range trace.nodes).map (fun n => slotJson (machine.slots n))).toArray),
    ("rows", Json.arr ((List.range trace.nodes).map (fun n => toJson (machine.rows n))).toArray),
    ("observations",
      Json.arr ((List.range trace.nodes).map (fun n => toJson (machine.observations n))).toArray),
    ("requests",
      Json.arr ((List.range trace.nodes).map (fun n => toJson (machine.requests n))).toArray)]

end Conformance.ObserverResync

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  match Lean.Json.parse input >>= Lean.fromJson? (α := List Conformance.ObserverResync.Trace) with
  | .error reason => throw (IO.userError reason)
  | .ok traces =>
      IO.println (Lean.toJson (traces.map Conformance.ObserverResync.execute)).compress
