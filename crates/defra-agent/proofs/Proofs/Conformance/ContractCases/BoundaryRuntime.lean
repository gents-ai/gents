import Proofs.Persistence
import Proofs.StorageObservation
import Proofs.Conformance.ContractCases.Types

/-!
# Boundary Runtime Witness Cases

Finite executable cases for service-local decisions that sit next to external
boundaries. These rows deliberately classify only Rust-owned policy outcomes;
they do not claim DefraDB durability, event delivery, endpoint freshness, or
provider/network behavior.
-/

namespace Conformance.ContractCases

inductive MutationResult where
  | success
  | failure
  deriving DecidableEq, Repr

namespace MutationResult

def toContract : MutationResult -> String
  | .success => "success"
  | .failure => "failure"

end MutationResult

inductive BackendProbeStatus where
  | healthy
  | unhealthy
  | unknown
  | stale
  deriving DecidableEq, Repr

namespace BackendProbeStatus

def toDefraDB : BackendProbeStatus -> String
  | .healthy => "healthy"
  | .unhealthy => "unhealthy"
  | .unknown => "unknown"
  | .stale => "stale"

end BackendProbeStatus

def hookDecisionForFailure (policy : PersistenceState.FailurePolicy) : String :=
  match policy with
  | .failOpen => "continue"
  | .failClosed => "terminate"

def hookResultForMutation
    (policy : PersistenceState.FailurePolicy)
    (result : MutationResult) : String :=
  match result with
  | .success => "ok"
  | .failure =>
      match policy with
      | .failOpen => "ok"
      | .failClosed => "err"

def storageObservationForMutation
    (policy : PersistenceState.FailurePolicy)
    (result : MutationResult) : StorageObservation :=
  match result with
  | .success => .successAcknowledged
  | .failure =>
      match policy with
      | .failOpen => .lostAcknowledged
      | .failClosed => .mutationFailed

def terminalWriteObservedBool (obs : StorageObservation) : Bool :=
  match obs with
  | .successAcknowledged => true
  | .readVisible => true
  | _ => false

def persistenceFailurePolicyCase
    (name : String)
    (policy : PersistenceState.FailurePolicy) :
    PersistenceFailurePolicyCase :=
  let postPersistence :=
    match PersistenceState.step? policy .committing .writeFail with
    | some state => state
    | none => .uncommitted
  let postObservation := storageObservationForMutation policy .failure
  { name := name
  , policy := policy.toDefraDB
  , action := "writeFail"
  , prePersistence := PersistenceState.toDefraDB .committing
  , postPersistence := postPersistence.toDefraDB
  , postStorageObservation := postObservation.toContract
  , hookDecision := hookDecisionForFailure policy
  , recordsFailure := true
  , recordsSuccess := false
  , externalDurabilityClaimed := false
  }

def persistenceFailurePolicyCases : List PersistenceFailurePolicyCase :=
  [ persistenceFailurePolicyCase
      "fail_closed_write_failure_terminates_without_success_ack"
      .failClosed
  , persistenceFailurePolicyCase
      "fail_open_write_failure_continues_as_lost_without_success_ack"
      .failOpen
  ]

def storageObservationRuntimeCase
    (name : String)
    (policy : PersistenceState.FailurePolicy)
    (result : MutationResult) : StorageObservationRuntimeCase :=
  let postObservation := storageObservationForMutation policy result
  { name := name
  , policy := policy.toDefraDB
  , mutationResult := result.toContract
  , postObservation := postObservation.toContract
  , postPersistence := (StorageObservation.toPersistence postObservation).toDefraDB
  , hookResult := hookResultForMutation policy result
  , recordsFailure := if result = .failure then true else false
  , recordsSuccess := if result = .success then true else false
  , terminalWriteObserved := terminalWriteObservedBool postObservation
  , externalVisibilityClaimed := false
  }

def storageObservationRuntimeCases : List StorageObservationRuntimeCase :=
  [ storageObservationRuntimeCase
      "fail_closed_success_ack_counts_local_commit"
      .failClosed
      .success
  , storageObservationRuntimeCase
      "fail_open_success_ack_counts_local_commit"
      .failOpen
      .success
  , storageObservationRuntimeCase
      "fail_closed_failure_reports_mutation_failed"
      .failClosed
      .failure
  , storageObservationRuntimeCase
      "fail_open_failure_acknowledges_lost_output"
      .failOpen
      .failure
  ]

def backendAvailable (enabled : Bool) (probeStatus : BackendProbeStatus) : Bool :=
  enabled && (if probeStatus = .healthy then true else false)

def backendHealthAdmissionCase
    (name : String)
    (enabled : Bool)
    (probeStatus : BackendProbeStatus) : BackendHealthAdmissionCase :=
  let expectedAvailable := backendAvailable enabled probeStatus
  { name := name
  , enabled := enabled
  , probeStatus := probeStatus.toDefraDB
  , expectedAvailable := expectedAvailable
  , admissionDecision := if expectedAvailable then "available" else "unavailable"
  , observedDocumentOnly := true
  , externalEndpointFreshnessClaimed := false
  }

def backendHealthAdmissionCases : List BackendHealthAdmissionCase :=
  [ backendHealthAdmissionCase
      "enabled_healthy_backend_is_available_from_observed_document"
      true
      .healthy
  , backendHealthAdmissionCase
      "disabled_healthy_backend_is_unavailable_from_observed_document"
      false
      .healthy
  , backendHealthAdmissionCase
      "enabled_unhealthy_backend_is_unavailable_from_observed_document"
      true
      .unhealthy
  , backendHealthAdmissionCase
      "enabled_unknown_backend_is_unavailable_from_observed_document"
      true
      .unknown
  , backendHealthAdmissionCase
      "enabled_stale_backend_is_unavailable_from_observed_document"
      true
      .stale
  ]

end Conformance.ContractCases
