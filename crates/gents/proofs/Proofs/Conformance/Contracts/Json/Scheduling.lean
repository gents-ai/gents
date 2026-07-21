import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases

/-!
# Scheduling JSON

Serializers for inference and fleet slot-accounting witness rows.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def inferenceSlotAccountingCaseJson (witness : InferenceSlotAccountingCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"property\":" ++ jsonString witness.property ++ ","
    ++ "\"backend_id\":" ++ jsonString witness.backendId ++ ","
    ++ "\"pre_state\":" ++ jsonString witness.preState ++ ","
    ++ "\"post_state\":" ++ jsonString witness.postState ++ ","
    ++ "\"contribution\":" ++ toString witness.contribution ++ ","
    ++ "\"expected_contribution\":" ++ toString witness.expectedContribution ++ ","
    ++ "\"pre_contribution\":" ++ toString witness.preContribution ++ ","
    ++ "\"post_contribution\":" ++ toString witness.postContribution ++ ","
    ++ "\"released_slot\":" ++ boolString witness.releasedSlot ++ ","
    ++ "\"permit_drop_terminalization\":"
      ++ boolString witness.permitDropTerminalization ++ ","
    ++ "\"row_states\":" ++ jsonStringArray witness.rowStates ++ ","
    ++ "\"row_backend_ids\":" ++ jsonStringArray witness.rowBackendIds ++ ","
    ++ "\"reconstructed_running_count\":"
      ++ toString witness.reconstructedRunningCount ++ ","
    ++ "\"max_concurrent\":" ++ toString witness.maxConcurrent ++ ","
    ++ "\"bounded_by_max_concurrent\":"
      ++ boolString witness.boundedByMaxConcurrent
    ++ "}"

def fleetSlotAccountingCaseJson (witness : FleetSlotAccountingCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"property\":" ++ jsonString witness.property ++ ","
    ++ "\"backend_id\":" ++ jsonString witness.backendId ++ ","
    ++ "\"request_state\":" ++ jsonString witness.requestState ++ ","
    ++ "\"admission_state\":" ++ jsonString witness.admissionState ++ ","
    ++ "\"contribution\":" ++ toString witness.contribution ++ ","
    ++ "\"expected_contribution\":" ++ toString witness.expectedContribution ++ ","
    ++ "\"active_count\":" ++ toString witness.activeCount ++ ","
    ++ "\"scheduler_running\":" ++ toString witness.schedulerRunning ++ ","
    ++ "\"slot_count\":" ++ toString witness.slotCount ++ ","
    ++ "\"row_states\":" ++ jsonStringArray witness.rowStates ++ ","
    ++ "\"row_backend_ids\":" ++ jsonStringArray witness.rowBackendIds ++ ","
    ++ "\"reconstructed_running_count\":"
      ++ toString witness.reconstructedRunningCount ++ ","
    ++ "\"max_concurrent\":" ++ toString witness.maxConcurrent ++ ","
    ++ "\"bounded_by_max_concurrent\":"
      ++ boolString witness.boundedByMaxConcurrent ++ ","
    ++ "\"aggregate_reconstructed_not_persisted\":"
      ++ boolString witness.aggregateReconstructedNotPersisted
    ++ "}"

end Conformance.Contracts
