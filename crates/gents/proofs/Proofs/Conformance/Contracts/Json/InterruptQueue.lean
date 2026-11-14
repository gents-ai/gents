import Proofs.Session.SessionQueue.InterruptCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.InterruptQueueContracts

open Lean SessionQueue SessionQueue.InterruptCases

structure Entry where
  requestId : Nat
  createdAt : Nat
  executionOrigin : String
  source : String
  policy : String
  queueKey : Option Nat
  queuedAfter : Option Nat
  deriving ToJson

structure EventWire where
  kind : String
  entry : Option Entry
  deriving ToJson

structure Input where
  name : String
  nodeId : Nat
  requesterId : Option Nat
  sessionId : Nat
  activeRequestId : Option Nat
  inputs : List EventWire
  deriving ToJson

/-- Native publisher timestamps do not bind the modeled order of equal-time
pending entries, so pending membership is observed in request-id order. -/
structure Observation where
  pending : List Nat
  terminal : List Nat
  latched : Bool
  deriving ToJson

def Entry.ofModel (entry : QueueEntry) : Entry :=
  { requestId := entry.requestId, createdAt := entry.createdAt
    executionOrigin := entry.origin.toDefraDB, source := entry.source.toDefraDB
    policy := entry.policy.toDefraDB, queueKey := entry.queueKey
    queuedAfter := entry.queuedAfter }

def originOf (name : String) : ExecutionOrigin :=
  if name == ExecutionOrigin.scheduled.toDefraDB then .scheduled else .interactive

def Entry.toModel (entry : Entry) : QueueEntry :=
  { requestId := entry.requestId, createdAt := entry.createdAt
    source := (QueueSource.fromDefraDB? entry.source).getD .user
    policy := (QueuePolicy.fromDefraDB? entry.policy).getD .append
    queueKey := entry.queueKey, queuedAfter := entry.queuedAfter
    origin := originOf entry.executionOrigin }

def EventWire.ofModel : Event → EventWire
  | .interrupt => ⟨"interrupt", none⟩
  | .captureInterrupt => ⟨"capture_interrupt", none⟩
  | .commitInterrupt => ⟨"commit_interrupt", none⟩
  | .enqueue entry => ⟨"enqueue", some (.ofModel entry)⟩

def EventWire.toModel (event : EventWire) : Event :=
  match event.kind, event.entry with
  | "enqueue", some entry => .enqueue entry.toModel
  | "capture_interrupt", _ => .captureInterrupt
  | "commit_interrupt", _ => .commitInterrupt
  | _, _ => .interrupt

example : cases.all (fun c => c.2.all fun event => match event with
    | .enqueue entry => (Entry.ofModel entry).toModel.requestId == entry.requestId &&
        (Entry.ofModel entry).toModel.source == entry.source &&
        (Entry.ofModel entry).toModel.policy == entry.policy &&
        (Entry.ofModel entry).toModel.origin == entry.origin
    | _ => true) := by native_decide

def Input.ofCase (c : String × List Event) : Input :=
  { name := c.1, nodeId := queue.scope.node, requesterId := queue.scope.requester
    sessionId := queue.scope.session, activeRequestId := queue.active
    inputs := c.2.map EventWire.ofModel }

def observe (i : Input) : Option Observation :=
  (observation (i.inputs.map EventWire.toModel)).map fun (pending, terminal, latched) =>
    { pending := pending.mergeSort (fun a b => a ≤ b), terminal, latched }

def overlapping (c : String × List Event) : Bool :=
  c.2.any fun event => match event with
    | .captureInterrupt | .commitInterrupt => true
    | _ => false

/-- The sequential runner drives each event through the native Local and HTTP
interrupt owners in order; a captured-then-committed interrupt needs the
paused HTTP transaction of the overlap runner. -/
@[conformance_family]
def sequential : Family Input (Option Observation) where
  inputs := (cases.filter fun c => !overlapping c).map Input.ofCase
  model := observe

/-- The overlap runner pauses a real HTTP interrupt transaction after its
queue scan and publishes a completion during or after that pause. -/
@[conformance_family]
def overlap : Family Input (Option Observation) where
  inputs := (cases.filter overlapping).map Input.ofCase
  model := observe

/-- The desktop runner interrupts as the signing User with the selected
Agent's observer running, after a rollback of the same latch and drain; it
publishes one pending wake before the interrupt. -/
@[conformance_family]
def completionBeforeInterrupt : Family Input (Option Observation) where
  inputs := (cases.filter fun c => c.1 == "completion_before_interrupt").map Input.ofCase
  model := observe

end Conformance.InterruptQueueContracts
