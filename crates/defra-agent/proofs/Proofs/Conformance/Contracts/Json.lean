import Proofs.Conformance.Contracts.Machines
import Proofs.Conformance.Triggers.Contracts
import Proofs.Conformance.ClientShell.Contracts
import Proofs.ApplyReconcile.ContractCases
import Proofs.ToolExecution
import Proofs.Conformance.Deviations
import Proofs.CommandPolicy.Cases
import Proofs.Conformance.CoverageLedger

/-!
# Conformance Snapshot JSON

JSON serializers and snapshot assembly for the Rust conformance emitter.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

def runtimeReconcileCaseJson (witness : RuntimeReconcileCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_phase\":" ++ jsonString witness.prePhase ++ ","
    ++ "\"post_phase\":" ++ jsonString witness.postPhase ++ ","
    ++ "\"pre_active_generation\":" ++ toString witness.preActiveGeneration ++ ","
    ++ "\"post_active_generation\":" ++ toString witness.postActiveGeneration ++ ","
    ++ "\"pre_router_generation\":" ++ toString witness.preRouterGeneration ++ ","
    ++ "\"post_router_generation\":" ++ toString witness.postRouterGeneration ++ ","
    ++ "\"pre_ready_generation_count\":" ++ toString witness.preReadyGenerationCount ++ ","
    ++ "\"post_ready_generation_count\":" ++ toString witness.postReadyGenerationCount ++ ","
    ++ "\"pre_live_generation_count\":" ++ toString witness.preLiveGenerationCount ++ ","
    ++ "\"post_live_generation_count\":" ++ toString witness.postLiveGenerationCount ++ ","
    ++ "\"pre_in_flight_count\":" ++ toString witness.preInFlightCount ++ ","
    ++ "\"post_in_flight_count\":" ++ toString witness.postInFlightCount ++ ","
    ++ "\"tracked_request_id\":" ++ toString witness.trackedRequestId ++ ","
    ++ "\"tracked_session_id\":" ++ toString witness.trackedSessionId ++ ","
    ++ "\"tracked_request_generation\":" ++ toString witness.trackedRequestGeneration ++ ","
    ++ "\"tracked_request_session\":" ++ toString witness.trackedRequestSession ++ ","
    ++ "\"tracked_request_behavior\":" ++ toString witness.trackedRequestBehavior ++ ","
    ++ "\"tracked_session_behavior\":" ++ toString witness.trackedSessionBehavior
    ++ "}"

def sessionRecoveryCaseJson (witness : SessionRecoveryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_latest_state\":" ++ jsonString witness.preLatestState ++ ","
    ++ "\"post_latest_state\":" ++ jsonString witness.postLatestState ++ ","
    ++ "\"pre_latest_admission\":" ++ jsonString witness.preLatestAdmission ++ ","
    ++ "\"post_latest_admission\":" ++ jsonString witness.postLatestAdmission ++ ","
    ++ "\"pre_failed_admission\":" ++ jsonString witness.preFailedAdmission ++ ","
    ++ "\"post_failed_admission\":" ++ jsonString witness.postFailedAdmission ++ ","
    ++ "\"post_new_admission\":" ++ jsonString witness.postNewAdmission ++ ","
    ++ "\"failed_id\":" ++ toString witness.failedId ++ ","
    ++ "\"new_id\":" ++ toString witness.newId ++ ","
    ++ "\"pre_latest_id\":" ++ toString witness.preLatestId ++ ","
    ++ "\"post_latest_id\":" ++ toString witness.postLatestId ++ ","
    ++ "\"pre_session_id\":" ++ toString witness.preSessionId ++ ","
    ++ "\"post_session_id\":" ++ toString witness.postSessionId ++ ","
    ++ "\"pre_behavior_id\":" ++ toString witness.preBehaviorId ++ ","
    ++ "\"post_behavior_id\":" ++ toString witness.postBehaviorId ++ ","
    ++ "\"pre_request_count\":" ++ toString witness.preRequestCount ++ ","
    ++ "\"post_request_count\":" ++ toString witness.postRequestCount ++ ","
    ++ "\"pre_retry_count\":" ++ toString witness.preRetryCount ++ ","
    ++ "\"post_retry_count\":" ++ toString witness.postRetryCount ++ ","
    ++ "\"max_retries\":" ++ toString witness.maxRetries ++ ","
    ++ "\"pre_deadline_exceeded\":" ++ boolString witness.preDeadlineExceeded ++ ","
    ++ "\"post_deadline_exceeded\":" ++ boolString witness.postDeadlineExceeded ++ ","
    ++ "\"pre_failed_is_latest\":" ++ boolString witness.preFailedIsLatest ++ ","
    ++ "\"post_failed_is_latest\":" ++ boolString witness.postFailedIsLatest ++ ","
    ++ "\"post_new_is_latest\":" ++ boolString witness.postNewIsLatest ++ ","
    ++ "\"pre_new_request_exists\":" ++ boolString witness.preNewRequestExists ++ ","
    ++ "\"old_request_retained\":" ++ boolString witness.oldRequestRetained ++ ","
    ++ "\"new_request_inserted\":" ++ boolString witness.newRequestInserted ++ ","
    ++ "\"origin_preserved\":" ++ boolString witness.originPreserved ++ ","
    ++ "\"backend_preserved\":" ++ boolString witness.backendPreserved
    ++ "}"

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

def toolPreflightCaseJson (witness : ToolExecution.PreflightCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"health\":" ++ jsonString witness.health.toDefraDB ++ ","
    ++ "\"schema_status\":" ++ jsonString witness.schema.toDefraDB ++ ","
    ++ "\"decision\":" ++ jsonString witness.decision.toContract ++ ","
    ++ "\"failure_class\":"
      ++ jsonOptionalString ((witness.decision.failureClass).map ToolExecution.FailureClass.toDefraDB)
    ++ "}"

def toolRetryCaseJson (witness : ToolExecution.RetryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"operation\":" ++ jsonString witness.operation.toDefraDB ++ ","
    ++ "\"idempotency\":" ++ jsonString witness.idempotency.toDefraDB ++ ","
    ++ "\"failure_class\":" ++ jsonString witness.failure.toDefraDB ++ ","
    ++ "\"disposition\":" ++ jsonString witness.disposition.toDefraDB
    ++ "}"

def jsonStringMatrix (values : List (List String)) : String :=
  jsonArray (values.map jsonStringArray)

def jsonOptionalStringArray : Option (List String) → String
  | none => "null"
  | some values => jsonStringArray values

def commandPolicyCaseJson (witness : CommandPolicy.CommandPolicyCase) : String :=
  let reason := witness.decision.denialReason?
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"category\":" ++ jsonString witness.category ++ ","
    ++ "\"mode\":" ++ jsonString witness.policy.mode.toDefraDB ++ ","
    ++ "\"allowed_argv_prefixes\":"
      ++ jsonStringMatrix witness.policy.allowedArgvPrefixes ++ ","
    ++ "\"forbidden_argv_prefixes\":"
      ++ jsonStringMatrix witness.policy.forbiddenArgvPrefixes ++ ","
    ++ "\"network_mode\":" ++ jsonString witness.policy.networkMode.toDefraDB ++ ","
    ++ "\"read_only_allowlist\":"
      ++ jsonStringArray witness.policy.readOnlyAllowlist ++ ","
    ++ "\"command\":" ++ jsonString witness.request.command ++ ","
    ++ "\"lookup_command\":" ++ jsonString witness.request.lookupCommand ++ ","
    ++ "\"args\":" ++ jsonStringArray witness.request.args ++ ","
    ++ "\"decision\":" ++ jsonString witness.decision.toContract ++ ","
    ++ "\"denial_reason\":"
      ++ jsonOptionalString (reason.map CommandPolicy.DenialReason.toContract) ++ ","
    ++ "\"matched_prefix\":"
      ++ jsonOptionalStringArray (reason.bind CommandPolicy.DenialReason.matchedPrefix?) ++ ","
    ++ "\"denied_argv\":"
      ++ jsonOptionalStringArray (reason.bind CommandPolicy.DenialReason.argv?) ++ ","
    ++ "\"denied_command\":"
      ++ jsonOptionalString (reason.bind CommandPolicy.DenialReason.command?) ++ ","
    ++ "\"denied_argument\":"
      ++ jsonOptionalString (reason.bind CommandPolicy.DenialReason.argument?) ++ ","
    ++ "\"denied_subcommand\":"
      ++ jsonOptionalString (reason.bind CommandPolicy.DenialReason.subcommand?)
    ++ "}"

def commandSandboxCaseJson (witness : CommandPolicy.CommandSandboxCase) : String :=
  let reason := witness.decision.denialReason?
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"category\":" ++ jsonString witness.category ++ ","
    ++ "\"mode\":" ++ jsonString witness.mode.toDefraDB ++ ","
    ++ "\"workspace_write_sandbox_enforced\":"
      ++ boolString witness.workspaceWriteSandboxEnforced ++ ","
    ++ "\"decision\":" ++ jsonString witness.decision.toContract ++ ","
    ++ "\"sandbox\":"
      ++ jsonOptionalString ((witness.decision.sandbox?).map CommandPolicy.SandboxKind.toContract) ++ ","
    ++ "\"denial_reason\":"
      ++ jsonOptionalString (reason.map CommandPolicy.DenialReason.toContract)
    ++ "}"

def commandEnvCaseJson (witness : CommandPolicy.CommandEnvCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"env_key\":" ++ jsonString witness.envKey.toContract ++ ","
    ++ "\"input_present\":" ++ boolString witness.inputPresent ++ ","
    ++ "\"input_name\":" ++ jsonString witness.inputName ++ ","
    ++ "\"input_value\":" ++ jsonString witness.inputValue ++ ","
    ++ "\"output_name\":" ++ jsonString witness.outputName ++ ","
    ++ "\"expected_value_kind\":"
      ++ jsonOptionalString (witness.expected.map CommandPolicy.EnvValue.toContract) ++ ","
    ++ "\"expected_output_value\":"
      ++ jsonOptionalString (witness.expected.map (fun value => value.toRustValue witness.inputValue))
    ++ "}"

def snapshotJson : String :=
  "{"
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ "\"vocabularies\":"
      ++ jsonArray (vocabularies.map VocabularyContract.toJson) ++ ","
    ++ "\"state_machines\":"
      ++ jsonArray (stateMachines.map StateMachineContract.toJson) ++ ","
    ++ "\"trigger_dispatch_case_count\":"
      ++ toString Conformance.TriggerContracts.triggerDispatchCaseCount ++ ","
    ++ "\"trigger_dispatch_cases\":"
      ++ Conformance.TriggerContracts.triggerDispatchCasesJson ++ ","
    ++ "\"frontend_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.frontendClientShellCaseCount ++ ","
    ++ "\"frontend_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.frontendClientShellCasesJson ++ ","
    ++ "\"desktop_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.desktopClientShellCaseCount ++ ","
    ++ "\"desktop_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.desktopClientShellCasesJson ++ ","
    ++ "\"runtime_reconcile_cases\":"
      ++ jsonArray (runtimeReconcileCases.map runtimeReconcileCaseJson) ++ ","
    ++ "\"apply_reconcile_cases\":"
      ++ ApplyReconcile.ContractCases.applyReconcileCasesJson ++ ","
    ++ "\"session_recovery_cases\":"
      ++ jsonArray (sessionRecoveryCases.map sessionRecoveryCaseJson) ++ ","
    ++ "\"inference_slot_accounting_cases\":"
      ++ jsonArray (inferenceSlotAccountingCases.map inferenceSlotAccountingCaseJson) ++ ","
    ++ "\"fleet_slot_accounting_cases\":"
      ++ jsonArray (fleetSlotAccountingCases.map fleetSlotAccountingCaseJson) ++ ","
    ++ "\"persistence_failure_policy_cases\":"
      ++ jsonArray
        (persistenceFailurePolicyCases.map persistenceFailurePolicyCaseJson) ++ ","
    ++ "\"storage_observation_runtime_cases\":"
      ++ jsonArray
        (storageObservationRuntimeCases.map storageObservationRuntimeCaseJson) ++ ","
    ++ "\"backend_health_admission_cases\":"
      ++ jsonArray
        (backendHealthAdmissionCases.map backendHealthAdmissionCaseJson) ++ ","
    ++ "\"tool_preflight_cases\":"
      ++ jsonArray (ToolExecution.preflightCases.map toolPreflightCaseJson) ++ ","
    ++ "\"tool_retry_cases\":"
      ++ jsonArray (ToolExecution.retryCases.map toolRetryCaseJson) ++ ","
    ++ "\"boundaries\":"
      ++ boundariesJson ++ ","
    ++ "\"deviations\":"
      ++ deviationsJson ++ ","
    ++ "\"command_policy_cases\":"
      ++ jsonArray (CommandPolicy.commandPolicyCases.map commandPolicyCaseJson) ++ ","
    ++ "\"command_sandbox_cases\":"
      ++ jsonArray (CommandPolicy.commandSandboxCases.map commandSandboxCaseJson) ++ ","
    ++ "\"command_env_cases\":"
      ++ jsonArray (CommandPolicy.commandEnvCases.map commandEnvCaseJson) ++ ","
    ++ "\"follow_up_hooks\":[],"
    ++ "\"coverage_ledger\":"
      ++ coverageLedgerJson
    ++ "}"

end Conformance.Contracts
