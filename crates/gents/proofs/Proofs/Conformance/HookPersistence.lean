import Proofs.Store.Persistence
import Proofs.Store.StorageObservation
import Proofs.Conformance.Contracts.Machines.Catalog
import Proofs.Conformance.Family

namespace Conformance.HookPersistence

open Lean

inductive MutationResult where
  | success
  | failure
  deriving DecidableEq, Repr, ToJson

deriving instance ToJson for PersistenceState.FailurePolicy

def hookDecisionForFailure : PersistenceState.FailurePolicy → String
  | .failOpen => "continue"
  | .failClosed => "terminate"

def hookResultForMutation (policy : PersistenceState.FailurePolicy) :
    MutationResult → String
  | .success => "ok"
  | .failure =>
      match policy with
      | .failOpen => "ok"
      | .failClosed => "err"

def recordsFailure (result : MutationResult) : Bool := decide (result = .failure)

def recordsSuccess (result : MutationResult) : Bool := decide (result = .success)

def storageObservationForMutation (policy : PersistenceState.FailurePolicy) :
    MutationResult → StorageObservation
  | .success => .successAcknowledged
  | .failure =>
      match policy with
      | .failOpen => .lostAcknowledged
      | .failClosed => .mutationFailed

def terminalWriteObserved : StorageObservation → Bool
  | .successAcknowledged => true
  | .readVisible => true
  | _ => false

example : PersistenceState.step? .failClosed .committing .writeFail = some .uncommitted := rfl
example : PersistenceState.step? .failOpen .committing .writeFail = some .lost := rfl
example : storageObservationForMutation .failClosed .failure = .mutationFailed := rfl
example : storageObservationForMutation .failOpen .failure = .lostAcknowledged := rfl

structure FailureInput where
  name : String
  policy : PersistenceState.FailurePolicy
  deriving ToJson

structure FailureOutput where
  hookDecision : String
  recordsFailure : Bool
  recordsSuccess : Bool
  deriving ToJson

@[conformance_family]
def failure : Family FailureInput FailureOutput where
  inputs :=
    [ { name := "fail_closed_write_failure_terminates_without_success_ack", policy := .failClosed }
    , { name := "fail_open_write_failure_continues_as_lost_without_success_ack"
        policy := .failOpen } ]
  model := fun i =>
    { hookDecision := hookDecisionForFailure i.policy
      recordsFailure := recordsFailure .failure
      recordsSuccess := recordsSuccess .failure }

structure MutationInput where
  name : String
  policy : PersistenceState.FailurePolicy
  mutationResult : MutationResult
  deriving ToJson

structure MutationOutput where
  hookResult : String
  recordsFailure : Bool
  recordsSuccess : Bool
  deriving ToJson

@[conformance_family]
def mutation : Family MutationInput MutationOutput where
  inputs :=
    [ { name := "fail_closed_success_ack_counts_local_commit", policy := .failClosed
        mutationResult := .success }
    , { name := "fail_open_success_ack_counts_local_commit", policy := .failOpen
        mutationResult := .success }
    , { name := "fail_closed_failure_reports_mutation_failed", policy := .failClosed
        mutationResult := .failure }
    , { name := "fail_open_failure_acknowledges_lost_output", policy := .failOpen
        mutationResult := .failure } ]
  model := fun i =>
    { hookResult := hookResultForMutation i.policy i.mutationResult
      recordsFailure := recordsFailure i.mutationResult
      recordsSuccess := recordsSuccess i.mutationResult }

example : (mutation.inputs.map fun i =>
    terminalWriteObserved (storageObservationForMutation i.policy i.mutationResult)) =
    [true, true, false, false] := rfl

structure VisibilityInput where
  name : String
  policy : PersistenceState.FailurePolicy
  preObservation : String
  action : String
  deriving ToJson

structure VisibilityOutput where
  postObservation : String
  postPersistence : String
  terminalWriteObserved : Bool
  deriving ToJson

def visibilityCases :
    List (String × StorageObservation × String × StorageObservation.Action) :=
  [ ("read_your_writes_success_ack_is_visible", .successAcknowledged, "readYourWrites",
      .readYourWrites)
  , ("stale_read_preserves_committed_observation", .successAcknowledged, "staleRead", .staleRead)
  , ("stale_event_preserves_committed_observation", .successAcknowledged, "staleEvent",
      .staleEvent)
  , ("event_after_stale_observation_is_visible", .staleObserved, "eventArrives", .eventArrives) ]

def visibilityModel (i : VisibilityInput) : VisibilityOutput :=
  let (pre, step) := ((visibilityCases.find? (·.1 == i.name)).map fun c => (c.2.1, c.2.2.2)).getD
    (.noMutation, .beginMutation)
  let post := (StorageObservation.step? i.policy pre step).getD pre
  { postObservation := post.toContract
    postPersistence := (StorageObservation.toPersistence post).toDefraDB
    terminalWriteObserved := terminalWriteObserved post }

/-- model-production-gap: the hook observes only its own mutation
acknowledgements; no production owner reads a committed row back, so
read-your-writes, stale reads, stale events and late event delivery are
DefraDB observations no production path classifies. -/
@[conformance_family]
def visibilityUnreached : Family VisibilityInput VisibilityOutput where
  inputs := visibilityCases.map fun (name, pre, action, _) =>
    { name, policy := .failClosed, preObservation := pre.toContract, action }
  model := visibilityModel

end Conformance.HookPersistence
