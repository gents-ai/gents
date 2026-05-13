import Proofs.Conformance.Contracts.Machines
import Proofs.Conformance.Triggers.Contracts
import Proofs.Conformance.ClientShell.Contracts
import Proofs.ApplyReconcile.ContractCases
import Proofs.ToolExecution
import Proofs.MCPHealth.Executable
import Proofs.Conformance.Deviations
import Proofs.CommandPolicy.Cases
import Proofs.Conformance.CoverageLedger
import Proofs.Recovery.ContractCases
import Proofs.Identity.Conformance
import Proofs.Conformance.EventDelivery

/-!
# Conformance Snapshot JSON

JSON serializers and snapshot assembly for the Rust conformance emitter.
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
    ++ "\"pre_failed_state\":" ++ jsonString witness.preFailedState ++ ","
    ++ "\"post_latest_state\":" ++ jsonString witness.postLatestState ++ ","
    ++ "\"post_failed_state\":" ++ jsonString witness.postFailedState ++ ","
    ++ "\"post_new_state\":" ++ jsonString witness.postNewState ++ ","
    ++ "\"pre_latest_admission\":" ++ jsonString witness.preLatestAdmission ++ ","
    ++ "\"post_latest_admission\":" ++ jsonString witness.postLatestAdmission ++ ","
    ++ "\"pre_failed_admission\":" ++ jsonString witness.preFailedAdmission ++ ","
    ++ "\"post_failed_admission\":" ++ jsonString witness.postFailedAdmission ++ ","
    ++ "\"post_new_admission\":" ++ jsonString witness.postNewAdmission ++ ","
    ++ "\"pre_origin\":" ++ jsonString witness.preOrigin ++ ","
    ++ "\"post_new_origin\":" ++ jsonString witness.postNewOrigin ++ ","
    ++ "\"pre_backend\":" ++ jsonString witness.preBackend ++ ","
    ++ "\"post_new_backend\":" ++ jsonString witness.postNewBackend ++ ","
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
    ++ "\"pre_request_ids\":" ++ jsonArray (witness.preRequestIds.map toString) ++ ","
    ++ "\"pre_failed_exists\":" ++ boolString witness.preFailedExists ++ ","
    ++ "\"pre_latest_exists\":" ++ boolString witness.preLatestExists ++ ","
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

def nativeFilesystemBoundaryCaseJson
    (witness : NativeFilesystemBoundaryCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"tool_name\":" ++ jsonString witness.toolName ++ ","
    ++ "\"work_class\":" ++ jsonString witness.workClass ++ ","
    ++ "\"boundary\":" ++ jsonString witness.boundary ++ ","
    ++ "\"inner_poll_blocks\":" ++ boolString witness.innerPollBlocks ++ ","
    ++ "\"request_deadline_ms\":" ++ toString witness.requestDeadlineMs ++ ","
    ++ "\"blocker_ms\":" ++ toString witness.blockerMs ++ ","
    ++ "\"expected_terminal\":" ++ jsonString witness.expectedTerminal ++ ","
    ++ "\"expected_failure_class\":"
      ++ jsonOptionalString witness.expectedFailureClass ++ ","
    ++ "\"queue_advances_before_blocker_returns\":"
      ++ boolString witness.queueAdvancesBeforeBlockerReturns
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

def mcpHealthCaseJson (witness : Proofs.MCPHealth.TransitionCase) : String :=
  let nextCountStr : String :=
    match witness.nextCount with
    | none => "null"
    | some n => toString n
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"start_state\":" ++ jsonString witness.startState.toDefraDB ++ ","
    ++ "\"start_count\":" ++ toString witness.startCount ++ ","
    ++ "\"event\":" ++ jsonString witness.event.toDefraDB ++ ","
    ++ "\"threshold_k\":" ++ toString witness.thresholdK ++ ","
    ++ "\"next_state\":"
      ++ jsonOptionalString
          (witness.nextState.map Proofs.MCPHealth.HealthState.toDefraDB) ++ ","
    ++ "\"next_count\":" ++ nextCountStr ++ ","
    ++ "\"rust_projection\":" ++ jsonOptionalString witness.rustProjection
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

def liveOverlayCaseJson (witness : LiveOverlayCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"responseStatus\":" ++ jsonString witness.responseStatus ++ ","
    ++ "\"materialized\":" ++ boolString witness.materialized ++ ","
    ++ "\"precedingToolCalls\":" ++ toString witness.precedingToolCalls ++ ","
    ++ "\"turnTerminal\":" ++ boolString witness.turnTerminal ++ ","
    ++ "\"turnLabel\":" ++ jsonString witness.turnLabel ++ ","
    ++ "\"hasContent\":" ++ boolString witness.hasContent ++ ","
    ++ "\"hasReasoning\":" ++ boolString witness.hasReasoning ++ ","
    ++ "\"expectOverlay\":" ++ boolString witness.expectOverlay
    ++ "}"

def jsonOptionalNat : Option Nat → String
  | none => "null"
  | some value => toString value

def queueDeadlineConformanceCaseJson
    (witness : QueueDeadlineConformanceCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"session_id\":" ++ toString witness.sessionId ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_active_request_id\":"
      ++ jsonOptionalNat witness.preActiveRequestId ++ ","
    ++ "\"post_active_request_id\":"
      ++ jsonOptionalNat witness.postActiveRequestId ++ ","
    ++ "\"pre_pending_request_ids\":"
      ++ jsonArray (witness.prePendingRequestIds.map toString) ++ ","
    ++ "\"post_pending_request_ids\":"
      ++ jsonArray (witness.postPendingRequestIds.map toString) ++ ","
    ++ "\"claimed_request_id\":"
      ++ jsonOptionalNat witness.claimedRequestId ++ ","
    ++ "\"blocked_by_active\":" ++ boolString witness.blockedByActive ++ ","
    ++ "\"superseded_request_ids\":"
      ++ jsonArray (witness.supersededRequestIds.map toString) ++ ","
    ++ "\"queue_key\":" ++ jsonOptionalString witness.queueKey ++ ","
    ++ "\"post_coalesced_pending_count\":"
      ++ toString witness.postCoalescedPendingCount ++ ","
    ++ "\"automated_drained_request_ids\":"
      ++ jsonArray (witness.automatedDrainedRequestIds.map toString) ++ ","
    ++ "\"preserved_user_pending_request_ids\":"
      ++ jsonArray (witness.preservedUserPendingRequestIds.map toString) ++ ","
    ++ "\"post_terminal_request_ids\":"
      ++ jsonArray (witness.postTerminalRequestIds.map toString) ++ ","
    ++ "\"pre_request_deadline\":"
      ++ jsonOptionalNat witness.preRequestDeadline ++ ","
    ++ "\"synthesized_claim_deadline\":"
      ++ jsonOptionalNat witness.synthesizedClaimDeadline ++ ","
    ++ "\"post_deadline\":" ++ jsonOptionalNat witness.postDeadline ++ ","
    ++ "\"explicit_deadline_preserved\":"
      ++ boolString witness.explicitDeadlinePreserved
    ++ "}"

def recoverySweepCaseJson (witness : RecoverySweepCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"sweep_id\":" ++ jsonString witness.sweepId ++ ","
    ++ "\"collection\":" ++ jsonString witness.collection ++ ","
    ++ "\"rust_function\":" ++ jsonString witness.rustFunction ++ ","
    ++ "\"cadence\":" ++ jsonString witness.cadence ++ ","
    ++ "\"implementation_status\":"
      ++ jsonString witness.implementationStatus ++ ","
    ++ "\"pre_state\":" ++ jsonString witness.preState ++ ","
    ++ "\"terminal_state\":" ++ jsonString witness.terminalState ++ ","
    ++ "\"measure_before\":" ++ toString witness.measureBefore ++ ","
    ++ "\"measure_after\":" ++ toString witness.measureAfter ++ ","
    ++ "\"deadline_audit_ref\":"
      ++ jsonString witness.deadlineAuditRef
    ++ "}"

def transcriptCaseJson (witness : TranscriptCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"group\":" ++ jsonString witness.group ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"legal\":" ++ boolString witness.legal ++ ","
    ++ "\"pre_message_count\":" ++ toString witness.preMessageCount ++ ","
    ++ "\"post_message_count\":" ++ toString witness.postMessageCount ++ ","
    ++ "\"pre_tool_call_count\":" ++ toString witness.preToolCallCount ++ ","
    ++ "\"post_tool_call_count\":" ++ toString witness.postToolCallCount ++ ","
    ++ "\"pre_in_flight_count\":" ++ toString witness.preInFlightCount ++ ","
    ++ "\"post_in_flight_count\":" ++ toString witness.postInFlightCount ++ ","
    ++ "\"assistant_sequence\":" ++ toString witness.assistantSequence ++ ","
    ++ "\"result_sequence\":" ++ toString witness.resultSequence ++ ","
    ++ "\"logical_result_id\":" ++ toString witness.logicalResultId ++ ","
    ++ "\"payload_hash\":" ++ toString witness.payloadHash ++ ","
    ++ "\"expected_pair_closed\":" ++ boolString witness.expectedPairClosed ++ ","
    ++ "\"expected_ordered\":" ++ boolString witness.expectedOrdered ++ ","
    ++ "\"expected_duplicate_reused_sequence\":"
      ++ boolString witness.expectedDuplicateReusedSequence ++ ","
    ++ "\"expected_strong_drain\":" ++ boolString witness.expectedStrongDrain
    ++ "}"

def snapshotJson : String :=
  "{"
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ "\"vocabularies\":"
      ++ jsonArray (vocabularies.map VocabularyContract.toJson) ++ ","
    ++ "\"state_machines\":"
      ++ jsonArray (stateMachines.map StateMachineContract.toJson) ++ ","
    ++ "\"request_transition_cases\":"
      ++ jsonArray (requestTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"process_transition_cases\":"
      ++ jsonArray (processTransitionCases.map lifecycleTransitionCaseJson) ++ ","
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
    ++ "\"native_filesystem_boundary_cases\":"
      ++ jsonArray
        (nativeFilesystemBoundaryCases.map nativeFilesystemBoundaryCaseJson) ++ ","
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
    ++ "\"live_overlay_cases\":"
      ++ jsonArray (liveOverlayCases.map liveOverlayCaseJson) ++ ","
    ++ "\"queue_deadline_conformance_cases\":"
      ++ jsonArray
        (queueDeadlineConformanceCases.map queueDeadlineConformanceCaseJson) ++ ","
    ++ "\"recovery_sweep_cases\":"
      ++ jsonArray
        (Recovery.recoverySweepCases.map recoverySweepCaseJson) ++ ","
    ++ "\"transcript_conformance_cases\":"
      ++ jsonArray
        (transcriptConformanceCases.map transcriptCaseJson) ++ ","
    ++ "\"mcp_health_cases\":"
      ++ jsonArray
        (Proofs.MCPHealth.transitionCases.map mcpHealthCaseJson) ++ ","
    ++ "\"follow_up_hooks\":[],"
    ++ "\"event_delivery_transition_case_count\":"
      ++ toString Conformance.EventDelivery.transitionCaseCount ++ ","
    ++ "\"event_delivery_transition_cases\":"
      ++ Conformance.EventDelivery.transitionCasesJson ++ ","
    ++ "\"event_delivery_source_instances\":"
      ++ Conformance.EventDelivery.sourceInstancesJson ++ ","
    ++ "\"event_delivery_convergence_traces\":"
      ++ Conformance.EventDelivery.convergenceTracesJson ++ ","
    ++ "\"coverage_ledger\":"
      ++ coverageLedgerJson
    ++ ",\"identity_structural_cases\":"
      ++ Identity.Conformance.structuralCasesJson
    ++ ",\"identity_contracts\":"
      ++ Identity.Conformance.identityContractsJson
    ++ "}"

end Conformance.Contracts
