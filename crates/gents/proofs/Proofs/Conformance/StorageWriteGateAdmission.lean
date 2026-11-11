import Proofs.Store.StorageWriteGate
import Proofs.Conformance.Family

namespace Conformance.StorageWriteGateAdmission
open StorageWriteGate StorageWriteGate.Admission

inductive Action where
  | request (owner : Nat)
  | observe (owner : Nat) (event : Event)
  | release (owner : Nat)
  deriving DecidableEq, Repr

inductive StepKind where
  | request | observe | release
  deriving DecidableEq, Repr

deriving instance Lean.ToJson for StepKind, StorageWriteGate.Event

structure Step where
  kind : StepKind
  owner : Nat
  event : Option StorageWriteGate.Event
  deriving Lean.ToJson

def Action.toStep : Action → Step
  | .request owner => ⟨.request, owner, none⟩
  | .observe owner event => ⟨.observe, owner, some event⟩
  | .release owner => ⟨.release, owner, none⟩

/-- An observation without an event is the identity `elapsed`. -/
def Step.toAction (step : Step) : Action :=
  match step.kind, step.event with
  | .request, _ => .request step.owner
  | .release, _ => .release step.owner
  | .observe, some event => .observe step.owner event
  | .observe, none => .observe step.owner .elapsed

structure Input where
  name : String
  initialClaimed : Bool
  actions : List Step
  deriving Lean.ToJson

def independentLeaseActions : List Action :=
  [ .request 1
  , .request 2
  , .observe 1 .storageReturned
  , .observe 1 .release
  , .release 1
  ]

def capacityActions : List Action :=
  (List.range (capacity + 1)).map Action.request

def cases : List Input :=
  [ { name := "independent_leases_survive_sibling_completion"
      initialClaimed := false
      actions := independentLeaseActions.map Action.toStep }
  , { name := "sixty_fifth_request_waits_at_capacity"
      initialClaimed := true
      actions := capacityActions.map Action.toStep } ]

def applyAction (pool : Pool) : Action → Pool
  | .request owner =>
      let requested := request pool owner
      (admitNext? requested).getD requested
  | .observe owner event => observe pool owner event
  | .release owner =>
      let released := release pool owner
      (admitNext? released).getD released

def trace (initial : Pool) : List Action → List Pool
  | [] => [initial]
  | action :: rest => initial :: trace (applyAction initial action) rest

def initialPool (testCase : Input) : Pool :=
  if testCase.initialClaimed then claimed else empty

structure Snapshot where
  activeCount : Nat
  activeOwners : List Nat
  waiting : List Nat
  storeClaimHeld : Bool
  deriving Lean.ToJson

structure Output where
  capacity : Nat
  snapshots : List Snapshot
  deriving Lean.ToJson

def snapshot (pool : Pool) : Snapshot :=
  { activeCount := activeCount pool
    activeOwners := pool.active.map Prod.fst
    waiting := pool.waiting
    storeClaimHeld := pool.storeClaimHeld }

@[conformance_family]
def leaseTrace : Conformance.Family Input Output where
  inputs := cases
  model := fun i =>
    let initial := initialPool i
    let states := trace initial (i.actions.map Step.toAction)
    { capacity := StorageWriteGate.Admission.capacity
      snapshots := (List.range i.actions.length).map fun index =>
        snapshot (states.getD (index + 1) initial) }

theorem independent_lease_trace_is_derived_from_model :
    let states := trace empty independentLeaseActions
    activeCount (states.getD 2 empty) = 2 ∧
      (ownerState? (states.getD 3 empty) 2).map State.phase = some .storage ∧
      activeCount (states.getD 5 empty) = 1 := by
  decide

set_option maxRecDepth 10000 in
theorem capacity_trace_keeps_sixty_fifth_waiting :
    let states := trace claimed capacityActions
    let final := states.getD (capacity + 1) claimed
    activeCount final = capacity ∧ final.waiting = [capacity] ∧
      final.storeClaimHeld := by
  decide

end Conformance.StorageWriteGateAdmission
