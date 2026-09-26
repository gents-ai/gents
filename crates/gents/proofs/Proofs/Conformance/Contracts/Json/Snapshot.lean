import Proofs.Conformance.ConfigurationScope
import Proofs.Conformance.GraphWorkspaceLineage
import Proofs.Conformance.OperatorBaseFreeze
import Proofs.Conformance.LogicalOutputObligation
import Proofs.Conformance.InvalidToolProgress
import Proofs.Conformance.RepeatedToolFailure
import Proofs.Conformance.ToolTimeouts
import Proofs.Conformance.MailboxNotification
import Proofs.Conformance.MailboxReply
import Proofs.Conformance.MailboxHandoff
import Proofs.Conformance.ArtifactAuthority
import Proofs.EventDelivery.SubagentSource
import Proofs.Conformance.WorkspacePathCapability
import Proofs.Conformance.Contracts.Json.Core
import Proofs.Conformance.Contracts.Json.Runtime
import Proofs.Conformance.Contracts.Json.Scheduling
import Proofs.Conformance.Contracts.Json.ToolExecution
import Proofs.Conformance.Contracts.Json.CommandPolicy
import Proofs.Conformance.Contracts.Json.ToolPolicy
import Proofs.Conformance.Contracts.Json.Lsp
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.Contracts.Json.BackgroundWork
import Proofs.Conformance.Contracts.Json.DelegatedChild
import Proofs.Conformance.Contracts.Json.DescendantGraph
import Proofs.Conformance.Contracts.Json.SpawnClaimFence
import Proofs.Conformance.Contracts.Json.ComposedInvariants
import Proofs.Conformance.Contracts.Json.CodexShim
import Proofs.Conformance.Contracts.Json.Workspace
import Proofs.Conformance.Contracts.Json.Callback
import Proofs.Conformance.Contracts.Json.SelfConfig
import Proofs.Conformance.Contracts.Json.Goal
import Proofs.Conformance.Contracts.Json.SessionHydration
import Proofs.Conformance.Contracts.Json.PairingReconcile
import Proofs.Conformance.Contracts.Json.Enrollment
import Proofs.Conformance.Contracts.Json.PromptAssembly
import Proofs.Conformance.Contracts.Json.RenderedCapture
import Proofs.Conformance.Contracts.Json.DurableReduction
import Proofs.Conformance.Contracts.Json.BackgroundWakeRows
import Proofs.Conformance.Contracts.Json.SessionDocuments
import Proofs.Conformance.Contracts.Json.RequestInput
import Proofs.Conformance.Contracts.Json.AggregateBudget
import Proofs.Conformance.Contracts.Json.RollingCompaction
import Proofs.Conformance.Contracts.Json.ReductionEngine
import Proofs.Conformance.Contracts.Json.CompactionProjectionJoin
import Proofs.Conformance.Contracts.Json.CompactionCanonicalProjection
import Proofs.CompletionRetry.Contracts
import Proofs.Conformance.Triggers.Contracts
import Proofs.Conformance.EventGroups
import Proofs.Conformance.ClientShell.Contracts
import Proofs.ApplyReconcile.ContractCases
import Proofs.Conformance.Deviations
import Proofs.Conformance.CoverageLedger
import Proofs.Identity.Conformance
import Proofs.Conformance.EventDelivery
import Proofs.Conformance.GraphPipeline
import Proofs.Conformance.GoalOperatorResume
import Proofs.Conformance.GoalClaimedPublication
import Proofs.Conformance.GoalRequestHead
import Proofs.Conformance.GraphFailureAttribution
import Proofs.Conformance.GraphLogicalInvocation
import Proofs.Conformance.RequestExecutionLease
import Proofs.Conformance.InferenceRegistry
import Proofs.Conformance.RootAdmission
import Proofs.Conformance.Contracts.Json.ExecutionGate
import Proofs.Conformance.Contracts.Json.DispatchObservation
import Proofs.Conformance.Contracts.Json.InterruptQueue
import Proofs.Conformance.Contracts.Json.WorkerCapacity
import Proofs.Conformance.Contracts.Json.PayloadPresentation
import Proofs.Conformance.Contracts.Json.R5Scenarios
import Proofs.Conformance.Eval
import Proofs.Conformance.Optimization

namespace Conformance.Contracts

def reservedChildDecisionString : EventDelivery.SubagentSource.MaterializationDecision → String
  | .created => "created"
  | .replayed => "replayed"
  | .conflict => "conflict"

def reservedChildBindingJson (binding : EventDelivery.SubagentSource.ReservedChildBinding) : String :=
  let workspaceJson := (binding.workspace.map
    Conformance.DelegatedChildContracts.stampJson).getD "null"
  "{" ++ "\"child\":" ++ toString binding.child ++ ","
    ++ "\"agent\":" ++ toString binding.agent ++ ","
    ++ "\"behavior\":" ++ toString binding.behavior ++ ","
    ++ "\"parent_request\":" ++ toString binding.parentRequest ++ ","
    ++ "\"parent_request_doc\":" ++ toString binding.parentRequestDoc ++ ","
    ++ "\"parent_tool\":" ++ toString binding.parentTool ++ ","
    ++ "\"parent_tool_doc\":" ++ toString binding.parentToolDoc ++ ","
    ++ "\"payload\":" ++ toString binding.payload ++ ","
    ++ "\"depth\":" ++ toString binding.depth ++ ","
    ++ "\"workspace\":" ++ workspaceJson ++ ","
    ++ "\"admission\":" ++ toString binding.admission ++ "}"

def reservedChildCaseJson (w : EventDelivery.SubagentSource.ReservedChildCase) : String :=
  let actual := EventDelivery.SubagentSource.ensureReservedChild w.stored w.candidate
  "{" ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"stored\":" ++ jsonArray (w.stored.map reservedChildBindingJson) ++ ","
    ++ "\"candidate\":" ++ reservedChildBindingJson w.candidate ++ ","
    ++ "\"expected_decision\":" ++ jsonString (reservedChildDecisionString actual.1) ++ ","
    ++ "\"expected_count\":" ++ toString actual.2.length ++ "}"

def localParentDepthCaseJson
    (value : EventDelivery.SubagentSource.LocalParentDepthCase) : String :=
  let observed := match value.storedParentDepth with
    | none => "null"
    | some depth => toString depth
  let expected := match EventDelivery.SubagentSource.admitLocalChildDepth
      value.suppliedParentDepth value.storedParentDepth with
    | .ok child => "{\"kind\":\"admitted\",\"child_depth\":" ++ toString child ++ "}"
    | .error .depthExceeded =>
        "{\"kind\":\"rejected\",\"reason\":\"depth_exceeded\"}"
    | .error .parentLinkageIncoherent =>
        "{\"kind\":\"rejected\",\"reason\":\"parent_linkage_incoherent\"}"
  "{\"name\":" ++ jsonString value.name ++
    ",\"supplied_parent_depth\":" ++ toString value.suppliedParentDepth ++
    ",\"stored_parent_depth\":" ++ observed ++
    ",\"expected\":" ++ expected ++ "}"

open Conformance.ContractCases

def snapshotJson : String :=
  "{"
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ "\"root_admission_cases\":"
      ++ Conformance.RootAdmissionContracts.casesJson ++ ","
    ++ "\"vocabularies\":"
      ++ jsonArray (vocabularies.map VocabularyContract.toJson) ++ ","
    ++ "\"state_machines\":"
      ++ jsonArray (stateMachines.map StateMachineContract.toJson) ++ ","
    ++ "\"child_failure_projections\":" ++ childFailureProjectionsJson ++ ","
    ++ "\"pairing_reconcile_cases\":" ++ pairingReconcileCasesJson ++ ","
    ++ "\"graph_failure_attribution_traces\":"
      ++ Conformance.GraphFailureAttributionContracts.traceCasesJson ++ ","
    ++ "\"goal_request_head_cases\":"
      ++ Conformance.GoalRequestHeadContracts.casesJson ++ ","
    ++ "\"graph_logical_invocation_cases\":"
      ++ Conformance.GraphLogicalInvocationContracts.casesJson ++ ","
    ++ "\"graph_invocation_publication_cases\":"
      ++ Conformance.GraphLogicalInvocationContracts.publicationCasesJson ++ ","
    ++ "\"goal_claimed_publication_cases\":"
      ++ Conformance.GoalClaimedPublicationContracts.casesJson ++ ","
    ++ "\"goal_operator_resume_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.resumeCasesJson ++ ","
    ++ "\"goal_config_reactivation_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.configCasesJson ++ ","
    ++ "\"graph_pipeline_validation_cases\":"
      ++ Conformance.GraphPipelineContracts.validationCasesJson ++ ","
    ++ "\"graph_pipeline_revision_gate_cases\":"
      ++ Conformance.GraphPipelineContracts.revisionGateCasesJson ++ ","
    ++ "\"graph_pipeline_run_terminal_cases\":"
      ++ Conformance.GraphPipelineContracts.runTerminalCasesJson ++ ","
    ++ "\"request_transition_cases\":"
      ++ jsonArray (requestTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"provider_eof_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.providerEofCasesJson ++ ","
    ++ "\"request_execution_lease_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.leaseCasesJson ++ ","
    ++ "\"request_execution_lease_trace_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.leaseTraceCasesJson ++ ","
    ++ "\"canonical_execution_gate_cases\":"
      ++ Conformance.ExecutionGateContracts.casesJson ++ ","
    ++ "\"interrupt_queue_cases\":"
      ++ Conformance.InterruptQueueContracts.casesJson ++ ","
    ++ "\"canonical_dispatch_observation_cases\":"
      ++ Conformance.DispatchObservationContracts.casesJson ++ ","
    ++ "\"canonical_spawned_target_rejection_cases\":"
      ++ Conformance.DispatchObservationContracts.spawnedTargetCasesJson ++ ","
    ++ "\"canonical_worker_capacity_cases\":"
      ++ Conformance.WorkerCapacityContracts.casesJson ++ ","
    ++ "\"canonical_payload_presentation_cases\":"
      ++ Conformance.PayloadPresentationContracts.casesJson ++ ","
    ++ "\"terminal_diagnostic_presentation_cases\":"
      ++ Conformance.TerminalDiagnosticContracts.casesJson ++ ","
    ++ "\"terminal_diagnostic_replay_cases\":"
      ++ Conformance.TerminalDiagnosticReplayContracts.casesJson ++ ","
    ++ "\"inference_registry_cases\":"
      ++ Conformance.InferenceRegistry.casesJson ++ ","
    ++ "\"process_transition_cases\":"
      ++ jsonArray (processTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"trigger_dispatch_case_count\":"
      ++ toString Conformance.TriggerContracts.triggerDispatchCaseCount ++ ","
    ++ "\"trigger_dispatch_cases\":"
      ++ Conformance.TriggerContracts.triggerDispatchCasesJson ++ ","
    ++ "\"event_group_case_count\":"
      ++ toString Conformance.EventGroupContracts.eventGroupCaseCount ++ ","
    ++ "\"event_group_cases\":"
      ++ Conformance.EventGroupContracts.eventGroupCasesJson ++ ","
    ++ "\"goal_decision_cases\":"
      ++ goalDecisionCasesJson ++ ","
    ++ "\"goal_readiness_gate_cases\":"
      ++ goalReadinessGateCasesJson ++ ","
    ++ "\"goal_transition_cases\":"
      ++ goalTransitionCasesJson ++ ","
    ++ "\"goal_create_cases\":" ++ goalCreateCasesJson ++ ","
    ++ "\"task_goal_publication_cases\":" ++ taskGoalPublicationCasesJson ++ ","
    ++ "\"task_goal_recovery_cases\":" ++ taskGoalRecoveryCasesJson ++ ","
    ++ "\"goal_submission_cases\":" ++ goalSubmissionCasesJson ++ ","
    ++ "\"goal_continuation_materialization_cases\":"
      ++ goalContinuationMaterializationCasesJson ++ ","
    ++ "\"session_hydration_decision_cases\":"
      ++ sessionHydrationDecisionCasesJson ++ ","
    ++ "\"session_hydration_closure_cases\":"
      ++ sessionHydrationClosureCasesJson ++ ","
    ++ "\"session_hydration_apply_cases\":"
      ++ sessionHydrationApplyCasesJson ++ ","
    ++ "\"session_hydration_progress_cases\":"
      ++ sessionHydrationProgressCasesJson ++ ","
    ++ "\"session_hydration_durable_cases\":"
      ++ sessionHydrationDurableCasesJson ++ ","
    ++ "\"enrollment_cases\":"
      ++ enrollmentCasesJson ++ ","
    ++ "\"enrollment_durable_projection_cases\":"
      ++ enrollmentDurableProjectionCasesJson ++ ","
    ++ "\"enrollment_encoding_cases\":"
      ++ enrollmentEncodingCasesJson ++ ","
    ++ "\"enrollment_digest_cases\":"
      ++ enrollmentDigestCasesJson ++ ","
    ++ "\"discovery_scope_cases\":" ++ Conformance.ConfigurationScope.discoveryCasesJson ++ ","
    ++ "\"budget_rehydration_cases\":" ++ budgetRehydrationCasesJson ++ ","
    ++ "\"configuration_scope_cases\":" ++ Conformance.ConfigurationScope.casesJson ++ ","
    ++ "\"session_document_cases\":" ++ sessionDocumentsJson ++ ","
    ++ "\"background_wake_row_cases\":" ++ backgroundWakeRowsCasesJson ++ ","
    ++ "\"request_input_cases\":" ++ requestInputCasesJson ++ ","
    ++ "\"agent_request_admission_cases\":"
      ++ agentRequestAdmissionCasesJson ++ ","
    ++ "\"frontend_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.frontendClientShellCaseCount ++ ","
    ++ "\"frontend_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.frontendClientShellCasesJson ++ ","
    ++ "\"desktop_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.desktopClientShellCaseCount ++ ","
    ++ "\"desktop_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.desktopClientShellCasesJson ++ ","
    ++ "\"request_lifecycle_operator_ui_cases\":"
      ++ Conformance.ClientShellContracts.requestLifecycleOperatorUiCasesJson ++ ","
    ++ "\"startup_readiness_cases\":"
      ++ startupReadinessCasesJson ++ ","
    ++ "\"readiness_publication_cases\":"
      ++ readinessPublicationCasesJson ++ ","
    ++ "\"runtime_reconcile_cases\":"
      ++ jsonArray (runtimeReconcileCases.map runtimeReconcileCaseJson) ++ ","
    ++ "\"client_behavior_readiness_cases\":"
      ++ jsonArray (clientBehaviorReadinessCases.map clientBehaviorReadinessCaseJson) ++ ","
    ++ "\"apply_reconcile_cases\":"
      ++ ApplyReconcile.ContractCases.applyReconcileCasesJson ++ ","
    ++ "\"eval_outcome_cases\":"
      ++ Conformance.Eval.evalOutcomeCasesJson ++ ","
    ++ "\"optimization_cases\":"
      ++ Conformance.Optimization.optimizationCasesJson ++ ","
    ++ "\"publish_if_cases\":"
      ++ ApplyReconcile.ContractCases.publishIfCasesJson ++ ","
    ++ "\"tool_policy_cases\":"
      ++ toolPolicyCasesJson ++ ","
    ++ "\"write_input_cases\":" ++ writeInputCasesJson ++ ","
    ++ "\"invocation_correlation_cases\":" ++ invocationCorrelationCasesJson ++ ","
    ++ "\"goal_capability_resolution_cases\":"
      ++ goalCapabilityResolutionCasesJson ++ ","
    ++ "\"lsp_action_cases\":"
      ++ lspActionCasesJson ++ ","
    ++ "\"self_config_field_tables\":"
      ++ selfConfigFieldTablesJson ++ ","
    ++ "\"self_config_cases\":"
      ++ selfConfigCasesJson ++ ","
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
    ++ "\"managed_exec_tool_boundary_cases\":"
      ++ jsonArray
        (managedExecToolBoundaryCases.map managedExecToolBoundaryCaseJson) ++ ","
    ++ "\"pairing_reconcile_shutdown_boundary_cases\":"
      ++ jsonArray
        (pairingReconcileShutdownBoundaryCases.map
          pairingReconcileShutdownBoundaryCaseJson) ++ ","
    ++ "\"pairing_reconcile_sweep_retry_boundary_cases\":"
      ++ jsonArray
        (pairingReconcileSweepRetryBoundaryCases.map
          pairingReconcileSweepRetryBoundaryCaseJson) ++ ","
    ++ "\"pairing_reconcile_sweep_scheduling_cases\":"
      ++ jsonArray
        (pairingReconcileSweepSchedulingCases.map
          pairingReconcileSweepSchedulingCaseJson) ++ ","
    ++ "\"managed_exec_liveness_cases\":"
      ++ jsonArray
        (managedExecLivenessCases.map managedExecLivenessCaseJson) ++ ","
    ++ "\"process_stop_cases\":"
      ++ jsonArray (processStopCases.map processStopCaseJson) ++ ","
    ++ "\"tool_preflight_cases\":"
      ++ jsonArray (ToolExecution.preflightCases.map toolPreflightCaseJson) ++ ","
    ++ "\"tool_retry_cases\":"
      ++ jsonArray (ToolExecution.retryCases.map toolRetryCaseJson) ++ ","
    ++ "\"completion_retry_cases\":"
      ++ CompletionRetry.Contracts.casesJson ++ ","
    ++ "\"boundaries\":"
      ++ boundariesJson ++ ","
    ++ "\"deviations\":"
      ++ deviationsJson ++ ","
    ++ "\"graph_workspace_lineage_cases\":" ++ Conformance.GraphWorkspaceLineageContracts.casesJson ++ ","
    ++ "\"artifact_mode_meet_cases\":" ++ artifactModeMeetCasesJson ++ ","
    ++ "\"artifact_admission_cases\":" ++ artifactAdmissionCasesJson ++ ","
    ++ "\"artifact_spawn_cases\":" ++ artifactSpawnCasesJson ++ ","
    ++ "\"command_policy_cases\":"
      ++ jsonArray (CommandPolicy.commandPolicyCases.map commandPolicyCaseJson) ++ ","
    ++ "\"command_sandbox_cases\":"
      ++ jsonArray (CommandPolicy.commandSandboxCases.map commandSandboxCaseJson) ++ ","
    ++ "\"command_env_cases\":"
      ++ jsonArray (CommandPolicy.commandEnvCases.map commandEnvCaseJson) ++ ","
    ++ "\"live_overlay_cases\":"
      ++ jsonArray (liveOverlayCases.map liveOverlayCaseJson) ++ ","
    ++ "\"request_progress_cases\":"
      ++ jsonArray (requestProgressCases.map requestProgressCaseJson) ++ ","
    ++ "\"pending_user_turn_cases\":"
      ++ jsonArray (pendingUserTurnCases.map pendingUserTurnCaseJson) ++ ","
    ++ "\"queued_steering_trace_cases\":"
      ++ jsonArray (QueuedSteering.traceObservations.map queuedSteeringTraceJson) ++ ","
    ++ "\"queued_steering_guard_cases\":"
      ++ jsonArray (QueuedSteering.guardObservations.map queuedSteeringGuardJson) ++ ","
    ++ "\"queue_deadline_conformance_cases\":"
      ++ jsonArray
        (queueDeadlineConformanceCases.map queueDeadlineConformanceCaseJson) ++ ","
    ++ "\"recovery_sweep_cases\":"
      ++ jsonArray
        (Recovery.recoverySweepCases.map recoverySweepCaseJson) ++ ","
    ++ "\"reserved_child_materialization_cases\":"
      ++ jsonArray
        (EventDelivery.SubagentSource.reservedChildCases.map reservedChildCaseJson) ++ ","
    ++ "\"local_parent_depth_cases\":"
      ++ jsonArray
        (EventDelivery.SubagentSource.localParentDepthCases.map localParentDepthCaseJson) ++ ","
    ++ "\"restart_disposition_cases\":"
      ++ jsonArray
        (Recovery.restartDispositionCases.map restartDispositionCaseJson) ++ ","
    ++ "\"r4c_background_work_cases\":"
      ++ jsonArray r4cBackgroundWorkCasesJson ++ ","
    ++ "\"tool_output_paging_cases\":"
      ++ jsonArray
        (toolOutputPagingCases.map toolOutputPagingCaseJson) ++ ","
    ++ "\"bridge_step_cases\":"
      ++ jsonArray
        (bridgeStepCases.map bridgeStepCaseJson) ++ ","
    ++ "\"interrupt_disposition_cases\":"
      ++ jsonArray
        (interruptDispositionCases.map interruptDispositionCaseJson) ++ ","
    ++ "\"codex_shim_projection_cases\":"
      ++ codexShimProjectionCasesJson ++ ","
    ++ "\"codex_shim_subagent_tool_cases\":"
      ++ codexShimSubagentToolCasesJson ++ ","
    ++ "\"codex_shim_subagent_status_cases\":"
      ++ codexShimSubagentStatusCasesJson ++ ","
    ++ "\"codex_shim_subagent_visibility_cases\":"
      ++ codexShimSubagentVisibilityCasesJson ++ ","
    ++ "\"codex_shim_subagent_metadata_cases\":"
      ++ codexShimSubagentMetadataCasesJson ++ ","
    ++ "\"codex_shim_subagent_listing_cases\":"
      ++ codexShimSubagentListingCasesJson ++ ","
    ++ "\"codex_shim_subagent_thread_shape_cases\":"
      ++ codexShimSubagentThreadShapeCasesJson ++ ","
    ++ "\"codex_shim_reasoning_projection_cases\":"
      ++ codexShimReasoningProjectionCasesJson ++ ","
    ++ "\"codex_shim_thread_status_cases\":"
      ++ codexShimThreadStatusCasesJson ++ ","
    ++ "\"codex_shim_behavior_selection_cases\":"
      ++ codexShimBehaviorSelectionCasesJson ++ ","
    ++ "\"codex_shim_tool_metadata_cases\":"
      ++ codexShimToolMetadataCasesJson ++ ","
    ++ "\"codex_shim_context_usage_cases\":"
      ++ codexShimContextUsageCasesJson ++ ","
    ++ "\"codex_shim_compaction_projection_cases\":"
      ++ codexShimCompactionProjectionCasesJson ++ ","
    ++ "\"codex_shim_turn_lifecycle_cases\":"
      ++ codexShimTurnLifecycleCasesJson ++ ","
    ++ "\"codex_shim_binding_cases\":"
      ++ codexShimBindingCasesJson ++ ","
    ++ "\"r6_backgrounding_cases\":"
      ++ jsonArray
        (r6BackgroundingCases.map r6BackgroundingCaseJson) ++ ","
    ++ "\"descendant_graph_cases\":"
      ++ descendantGraphCasesJson ++ ","
    ++ "\"descendant_cursor_cases\":"
      ++ descendantCursorCasesJson ++ ","
    ++ "\"spawn_fence_cases\":"
      ++ spawnFenceCasesJson ++ ","
    ++ "\"r5_cross_principal_cases\":"
      ++ jsonArray
        (r5CrossPrincipalCases.map r5CrossPrincipalCaseJson) ++ ","
    ++ "\"r5_scenario_cases\":" ++ r5ScenarioCasesJson ++ ","
    ++ "\"composed_invariant_witnesses\":"
      ++ jsonArray
        (composedInvariantWitnesses.map composedInvariantWitnessJson) ++ ","
    ++ "\"cancel_propagation_cases\":"
      ++ jsonArray
        (cancelPropagationCases.map cancelPropagationCaseJson) ++ ","
    ++ "\"logical_output_obligation_cases\":"
      ++ Conformance.LogicalOutputObligationContracts.casesJson ++ ","
    ++ "\"mailbox_notification_cases\":"
      ++ Conformance.MailboxNotificationContracts.casesJson ++ ","
    ++ "\"mailbox_reply_cases\":"
      ++ Conformance.MailboxReplyContracts.casesJson ++ ","
    ++ "\"mailbox_handoff_cases\":"
      ++ Conformance.MailboxHandoffContracts.casesJson ++ ","
    ++ "\"invalid_tool_progress_cases\":"
      ++ Conformance.InvalidToolProgressContracts.casesJson ++ ","
    ++ "\"repeated_tool_failure_cases\":"
      ++ Conformance.RepeatedToolFailureContracts.casesJson ++ ","
    ++ "\"tool_timeout_cases\":"
      ++ Conformance.ToolTimeouts.casesJson ++ ","
    ++ "\"operator_base_freeze_cases\":"
      ++ Conformance.OperatorBaseFreezeContracts.casesJson ++ ","
    ++ "\"workspace_path_capability_cases\":"
      ++ Conformance.WorkspacePathCapabilityContracts.casesJson ++ ","
    ++ "\"workspace_path_alias_cases\":"
      ++ Conformance.WorkspacePathCapabilityContracts.aliasCasesJson ++ ","
    ++ "\"workspace_cases\":"
      ++ workspaceCasesJson ++ ","
    ++ "\"workspace_binding_cases\":"
      ++ workspaceBindingCasesJson ++ ","
    ++ "\"callback_transition_case_count\":" ++ toString callbackTransitionCaseCount ++ ","
    ++ "\"callback_transition_cases\":" ++ callbackTransitionCasesJson ++ ","
    ++ "\"callback_cases\":"
      ++ callbackCasesJson ++ ","
    ++ "\"r6_background_theorem_witnesses\":"
      ++ jsonArray
        (r6BackgroundTheoremWitnesses.map backgroundTheoremWitnessJson) ++ ","
    ++ "\"subagent_delegation_graph_cases\":"
      ++ jsonArray
        (subagentDelegationGraphCases.map subagentDelegationGraphCaseJson) ++ ","
    ++ "\"delegated_child_resolution_cases\":"
      ++ Conformance.DelegatedChildContracts.casesJson ++ ","
    ++ "\"transcript_conformance_cases\":"
      ++ jsonArray
        (transcriptConformanceCases.map transcriptCaseJson) ++ ","
    ++ "\"canonical_output_projection_cases\":"
      ++ jsonArray
        (StreamingResponse.outputProjectionCases.map outputProjectionCaseJson) ++ ","
    ++ "\"current_input_cases\":" ++ currentInputCasesJson ++ ","
    ++ "\"prompt_assembly_sanitize_cases\":"
      ++ promptAssemblySanitizeCasesJson ++ ","
    ++ "\"prompt_assembly_layer_cases\":"
      ++ promptAssemblyLayerCasesJson ++ ","
    ++ "\"prompt_assembly_repair_cases\":"
      ++ promptAssemblyRepairCasesJson ++ ","
    ++ "\"prompt_assembly_budget_cases\":"
      ++ promptAssemblyBudgetCasesJson ++ ","
    ++ "\"prompt_assembly_turn_budget_cases\":"
      ++ promptAssemblyTurnBudgetCasesJson ++ ","
    ++ "\"prompt_assembly_retention_cases\":"
      ++ promptAssemblyRetentionCasesJson ++ ","
    ++ "\"prompt_assembly_claude_map_cases\":"
      ++ promptAssemblyClaudeMapCasesJson ++ ","
    ++ "\"prompt_assembly_claude_body_cases\":"
      ++ promptAssemblyClaudeBodyCasesJson ++ ","
    ++ "\"prompt_assembly_claude_stream_cases\":"
      ++ promptAssemblyClaudeStreamCasesJson ++ ","
    ++ "\"rendered_capture_cases\":"
      ++ renderedCaptureCasesJson ++ ","
    ++ "\"rendered_capture_storage_cases\":"
      ++ renderedCaptureStorageCasesJson ++ ","
    ++ "\"durable_reduction_cases\":"
      ++ durableReductionCasesJson ++ ","
    ++ "\"rolling_compaction_cases\":"
      ++ rollingCompactionCasesJson ++ ","
    ++ "\"reduction_engine_cases\":"
      ++ reductionEngineCasesJson ++ ","
    ++ "\"compaction_projection_join_cases\":"
      ++ compactionProjectionJoinCasesJson ++ ","
    ++ "\"compaction_canonical_projection_cases\":"
      ++ compactionCanonicalProjectionCasesJson ++ ","
    ++ "\"repaired_projection_admission_cases\":"
      ++ repairedProjectionAdmissionCasesJson ++ ","
    ++ "\"rendered_capture_key_cases\":"
      ++ renderedCaptureKeyCasesJson ++ ","
    ++ "\"capture_scope_cases\":"
      ++ captureScopeCasesJson ++ ","
    ++ "\"capture_order_cases\":"
      ++ captureOrderCasesJson ++ ","
    ++ "\"aggregate_token_budget_cases\":"
      ++ aggregateTokenBudgetCasesJson ++ ","
    ++ "\"compaction_reducer_cases\":"
      ++ jsonArray
        (Compaction.compactionReducerCases.map compactionReducerCaseJson) ++ ","
    ++ "\"compaction_cursor_cases\":"
      ++ jsonArray
        (Compaction.compactionCursorCases.map compactionCursorCaseJson) ++ ","
    ++ "\"mcp_health_cases\":"
      ++ jsonArray
        (Proofs.MCPHealth.transitionCases.map mcpHealthCaseJson) ++ ","
    ++ "\"backend_health_cases\":"
      ++ jsonArray
        (Proofs.BackendHealth.transitionCases.map backendHealthCaseJson) ++ ","
    ++ "\"follow_up_hooks\":"
      ++ followUpHooksJson ++ ","
    ++ "\"event_group_capture_case_count\":" ++ toString Conformance.EventDelivery.eventGroupCaptureCaseCount ++ ","
    ++ "\"event_group_capture_cases\":" ++ Conformance.EventDelivery.eventGroupCaptureCasesJson ++ ","
    ++ "\"event_group_clock_case_count\":" ++ toString Conformance.EventDelivery.eventGroupClockCaseCount ++ ","
    ++ "\"event_group_clock_cases\":" ++ Conformance.EventDelivery.eventGroupClockCasesJson ++ ","
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
    ++ ",\"feature_surface_requirements\":"
      ++ featureSurfaceRequirementsJson
    ++ ",\"feature_matrix\":"
      ++ featureMatrixJson
    ++ ",\"identity_structural_cases\":"
      ++ Identity.Conformance.structuralCasesJson
    ++ ",\"identity_permission_cases\":"
      ++ Identity.Conformance.identityPermissionCasesJson
    ++ ",\"identity_contracts\":"
      ++ Identity.Conformance.identityContractsJson
    ++ "}"

end Conformance.Contracts
