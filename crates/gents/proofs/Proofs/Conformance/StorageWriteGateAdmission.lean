import Proofs.StorageWriteGate
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.StorageWriteGateAdmission
open StorageWriteGate StorageWriteGate.Admission Conformance.Contracts

inductive Action where
  | request (owner : Nat)
  | observe (owner : Nat) (event : Event)
  | release (owner : Nat)
  deriving DecidableEq, Repr

structure Case where
  name : String
  initialClaimed : Bool
  actions : List Action
  checkpoints : List Nat
  deriving DecidableEq, Repr

def independentLeaseActions : List Action :=
  [ .request 1
  , .request 2
  , .observe 1 .storageReturned
  , .observe 1 .release
  , .release 1
  ]

def capacityActions : List Action :=
  (List.range (capacity + 1)).map Action.request

def cases : List Case :=
  [ ⟨"independent_leases_survive_sibling_completion", false,
      independentLeaseActions,
      (List.range independentLeaseActions.length).map Nat.succ⟩
  , ⟨"sixty_fifth_request_waits_at_capacity", true,
      capacityActions, (List.range capacityActions.length).map Nat.succ⟩
  ]

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

def initialPool (testCase : Case) : Pool :=
  if testCase.initialClaimed then claimed else empty

def phaseName : Phase → String
  | .storage => "storage"
  | .cleanup => "cleanup"
  | .dropping => "dropping"
  | .releasable => "releasable"
  | .released => "released"

def eventName : Event → String
  | .siblingWait => "sibling_wait"
  | .elapsed => "elapsed"
  | .rollbackOwnerScheduled => "rollback_owner_scheduled"
  | .storageReturned => "storage_returned"
  | .deadlineObserved => "deadline_observed"
  | .cleanupReturned => "cleanup_returned"
  | .cleanupDeadlineObserved => "cleanup_deadline_observed"
  | .dropReturned => "drop_returned"
  | .release => "release"

def natArray (values : List Nat) : String :=
  jsonArray (values.map toString)

def actionJson : Action → String
  | .request owner =>
      "{\"kind\":\"request\",\"owner\":" ++ toString owner ++ "}"
  | .observe owner event =>
      "{\"kind\":\"observe\",\"owner\":" ++ toString owner ++
        ",\"event\":" ++ jsonString (eventName event) ++ "}"
  | .release owner =>
      "{\"kind\":\"release\",\"owner\":" ++ toString owner ++ "}"

def leaseJson (entry : Nat × State) : String :=
  "{\"owner\":" ++ toString entry.1 ++
    ",\"phase\":" ++ jsonString (phaseName entry.2.phase) ++ "}"

def snapshotJson (afterActions : Nat) (pool : Pool) : String :=
  "{\"after_actions\":" ++ toString afterActions ++
    ",\"active_count\":" ++ toString (activeCount pool) ++
    ",\"active_owners\":" ++ natArray (pool.active.map Prod.fst) ++
    ",\"waiting\":" ++ natArray pool.waiting ++
    ",\"leases\":" ++ jsonArray (pool.active.map leaseJson) ++
    ",\"store_claim_held\":" ++ (if pool.storeClaimHeld then "true" else "false") ++
    "}"

def checkpointJson (testCase : Case) (states : List Pool) (index : Nat) : String :=
  snapshotJson index (states.getD index (initialPool testCase))

def caseJson (testCase : Case) : String :=
  let states := trace (initialPool testCase) testCase.actions
  "{\"name\":" ++ jsonString testCase.name ++
    ",\"capacity\":" ++ toString capacity ++
    ",\"initial_claimed\":" ++ (if testCase.initialClaimed then "true" else "false") ++
    ",\"actions\":" ++ jsonArray (testCase.actions.map actionJson) ++
    ",\"checkpoints\":" ++ jsonArray
      (testCase.checkpoints.map (checkpointJson testCase states)) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

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

def main : IO Unit := IO.println Conformance.StorageWriteGateAdmission.casesJson
