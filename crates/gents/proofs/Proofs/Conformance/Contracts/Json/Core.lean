import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases

/-!
# Core Lifecycle JSON

Serializers for request/process lifecycle, persistence, storage observation,
and backend admission witness rows.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def lifecycleTransitionCaseJson (witness : LifecycleTransitionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"domain\":" ++ jsonString witness.domain ++ ","
    ++ "\"from\":" ++ jsonString witness.fromState ++ ","
    ++ "\"to\":" ++ jsonString witness.toState ++ ","
    ++ "\"classification\":" ++ jsonString witness.classification ++ ","
    ++ "\"action\":" ++ jsonOptionalString witness.action ++ ","
    ++ "\"boundary\":" ++ jsonOptionalString witness.boundary
    ++ "}"

def persistenceFailurePolicyCaseJson
    (witness : PersistenceFailurePolicyCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"policy\":" ++ jsonString witness.policy ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"pre_persistence\":" ++ jsonString witness.prePersistence ++ ","
    ++ "\"post_persistence\":" ++ jsonString witness.postPersistence ++ ","
    ++ "\"post_storage_observation\":"
      ++ jsonString witness.postStorageObservation ++ ","
    ++ "\"hook_decision\":" ++ jsonString witness.hookDecision ++ ","
    ++ "\"records_failure\":" ++ boolString witness.recordsFailure ++ ","
    ++ "\"records_success\":" ++ boolString witness.recordsSuccess ++ ","
    ++ "\"external_durability_claimed\":"
      ++ boolString witness.externalDurabilityClaimed
    ++ "}"

def storageObservationRuntimeCaseJson
    (witness : StorageObservationRuntimeCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"policy\":" ++ jsonString witness.policy ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"pre_observation\":" ++ jsonString witness.preObservation ++ ","
    ++ "\"mutation_result\":" ++ jsonString witness.mutationResult ++ ","
    ++ "\"post_observation\":" ++ jsonString witness.postObservation ++ ","
    ++ "\"post_persistence\":" ++ jsonString witness.postPersistence ++ ","
    ++ "\"hook_result\":" ++ jsonString witness.hookResult ++ ","
    ++ "\"records_failure\":" ++ boolString witness.recordsFailure ++ ","
    ++ "\"records_success\":" ++ boolString witness.recordsSuccess ++ ","
    ++ "\"terminal_write_observed\":"
      ++ boolString witness.terminalWriteObserved ++ ","
    ++ "\"external_visibility_claimed\":"
      ++ boolString witness.externalVisibilityClaimed
    ++ "}"

def backendHealthAdmissionCaseJson
    (witness : BackendHealthAdmissionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"enabled\":" ++ boolString witness.enabled ++ ","
    ++ "\"probe_status\":" ++ jsonString witness.probeStatus ++ ","
    ++ "\"expected_available\":"
      ++ boolString witness.expectedAvailable ++ ","
    ++ "\"admission_decision\":"
      ++ jsonString witness.admissionDecision ++ ","
    ++ "\"observed_document_only\":"
      ++ boolString witness.observedDocumentOnly ++ ","
    ++ "\"external_endpoint_freshness_claimed\":"
      ++ boolString witness.externalEndpointFreshnessClaimed
    ++ "}"

end Conformance.Contracts
