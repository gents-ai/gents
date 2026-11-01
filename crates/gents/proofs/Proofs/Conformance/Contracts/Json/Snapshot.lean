import Proofs.Conformance.Contracts.Json.TerminalSelection
import Proofs.Conformance.WorkspaceCommandGrants
import Proofs.Conformance.StorageFailure
import Proofs.Conformance.RequestPatchPublicationCases
import Proofs.Conformance.AccessReview
import Proofs.Conformance.OperatorPaths
import Proofs.Conformance.WorkspaceCommandRoot
import Proofs.Conformance.CommandIsolation
import Proofs.Conformance.CompactionCheckpoint
import Proofs.Conformance.ClientDataPublicationCases
import Proofs.Conformance.ClientTelemetry
import Proofs.Conformance.ClientObservationScope
import Proofs.Conformance.InferenceObservation
import Proofs.Conformance.ClientInstallation
import Proofs.Conformance.RuntimeServiceReplacement
import Proofs.Conformance.ClientSettings
import Proofs.Conformance.ClientRequestClosureCases
import Proofs.Conformance.ClientRequestCacheIdentityCases
import Proofs.Conformance.SessionQueueOrderCases
import Proofs.Conformance.CountCarrier
import Proofs.Conformance.ConfigurationScope
import Proofs.Conformance.Initialization
import Proofs.Conformance.EnrollmentHydrationRoutes
import Proofs.Conformance.EnrollmentPendingRequests
import Proofs.Conformance.ClientObservationAdmission
import Proofs.Conformance.OperatorBaseFreeze
import Proofs.Conformance.LogicalOutputObligation
import Proofs.Conformance.InvalidToolProgress
import Proofs.Conformance.SchemaArgumentRepair
import Proofs.Conformance.RepeatedToolFailure
import Proofs.Conformance.ToolTimeouts
import Proofs.Conformance.PluginResources
import Proofs.Conformance.PluginJob
import Proofs.Conformance.MailboxNotification
import Proofs.Conformance.MailboxReply
import Proofs.Conformance.MailboxQuestion
import Proofs.Conformance.MailboxHandoff
import Proofs.Conformance.MailboxSweep
import Proofs.Conformance.SessionReplyPreview
import Proofs.Conformance.ArtifactAuthority
import Proofs.Conformance.WorkspacePathCapability
import Proofs.Conformance.FileImageHydration
import Proofs.Conformance.FileInventory
import Proofs.Conformance.TaskHooks
import Proofs.Conformance.StoreEncryptionUpgrade
import Proofs.Conformance.Contracts.Json.Core
import Proofs.Conformance.Contracts.Json.Runtime
import Proofs.Conformance.Contracts.Json.CatalogRefresh
import Proofs.Conformance.Contracts.Json.ClaudePlatform
import Proofs.Conformance.Contracts.Json.PackInstallation
import Proofs.Conformance.Contracts.Json.Scheduling
import Proofs.Conformance.Contracts.Json.ToolExecution
import Proofs.Conformance.Contracts.Json.CommandPolicy
import Proofs.Conformance.Contracts.Json.ToolPolicy
import Proofs.Conformance.Contracts.Json.Lsp
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.Contracts.Json.BackgroundWork
import Proofs.Conformance.Contracts.Json.ComposedInvariants
import Proofs.Conformance.Contracts.Json.CodexShim
import Proofs.Conformance.Contracts.Json.Workspace
import Proofs.Conformance.Contracts.Json.Callback
import Proofs.Conformance.Contracts.Json.SelfConfig
import Proofs.Conformance.Contracts.Json.SelfConfigAgent
import Proofs.Conformance.Contracts.Json.Goal
import Proofs.Conformance.GoalClaimedReadiness
import Proofs.Conformance.Contracts.Json.SessionHydration
import Proofs.Conformance.Contracts.Json.PairingReconcile
import Proofs.Conformance.Contracts.Json.Enrollment
import Proofs.Conformance.TitleUsage
import Proofs.Conformance.ProviderUsage
import Proofs.Conformance.TitleAdmission
import Proofs.Conformance.TitleText
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
import Proofs.AccountRotation.Contracts
import Proofs.Conformance.Triggers.Contracts
import Proofs.Conformance.EventGroups
import Proofs.Conformance.ClientShell.Contracts
import Proofs.Conformance.ClientLiveDelta
import Proofs.ApplyReconcile.ContractCases
import Proofs.Conformance.Deviations
import Proofs.Conformance.CoverageLedger
import Proofs.Identity.Conformance
import Proofs.Conformance.EventDelivery
import Proofs.Conformance.TriggerDelivery
import Proofs.Conformance.GoalOperatorResume
import Proofs.Conformance.GoalClaimedPublication
import Proofs.Conformance.GoalResetResume
import Proofs.Conformance.GoalWakeAncestry
import Proofs.Conformance.GoalWakeReceipt
import Proofs.Conformance.GoalWakeCoalescing
import Proofs.Conformance.GoalRequestAncestry
import Proofs.Conformance.GoalQueueAssignment
import Proofs.Conformance.GoalFirstRequestBinding
import Proofs.Conformance.GoalRequestHead
import Proofs.Conformance.NativeRefinement
import Proofs.Conformance.GoalRequestAssignment
import Proofs.Conformance.GoalDeliveryInput
import Proofs.Conformance.GoalToolContext
import Proofs.Conformance.GoalEdit
import Proofs.Conformance.GoalClientControl
import Proofs.Conformance.GoalClientMutation
import Proofs.Conformance.GoalClientDelivery
import Proofs.Conformance.GoalClientStart
import Proofs.Conformance.GoalObservation
import Proofs.Conformance.PluginApproval
import Proofs.Conformance.RequestExecutionLease
import Proofs.Conformance.InferenceRegistry
import Proofs.Conformance.RootAdmission
import Proofs.Conformance.Contracts.Json.ExecutionGate
import Proofs.Conformance.Contracts.Json.DispatchObservation
import Proofs.Conformance.Contracts.Json.InterruptQueue
import Proofs.Conformance.Contracts.Json.SessionControl
import Proofs.Conformance.Contracts.Json.FoldQueue
import Proofs.Conformance.Contracts.Json.WorkerCapacity
import Proofs.Conformance.Contracts.Json.CausalHop
import Proofs.Conformance.Contracts.Json.PayloadPresentation
import Proofs.Conformance.Eval
import Proofs.Conformance.Optimization
import Proofs.ScopeTemplates.State

namespace Conformance.Contracts

open Conformance.ContractCases

def scopeRulesJson (rules : List ScopeTemplates.CollectionRule) : String :=
  jsonArray (rules.map fun rule =>
    "{\"collection\":" ++ jsonString rule.collection
      ++ ",\"field\":" ++ jsonString rule.field
      ++ ",\"source\":" ++ jsonString (match rule.source with
          | .localDid => "localDid" | .peerDid => "peerDid" | .homeDid => "homeDid") ++ "}")

/-- The end of `snapshotJson`, split so its concatenation chain stays within
the interpreter's recursion limit. -/
def snapshotJsonTail : String :=
  ""
    ++ "\"prompt_assembly_retention_cases\":"
      ++ promptAssemblyRetentionCasesJson ++ ","
    ++ "\"prompt_assembly_compaction_trigger_cases\":"
      ++ promptAssemblyCompactionTriggerCasesJson ++ ","
    ++ "\"prompt_assembly_claude_map_cases\":"
      ++ promptAssemblyClaudeMapCasesJson ++ ","
    ++ "\"prompt_assembly_claude_body_cases\":"
      ++ promptAssemblyClaudeBodyCasesJson ++ ","
    ++ "\"prompt_assembly_claude_attribution_cases\":"
      ++ promptAssemblyClaudeAttributionCasesJson ++ ","
    ++ "\"prompt_assembly_claude_cache_cases\":"
      ++ promptAssemblyClaudeCacheCasesJson ++ ","
    ++ "\"prompt_assembly_claude_origin_cases\":"
      ++ promptAssemblyClaudeOriginCasesJson ++ ","
    ++ "\"prompt_assembly_claude_field_cases\":"
      ++ promptAssemblyClaudeFieldCasesJson ++ ","
    ++ "\"prompt_assembly_claude_field_row_cases\":"
      ++ promptAssemblyClaudeFieldRowCasesJson ++ ","
    ++ "\"claude_output_config_cases\":" ++ Conformance.CompactionCheckpoint.outputCasesJson.compress ++ ","
    ++ "\"checkpoint_fallback_cases\":" ++ Conformance.CompactionCheckpoint.checkpointCasesJson.compress ++ ","
    ++ "\"checkpoint_list_fields\":" ++ Conformance.CompactionCheckpoint.listFieldsJson.compress ++ ","
    ++ "\"prompt_assembly_claude_stream_cases\":"
      ++ promptAssemblyClaudeStreamCasesJson ++ ","
    ++ "\"prompt_assembly_claude_thinking_stream_cases\":"
      ++ promptAssemblyClaudeThinkingStreamCasesJson ++ ","
    ++ "\"prompt_assembly_claude_replay_cases\":"
      ++ promptAssemblyClaudeReplayCasesJson ++ ","
    ++ "\"prompt_assembly_claude_checkpoint_cases\":"
    ++ promptAssemblyClaudeCheckpointCasesJson ++ ","
    ++ "\"prompt_assembly_reasoning_suffix_cases\":"
    ++ promptAssemblyReasoningSuffixCasesJson ++ ","
    ++ "\"prompt_assembly_replay_shape_cases\":"
    ++ promptAssemblyReplayShapeCasesJson ++ ","
    ++ "\"prompt_assembly_replay_prefix_cases\":"
    ++ promptAssemblyReplayPrefixCasesJson ++ ","
    ++ "\"protected_replay_compaction_cases\":"
    ++ protectedReplayCompactionCasesJson ++ ","
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
    ++ "\"task_hook_admission_cases\":"
      ++ Conformance.TaskHooksContracts.admissionCasesJson ++ ","
    ++ "\"store_encryption_upgrade_cases\":"
      ++ Conformance.StoreEncryptionUpgrade.casesJson ++ ","
    ++ "\"task_hook_run_cases\":"
      ++ Conformance.TaskHooksContracts.runCasesJson ++ ","
    ++ "\"task_hook_recovery_cases\":"
      ++ Conformance.TaskHooksContracts.recoveryCasesJson ++ ","
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
    ++ "\"backend_probe_schedule_cases\":"
      ++ jsonArray
        (Proofs.BackendHealth.scheduleCases.map backendProbeScheduleCaseJson) ++ ","
    ++ "\"follow_up_hooks\":"
      ++ followUpHooksJson ++ ","
    ++ "\"trigger_delivery\":" ++ Conformance.TriggerDelivery.casesJson ++ ","
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

def snapshotJson : String :=
  "{"
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ "\"storage_failure_cases\":" ++ Conformance.StorageFailureContracts.casesJson ++ ","
    ++ "\"root_admission_cases\":"
      ++ Conformance.RootAdmissionContracts.casesJson ++ ","
    ++ "\"vocabularies\":"
      ++ jsonArray (vocabularies.map VocabularyContract.toJson) ++ ","
    ++ "\"state_machines\":"
      ++ jsonArray (stateMachines.map StateMachineContract.toJson) ++ ","
    ++ "\"pairing_reconcile_cases\":" ++ pairingReconcileCasesJson ++ ","
    ++ "\"session_message_goal_input_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.messageInputCasesJson ++ ","
    ++ "\"goal_delivery_input_cases\":"
      ++ Conformance.GoalDeliveryInput.json ++ ","
    ++ "\"goal_first_request_binding_cases\":"
      ++ Conformance.GoalFirstRequestBinding.json ++ ","
    ++ "\"goal_continuation_assignment_cases\":"
      ++ Conformance.GoalFirstRequestBinding.continuationsJson ++ ","
    ++ "\"goal_input_binding_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.inputCasesJson ++ ","
    ++ "\"goal_fresh_session_input_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.freshSessionCasesJson ++ ","
    ++ "\"goal_assignment_history_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.historyCasesJson ++ ","
    ++ "\"goal_request_assignment_cases\":"
      ++ Conformance.GoalRequestAssignmentContracts.casesJson ++ ","
    ++ "\"goal_wake_receipt_cases\":"
      ++ Conformance.GoalWakeReceiptContracts.casesJson ++ ","
    ++ "\"goal_wake_coalescing_cases\":"
      ++ Conformance.GoalWakeCoalescingContracts.casesJson ++ ","
    ++ "\"goal_wake_ancestry_cases\":"
      ++ Conformance.GoalWakeAncestryContracts.casesJson ++ ","
    ++ "\"goal_tool_context_cases\":"
      ++ Conformance.GoalToolContextContracts.casesJson ++ ","
    ++ "\"goal_request_scope_cases\":"
      ++ Conformance.GoalRequestAncestryContracts.scopeCasesJson ++ ","
    ++ "\"goal_request_ancestry_cases\":"
      ++ Conformance.GoalRequestAncestryContracts.casesJson ++ ","
    ++ "\"goal_queue_assignment_cases\":"
      ++ Conformance.GoalQueueAssignmentContracts.casesJson ++ ","
    ++ "\"goal_request_head_cases\":"
      ++ Conformance.GoalRequestHeadContracts.casesJson ++ ","
    ++ "\"durable_observation_cases\":"
      ++ Conformance.NativeRefinement.observationCasesJson ++ ","
    ++ "\"plugin_job_commit_kill_cases\":"
      ++ Conformance.PluginJob.commitKillCasesJson ++ ","
    ++ "\"operator_job_control_cases\":"
      ++ Conformance.NativeRefinement.operatorControlCasesJson ++ ","
    ++ "\"operator_process_control_cases\":"
      ++ Conformance.NativeRefinement.operatorProcessCasesJson ++ ","
    ++ "\"process_stop_action_cases\":"
      ++ Conformance.NativeRefinement.processStopActionCasesJson ++ ","
    ++ "\"background_process_identity_cases\":"
      ++ Conformance.NativeRefinement.processIdentityCasesJson ++ ","
    ++ "\"plugin_target_resolution_cases\":"
      ++ Conformance.NativeRefinement.targetCasesJson ++ ","
    ++ "\"goal_edit_cases\":" ++ Conformance.GoalEditContracts.casesJson ++ ","
    ++ "\"goal_client_control_cases\":" ++ Conformance.GoalClientControlContracts.casesJson ++ ","
    ++ "\"goal_client_start_cases\":" ++ Conformance.GoalClientStartContracts.casesJson ++ ","
    ++ "\"goal_client_receipt_cases\":" ++ Conformance.GoalClientMutationContracts.receiptCasesJson ++ ","
    ++ "\"goal_client_delivery_cases\":" ++ Conformance.GoalClientDeliveryContracts.casesJson.compress ++ ","
    ++ "\"goal_observation_cases\":" ++ Conformance.GoalObservationContracts.casesJson ++ ","
    ++ "\"workspace_permission_write_cases\":" ++ workspacePermissionWriteCasesJson ++ ","
    ++ "\"workspace_permission_cases\":" ++ workspacePermissionCasesJson ++ ","
    ++ "\"workspace_command_root_cases\":" ++ Conformance.WorkspaceCommandRoot.casesJson.compress ++ ","
    ++ "\"operator_path_cases\":" ++ Conformance.OperatorPaths.casesJson.compress ++ ","
    ++ "\"access_review_cases\":" ++ Conformance.AccessReview.casesJson.compress ++ ","
    ++ "\"access_review_task_cases\":" ++ Conformance.AccessReview.taskCasesJson.compress ++ ","
    ++ "\"plugin_approval_cases\":" ++ Conformance.PluginApproval.casesJson.compress ++ ","
    ++ "\"workspace_command_grant_answers\":" ++ Conformance.WorkspaceCommandGrants.answersJson.compress ++ ","
    ++ "\"workspace_command_grant_lookups\":" ++ Conformance.WorkspaceCommandGrants.lookupsJson.compress ++ ","
    ++ "\"workspace_command_grant_revocations\":" ++ Conformance.WorkspaceCommandGrants.revocationsJson.compress ++ ","
    ++ "\"goal_objective_whitespace_codepoints\":"
      ++ Conformance.GoalEditContracts.whitespaceCodepointsJson ++ ","
    ++ "\"goal_claimed_publication_cases\":"
      ++ Conformance.GoalClaimedPublicationContracts.casesJson ++ ","
    ++ "\"goal_operator_resume_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.resumeCasesJson ++ ","
    ++ "\"goal_operator_resume_on_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.resumeOnCasesJson ++ ","
    ++ "\"goal_config_reactivation_cases\":"
      ++ Conformance.GoalOperatorResumeContracts.configCasesJson ++ ","
    ++ "\"goal_reset_resume_cases\":"
      ++ Conformance.GoalResetResumeContracts.casesJson ++ ","
    ++ "\"request_transition_cases\":"
      ++ jsonArray (requestTransitionCases.map lifecycleTransitionCaseJson) ++ ","
    ++ "\"provider_eof_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.providerEofCasesJson ++ ","
    ++ "\"request_execution_lease_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.leaseCasesJson ++ ","
    ++ "\"request_execution_lease_observed_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.observedLeaseCasesJson ++ ","
    ++ "\"request_execution_lease_trace_cases\":"
      ++ Conformance.RequestExecutionLeaseContracts.leaseTraceCasesJson ++ ","
    ++ "\"canonical_execution_gate_cases\":"
      ++ Conformance.ExecutionGateContracts.casesJson ++ ","
    ++ "\"interrupt_queue_cases\":"
      ++ Conformance.InterruptQueueContracts.casesJson ++ ","
    ++ "\"session_control_cases\":"
      ++ Conformance.SessionControlContracts.casesJson ++ ","
    ++ "\"session_input_edit_cases\":" ++ FoldQueueContracts.inputEditCasesJson ++ ","
    ++ "\"session_input_edit_client_observation_cases\":"
      ++ FoldQueueContracts.inputEditClientObservationCasesJson ++ ","
    ++ "\"queue_management_cases\":" ++ FoldQueueContracts.managementCasesJson ++ ","
    ++ "\"steering_publication_cases\":" ++ FoldQueueContracts.steeringPublicationCasesJson ++ ","
    ++ "\"fold_queue_cases\":"
      ++ Conformance.FoldQueueContracts.casesJson ++ ","
    ++ "\"fold_turn_input_cases\":"
      ++ Conformance.FoldQueueContracts.turnInputCasesJson ++ ","
    ++ "\"handover_fold_cases\":"
      ++ Conformance.FoldQueueContracts.handoverCasesJson ++ ","
    ++ "\"fold_publication_cases\":"
      ++ Conformance.FoldQueueContracts.publicationCasesJson ++ ","
    ++ "\"retry_selection_cases\":"
      ++ Conformance.FoldQueueContracts.retrySelectionCasesJson ++ ","
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
    ++ "\"goal_claimed_readiness_cases\":"
      ++ Conformance.GoalClaimedReadiness.casesJson ++ ","
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
    ++ "\"session_hydration_start_cases\":"
      ++ sessionHydrationStartCasesJson ++ ","
    ++ "\"session_hydration_admission_cases\":"
      ++ sessionHydrationAdmissionCasesJson ++ ","
    ++ "\"enrollment_cases\":"
      ++ enrollmentCasesJson ++ ","
    ++ "\"enrollment_durable_projection_cases\":"
      ++ enrollmentDurableProjectionCasesJson ++ ","
    ++ "\"enrollment_hydration_route_cases\":"
      ++ Conformance.EnrollmentHydrationRoutes.casesJson ++ ","
    ++ "\"enrollment_pending_request_cases\":"
      ++ Conformance.EnrollmentPendingRequests.casesJson ++ ","
    ++ "\"client_observation_admission_cases\":"
      ++ Conformance.ClientObservationAdmission.casesJson ++ ","
    ++ "\"enrollment_encoding_cases\":"
      ++ enrollmentEncodingCasesJson ++ ","
    ++ "\"enrollment_digest_cases\":"
      ++ enrollmentDigestCasesJson ++ ","
    ++ "\"discovery_scope_cases\":" ++ Conformance.ConfigurationScope.discoveryCasesJson ++ ","
    ++ "\"budget_rehydration_cases\":" ++ budgetRehydrationCasesJson ++ ","
    ++ "\"configuration_scope_cases\":" ++ Conformance.ConfigurationScope.casesJson ++ ","
    ++ "\"initial_inference_cases\":" ++ Conformance.Initialization.casesJson ++ ","
    ++ "\"session_document_cases\":" ++ sessionDocumentsJson ++ ","
    ++ "\"session_reply_preview_cases\":" ++ Conformance.SessionReplyPreview.casesJson ++ ","
    ++ "\"background_wake_row_cases\":" ++ backgroundWakeRowsCasesJson ++ ","
    ++ "\"request_input_cases\":" ++ requestInputCasesJson ++ ","
    ++ "\"agent_request_admission_cases\":"
      ++ agentRequestAdmissionCasesJson ++ ","
    ++ "\"title_request_admission_cases\":"
      ++ titleRequestAdmissionCasesJson ++ ","
    ++ "\"title_request_purpose_wire_cases\":"
      ++ titlePurposeWireCasesJson ++ ","
    ++ "\"title_text_cases\":" ++ Conformance.TitleText.casesJson ++ ","
    ++ "\"title_usage_cases\":"
      ++ Conformance.TitleUsage.casesJson ++ ","
    ++ "\"provider_limit_read_cases\":"
      ++ Conformance.ProviderUsage.limitReadCasesJson ++ ","
    ++ "\"provider_usage_cases\":"
      ++ Conformance.ProviderUsage.casesJson ++ ","
    ++ "\"title_admission_join_cases\":"
      ++ Conformance.TitleAdmission.casesJson ++ ","
    ++ "\"frontend_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.frontendClientShellCaseCount ++ ","
    ++ "\"frontend_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.frontendClientShellCasesJson ++ ","
    ++ "\"desktop_client_shell_case_count\":"
      ++ toString Conformance.ClientShellContracts.desktopClientShellCaseCount ++ ","
    ++ "\"desktop_client_shell_cases\":"
      ++ Conformance.ClientShellContracts.desktopClientShellCasesJson ++ ","
    ++ "\"client_installation_cases\":"
      ++ Conformance.ClientInstallation.casesJson ++ ","
    ++ "\"client_installation_compatibility_cases\":"
      ++ Conformance.ClientInstallation.compatibilityCasesJson ++ ","
    ++ "\"runtime_service_replacement_cases\":"
      ++ Conformance.RuntimeServiceReplacement.casesJson ++ ","
    ++ "\"client_telemetry_cases\":" ++ Conformance.ClientTelemetry.casesJson ++ ","
    ++ "\"client_observation_scope_cases\":"
      ++ Conformance.ClientObservationScope.casesJson ++ ","
    ++ "\"inference_observation_cases\":"
      ++ Conformance.InferenceObservation.casesJson ++ ","
    ++ "\"client_settings_cases\":"
      ++ clientSettingsCases.compress ++ ","
    ++ "\"client_data_publication_cases\":"
      ++ Conformance.ClientDataPublication.casesJson ++ ","
    ++ "\"client_request_patch_publication_cases\":"
      ++ Conformance.RequestPatchPublicationCases.casesJson ++ ","
    ++ "\"client_request_closure_cases\":"
      ++ Conformance.ClientRequestClosure.casesJson ++ ","
    ++ "\"client_request_cache_identity_cases\":"
      ++ Conformance.ClientRequestCacheIdentity.casesJson ++ ","
    ++ "\"client_session_queue_order_cases\":"
      ++ Conformance.SessionQueueOrder.casesJson ++ ","
    ++ "\"client_session_turn_cases\":"
      ++ Conformance.ClientShellContracts.sessionTurnCasesJson ++ ","
    ++ "\"request_lifecycle_operator_ui_cases\":"
      ++ Conformance.ClientShellContracts.requestLifecycleOperatorUiCasesJson ++ ","
    ++ "\"client_live_delta_cases\":"
      ++ Conformance.ClientLiveDeltaContracts.casesJson ++ ","
    ++ "\"catalog_refresh_cases\":"
      ++ CatalogRefresh.casesJson ++ ","
    ++ "\"claude_platform_cases\":"
      ++ ClaudePlatform.casesJson ++ ","
    ++ "\"plugin_reference_cases\":"
      ++ PackInstallation.casesJson ++ ","
    ++ "\"plugin_pin_cases\":" ++ PackInstallation.pinCasesJson ++ ","
    ++ "\"pack_wire_lock_cases\":" ++ PackInstallation.wireLockCasesJson ++ ","
    ++ "\"pack_wire_target_cases\":" ++ PackInstallation.wireTargetCasesJson ++ ","
    ++ "\"pack_finality_cases\":" ++ PackInstallation.finalityCasesJson ++ ","
    ++ "\"snapshot_demotion_cases\":"
      ++ snapshotDemotionCasesJson ++ ","
    ++ "\"startup_readiness_cases\":"
      ++ startupReadinessCasesJson ++ ","
    ++ "\"readiness_publication_cases\":"
      ++ readinessPublicationCasesJson ++ ","
    ++ "\"runtime_reconcile_cases\":"
      ++ jsonArray (runtimeReconcileCases.map runtimeReconcileCaseJson) ++ ","
    ++ "\"client_agent_readiness_cases\":"
      ++ jsonArray (clientAgentReadinessCases.map clientAgentReadinessCaseJson) ++ ","
    ++ "\"apply_reconcile_cases\":"
      ++ ApplyReconcile.ContractCases.applyReconcileCasesJson ++ ","
    ++ "\"eval_outcome_cases\":"
      ++ Conformance.Eval.evalOutcomeCasesJson ++ ","
    ++ "\"eval_document_capture_cases\":"
      ++ Conformance.Eval.evalDocumentCaptureCasesJson ++ ","
    ++ "\"optimization_cases\":"
      ++ Conformance.Optimization.optimizationCasesJson ++ ","
    ++ "\"publish_if_cases\":"
      ++ ApplyReconcile.ContractCases.publishIfCasesJson ++ ","
    ++ "\"tool_policy_cases\":"
      ++ toolPolicyCasesJson ++ ","
    ++ "\"application_write_cases\":" ++ applicationWriteCasesJson ++ ","
    ++ "\"field_read_cases\":" ++ fieldReadCasesJson ++ ","
    ++ "\"write_input_cases\":" ++ writeInputCasesJson ++ ","
    ++ "\"invocation_correlation_cases\":" ++ invocationCorrelationCasesJson ++ ","
    ++ "\"goal_capability_resolution_cases\":"
      ++ goalCapabilityResolutionCasesJson ++ ","
    ++ "\"lsp_action_cases\":"
      ++ lspActionCasesJson ++ ","
    ++ "\"self_config_field_tables\":"
      ++ selfConfigFieldTablesJson ++ ","
    ++ "\"schema_publication_cases\":" ++ schemaPublicationCasesJson ++ ","
    ++ "\"self_config_cases\":"
      ++ selfConfigCasesJson ++ ","
    ++ "\"agent_decision_cases\":"
      ++ agentDecisionCasesJson ++ ","
    ++ "\"agent_materialization_cases\":"
      ++ agentMaterializationCasesJson ++ ","
    ++ "\"sibling_tools_cases\":"
      ++ siblingToolsCasesJson ++ ","
    ++ "\"self_config_selection_cases\":"
      ++ selfConfigSelectionCasesJson ++ ","
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
    ++ "\"account_rotation_select_cases\":"
      ++ AccountRotation.Contracts.selectCasesJson ++ ","
    ++ "\"account_rotation_attempt_cases\":"
      ++ AccountRotation.Contracts.attemptCasesJson ++ ","
    ++ "\"account_rotation_observe_cases\":"
      ++ AccountRotation.Contracts.observeCasesJson ++ ","
    ++ "\"boundaries\":"
      ++ boundariesJson ++ ","
    ++ "\"deviations\":"
      ++ deviationsJson ++ ","
    ++ "\"artifact_mode_meet_cases\":" ++ artifactModeMeetCasesJson ++ ","
    ++ "\"artifact_admission_cases\":" ++ artifactAdmissionCasesJson ++ ","
    ++ "\"artifact_spawn_cases\":" ++ artifactSpawnCasesJson ++ ","
    ++ "\"command_policy_cases\":"
      ++ jsonArray (CommandPolicy.commandPolicyCases.map commandPolicyCaseJson) ++ ","
    ++ "\"command_sandbox_cases\":"
      ++ jsonArray (CommandPolicy.commandSandboxCases.map commandSandboxCaseJson) ++ ","
    ++ "\"command_isolation_cases\":" ++ Conformance.CommandIsolation.casesJson.compress ++ ","
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
    ++ "\"agent_delivery_cases\":"
      ++ jsonArray (QueuedSteering.agentDeliveryCases.map agentDeliveryCaseJson) ++ ","
    ++ "\"interrupt_delivery_cases\":"
      ++ jsonArray (QueuedSteering.interruptDeliveryCases.map interruptDeliveryCaseJson) ++ ","
    ++ "\"agent_steer_disposition_cases\":"
      ++ jsonArray (QueuedSteering.agentSteerDispositionCases.map agentSteerDispositionCaseJson) ++ ","
    ++ "\"agent_steer_target_cases\":"
      ++ jsonArray (QueuedSteering.agentSteerTargetCases.map agentSteerTargetCaseJson) ++ ","
    ++ "\"agent_steering_boundary_cases\":"
      ++ jsonArray (QueuedSteering.boundaryCases.map boundaryCaseJson) ++ ","
    ++ "\"agent_steer_reachability_cases\":"
      ++ jsonArray (QueuedSteering.agentSteerReachabilityCases.map agentSteerReachabilityCaseJson) ++ ","
    ++ "\"agent_folded_binding_cases\":"
      ++ jsonArray (QueuedSteering.foldedBindingCases.map foldedBindingCaseJson) ++ ","
    ++ "\"agent_message_outcome_cases\":"
      ++ jsonArray (QueuedSteering.agentMessageOutcomeCases.map agentMessageOutcomeCaseJson) ++ ","
    ++ "\"queue_deadline_conformance_cases\":"
      ++ jsonArray
        (queueDeadlineConformanceCases.map queueDeadlineConformanceCaseJson) ++ ","
    ++ "\"recovery_sweep_cases\":"
      ++ jsonArray
        (Recovery.recoverySweepCases.map recoverySweepCaseJson) ++ ","
    ++ "\"session_message_kill_cases\":"
      ++ jsonArray (Recovery.killCases.map fun (observation, action) =>
        "{\"observation\":" ++ jsonString observation.toContract
          ++ ",\"action\":" ++ jsonString action.toContract ++ "}") ++ ","
    ++ "\"restart_disposition_cases\":"
      ++ jsonArray
        (Recovery.restartDispositionCases.map restartDispositionCaseJson) ++ ","
    ++ "\"r4c_background_work_cases\":"
      ++ jsonArray r4cBackgroundWorkCasesJson ++ ","
    ++ "\"tool_output_paging_cases\":"
      ++ jsonArray
        (toolOutputPagingCases.map toolOutputPagingCaseJson) ++ ","
    ++ "\"interrupt_disposition_cases\":"
      ++ jsonArray
        (interruptDispositionCases.map interruptDispositionCaseJson) ++ ","
    ++ "\"codex_shim_projection_cases\":"
      ++ codexShimProjectionCasesJson ++ ","
    ++ "\"codex_shim_reasoning_projection_cases\":"
      ++ codexShimReasoningProjectionCasesJson ++ ","
    ++ "\"codex_shim_thread_status_cases\":"
      ++ codexShimThreadStatusCasesJson ++ ","
    ++ "\"codex_shim_agent_selection_cases\":"
      ++ codexShimAgentSelectionCasesJson ++ ","
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
    ++ "\"composed_invariant_witnesses\":"
      ++ jsonArray
        (composedInvariantWitnesses.map composedInvariantWitnessJson) ++ ","
    ++ "\"count_carrier_cases\":" ++ Conformance.CountCarrier.casesJson ++ ","
    ++ "\"logical_output_obligation_cases\":"
      ++ Conformance.LogicalOutputObligationContracts.casesJson ++ ","
    ++ "\"mailbox_notification_cases\":"
      ++ Conformance.MailboxNotificationContracts.casesJson ++ ","
    ++ "\"mailbox_reply_cases\":"
      ++ Conformance.MailboxReplyContracts.casesJson ++ ","
    ++ "\"mailbox_question_cases\":"
      ++ Conformance.MailboxQuestionContracts.casesJson ++ ","
    ++ "\"mailbox_handoff_cases\":"
      ++ Conformance.MailboxHandoffContracts.casesJson ++ ","
    ++ "\"mailbox_sweep_cases\":"
      ++ Conformance.MailboxSweepContracts.casesJson ++ ","
    ++ "\"schema_argument_repair_cases\":"
      ++ Conformance.SchemaArgumentRepair.casesJson ++ ","
    ++ "\"invalid_tool_progress_cases\":"
      ++ Conformance.InvalidToolProgressContracts.casesJson ++ ","
    ++ "\"repeated_tool_failure_cases\":"
      ++ Conformance.RepeatedToolFailureContracts.casesJson ++ ","
    ++ "\"plugin_resource_cases\":"
      ++ Conformance.PluginResources.casesJson ++ ","
    ++ "\"plugin_data_cases\":" ++ Conformance.PluginResources.dataCasesJson ++ ","
    ++ "\"plugin_calls_route_cases\":" ++ Conformance.PluginResources.routeCasesJson ++ ","
    ++ "\"plugin_job_admission_cases\":" ++ Conformance.PluginJob.admissionCasesJson ++ ","
    ++ "\"plugin_job_adoption_cases\":" ++ Conformance.PluginJob.adoptionCasesJson ++ ","
    ++ "\"plugin_job_key_cases\":" ++ Conformance.PluginJob.keyCasesJson ++ ","
    ++ "\"plugin_job_control_cases\":" ++ Conformance.PluginJob.controlCasesJson ++ ","
    ++ "\"plugin_job_authority_cases\":" ++ Conformance.PluginJob.authorityCasesJson ++ ","
    ++ "\"plugin_job_wire_cases\":" ++ Conformance.PluginJob.wireCasesJson ++ ","
    ++ "\"plugin_job_lineage_cases\":" ++ Conformance.PluginJob.lineageCasesJson ++ ","
    ++ "\"plugin_job_recovery_cases\":" ++ Conformance.PluginJob.recoveryCasesJson ++ ","
    ++ "\"tool_timeout_cases\":"
      ++ Conformance.ToolTimeouts.casesJson ++ ","
    ++ "\"scope_collection_rules\":{"
      ++ "\"agentTargetCallerRules\":"
        ++ scopeRulesJson ScopeTemplates.agentTargetCallerRules
      ++ ",\"agentTargetHostRules\":" ++ scopeRulesJson ScopeTemplates.agentTargetHostRules ++ "},"
    ++ "\"causal_hop_contract\":"
      ++ Conformance.CausalHopContracts.contractJson ++ ","
    ++ "\"operator_base_freeze_cases\":"
      ++ Conformance.OperatorBaseFreezeContracts.casesJson ++ ","
    ++ "\"workspace_path_capability_cases\":"
      ++ Conformance.WorkspacePathCapabilityContracts.casesJson ++ ","
    ++ "\"file_inventory_refusal_cases\":"
      ++ Conformance.FileInventoryContracts.refusalCasesJson ++ ","
    ++ "\"file_inventory_reply_journal_cases\":"
      ++ Conformance.FileInventoryContracts.replyJournalCasesJson ++ ","
    ++ "\"file_inventory_operator_owner_cases\":"
      ++ Conformance.FileInventoryContracts.operatorOwnerCasesJson ++ ","
    ++ "\"file_inventory_client_progress_cases\":"
      ++ Conformance.FileInventoryContracts.clientProgressCasesJson ++ ","
    ++ "\"file_inventory_client_scope_cases\":"
      ++ Conformance.FileInventoryContracts.clientScopeCasesJson ++ ","
    ++ "\"file_inventory_preparation_cases\":"
      ++ Conformance.FileInventoryContracts.preparationCasesJson ++ ","
    ++ "\"file_inventory_completion_cases\":"
      ++ Conformance.FileInventoryContracts.completionCasesJson ++ ","
    ++ "\"file_inventory_page_grant_cases\":"
      ++ Conformance.FileInventoryContracts.pageGrantCasesJson ++ ","
    ++ "\"file_inventory_demand_cases\":"
      ++ Conformance.FileInventoryContracts.demandCasesJson ++ ","
    ++ "\"file_inventory_issuance_cases\":"
      ++ Conformance.FileInventoryContracts.issuanceCasesJson ++ ","
    ++ "\"file_inventory_fork_cases\":"
      ++ Conformance.FileInventoryContracts.forkCasesJson ++ ","
    ++ "\"file_inventory_source_projection_cases\":"
      ++ Conformance.FileInventoryContracts.sourceProjectionCasesJson ++ ","
    ++ "\"file_inventory_reader_cases\":"
      ++ Conformance.FileInventoryContracts.readerCasesJson ++ ","
    ++ "\"file_inventory_selected_image_cases\":"
      ++ Conformance.FileInventoryContracts.selectedImageCasesJson ++ ","
    ++ "\"file_inventory_receiver_cases\":"
      ++ Conformance.FileInventoryContracts.receiverCasesJson ++ ","
    ++ "\"file_inventory_order_window_cases\":"
      ++ Conformance.FileInventoryContracts.orderingWindowCasesJson ++ ","
    ++ "\"file_inventory_order_resolution_cases\":"
      ++ Conformance.FileInventoryContracts.orderingResolutionCasesJson ++ ","
    ++ "\"file_inventory_order_comparison_cases\":"
      ++ Conformance.FileInventoryContracts.orderingComparisonCasesJson ++ ","
    ++ "\"file_inventory_order_entry_cases\":"
      ++ Conformance.FileInventoryContracts.orderingEntryCasesJson ++ ","
    ++ "\"file_image_hydration_cases\":"
      ++ Conformance.FileImageHydrationContracts.casesJson ++ ","
    ++ "\"file_image_hydration_route_cases\":"
      ++ Conformance.FileImageHydrationContracts.routeCasesJson ++ ","
    ++ "\"file_review_subscription_cases\":"
      ++ Conformance.FileImageHydrationContracts.subscriptionCasesJson ++ ","
    ++ "\"file_image_hydration_completion_cases\":"
      ++ Conformance.FileImageHydrationContracts.completionCasesJson ++ ","
    ++ "\"file_image_attempt_cases\":"
      ++ Conformance.FileImageHydrationContracts.attemptCasesJson ++ ","
    ++ "\"stored_file_image_access_cases\":"
      ++ Conformance.FileImageHydrationContracts.storageCasesJson ++ ","
    ++ "\"file_image_read_grant_cases\":"
      ++ Conformance.FileImageHydrationContracts.readGrantCasesJson ++ ","
    ++ "\"workspace_path_alias_cases\":"
      ++ Conformance.WorkspacePathCapabilityContracts.aliasCasesJson ++ ","
    ++ "\"workspace_cases\":"
      ++ workspaceCasesJson ++ ","
    ++ "\"workspace_binding_cases\":"
      ++ workspaceBindingCasesJson ++ ","
    ++ "\"callback_transition_case_count\":" ++ toString callbackTransitionCaseCount ++ ","
    ++ "\"callback_transition_cases\":" ++ callbackTransitionCasesJson ++ ","
    ++ "\"callback_retry_cases\":" ++ callbackRetryCasesJson ++ ","
    ++ "\"callback_recovery_cases\":" ++ callbackRecoveryCasesJson ++ ","
    ++ "\"callback_cases\":"
      ++ callbackCasesJson ++ ","
    ++ "\"r6_background_theorem_witnesses\":"
      ++ jsonArray
        (r6BackgroundTheoremWitnesses.map backgroundTheoremWitnessJson) ++ ","
    ++ "\"transcript_conformance_cases\":"
      ++ jsonArray
        (transcriptConformanceCases.map transcriptCaseJson) ++ ","
    ++ "\"request_terminal_selection_cases\":" ++ TerminalSelection.casesJson ++ ","
    ++ "\"canonical_output_projection_cases\":"
      ++ jsonArray
        (StreamingResponse.outputProjectionCases.map outputProjectionCaseJson) ++ ","
    ++ "\"reasoning_audit_cases\":" ++ jsonArray
      (StreamingResponse.ReasoningAudit.auditCases.map reasoningAuditCaseJson) ++ ","
    ++ "\"reasoning_signature_cases\":" ++ jsonArray
      (StreamingResponse.ReasoningAudit.signatureCases.map reasoningSignatureCaseJson) ++ ","
    ++ "\"reasoning_delta_cases\":" ++ jsonArray
      (StreamingResponse.ReasoningDeltas.cases.map reasoningDeltaCaseJson) ++ ","
    ++ "\"auxiliary_output_cases\":" ++ jsonArray
      (CanonicalOutput.Execution.AuxiliaryCases.cases.map auxiliaryOutputCaseJson) ++ ","
    ++ "\"prompt_assembly_claude_wire_start_cases\":" ++
      promptAssemblyClaudeWireStartCasesJson ++ ","
    ++ "\"routing_affinity_cases\":" ++ routingAffinityCasesJson ++ ","
    ++ "\"prompt_assembly_service_tier_cases\":" ++
      promptAssemblyServiceTierCasesJson ++ ","
    ++ "\"prompt_assembly_responses_storage_cases\":" ++
      promptAssemblyResponsesStorageCasesJson ++ ","
    ++ "\"prompt_assembly_responses_effort_cases\":" ++
      promptAssemblyResponsesEffortCasesJson ++ ","
    ++ "\"retry_frontier_cases\":" ++ retryFrontierCasesJson ++ ","
    ++ "\"retry_entry_cases\":" ++ retryEntryCasesJson ++ ","
    ++ "\"current_input_cases\":" ++ currentInputCasesJson ++ ","
    ++ "\"wake_input_cases\":" ++ wakeInputCasesJson ++ ","
    ++ "\"prompt_assembly_sanitize_cases\":"
      ++ promptAssemblySanitizeCasesJson ++ ","
    ++ "\"provider_tool_identity_cases\":" ++ providerToolIdentityCasesJson ++ ","

    ++ "\"discovery_guidance_cases\":" ++ discoveryGuidanceCasesJson ++ ","
    ++ "\"prompt_assembly_assistant_order_cases\":"
      ++ promptAssemblyAssistantOrderCasesJson ++ ","
    ++ "\"prompt_assembly_mode_sanitize_cases\":"
      ++ promptAssemblyModeSanitizeCasesJson ++ ","
    ++ "\"prompt_assembly_layer_cases\":"
      ++ promptAssemblyLayerCasesJson ++ ","
    ++ "\"prompt_assembly_repair_cases\":"
      ++ promptAssemblyRepairCasesJson ++ ","
    ++ "\"prompt_assembly_budget_cases\":"
      ++ promptAssemblyBudgetCasesJson ++ ","
    ++ "\"prompt_assembly_turn_budget_cases\":"
      ++ promptAssemblyTurnBudgetCasesJson ++ ","
    ++ snapshotJsonTail

end Conformance.Contracts
