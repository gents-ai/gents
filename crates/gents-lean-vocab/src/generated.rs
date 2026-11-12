use crate::*;
use crate::canonical_output::LeanCanonicalMessage;
use crate::required_nullable;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn lean_agent_decision_cases() -> &'static [LeanAgentDecisionCase] {
    &lean_contract_snapshot().agent_decision_cases
}

pub fn lean_agent_materialization_cases() -> &'static [LeanAgentMaterializationCase] {
    &lean_contract_snapshot().agent_materialization_cases
}

pub fn lean_agent_request_admission_cases() -> &'static [LeanAgentRequestAdmissionCase] {
    &lean_contract_snapshot().agent_request_admission_cases
}

pub fn lean_aggregate_token_budget_cases() -> &'static [LeanAggregateTokenBudgetCase] {
    &lean_contract_snapshot().aggregate_token_budget_cases
}

pub fn lean_application_write_cases() -> &'static [LeanApplicationWriteCase] {
    &lean_contract_snapshot().application_write_cases
}

pub fn lean_auxiliary_output_cases() -> &'static [LeanAuxiliaryOutputCase] {
    &lean_contract_snapshot().auxiliary_output_cases
}

pub fn lean_backend_health_admission_cases() -> &'static [LeanBackendHealthAdmissionCase] {
    &lean_contract_snapshot().backend_health_admission_cases
}

pub fn lean_backend_health_cases() -> &'static [LeanBackendHealthCase] {
    &lean_contract_snapshot().backend_health_cases
}

pub fn lean_backend_probe_schedule_cases() -> &'static [LeanBackendProbeScheduleCase] {
    &lean_contract_snapshot().backend_probe_schedule_cases
}

pub fn lean_budget_rehydration_cases() -> &'static [LeanBudgetRehydrationCase] {
    &lean_contract_snapshot().budget_rehydration_cases
}

pub fn lean_callback_cases() -> &'static [LeanCallbackCase] {
    &lean_contract_snapshot().callback_cases
}

pub fn lean_callback_recovery_cases() -> &'static [LeanCallbackRecoveryCase] {
    &lean_contract_snapshot().callback_recovery_cases
}

pub fn lean_callback_retry_cases() -> &'static [LeanCallbackRetryCase] {
    &lean_contract_snapshot().callback_retry_cases
}

pub fn lean_canonical_dispatch_observation_cases() -> &'static [LeanDispatchObservationCase] {
    &lean_contract_snapshot().canonical_dispatch_observation_cases
}

pub fn lean_canonical_execution_gate_cases() -> &'static [LeanCanonicalExecutionCase] {
    &lean_contract_snapshot().canonical_execution_gate_cases
}

pub fn lean_canonical_output_projection_cases() -> &'static [LeanCanonicalOutputProjectionCase] {
    &lean_contract_snapshot().canonical_output_projection_cases
}

pub fn lean_canonical_payload_presentation_cases() -> &'static [LeanPayloadPresentationCase] {
    &lean_contract_snapshot().canonical_payload_presentation_cases
}

pub fn lean_canonical_spawned_target_rejection_cases() -> &'static [LeanSpawnedTargetRejectionCase] {
    &lean_contract_snapshot().canonical_spawned_target_rejection_cases
}

pub fn lean_canonical_worker_capacity_cases() -> &'static [LeanWorkerCapacityCase] {
    &lean_contract_snapshot().canonical_worker_capacity_cases
}

pub fn lean_capture_order_cases() -> &'static [LeanCaptureOrderCase] {
    &lean_contract_snapshot().capture_order_cases
}

pub fn lean_capture_scope_cases() -> &'static [LeanCaptureScopeCase] {
    &lean_contract_snapshot().capture_scope_cases
}

pub fn lean_client_agent_readiness_cases() -> &'static [LeanClientAgentReadinessCase] {
    &lean_contract_snapshot().client_agent_readiness_cases
}

pub fn lean_client_session_queue_order_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().client_session_queue_order_cases
}

pub fn lean_client_session_turn_cases() -> &'static [LeanSessionTurnCase] {
    &lean_contract_snapshot().client_session_turn_cases
}

pub fn lean_command_env_cases() -> &'static [LeanCommandEnvCase] {
    &lean_contract_snapshot().command_env_cases
}

pub fn lean_command_policy_cases() -> &'static [LeanCommandPolicyCase] {
    &lean_contract_snapshot().command_policy_cases
}

pub fn lean_compaction_canonical_projection_cases() -> &'static [LeanCanonicalCompactionCase] {
    &lean_contract_snapshot().compaction_canonical_projection_cases
}

pub fn lean_compaction_cursor_cases() -> &'static [LeanCompactionCursorCase] {
    &lean_contract_snapshot().compaction_cursor_cases
}

pub fn lean_compaction_projection_join_cases() -> &'static [LeanCompactionProjectionJoinCase] {
    &lean_contract_snapshot().compaction_projection_join_cases
}

pub fn lean_compaction_reducer_cases() -> &'static [LeanCompactionReducerCase] {
    &lean_contract_snapshot().compaction_reducer_cases
}

pub fn lean_completion_retry_cases() -> &'static [LeanCompletionRetryCase] {
    &lean_contract_snapshot().completion_retry_cases
}

pub fn lean_composed_invariant_witnesses() -> &'static [LeanComposedInvariantWitness] {
    &lean_contract_snapshot().composed_invariant_witnesses
}

pub fn lean_desktop_client_shell_cases() -> &'static [LeanClientShellCase] {
    &lean_contract_snapshot().desktop_client_shell_cases
}

pub fn lean_durable_reduction_cases() -> &'static [LeanDurableReductionCase] {
    &lean_contract_snapshot().durable_reduction_cases
}

pub fn lean_enrollment_cases() -> &'static [LeanEnrollmentCase] {
    &lean_contract_snapshot().enrollment_cases
}

pub fn lean_enrollment_digest_cases() -> &'static [LeanEnrollmentDigestCase] {
    &lean_contract_snapshot().enrollment_digest_cases
}

pub fn lean_enrollment_durable_projection_cases() -> &'static [LeanEnrollmentDurableProjectionCase] {
    &lean_contract_snapshot().enrollment_durable_projection_cases
}

pub fn lean_enrollment_encoding_cases() -> &'static [LeanEnrollmentEncodingCase] {
    &lean_contract_snapshot().enrollment_encoding_cases
}

pub fn lean_eval_document_capture_cases() -> &'static [LeanEvalDocumentCaptureCase] {
    &lean_contract_snapshot().eval_document_capture_cases
}

pub fn lean_eval_outcome_cases() -> &'static [LeanEvalOutcomeCase] {
    &lean_contract_snapshot().eval_outcome_cases
}

pub fn lean_event_delivery_convergence_traces() -> &'static [LeanEventDeliveryConvergenceTrace] {
    &lean_contract_snapshot().event_delivery_convergence_traces
}

pub fn lean_event_delivery_source_instances() -> &'static [LeanEventDeliverySourceInstance] {
    &lean_contract_snapshot().event_delivery_source_instances
}

pub fn lean_event_delivery_transition_cases() -> &'static [LeanEventDeliveryTransitionCase] {
    &lean_contract_snapshot().event_delivery_transition_cases
}

pub fn lean_event_group_cases() -> &'static [LeanEventGroupCase] {
    &lean_contract_snapshot().event_group_cases
}

pub fn lean_feature_matrix() -> &'static LeanFeatureMatrix {
    &lean_contract_snapshot().feature_matrix
}

pub fn lean_feature_surface_requirements() -> &'static [LeanFeatureSurfaceRequirement] {
    &lean_contract_snapshot().feature_surface_requirements
}

pub fn lean_file_inventory_closure_cases() -> &'static [LeanSessionClosureCase] {
    &lean_contract_snapshot().file_inventory_closure_cases
}

pub fn lean_fold_publication_cases() -> &'static [LeanFoldPublicationCase] {
    &lean_contract_snapshot().fold_publication_cases
}

pub fn lean_fold_queue_cases() -> &'static [LeanFoldQueueCase] {
    &lean_contract_snapshot().fold_queue_cases
}

pub fn lean_fold_turn_input_cases() -> &'static [LeanFoldTurnInputCase] {
    &lean_contract_snapshot().fold_turn_input_cases
}

pub fn lean_goal_capability_resolution_cases() -> &'static [LeanGoalCapabilityResolutionCase] {
    &lean_contract_snapshot().goal_capability_resolution_cases
}

pub fn lean_goal_create_cases() -> &'static [LeanGoalCreateCase] {
    &lean_contract_snapshot().goal_create_cases
}

pub fn lean_handover_fold_cases() -> &'static [LeanHandoverFoldCase] {
    &lean_contract_snapshot().handover_fold_cases
}

pub fn lean_identity_contracts() -> &'static [LeanIdentityContract] {
    &lean_contract_snapshot().identity_contracts
}

pub fn lean_identity_derivation_cases() -> &'static [LeanIdentityDerivationCase] {
    &lean_contract_snapshot().identity_derivation_cases
}

pub fn lean_identity_household_cases() -> &'static [LeanIdentityHouseholdCase] {
    &lean_contract_snapshot().identity_household_cases
}

pub fn lean_identity_permission_cases() -> &'static [LeanIdentityPermissionCase] {
    &lean_contract_snapshot().identity_permission_cases
}

pub fn lean_inference_slot_accounting_cases() -> &'static [LeanInferenceSlotAccountingCase] {
    &lean_contract_snapshot().inference_slot_accounting_cases
}

pub fn lean_interrupt_disposition_cases() -> &'static [LeanInterruptDispositionCase] {
    &lean_contract_snapshot().interrupt_disposition_cases
}

pub fn lean_interrupt_queue_cases() -> &'static [LeanInterruptQueueCase] {
    &lean_contract_snapshot().interrupt_queue_cases
}

pub fn lean_lifecycle_hook_admit_cases() -> &'static [LeanLifecycleHookAdmitCase] {
    &lean_contract_snapshot().lifecycle_hook_admit_cases
}

pub fn lean_lifecycle_hook_combine_cases() -> &'static [LeanLifecycleHookCombineCase] {
    &lean_contract_snapshot().lifecycle_hook_combine_cases
}

pub fn lean_lifecycle_hook_delivery_cases() -> &'static [LeanLifecycleHookDeliveryCase] {
    &lean_contract_snapshot().lifecycle_hook_delivery_cases
}

pub fn lean_lifecycle_hook_effect_cases() -> &'static [LeanLifecycleHookEffectCase] {
    &lean_contract_snapshot().lifecycle_hook_effect_cases
}

pub fn lean_lifecycle_hook_gate_cases() -> &'static [LeanLifecycleHookGateCase] {
    &lean_contract_snapshot().lifecycle_hook_gate_cases
}

pub fn lean_lifecycle_hook_goal_halt_cases() -> &'static [LeanLifecycleHookGoalHaltCase] {
    &lean_contract_snapshot().lifecycle_hook_goal_halt_cases
}

pub fn lean_lifecycle_hook_goal_resume_cases() -> &'static [LeanLifecycleHookGoalResumeCase] {
    &lean_contract_snapshot().lifecycle_hook_goal_resume_cases
}

pub fn lean_lifecycle_hook_grant_cases() -> &'static [LeanLifecycleHookGrantCase] {
    &lean_contract_snapshot().lifecycle_hook_grant_cases
}

pub fn lean_lifecycle_hook_history_cases() -> &'static [LeanLifecycleHookHistoryCase] {
    &lean_contract_snapshot().lifecycle_hook_history_cases
}

pub fn lean_lifecycle_hook_must_commit_cases() -> &'static [LeanLifecycleHookMustCommitCase] {
    &lean_contract_snapshot().lifecycle_hook_must_commit_cases
}

pub fn lean_lifecycle_hook_owed_delivery_cases() -> &'static LeanLifecycleHookOwedDeliveryCases {
    &lean_contract_snapshot().lifecycle_hook_owed_delivery_cases
}

pub fn lean_lifecycle_hook_queue_scope_cases() -> &'static [LeanLifecycleHookQueueScopeCase] {
    &lean_contract_snapshot().lifecycle_hook_queue_scope_cases
}

pub fn lean_lifecycle_hook_rerun_cases() -> &'static [LeanLifecycleHookRerunCase] {
    &lean_contract_snapshot().lifecycle_hook_rerun_cases
}

pub fn lean_lifecycle_hook_stage_cases() -> &'static [LeanLifecycleHookStageCase] {
    &lean_contract_snapshot().lifecycle_hook_stage_cases
}

pub fn lean_lifecycle_hook_supersede_cases() -> &'static [LeanLifecycleHookSupersedeCase] {
    &lean_contract_snapshot().lifecycle_hook_supersede_cases
}

pub fn lean_live_overlay_cases() -> &'static [LeanLiveOverlayCase] {
    &lean_contract_snapshot().live_overlay_cases
}

pub fn lean_lsp_action_cases() -> &'static [LeanLspActionCase] {
    &lean_contract_snapshot().lsp_action_cases
}

pub fn lean_managed_exec_liveness_cases() -> &'static [LeanManagedExecLivenessCase] {
    &lean_contract_snapshot().managed_exec_liveness_cases
}

pub fn lean_managed_exec_tool_boundary_cases() -> &'static [LeanManagedExecToolBoundaryCase] {
    &lean_contract_snapshot().managed_exec_tool_boundary_cases
}

pub fn lean_mcp_health_cases() -> &'static [LeanMcpHealthCase] {
    &lean_contract_snapshot().mcp_health_cases
}

pub fn lean_native_filesystem_boundary_cases() -> &'static [LeanNativeFilesystemBoundaryCase] {
    &lean_contract_snapshot().native_filesystem_boundary_cases
}

pub fn lean_optimization_cases() -> &'static LeanOptimizationCases {
    &lean_contract_snapshot().optimization_cases
}

pub fn lean_pack_eval_subject_cases() -> &'static [LeanPackEvalSubjectCase] {
    &lean_contract_snapshot().pack_eval_subject_cases
}

pub fn lean_pairing_reconcile_cases() -> &'static [LeanPairingReconcileCase] {
    &lean_contract_snapshot().pairing_reconcile_cases
}

pub fn lean_pairing_reconcile_shutdown_boundary_cases() -> &'static [LeanPairingReconcileShutdownBoundaryCase] {
    &lean_contract_snapshot().pairing_reconcile_shutdown_boundary_cases
}

pub fn lean_pairing_reconcile_sweep_retry_boundary_cases() -> &'static [LeanPairingReconcileSweepRetryBoundaryCase] {
    &lean_contract_snapshot().pairing_reconcile_sweep_retry_boundary_cases
}

pub fn lean_pairing_reconcile_sweep_scheduling_cases() -> &'static [LeanPairingReconcileSweepSchedulingCase] {
    &lean_contract_snapshot().pairing_reconcile_sweep_scheduling_cases
}

pub fn lean_pending_user_turn_cases() -> &'static [LeanPendingUserTurnCase] {
    &lean_contract_snapshot().pending_user_turn_cases
}

pub fn lean_persistence_failure_policy_cases() -> &'static [LeanPersistenceFailurePolicyCase] {
    &lean_contract_snapshot().persistence_failure_policy_cases
}

pub fn lean_plugin_calls_route_cases() -> &'static [LeanPluginCallsRouteCase] {
    &lean_contract_snapshot().plugin_calls_route_cases
}

pub fn lean_plugin_data_cases() -> &'static LeanPluginDataCases {
    &lean_contract_snapshot().plugin_data_cases
}

pub fn lean_plugin_job_admission_cases() -> &'static [LeanPluginJobAdmissionCase] {
    &lean_contract_snapshot().plugin_job_admission_cases
}

pub fn lean_plugin_job_adoption_cases() -> &'static [LeanPluginJobAdoptionCase] {
    &lean_contract_snapshot().plugin_job_adoption_cases
}

pub fn lean_plugin_job_authority_cases() -> &'static [LeanPluginJobAuthorityCase] {
    &lean_contract_snapshot().plugin_job_authority_cases
}

pub fn lean_plugin_job_control_cases() -> &'static [LeanPluginJobControlCase] {
    &lean_contract_snapshot().plugin_job_control_cases
}

pub fn lean_plugin_job_key_cases() -> &'static [LeanPluginJobKeyCase] {
    &lean_contract_snapshot().plugin_job_key_cases
}

pub fn lean_plugin_job_lineage_cases() -> &'static [LeanPluginJobLineageCase] {
    &lean_contract_snapshot().plugin_job_lineage_cases
}

pub fn lean_plugin_job_recovery_cases() -> &'static [LeanPluginJobRecoveryCase] {
    &lean_contract_snapshot().plugin_job_recovery_cases
}

pub fn lean_plugin_job_wake_cases() -> &'static [LeanPluginJobWakeCase] {
    &lean_contract_snapshot().plugin_job_wake_cases
}

pub fn lean_plugin_job_wire_cases() -> &'static [LeanPluginJobWireCase] {
    &lean_contract_snapshot().plugin_job_wire_cases
}

pub fn lean_plugin_reference_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().plugin_reference_cases
}

pub fn lean_process_stop_cases() -> &'static [LeanProcessStopCase] {
    &lean_contract_snapshot().process_stop_cases
}

pub fn lean_process_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().process_transition_cases
}

pub fn lean_prompt_assembly_assistant_order_cases() -> &'static [LeanPromptAssemblyAssistantOrderCase] {
    &lean_contract_snapshot().prompt_assembly_assistant_order_cases
}

pub fn lean_prompt_assembly_budget_cases() -> &'static [LeanPromptAssemblyBudgetCase] {
    &lean_contract_snapshot().prompt_assembly_budget_cases
}

pub fn lean_prompt_assembly_claude_body_cases() -> &'static [LeanPromptAssemblyClaudeBodyCase] {
    &lean_contract_snapshot().prompt_assembly_claude_body_cases
}

pub fn lean_prompt_assembly_claude_checkpoint_cases() -> &'static [LeanPromptAssemblyClaudeCheckpointCase] {
    &lean_contract_snapshot().prompt_assembly_claude_checkpoint_cases
}

pub fn lean_prompt_assembly_claude_map_cases() -> &'static [LeanPromptAssemblyClaudeMapCase] {
    &lean_contract_snapshot().prompt_assembly_claude_map_cases
}

pub fn lean_prompt_assembly_claude_replay_cases() -> &'static [LeanPromptAssemblyClaudeReplayCase] {
    &lean_contract_snapshot().prompt_assembly_claude_replay_cases
}

pub fn lean_prompt_assembly_claude_stream_cases() -> &'static [LeanPromptAssemblyClaudeStreamCase] {
    &lean_contract_snapshot().prompt_assembly_claude_stream_cases
}

pub fn lean_prompt_assembly_claude_thinking_stream_cases() -> &'static [LeanPromptAssemblyClaudeThinkingStreamCase] {
    &lean_contract_snapshot().prompt_assembly_claude_thinking_stream_cases
}

pub fn lean_prompt_assembly_compaction_trigger_cases() -> &'static [LeanPromptAssemblyCompactionTriggerCase] {
    &lean_contract_snapshot().prompt_assembly_compaction_trigger_cases
}

pub fn lean_prompt_assembly_layer_cases() -> &'static [LeanPromptAssemblyLayerCase] {
    &lean_contract_snapshot().prompt_assembly_layer_cases
}

pub fn lean_prompt_assembly_mode_sanitize_cases() -> &'static [LeanPromptAssemblyModeSanitizeCase] {
    &lean_contract_snapshot().prompt_assembly_mode_sanitize_cases
}

pub fn lean_prompt_assembly_reasoning_suffix_cases() -> &'static [LeanPromptAssemblyReasoningSuffixCase] {
    &lean_contract_snapshot().prompt_assembly_reasoning_suffix_cases
}

pub fn lean_prompt_assembly_repair_cases() -> &'static [LeanPromptAssemblyRepairCase] {
    &lean_contract_snapshot().prompt_assembly_repair_cases
}

pub fn lean_prompt_assembly_replay_prefix_cases() -> &'static [LeanPromptAssemblyReplayPrefixCase] {
    &lean_contract_snapshot().prompt_assembly_replay_prefix_cases
}

pub fn lean_prompt_assembly_replay_shape_cases() -> &'static [LeanPromptAssemblyReplayShapeCase] {
    &lean_contract_snapshot().prompt_assembly_replay_shape_cases
}

pub fn lean_prompt_assembly_responses_effort_cases() -> &'static [LeanPromptAssemblyResponsesEffortCase] {
    &lean_contract_snapshot().prompt_assembly_responses_effort_cases
}

pub fn lean_prompt_assembly_responses_storage_cases() -> &'static [LeanPromptAssemblyResponsesStorageCase] {
    &lean_contract_snapshot().prompt_assembly_responses_storage_cases
}

pub fn lean_prompt_assembly_retention_cases() -> &'static [LeanPromptAssemblyRetentionCase] {
    &lean_contract_snapshot().prompt_assembly_retention_cases
}

pub fn lean_prompt_assembly_sanitize_cases() -> &'static [LeanPromptAssemblySanitizeCase] {
    &lean_contract_snapshot().prompt_assembly_sanitize_cases
}

pub fn lean_prompt_assembly_service_tier_cases() -> &'static [LeanPromptAssemblyServiceTierCase] {
    &lean_contract_snapshot().prompt_assembly_service_tier_cases
}

pub fn lean_prompt_assembly_turn_budget_cases() -> &'static [LeanPromptAssemblyTurnBudgetCase] {
    &lean_contract_snapshot().prompt_assembly_turn_budget_cases
}

pub fn lean_protected_replay_compaction_cases() -> &'static [LeanProtectedReplayCompactionCase] {
    &lean_contract_snapshot().protected_replay_compaction_cases
}

pub fn lean_provider_eof_cases() -> &'static [LeanProviderEofCase] {
    &lean_contract_snapshot().provider_eof_cases
}

pub fn lean_queue_deadline_cases() -> &'static [LeanQueueDeadlineConformanceCase] {
    &lean_contract_snapshot().queue_deadline_conformance_cases
}

pub fn lean_queue_management_cases() -> &'static [LeanQueueManagementCase] {
    &lean_contract_snapshot().queue_management_cases
}

pub fn lean_queued_steering_guard_cases() -> &'static [LeanQueuedSteeringGuardCase] {
    &lean_contract_snapshot().queued_steering_guard_cases
}

pub fn lean_queued_steering_trace_cases() -> &'static [LeanQueuedSteeringTraceCase] {
    &lean_contract_snapshot().queued_steering_trace_cases
}

pub fn lean_r4c_background_work_cases() -> &'static [LeanR4cBackgroundWorkCase] {
    &lean_contract_snapshot().r4c_background_work_cases
}

pub fn lean_r6_background_theorem_witnesses() -> &'static [LeanBackgroundTheoremWitness] {
    &lean_contract_snapshot().r6_background_theorem_witnesses
}

pub fn lean_r6_backgrounding_cases() -> &'static [LeanR6BackgroundingCase] {
    &lean_contract_snapshot().r6_backgrounding_cases
}

pub fn lean_readiness_publication_cases() -> &'static [LeanReadinessPublicationCase] {
    &lean_contract_snapshot().readiness_publication_cases
}

pub fn lean_reasoning_audit_cases() -> &'static [LeanReasoningAuditCase] {
    &lean_contract_snapshot().reasoning_audit_cases
}

pub fn lean_reasoning_delta_cases() -> &'static [LeanReasoningDeltaCase] {
    &lean_contract_snapshot().reasoning_delta_cases
}

pub fn lean_reasoning_signature_cases() -> &'static [LeanReasoningSignatureCase] {
    &lean_contract_snapshot().reasoning_signature_cases
}

pub fn lean_recovery_sweep_cases() -> &'static [LeanRecoverySweepCase] {
    &lean_contract_snapshot().recovery_sweep_cases
}

pub fn lean_rendered_capture_cases() -> &'static [LeanRenderedCaptureCase] {
    &lean_contract_snapshot().rendered_capture_cases
}

pub fn lean_rendered_capture_key_cases() -> &'static [LeanRenderedCaptureKeyCase] {
    &lean_contract_snapshot().rendered_capture_key_cases
}

pub fn lean_repaired_projection_admission_cases() -> &'static [LeanRepairedProjectionCase] {
    &lean_contract_snapshot().repaired_projection_admission_cases
}

pub fn lean_request_execution_lease_cases() -> &'static [LeanRequestExecutionLeaseCase] {
    &lean_contract_snapshot().request_execution_lease_cases
}

pub fn lean_request_execution_lease_observed_cases() -> &'static [LeanRequestExecutionObservedCase] {
    &lean_contract_snapshot().request_execution_lease_observed_cases
}

pub fn lean_request_execution_lease_trace_cases() -> &'static [LeanRequestExecutionLeaseTraceCase] {
    &lean_contract_snapshot().request_execution_lease_trace_cases
}

pub fn lean_request_lifecycle_operator_ui_cases() -> &'static [LeanClientShellCase] {
    &lean_contract_snapshot().request_lifecycle_operator_ui_cases
}

pub fn lean_request_progress_cases() -> &'static [LeanRequestProgressCase] {
    &lean_contract_snapshot().request_progress_cases
}

pub fn lean_request_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().request_transition_cases
}

pub fn lean_restart_disposition_cases() -> &'static [LeanRestartDispositionCase] {
    &lean_contract_snapshot().restart_disposition_cases
}

pub fn lean_retry_selection_cases() -> &'static [LeanRetrySelectionCase] {
    &lean_contract_snapshot().retry_selection_cases
}

pub fn lean_runtime_reconcile_cases() -> &'static [LeanRuntimeReconcileCase] {
    &lean_contract_snapshot().runtime_reconcile_cases
}

pub fn lean_self_config_cases() -> &'static [LeanSelfConfigCase] {
    &lean_contract_snapshot().self_config_cases
}

pub fn lean_self_config_field_tables() -> &'static [LeanSelfConfigFieldTable] {
    &lean_contract_snapshot().self_config_field_tables
}

pub fn lean_self_config_selection_cases() -> &'static LeanSelfConfigSelectionCases {
    &lean_contract_snapshot().self_config_selection_cases
}

pub fn lean_session_control_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().session_control_cases
}

pub fn lean_session_input_edit_cases() -> &'static [LeanSessionInputEditCase] {
    &lean_contract_snapshot().session_input_edit_cases
}

pub fn lean_sibling_tools_cases() -> &'static [LeanSiblingToolsCase] {
    &lean_contract_snapshot().sibling_tools_cases
}

pub fn lean_snapshot_demotion_cases() -> &'static [LeanSnapshotDemotionCase] {
    &lean_contract_snapshot().snapshot_demotion_cases
}

pub fn lean_startup_readiness_cases() -> &'static [LeanStartupReadinessCase] {
    &lean_contract_snapshot().startup_readiness_cases
}

pub fn lean_steering_publication_cases() -> &'static [LeanFoldPublicationCase] {
    &lean_contract_snapshot().steering_publication_cases
}

pub fn lean_storage_observation_runtime_cases() -> &'static [LeanStorageObservationRuntimeCase] {
    &lean_contract_snapshot().storage_observation_runtime_cases
}

pub fn lean_task_goal_publication_cases() -> &'static [LeanTaskGoalPublicationCase] {
    &lean_contract_snapshot().task_goal_publication_cases
}

pub fn lean_task_hook_admission_cases() -> &'static [LeanTaskHookAdmissionCase] {
    &lean_contract_snapshot().task_hook_admission_cases
}

pub fn lean_task_hook_recovery_cases() -> &'static [LeanTaskHookRecoveryCase] {
    &lean_contract_snapshot().task_hook_recovery_cases
}

pub fn lean_task_hook_run_cases() -> &'static [LeanTaskHookRunCase] {
    &lean_contract_snapshot().task_hook_run_cases
}

pub fn lean_terminal_diagnostic_presentation_cases() -> &'static [LeanTerminalDiagnosticPresentationCase] {
    &lean_contract_snapshot().terminal_diagnostic_presentation_cases
}

pub fn lean_terminal_diagnostic_replay_cases() -> &'static [LeanTerminalDiagnosticReplayCase] {
    &lean_contract_snapshot().terminal_diagnostic_replay_cases
}

pub fn lean_title_admission_join_cases() -> &'static [LeanTitleAdmissionJoinCase] {
    &lean_contract_snapshot().title_admission_join_cases
}

pub fn lean_title_request_admission_cases() -> &'static [LeanTitleRequestAdmissionCase] {
    &lean_contract_snapshot().title_request_admission_cases
}

pub fn lean_title_request_purpose_wire_cases() -> &'static [LeanTitleRequestPurposeWireCase] {
    &lean_contract_snapshot().title_request_purpose_wire_cases
}

pub fn lean_title_usage_cases() -> &'static [LeanTitleUsageCase] {
    &lean_contract_snapshot().title_usage_cases
}

pub fn lean_tool_output_paging_cases() -> &'static [LeanToolOutputPagingCase] {
    &lean_contract_snapshot().tool_output_paging_cases
}

pub fn lean_tool_policy_cases() -> &'static [LeanToolPolicyCase] {
    &lean_contract_snapshot().tool_policy_cases
}

pub fn lean_tool_preflight_cases() -> &'static [LeanToolPreflightCase] {
    &lean_contract_snapshot().tool_preflight_cases
}

pub fn lean_transcript_cases() -> &'static [LeanTranscriptCase] {
    &lean_contract_snapshot().transcript_conformance_cases
}

pub fn lean_trigger_dispatch_cases() -> &'static [LeanTriggerDispatchCase] {
    &lean_contract_snapshot().trigger_dispatch_cases
}

pub fn lean_workspace_binding_cases() -> &'static [LeanWorkspaceBindingCase] {
    &lean_contract_snapshot().workspace_binding_cases
}

pub fn lean_workspace_cases() -> &'static [LeanWorkspaceCase] {
    &lean_contract_snapshot().workspace_cases
}

pub fn lean_write_input_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().write_input_cases
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAccessReviewTaskCase {
    pub agent: String,
    #[serde(rename = "approvedPrompt")]
    pub approved_prompt: String,
    pub document: LeanAccessReviewTaskCaseDocument,
    pub eligible: bool,
    pub node: String,
    #[serde(rename = "selectedEligible")]
    pub selected_eligible: bool,
    #[serde(rename = "taskId")]
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAccessReviewTaskCaseDocument {
    pub agent_did: String,
    pub emit_outcome: bool,
    pub enabled: bool,
    pub goal_objective_template: Option<String>,
    pub goal_token_budget: Option<u64>,
    pub hooks: Vec<LeanAccessReviewTaskCaseDocumentHooksItem>,
    pub node_did: String,
    pub output_schema_ref: Option<String>,
    pub prompt_template: String,
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAccessReviewTaskCaseDocumentHooksItem {
    pub command: Vec<String>,
    pub hook_id: String,
    pub phase: String,
}

/// The acting actor of an agent decision row (Lean `SelfConfig.Actor`) with
/// the grants native ACP gives it: `catalog` is `nodes manage_agents` on the Node,
/// `configures` the Agents whose document it may update.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentActor {
    pub did: String,
    pub kind: String,
    pub catalog: bool,
    pub configures: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentBaselineCase {
    pub name: String,
    pub actor: String,
    pub steps: Vec<LeanAgentBaselineStep>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentBaselineStep {
    pub action: String,
    pub committed: bool,
    pub installed: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentBlockSigningCase {
    pub name: String,
    pub managed: bool,
    pub valid: bool,
    pub node: String,
    pub actor: String,
    pub signer: Option<String>,
    pub binding_node: Option<String>,
    pub root_held: bool,
    pub expected: String,
    pub config_tool: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentCandidate {
    pub node_did: String,
    pub agents: Vec<LeanCandidateAgent>,
    pub contexts: Vec<LeanCandidateContext>,
    pub inference_profiles: Vec<LeanCandidateProfile>,
    pub inference_backends: Vec<LeanCandidateBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentCatalogEntry {
    pub agent_did: String,
    pub enabled: bool,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentCreateInput {
    pub display_name: String,
    pub system_prompt: String,
    pub inference_profile_id: String,
    pub clone_from: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentDecisionCase {
    #[serde(flatten)]
    pub inputs: LeanAgentDecisionInputs,
    pub accepted: bool,
    /// Lean `SelfConfig.AgentRefusal`, snake case; null when accepted.
    pub refusal: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentDecisionInputs {
    pub name: String,
    pub agents: Vec<LeanAgentCatalogEntry>,
    pub default_id: Option<String>,
    pub published_profiles: Vec<String>,
    pub actor: LeanAgentActor,
    /// Every front end decides this row alike (Lean
    /// `SelfConfig.admission_ignores_front_end`).
    pub front_ends: Vec<String>,
    pub operation: String,
    pub target: String,
    pub make_default: bool,
    pub create_input: Option<LeanAgentCreateInput>,
    pub edit_patch: Option<Vec<LeanSelfConfigPatchEntry>>,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentDeliveryCase {
    pub delivery: Option<String>,
    pub interrupt: Option<bool>,
    pub expected: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentFoldedBindingCase {
    pub caused_requester: u64,
    pub head_requester: u64,
    pub caused_agent: u64,
    pub head_agent: u64,
    pub named_head: u64,
    pub actual_head: u64,
    pub expected: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentInterruptCase {
    pub name: String,
    pub caller_session: String,
    pub target_session: String,
    pub target_origin_cause: Option<String>,
    pub same_node: bool,
    pub task_triggered: bool,
    pub expected_allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentMaterializationCase {
    #[serde(flatten)]
    pub inputs: LeanAgentDecisionInputs,
    pub candidate: LeanAgentCandidate,
    pub session: Option<LeanAgentResolvedSession>,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentMessageOutcomeCase {
    pub folded: bool,
    pub caused: String,
    pub head: String,
    pub expected: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentRequestAdmissionCase {
    pub name: String,
    pub observation_available: bool,
    pub kind: String,
    pub signature_valid: bool,
    pub signed_fields_match: bool,
    pub branch_fields_exact: bool,
    pub pending_deadline_absent: bool,
    pub signer_matches_requester: bool,
    pub requester_matches_target: bool,
    pub signer_matches_target: bool,
    pub signer_matches_issuer: bool,
    pub requester_matches_session_scope: bool,
    pub current_approval: bool,
    pub exact_generation: bool,
    pub authorization_fresh: bool,
    pub runtime_evidence_present: bool,
    pub runtime_source_kind: String,
    pub target_runtime_attestation_valid: bool,
    pub source_binding_current: bool,
    pub trigger_config_document_binding_current: bool,
    pub source_document_binding_current: bool,
    pub target_policy_allows: bool,
    pub native_create_authorized: bool,
    pub hop_within_bound: bool,
    pub expected_admitted: bool,
    pub expected_disposition: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentResolvedSession {
    pub instructions: String,
    pub skill_ids: Vec<String>,
    pub tool_names: Vec<String>,
    pub backend_id: String,
    pub model: String,
    pub effort: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentSteerDispositionCase {
    pub active: bool,
    pub compatible: bool,
    pub expected: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentSteerReachabilityCase {
    pub inputs: Vec<LeanAgentSteerReachabilityInput>,
    pub target: u64,
    pub expected: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentSteerReachabilityInput {
    pub id: u64,
    pub kind: String,
    pub compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentSteerTargetCase {
    pub active_head: u64,
    pub expected: bool,
    pub planned_head: u64,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentSteeringBoundaryCase {
    pub inputs: Vec<LeanAgentSteeringBoundaryInput>,
    pub expected: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentSteeringBoundaryInput {
    pub id: u64,
    pub kind: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanAggregateTokenBudgetCase {
    pub request_doc_id: String,
    pub prior_request_doc_ids: Vec<String>,
    pub prior_call_kinds: Vec<String>,
    pub name: String,
    pub limit: u64,
    pub used: u64,
    pub prior_prompt_tokens: Vec<u64>,
    pub prior_completion_tokens: Vec<u64>,
    pub input_tokens: u64,
    pub configured_max_output_tokens: u64,
    pub reported_input_tokens: u64,
    pub reported_output_tokens: u64,
    pub reported_total_tokens: u64,
    pub usage_present: bool,
    pub terminal_valid: bool,
    pub effective_output_tokens: u64,
    pub can_dispatch: bool,
    pub charged_tokens: u64,
    pub charge_result: String,
    pub next_used: Option<u64>,
    pub post_charge_action: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanApplicationWriteCase {
    pub admitted: bool,
    pub application: bool,
    pub bounded: bool,
    pub credential: bool,
    pub digest_matches: bool,
    pub granted: bool,
    pub limit: u64,
    pub may_apply: bool,
    pub operation: String,
    pub preview: bool,
    pub protected: bool,
    pub targets: u64,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LeanAssistantOrderMode {
    Grouped,
    NativePreserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliaryAuthority {
    pub claimed_request_state: String,
    pub generation: u64,
    pub begin_boundary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanAuxiliaryKind {
    Compaction,
    CompactionFallback,
    Title,
    AccessReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliaryOutputCase {
    pub name: String,
    pub raw: LeanCanonicalSegment,
    pub closing: LeanCanonicalSegment,
    pub stale_closing: LeanCanonicalSegment,
    pub recovery_closing: LeanCanonicalSegment,
    pub authority: LeanAuxiliaryAuthority,
    pub publication_message: LeanCanonicalMessage<LeanPayloadSpec>,
    pub native_publication_closing: LeanCanonicalSegment,
    pub native_publication_message: LeanCanonicalMessage<LeanPayloadSpec>,
    pub observation: LeanCanonicalOutputObservation,
    pub expected: LeanAuxiliaryOutputExpected,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliaryOutputExpected {
    pub append_accepted: bool,
    pub claimed_append_accepted: bool,
    pub begin_accepted: bool,
    pub begun_append_accepted: bool,
    pub close_accepted: bool,
    pub stale_close_replay_accepted: bool,
    pub recovery_accepted: bool,
    pub recovery_with_header_accepted: bool,
    pub publication_accepted: bool,
    pub empty_header_accepted: bool,
    pub native_publication_accepted: bool,
    pub audit: LeanReasoningAuditResult,
    pub public_view: LeanCanonicalOutputView,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliarySessionChildBindingCase {
    pub allowed: bool,
    pub authenticated: String,
    pub binding: LeanAuxiliarySessionChildBindingCaseBinding,
    pub committed: bool,
    pub creator: String,
    pub grant_ok: bool,
    pub initial_parent_granted: bool,
    pub initial_parent_linked: bool,
    pub mark_ok: bool,
    pub name: String,
    pub native_owner: Option<String>,
    pub parent_granted: bool,
    pub parent_linked: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAuxiliarySessionChildBindingCaseBinding {
    pub actor: String,
    #[serde(rename = "auxiliaryAgent")]
    pub auxiliary_agent: String,
    #[serde(rename = "auxiliaryDoc")]
    pub auxiliary_doc: String,
    #[serde(rename = "auxiliaryId")]
    pub auxiliary_id: String,
    #[serde(rename = "auxiliaryNode")]
    pub auxiliary_node: String,
    #[serde(rename = "auxiliaryPresent")]
    pub auxiliary_present: bool,
    #[serde(rename = "auxiliaryPurpose")]
    pub auxiliary_purpose: String,
    #[serde(rename = "auxiliaryRequester")]
    pub auxiliary_requester: String,
    #[serde(rename = "auxiliarySession")]
    pub auxiliary_session: String,
    #[serde(rename = "auxiliaryShapeValid")]
    pub auxiliary_shape_valid: bool,
    #[serde(rename = "auxiliarySignatureValid")]
    pub auxiliary_signature_valid: bool,
    #[serde(rename = "childAgent")]
    pub child_agent: String,
    #[serde(rename = "childNode")]
    pub child_node: String,
    #[serde(rename = "childRequestDoc")]
    pub child_request_doc: String,
    #[serde(rename = "childRequester")]
    pub child_requester: Option<String>,
    #[serde(rename = "childSession")]
    pub child_session: String,
    #[serde(rename = "childSessionDoc")]
    pub child_session_doc: String,
    pub collection: String,
    #[serde(rename = "linkedParentDoc")]
    pub linked_parent_doc: String,
    #[serde(rename = "linkedParentId")]
    pub linked_parent_id: String,
    #[serde(rename = "parentAgent")]
    pub parent_agent: String,
    #[serde(rename = "parentDoc")]
    pub parent_doc: String,
    #[serde(rename = "parentId")]
    pub parent_id: String,
    #[serde(rename = "parentNode")]
    pub parent_node: String,
    #[serde(rename = "parentPresent")]
    pub parent_present: bool,
    #[serde(rename = "parentPurpose")]
    pub parent_purpose: String,
    #[serde(rename = "parentRequester")]
    pub parent_requester: Option<String>,
    #[serde(rename = "parentSession")]
    pub parent_session: String,
    #[serde(rename = "parentSignatureValid")]
    pub parent_signature_valid: bool,
    #[serde(rename = "sessionAgent")]
    pub session_agent: String,
    #[serde(rename = "sessionDoc")]
    pub session_doc: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "sessionNode")]
    pub session_node: String,
    #[serde(rename = "sessionRequester")]
    pub session_requester: Option<String>,
    pub source: String,
    #[serde(rename = "sourceParentId")]
    pub source_parent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendCaptureScopeCase {
    pub persisted: Vec<u64>,
    pub next: u64,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendGenerationMarkCase {
    pub marked: Option<u64>,
    pub observed: Option<u64>,
    pub applies: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendHealthAdmissionCase {
    pub name: String,
    pub enabled: bool,
    pub probe_status: String,
    pub expected_available: bool,
    pub admission_decision: String,
    pub observed_document_only: bool,
    pub external_endpoint_freshness_claimed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendHealthCase {
    pub name: String,
    pub start_state: String,
    pub start_count: usize,
    pub event: String,
    pub threshold_k: usize,
    pub next_state: String,
    pub next_count: usize,
    pub blocks_routing: bool,
    pub readiness_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendHealthWakeCase {
    pub started_ms: i64,
    pub now_ms: i64,
    pub observed_ms: Option<i64>,
    pub fresh: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendParkedFinalizeCase {
    pub request: gents_protocol::request_lifecycle::RequestLifecycleState,
    pub outcome: gents_protocol::request_lifecycle::RequestLifecycleState,
    pub allowed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendProbeConfiguration {
    pub backend_id: u32,
    pub revision: u32,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendProbeScheduleCase {
    pub name: String,
    pub known: Vec<LeanBackendProbeConfiguration>,
    pub current: Vec<LeanBackendProbeConfiguration>,
    pub periodic: bool,
    pub due: Vec<u32>,
    pub observed: Vec<LeanBackendProbeConfiguration>,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendReparkCase {
    pub resuming: bool,
    pub observed_generation: u64,
    pub observed_expiry: i64,
    pub expected_generation: u64,
    pub expected_expiry: i64,
    pub now: i64,
    pub allowed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendResumeAdmissionCase {
    pub waiting: bool,
    pub version_present: bool,
    pub claimed: bool,
    pub same_identity: bool,
    pub same_deadline: bool,
    pub same_backend: bool,
    pub admitted: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendResumedTurnCase {
    pub completed: Vec<u32>,
    pub next: usize,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendWakeCase {
    pub now_ms: i64,
    pub deadline_ms: i64,
    pub attempts: u32,
    pub wake_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanBackgroundProcessIdentityCase {
    pub expected_cancel: bool,
    pub owned_doc: String,
    pub owned_id: String,
    pub requested_doc: String,
    pub requested_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanBackgroundTheoremKindField {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanBackgroundTheoremWitness {
    pub theorem_name: String,
    pub witness_kind: String,
    pub scenario: String,
    pub numeric_bound: usize,
    pub kind_fields: Vec<LeanBackgroundTheoremKindField>,
}

#[derive(Debug, Deserialize)]
pub struct LeanBoundary {
    pub id: String,
    pub domain: String,
    pub subject: String,
    pub statement: String,
    pub accepted_failure_mode: Option<String>,
    pub accepted_follow_up: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanBudgetRehydrationCase {
    pub name: String,
    pub request_doc_id: String,
    pub pinned_limit: Option<u64>,
    pub rows: Vec<LeanDurableUsageRow>,
    pub ledger: Option<LeanRehydratedBudget>,
}

#[derive(Debug, Deserialize)]
pub struct LeanCallbackCase {
    pub agent_did: String,
    pub journal_prefix_legal: bool,
    pub name: String,
    pub invocation_id: String,
    pub owner_node_did: String,
    pub state: String,
    pub journal: Vec<String>,
    pub result_emitted: bool,
    pub legal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanCallbackRecoveryCase {
    pub name: String,
    pub journal: Vec<String>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub post_state: String,
    pub post_journal: Vec<String>,
    pub retry_allowed_after: bool,
    pub deny_post_state: String,
    pub deny_post_journal: Vec<String>,
    pub retry_allowed_after_deny: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackRetryCase {
    pub name: String,
    pub state: String,
    pub journal: Vec<String>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCase {
    pub name: String,
    pub post: LeanCallbackTransitionCasePost,
    pub pre: LeanCallbackTransitionCasePre,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePost {
    pub agent_did: String,
    pub attempts: u64,
    pub input: String,
    pub invocation_id: String,
    pub journal: Vec<LeanCallbackTransitionCasePostJournalItem>,
    pub origin_group_key: LeanCallbackTransitionCasePostOriginGroupKey,
    pub owner_node_did: String,
    pub result_emitted: bool,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePostJournalItem {
    pub index: u64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePostOriginGroupKey {
    pub consumer: LeanCallbackTransitionCasePostOriginGroupKeyConsumer,
    pub consumer_config_key: String,
    pub correlation: String,
    pub node_did: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePostOriginGroupKeyConsumer {
    pub binding_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePre {
    pub agent_did: String,
    pub attempts: u64,
    pub input: String,
    pub invocation_id: String,
    pub journal: Vec<LeanCallbackTransitionCasePreJournalItem>,
    pub origin_group_key: LeanCallbackTransitionCasePreOriginGroupKey,
    pub owner_node_did: String,
    pub result_emitted: bool,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePreJournalItem {
    pub index: u64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePreOriginGroupKey {
    pub consumer: LeanCallbackTransitionCasePreOriginGroupKeyConsumer,
    pub consumer_config_key: String,
    pub correlation: String,
    pub node_did: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCallbackTransitionCasePreOriginGroupKeyConsumer {
    pub binding_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateAgent {
    pub agent_did: String,
    pub creation_nonce: String,
    pub context_id: Option<String>,
    pub inference_profile_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateBackend {
    pub backend_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateContext {
    pub context_id: String,
    pub system_prompt: String,
    pub skill_ids: Vec<String>,
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateProfile {
    pub profile_id: String,
    pub backend_id: String,
    pub model_name: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalClosure {
    Closed { outcome: LeanOutcome, segments: u64, stream_bytes: Vec<u64> },
    Retracted,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCompactionCase {
    pub name: String,
    pub input: LeanCanonicalCompactionInput,
    pub expected: LeanCanonicalCompactionObservation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCompactionInput {
    pub session: u64,
    pub request: u64,
    pub segments: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub source: Vec<u64>,
    pub context_window: u64,
    pub threshold_basis_points: u64,
    pub configured_max_output_tokens: u64,
    pub can_fit: bool,
    pub prefix_length: u64,
    pub checkpoint: u64,
    pub initial_estimate_tokens: u64,
    pub rebuilt_estimate_tokens: u64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCompactionObservation {
    /// None: reconstruction failed before the projection callback; Some(empty):
    /// a successful callback with an empty native list.
    pub initial_native: Option<Vec<LeanReconstructedMessage>>,
    pub rebuilt_checkpoint: Option<u64>,
    pub rebuilt_native: Option<Vec<LeanReconstructedMessage>>,
    pub result: String,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCoordinate {
    pub request: u64,
    pub source: LeanCanonicalSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalDeclaration {
    pub block: u64,
    pub part: u64,
    pub kind: LeanPayloadKind,
    pub tool: Option<LeanToolIdentity>,
    pub media_kind: Option<LeanMediaKind>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalExecutionCase {
    TraceSummary { name: String, completed: bool },
    NativeExecution {
        name: String,
        seed: LeanCanonicalExecutionSeed,
        query_document: u64,
        operations: Vec<LeanCanonicalExecutionOperation>,
        expected_observations: Vec<LeanCanonicalExecutionObservation>,
        native_gap: Option<String>,
    },
    ModelExecution {
        name: String,
        seed: LeanCanonicalExecutionSeed,
        query_document: u64,
        operations: Vec<LeanCanonicalExecutionOperation>,
        expected_observations: Vec<LeanCanonicalExecutionObservation>,
        native_gap: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalExecutionObservation {
    pub accepted: bool,
    pub generation: Option<u64>,
    pub terminal_generation: Option<u64>,
    pub request_state: String,
    #[serde(deserialize_with = "required_nullable")]
    pub terminal_selection: Option<LeanTerminalSelection>,
    pub tool_state: Option<String>,
    pub tool_stuck_since: Option<u64>,
    pub in_flight: bool,
    pub next_sequence: u64,
    pub accepted_sequence: Option<u64>,
    pub physical_tool_request: Option<u64>,
    pub lease_deadline: Option<u64>,
    pub compaction_cursor: Option<u64>,
    pub segments: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalExecutionOperation {
    AcceptForeground {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        admissions: Vec<LeanCanonicalToolAdmission>,
    },
    AcceptBackground {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        admissions: Vec<LeanCanonicalToolAdmission>,
    },
    Dispatch {
        actor: u64,
        now: u64,
        generation: u64,
        call: u64,
        cancellation_allows: bool,
        tool_policy_allows: bool,
    },
    CloseForegroundTool {
        actor: u64,
        now: u64,
        document: u64,
        authority_outcome: String,
        record: LeanCanonicalSegment,
    },
    CompleteForegroundTool {
        actor: u64,
        now: u64,
        document: u64,
        authority_outcome: String,
        record: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    DeliverForegroundResult {
        actor: u64,
        now: u64,
        document: u64,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    TerminalizeCompleted {
        actor: u64,
        now: u64,
        generation: u64,
        outcome: String,
        selection: LeanTerminalSelection,
    },
    RecoverExpiredGeneration {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        duration: u64,
        deadline: u64,
        items: Vec<LeanCanonicalRecoveryItem>,
    },
    RecoverExpiredTerminal {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: String,
        selection: LeanTerminalSelection,
        items: Vec<LeanCanonicalRecoveryItem>,
    },
    ClosePartial {
        actor: u64,
        now: u64,
        generation: u64,
        item: LeanCanonicalRecoveryItem,
    },
    CloseAuxiliary {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
    },
    RetractBeforeRetry {
        actor: u64,
        now: u64,
        generation: u64,
        record: LeanCanonicalSegment,
    },
    RenewLease { actor: u64, now: u64, generation: u64, expected_deadline: u64 },
    AppendOutput { actor: u64, now: u64, generation: u64, record: LeanCanonicalSegment },
    AppendToolOutput {
        actor: u64,
        now: u64,
        document: u64,
        record: LeanCanonicalSegment,
    },
    AppendOutputWhileSiblingWaits {
        actor: u64,
        now: u64,
        generation: u64,
        record: LeanCanonicalSegment,
    },
    AcceptTurn {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        admissions: Vec<LeanCanonicalToolAdmission>,
    },
    BackgroundTool {
        actor: u64,
        now: u64,
        generation: u64,
        document: u64,
        action: String,
    },
    PublishBackgroundReceipt {
        actor: u64,
        now: u64,
        parent_document: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    AdmitSpawnedBackground {
        actor: u64,
        now: u64,
        generation: u64,
        admission: LeanCanonicalSpawnedToolAdmission,
    },
    PublishAuthored {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    AdvanceCompactionCursor { actor: u64, now: u64, cursor: u64 },
    RevokeCorrupt {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: String,
        selection: LeanTerminalSelection,
    },
    DeliverReplicatedSegment { actor: u64, now: u64, record: LeanCanonicalSegment },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalExecutionSeed {
    pub purpose: LeanRequestPurpose,
    #[serde(deserialize_with = "required_nullable")]
    pub title_binding: Option<LeanCanonicalTitleBinding>,
    pub request_id: u64,
    pub session_id: u64,
    pub node_did: u64,
    pub lease: LeanRequestExecutionWorld,
    pub transcript_session_id: u64,
    pub next_sequence: u64,
    pub segments: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub tool_calls: Vec<u64>,
    pub in_flight: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalFlush {
    pub ordinal: u64,
    pub runs: Vec<LeanCanonicalRun>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalOutputObservation {
    pub request: u64,
    pub session: u64,
    pub records: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub denied_headers: Vec<u64>,
    pub denied_segments: Vec<u64>,
    pub dependency_denials: Vec<LeanDependencyDenial>,
    pub owner: LeanOutputOwner,
    pub target: LeanOutputTarget,
    pub request_terminal: bool,
    pub terminal_selection: Option<LeanTerminalSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalOutputProjectionCase {
    pub name: String,
    pub input: LeanCanonicalOutputObservation,
    pub expected: LeanCanonicalOutputView,
    pub rendered_kinds: Vec<LeanRenderedKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalOutputView {
    Absent,
    Live { streams: Vec<LeanCanonicalStream> },
    Settling { streams: Vec<LeanCanonicalStream> },
    Loading,
    Denied,
    Conflicted,
    Invalid,
    Retracted,
    RetainedPartial { streams: Vec<LeanCanonicalStream> },
    Published {
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        native: Box<LeanReconstructedMessage>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalPath {
    pub anchor: String,
    pub components: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalRecoveryItem {
    pub closing: LeanCanonicalSegment,
    pub message: Option<LeanCanonicalMessage<LeanPayloadSpec>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalRun {
    pub stream: u64,
    pub bytes: u64,
    pub declaration: Option<LeanCanonicalDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalSegment {
    pub id: u64,
    pub coordinate: LeanCanonicalCoordinate,
    pub writer: LeanCanonicalWriter,
    pub flush: Option<LeanCanonicalFlush>,
    pub close: Option<LeanCanonicalClosure>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalSource {
    Provider { scope: u64, turn: u64, attempt: u64 },
    Auxiliary { auxiliary_kind: LeanAuxiliaryKind, scope: u64, turn: u64, attempt: u64 },
    Tool { call: u64 },
    Authored { key: u64 },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalSpawnedToolAdmission {
    pub document: u64,
    pub parent_tool_document: u64,
    pub call_id: u64,
    pub request_id: u64,
    pub state: String,
    pub operation: String,
    pub deadline: u64,
    pub started_at: Option<u64>,
    pub current_time: u64,
    pub failure_class: Option<String>,
    pub persistence: String,
    pub await_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalStream {
    pub declaration: LeanCanonicalDeclaration,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalTitleBinding {
    pub purpose: LeanRequestPurpose,
    pub physical_request: u64,
    pub logical_request: u64,
    pub parent_physical: u64,
    pub parent_logical: u64,
    pub node: u64,
    pub session: u64,
    pub authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalToolAdmission {
    pub document: u64,
    pub call_id: u64,
    pub request_id: u64,
    pub state: String,
    pub operation: String,
    pub deadline: u64,
    pub started_at: Option<u64>,
    pub current_time: u64,
    pub failure_class: Option<String>,
    pub persistence: String,
    pub await_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalWriter {
    Request { generation: u64 },
    Tool { call: u64 },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCapObservationCase {
    pub name: String,
    pub visible: bool,
    pub cap: Option<u32>,
    pub hop: u32,
    pub expected_bound: Option<u32>,
    pub expected: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanCaptureOrderCase {
    pub name: String,
    pub left_label: String,
    pub left_turn: i64,
    pub left_attempt: i64,
    pub right_label: String,
    pub right_turn: i64,
    pub right_attempt: i64,
    pub left_before_right: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanCaptureScopeCase {
    pub label: String,
    pub kind: String,
    pub seq: u64,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanCasePairs {
    pub pairs: u64,
    pub sum_baseline: u64,
    pub sum_candidate: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCausalHopCallCase {
    pub name: String,
    pub max_request_hop: Option<u32>,
    pub events: Vec<String>,
    pub expected_hops: Vec<u32>,
    pub expected_admitted: Vec<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCausalHopChainCase {
    pub name: String,
    pub max_request_hop: Option<u32>,
    pub steps: Vec<LeanCausalHopChainStep>,
    pub expected_hops: Vec<u32>,
    pub expected_admitted: Vec<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCausalHopChainStep {
    pub kind: String,
    pub own_predecessor_hop: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCausalHopStepCase {
    pub name: String,
    pub cause: String,
    pub cause_hop: Option<u32>,
    pub own_predecessor_hop: u32,
    pub max_request_hop: Option<u32>,
    pub expected_hop: u32,
    pub expected_admitted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCausedFacts {
    pub terminal: bool,
    pub this_runtime: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanClaimedDid {
    Key(String),
    Derived { node_key: String, creation_nonce: String },
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanClaudeCheckpointResolution {
    pub tag: LeanCanonicalCoordinate,
    pub origin: LeanClaudeReplayOrigin,
    pub issuer_family: String,
    pub issuer_endpoint: String,
    pub wire: LeanReasoningReplayWire,
    pub physical_header: String,
    pub complete: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub captured: Option<Vec<LeanReplayFlatItem>>,
    pub expected_reasoning: Vec<LeanClaudeReasoningWitness>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeContentStep {
    #[serde(deserialize_with = "required_nullable")]
    pub provisional_thinking: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub provisional_signature: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub provisional_redacted: Option<String>,
    pub sealed: Vec<LeanClaudeStreamBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeReasoningPart {
    pub kind: String,
    pub payload: String,
    pub signature: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanClaudeReasoningWitness {
    pub block_index: usize,
    pub parts: Vec<LeanClaudeReplayInputPart>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeReplayBlock {
    pub kind: String,
    pub payload: Option<Vec<u8>>,
    pub signature: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<Vec<u8>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeReplayInputBlock {
    pub kind: String,
    pub payload: Option<Vec<u8>>,
    pub parts: Option<Vec<LeanClaudeReplayInputPart>>,
    pub id: Option<String>,
    pub doc_id: Option<u64>,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<Vec<u8>>,
    pub signature: Option<String>,
    pub additional_params: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeReplayInputPart {
    pub kind: String,
    pub payload: Vec<u8>,
    pub signature: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LeanClaudeReplayOrigin {
    ClaudeSubscription,
    AcceptedProvider,
    RecoveredProvider,
    Foreign,
    Missing,
    Ambiguous,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeStreamBlock {
    pub kind: String,
    pub value: Option<String>,
    pub parts: Option<Vec<LeanClaudeReasoningPart>>,
    pub id: Option<u64>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeStreamEvent {
    pub kind: String,
    pub value: Option<String>,
    pub id: Option<u64>,
    pub name: Option<String>,
    pub input: Option<String>,
    pub index: Option<u64>,
    pub fragment: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanClaudeTaggedReplayRow {
    #[serde(deserialize_with = "required_nullable")]
    pub source: Option<LeanCanonicalCoordinate>,
    #[serde(deserialize_with = "required_nullable")]
    pub physical_header: Option<String>,
    pub block_indices: Vec<usize>,
    pub blocks: Vec<LeanClaudeReplayInputBlock>,
}

#[derive(Debug, Deserialize)]
pub struct LeanClientAgentReadinessCase {
    pub name: String,
    pub observation_present: bool,
    pub observation_kind: String,
    pub process_state: String,
    pub active_generation: u64,
    pub router_generation: u64,
    pub runnable: bool,
    pub unavailable: bool,
    pub startup_demoted: bool,
    pub runtime_unavailable_reason: String,
    pub expected_state: String,
    pub expected_reason: Option<String>,
    pub expected_runtime_admissible: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientChatReadinessCase {
    pub refusal: Option<String>,
    pub route: Option<LeanClientChatReadinessCaseRoute>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientChatReadinessCaseRoute {
    pub managed: bool,
    pub operator_endpoint: bool,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientChatReadinessCausesItem {
    pub cause: String,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCases {
    pub notice_delivery: LeanClientDataPublicationCasesNoticeDelivery,
    pub publication: Vec<LeanClientDataPublicationCasesPublicationItem>,
    pub recovery: Vec<LeanClientDataPublicationCasesRecoveryItem>,
    pub scoped_publication: Option<
        Vec<LeanClientDataPublicationCasesScopedPublicationItem>,
    >,
    pub scoped_replacement: Option<
        Vec<LeanClientDataPublicationCasesScopedReplacementItem>,
    >,
    pub node_publication: Vec<LeanClientDataPublicationCasesNodePublicationItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNodePublicationItem {
    pub accepted: bool,
    pub actions: Vec<LeanClientDataPublicationCasesNodePublicationItemActionsItem>,
    pub expected: Vec<LeanClientDataPublicationCasesNodePublicationItemExpectedItem>,
    pub merge: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNodePublicationItemActionsItem {
    pub node: u64,
    pub op: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNodePublicationItemExpectedItem {
    pub node: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNoticeDelivery {
    pub deliveries: Vec<LeanClientDataPublicationCasesNoticeDeliveryDeliveriesItem>,
    pub initial: LeanClientDataPublicationCasesNoticeDeliveryInitial,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNoticeDeliveryDeliveriesItem {
    pub expected: LeanClientDataPublicationCasesNoticeDeliveryDeliveriesItemExpected,
    pub incoming: LeanClientDataPublicationCasesNoticeDeliveryDeliveriesItemIncoming,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNoticeDeliveryDeliveriesItemExpected {
    pub provenance: u64,
    pub request_event: u64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNoticeDeliveryDeliveriesItemIncoming {
    pub provenance: u64,
    pub request_event: u64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesNoticeDeliveryInitial {
    pub provenance: u64,
    pub request_event: u64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesPublicationItem {
    pub accepted: bool,
    pub actions: Vec<String>,
    pub current: LeanClientDataPublicationCasesPublicationItemCurrent,
    pub initial: LeanClientDataPublicationCasesPublicationItemInitial,
    pub initial_request_ids: Vec<String>,
    pub name: String,
    pub publish_request_ids: Vec<String>,
    pub read: LeanClientDataPublicationCasesPublicationItemRead,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesPublicationItemCurrent {
    pub data_epoch: u64,
    pub provenance_epoch: u64,
    pub request_event_epoch: u64,
    pub transcript_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesPublicationItemInitial {
    pub data_epoch: u64,
    pub provenance_epoch: u64,
    pub request_event_epoch: u64,
    pub transcript_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesPublicationItemRead {
    pub data_epoch: u64,
    pub request_event_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesRecoveryItem {
    pub actions: Vec<LeanClientDataPublicationCasesRecoveryItemActionsItem>,
    pub full: Option<bool>,
    pub generation: u64,
    pub initial: LeanClientDataPublicationCasesRecoveryItemInitial,
    pub name: String,
    pub pending_scopes: Vec<u64>,
    pub retry_scheduled: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesRecoveryItemActionsItem {
    pub op: String,
    pub scope: Option<u64>,
    pub scopes: Option<Vec<u64>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesRecoveryItemInitial {
    pub full: Option<bool>,
    pub generation: u64,
    pub pending_scopes: Vec<serde_json::Value>,
    pub retry_scheduled: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedPublicationItem {
    pub accepted: bool,
    pub actions: Vec<LeanClientDataPublicationCasesScopedPublicationItemActionsItem>,
    pub expected: Vec<LeanClientDataPublicationCasesScopedPublicationItemExpectedItem>,
    pub incoming: Vec<LeanClientDataPublicationCasesScopedPublicationItemIncomingItem>,
    pub initial: Vec<LeanClientDataPublicationCasesScopedPublicationItemInitialItem>,
    pub merge: bool,
    pub name: String,
    pub owners: Option<Vec<u64>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedPublicationItemActionsItem {
    pub op: String,
    pub owner: Option<u64>,
    pub owners: Option<Vec<u64>>,
    pub rows: Option<
        Vec<LeanClientDataPublicationCasesScopedPublicationItemActionsItemRowsItem>,
    >,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedPublicationItemActionsItemRowsItem {
    pub key: String,
    pub owner: u64,
    pub source: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedPublicationItemExpectedItem {
    pub key: String,
    pub owner: u64,
    pub source: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedPublicationItemIncomingItem {
    pub key: String,
    pub owner: u64,
    pub source: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedPublicationItemInitialItem {
    pub key: String,
    pub owner: u64,
    pub source: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItem {
    pub current_configs: Vec<
        LeanClientDataPublicationCasesScopedReplacementItemCurrentConfigsItem,
    >,
    pub current_observations: Vec<
        LeanClientDataPublicationCasesScopedReplacementItemCurrentObservationsItem,
    >,
    pub expected_authoritative_owners: Vec<u64>,
    pub expected_configs: Vec<
        LeanClientDataPublicationCasesScopedReplacementItemExpectedConfigsItem,
    >,
    pub expected_observations: Vec<
        LeanClientDataPublicationCasesScopedReplacementItemExpectedObservationsItem,
    >,
    pub incoming_authoritative_owners: Vec<u64>,
    pub incoming_configs: Vec<
        LeanClientDataPublicationCasesScopedReplacementItemIncomingConfigsItem,
    >,
    pub incoming_observations: Vec<
        LeanClientDataPublicationCasesScopedReplacementItemIncomingObservationsItem,
    >,
    pub incoming_trigger_owners: Vec<u64>,
    pub lookups: Vec<LeanClientDataPublicationCasesScopedReplacementItemLookupsItem>,
    pub name: String,
    pub scope: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItemCurrentConfigsItem {
    pub key: String,
    pub owner: u64,
    pub source: Option<serde_json::Value>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItemCurrentObservationsItem {
    pub key: String,
    pub source: Option<u64>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItemExpectedConfigsItem {
    pub key: String,
    pub owner: u64,
    pub source: Option<u64>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItemExpectedObservationsItem {
    pub key: String,
    pub source: Option<u64>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItemIncomingConfigsItem {
    pub key: String,
    pub owner: u64,
    pub source: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItemIncomingObservationsItem {
    pub key: String,
    pub source: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDataPublicationCasesScopedReplacementItemLookupsItem {
    pub owner: u64,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientDirectSessionReadCase {
    pub expected: String,
    pub session_row: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientLiveDeltaCase {
    pub accepted: bool,
    pub base: Option<u64>,
    pub current: Option<u64>,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientProcessReceiptCase {
    pub agent: String,
    pub author: String,
    pub call: String,
    pub document: String,
    pub expected: bool,
    pub intent_doc: String,
    pub observed_result: String,
    pub signature_valid: bool,
    pub signed_result: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestCacheIdentityCase {
    pub current: Vec<LeanClientRequestCacheIdentityCaseCurrentItem>,
    pub expected_rows: Vec<LeanClientRequestCacheIdentityCaseExpectedRowsItem>,
    pub expected_turn_doc: Option<u64>,
    pub incoming: Vec<LeanClientRequestCacheIdentityCaseIncomingItem>,
    pub name: String,
    pub selected_doc: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestCacheIdentityCaseCurrentItem {
    pub doc: Option<u64>,
    pub folded_into: Option<u64>,
    pub node: Option<u64>,
    pub queued_after: Option<serde_json::Value>,
    pub request: u64,
    pub requester: Option<u64>,
    pub retry_parent: Option<serde_json::Value>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestCacheIdentityCaseExpectedRowsItem {
    pub doc: Option<u64>,
    pub folded_into: Option<u64>,
    pub node: Option<u64>,
    pub queued_after: Option<serde_json::Value>,
    pub request: u64,
    pub requester: Option<u64>,
    pub retry_parent: Option<serde_json::Value>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestCacheIdentityCaseIncomingItem {
    pub doc: Option<u64>,
    pub folded_into: Option<serde_json::Value>,
    pub node: Option<u64>,
    pub queued_after: Option<serde_json::Value>,
    pub request: u64,
    pub requester: Option<u64>,
    pub retry_parent: Option<serde_json::Value>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestPatchPublicationCases {
    pub fresh_read: Vec<LeanClientRequestPatchPublicationCasesFreshReadItem>,
    pub publications: Vec<LeanClientRequestPatchPublicationCasesPublicationsItem>,
    pub reuse: Vec<LeanClientRequestPatchPublicationCasesReuseItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestPatchPublicationCasesFreshReadItem {
    pub expected_action: String,
    pub fence_accepted: bool,
    pub name: String,
    pub patch_available: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestPatchPublicationCasesPublicationsItem {
    pub accepted: bool,
    pub before: String,
    pub expected_after: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRequestPatchPublicationCasesReuseItem {
    pub expected_reuse: bool,
    pub fence_accepted: bool,
    pub memoized: bool,
    pub name: String,
    pub rows_current: bool,
    pub same_signature: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientRoutePairingLegsItem {
    pub combined: Option<bool>,
    pub inbound: String,
    pub outbound: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSettingsCases {
    pub fields: Vec<LeanClientSettingsCasesFieldsItem>,
    pub revisions: Vec<LeanClientSettingsCasesRevisionsItem>,
    #[serde(rename = "savedProjections")]
    pub saved_projections: Vec<LeanClientSettingsCasesSavedProjectionsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSettingsCasesFieldsItem {
    pub collection: String,
    pub editable: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSettingsCasesRevisionsItem {
    pub current: Option<LeanClientSettingsCasesRevisionsItemCurrent>,
    pub expected: Option<LeanClientSettingsCasesRevisionsItemExpected>,
    pub matches: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSettingsCasesRevisionsItemCurrent {
    pub composite: u64,
    pub document: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSettingsCasesRevisionsItemExpected {
    pub composite: u64,
    pub document: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSettingsCasesSavedProjectionsItem {
    pub adopt: bool,
    pub content: bool,
    pub physical: bool,
    pub read: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanClientShellCase {
    pub name: String,
    pub property: String,
    pub input: String,
    pub pre_selection_session: Option<usize>,
    pub post_selection_session: Option<usize>,
    pub pre_workflow_kind: String,
    pub pre_workflow_request: Option<usize>,
    pub post_workflow_kind: String,
    pub post_workflow_request: Option<usize>,
    pub selection_preserved: bool,
    pub workflow_advanced: bool,
    pub transport_noop: bool,
    pub can_submit_before: bool,
    pub can_submit_after: bool,
    pub send_decision: String,
    pub send_blocked_reason: Option<String>,
    pub frontend_expected_workflow_kind: String,
    pub frontend_expected_send_status: String,
    pub frontend_expected_send_blocked_reason: Option<String>,
    pub frontend_expected_active_request_id: Option<usize>,
    pub desktop_selected_session_id: Option<usize>,
    pub desktop_snapshot_present: bool,
    pub desktop_preferred_request_id: Option<usize>,
    pub desktop_observed_request_id: Option<usize>,
    pub desktop_observed_turn_state: Option<String>,
    pub desktop_queued_request_ids: Vec<usize>,
    pub desktop_folded_request_ids: Vec<usize>,
    pub desktop_rows: Vec<LeanSessionTurnRow>,
    pub desktop_expected_latest_request_id: Option<usize>,
    pub desktop_expected_turn_state: Option<String>,
    pub desktop_expect_pending_turn: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSignedControlCase {
    pub action: String,
    pub agent: String,
    pub author: String,
    pub call: String,
    pub document: String,
    pub expected: bool,
    pub node: String,
    pub request: String,
    pub session: String,
    pub signature_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanClientSyncHealthCase {
    pub cause: Option<String>,
    pub configured_peers: u64,
    pub dialed_peers: u64,
    pub enrollment: Vec<String>,
    pub next_action: Option<String>,
    pub runtimes: Vec<Option<String>>,
    pub state: String,
    pub transport: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanCommandEnvCase {
    pub name: String,
    pub env_key: String,
    pub input_present: bool,
    pub input_name: String,
    pub input_value: String,
    pub output_name: String,
    pub expected_value_kind: Option<String>,
    pub expected_output_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCommandIsolationCase {
    #[serde(rename = "effectiveNetwork")]
    pub effective_network: String,
    pub mode: String,
    pub network: String,
    #[serde(rename = "protectsCredentials")]
    pub protects_credentials: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanCommandPolicyCase {
    pub name: String,
    pub category: String,
    pub mode: String,
    pub allowed_argv_prefixes: Vec<Vec<String>>,
    pub forbidden_argv_prefixes: Vec<Vec<String>>,
    pub network_mode: String,
    pub read_only_allowlist: Vec<String>,
    pub command: String,
    pub lookup_command: String,
    pub args: Vec<String>,
    pub decision: String,
    pub denial_reason: Option<String>,
    pub matched_prefix: Option<Vec<String>>,
    pub denied_argv: Option<Vec<String>>,
    pub denied_command: Option<String>,
    pub denied_argument: Option<String>,
    pub denied_subcommand: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanCommandResult {
    Exited { code: i64 },
    LaunchFailed,
    TimedOut,
    Interrupted,
}

#[derive(Debug, Deserialize)]
pub struct LeanCompactionCursorCase {
    pub name: String,
    pub compacted: usize,
    pub expected_cursor: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCompactionInputRow {
    Ordinary { role: String, text: String },
    AssistantToolCalls { role: String, call_ids: Vec<String> },
    ToolResult { role: String, call_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCompactionProjectionJoinCase {
    pub name: String,
    pub source: Vec<usize>,
    pub context_window: usize,
    pub threshold_basis_points: usize,
    pub configured_max_output_tokens: usize,
    pub can_fit: bool,
    pub prefix_length: usize,
    pub checkpoint: usize,
    pub initial_projection_succeeds: bool,
    pub initial_estimate_tokens: Option<usize>,
    pub rebuild_succeeds: bool,
    pub rebuilt_estimate_tokens: Option<usize>,
    pub initial_projection_input: Vec<usize>,
    pub rebuilt_projection_input: Option<LeanRebuiltProjectionInput>,
    pub result: String,
    pub output_tokens: Option<usize>,
    pub committed_reductions: usize,
}

#[derive(Debug, Deserialize)]
pub struct LeanCompactionReducerCase {
    pub input_rows: Vec<LeanCompactionInputRow>,
    pub projected_prefix_ready: bool,
    pub name: String,
    pub group: String,
    pub reducer: String,
    pub legal: bool,
    pub pre_message_count: usize,
    pub post_message_count: usize,
    pub preserves_pairs: bool,
    pub preserves_order: bool,
    pub gate_open: Option<bool>,
    pub publication_ready: bool,
    pub provider_fixpoint: bool,
    pub turn_boundary: bool,
    pub safe_to_reduce: bool,
    pub reducer_is_identity: bool,
    pub reducer_is_idempotent: bool,
    pub split_index: usize,
    pub safe_boundary: usize,
    pub retained_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCompletionRetryCase {
    pub name: String,
    pub domain: String,
    pub action: String,
    pub failure_origin: Option<String>,
    pub classified_failure: Option<String>,
    pub legal: bool,
    pub pre_phase: String,
    pub pre_now: i64,
    pub pre_deadline: Option<i64>,
    pub pre_scheduled_wake: Option<i64>,
    pub expected_phase: Option<String>,
    pub expected_transport_used: Option<usize>,
    pub expected_resample_used: Option<usize>,
    pub expected_attempt: Option<usize>,
    pub expected_account_used: Option<usize>,
    pub expected_repair_used: Option<bool>,
    pub expected_usage_charged: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanComposedInvariantWitness {
    pub theorem_name: String,
    pub witness_kind: String,
    pub scenario: String,
    pub rust_path: String,
    pub trace_step_count: usize,
    pub transition_path: Vec<String>,
    pub pre_request_state: String,
    pub pre_request_admission: String,
    pub tool_pre_state: String,
    pub tool_post_state: String,
    pub request_id: usize,
    pub tool_request_id: usize,
    pub tool_call_id: usize,
    pub request_deadline: usize,
    pub request_current_time: usize,
    pub tool_deadline: usize,
    pub tool_current_time: usize,
    pub deadline_exceeded: bool,
    pub well_formed_source: String,
    pub pre_tool_persisted: bool,
    pub cancel_cause: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanConfigurationImmutableCase {
    pub observed: String,
    pub proposed: String,
    pub allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanConfigurationSignedCreateCollection {
    pub collection: String,
    pub supported: bool,
    pub update_supported: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanConfigurationSignedWriteRetryCase {
    pub dispatched: bool,
    pub confirmed_abort: bool,
    pub replay_authorized: bool,
    pub may_retry: bool,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct LeanContextAnchor {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_input_tokens: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanContractSnapshot {
    pub tool_admission_handoff_cases: Vec<LeanToolAdmissionHandoffCase>,
    pub delegated_publication_cases: Vec<serde_json::Value>,
    pub delegated_finality_cases: Vec<LeanDelegatedFinalityCase>,
    pub generated_by: String,
    pub schema_publication_cases: Vec<LeanSchemaPublicationCase>,
    pub root_admission_cases: Vec<LeanRootAdmissionCase>,
    pub vocabularies: Vec<LeanVocabularyContract>,
    pub state_machines: Vec<LeanStateMachineContract>,
    pub pairing_reconcile_cases: Vec<LeanPairingReconcileCase>,
    pub goal_claimed_publication_cases: Vec<LeanGoalClaimedPublicationCase>,
    pub goal_claimed_identity_cases: Vec<LeanGoalClaimedIdentityCase>,
    pub goal_claimed_target_cases: Vec<LeanGoalClaimedTargetCase>,
    pub goal_backed_runtime_cases: Vec<LeanGoalBackedRuntimeCase>,
    pub goal_wake_coalescing_cases: Vec<serde_json::Value>,
    pub goal_wake_ancestry_cases: Vec<LeanGoalWakeAncestryCase>,
    pub goal_request_scope_cases: Vec<serde_json::Value>,
    pub goal_request_ancestry_cases: Vec<LeanGoalRequestAncestryCase>,
    pub goal_queue_assignment_cases: Vec<LeanGoalQueueAssignmentCase>,
    pub goal_first_request_binding_cases: Vec<LeanGoalFirstRequestBindingCase>,
    pub goal_continuation_assignment_cases: Vec<LeanGoalContinuationAssignmentCase>,
    pub plugin_job_commit_kill_cases: Vec<LeanPluginJobCommitKillCase>,
    pub client_process_claim_cases: Vec<serde_json::Value>,
    pub client_process_receipt_cases: Vec<LeanClientProcessReceiptCase>,
    pub client_control_grant_cases: Vec<serde_json::Value>,
    pub client_signed_control_cases: Vec<LeanClientSignedControlCase>,
    pub process_stop_action_cases: Vec<serde_json::Value>,
    pub background_process_identity_cases: Vec<LeanBackgroundProcessIdentityCase>,
    pub durable_observation_cases: Vec<LeanDurableObservationCase>,
    pub plugin_target_resolution_cases: Vec<LeanPluginTargetResolutionCase>,
    pub goal_request_assignment_cases: Vec<LeanGoalRequestAssignmentCase>,
    pub goal_wake_receipt_cases: Vec<LeanGoalWakeReceiptCase>,
    pub goal_tool_context_cases: Vec<LeanGoalToolContextCase>,
    pub goal_input_binding_cases: Vec<LeanGoalInputBindingCase>,
    pub goal_fresh_session_input_cases: Vec<LeanGoalFreshSessionInputCase>,
    pub session_message_goal_input_cases: Vec<LeanSessionMessageGoalInputCase>,
    pub goal_delivery_input_cases: Vec<serde_json::Value>,
    pub goal_operator_resume_cases: Vec<LeanGoalOperatorResumeCase>,
    pub goal_resume_publication_cases: Vec<LeanGoalResumePublicationCase>,
    pub goal_operator_resume_on_cases: Vec<LeanGoalOperatorResumeOnCase>,
    pub goal_config_reactivation_cases: Vec<LeanGoalConfigReactivationCase>,
    pub goal_reset_resume_cases: Vec<LeanGoalResetResumeCase>,
    pub trigger_delivery_admission_cases: serde_json::Value,
    pub trigger_delivery_queue_cases: serde_json::Value,
    pub trigger_delivery_observed_claim_cases: serde_json::Value,
    pub trigger_delivery_assignment_root_cases: serde_json::Value,
    pub trigger_delivery_outcome_cases: serde_json::Value,
    pub trigger_delivery_outcome_trace_cases: serde_json::Value,
    pub trigger_delivery_retained_outcome_cases: serde_json::Value,
    pub trigger_delivery_cursor_cases: serde_json::Value,
    pub trigger_delivery_callback_cursor_cases: serde_json::Value,
    pub trigger_delivery_callback_visibility_cases: serde_json::Value,
    pub trigger_delivery_identity_cases: serde_json::Value,
    pub trigger_delivery_session_cases: serde_json::Value,
    pub trigger_delivery_session_config_cases: serde_json::Value,
    pub trigger_fire_budget_cases: serde_json::Value,
    pub trigger_fire_budget_config_cases: serde_json::Value,
    pub trigger_fire_budget_default: serde_json::Value,
    pub trigger_fire_budget_status: serde_json::Value,
    pub trigger_delivery_option_cases: serde_json::Value,
    pub trigger_delivery_latest_admission_cases: serde_json::Value,
    pub trigger_delivery_self_session_cases: serde_json::Value,
    pub operator_base_freeze_cases: Vec<serde_json::Value>,
    pub workspace_path_capability_cases: Vec<LeanWorkspacePathCapabilityCase>,
    pub file_inventory_direct_reader_cases: Vec<serde_json::Value>,
    pub file_inventory_fork_cases: Vec<serde_json::Value>,
    pub file_inventory_reader_cases: Vec<serde_json::Value>,
    pub file_inventory_source_projection_cases: Vec<serde_json::Value>,
    pub file_inventory_order_window_cases: Vec<serde_json::Value>,
    pub file_inventory_order_resolution_cases: Vec<serde_json::Value>,
    pub file_inventory_order_comparison_cases: Vec<serde_json::Value>,
    pub file_inventory_order_entry_cases: Vec<serde_json::Value>,
    pub file_review_route_cases: Vec<LeanFileReviewRouteCase>,
    pub file_review_subscription_cases: Vec<LeanFileReviewSubscriptionCase>,
    pub workspace_path_alias_cases: Vec<LeanWorkspacePathAliasCase>,
    pub logical_output_obligation_cases: Vec<LeanLogicalOutputObligationCase>,
    pub count_carrier_cases: serde_json::Value,
    pub schema_argument_repair_cases: Vec<LeanSchemaArgumentRepairCase>,
    pub invalid_tool_progress_cases: Vec<LeanInvalidToolProgressCase>,
    pub repeated_tool_failure_cases: Vec<serde_json::Value>,
    pub tool_timeout_cases: LeanToolTimeoutCases,
    pub plugin_resource_consent_cases: serde_json::Value,
    pub plugin_resource_budget_cases: serde_json::Value,
    pub plugin_resource_publication_cases: serde_json::Value,
    pub plugin_resource_tool_call_cases: serde_json::Value,
    pub plugin_resource_child_admission_cases: serde_json::Value,
    pub plugin_resource_tool_effect_cases: serde_json::Value,
    pub plugin_resource_tool_effect_receipt_cases: serde_json::Value,
    pub plugin_resource_tool_effect_terminal_cases: serde_json::Value,
    pub plugin_resource_tool_effect_accounting_cases: serde_json::Value,
    pub plugin_resource_model_slot_cases: serde_json::Value,
    pub plugin_resource_network_cases: serde_json::Value,
    pub plugin_resource_call_access_cases: serde_json::Value,
    pub plugin_resource_invocation_cases: serde_json::Value,
    pub plugin_resource_receipt_cases: serde_json::Value,
    pub plugin_resource_scratch_cases: serde_json::Value,
    pub plugin_resource_host_import_cases: serde_json::Value,
    pub plugin_data_cases: LeanPluginDataCases,
    pub plugin_calls_route_cases: Vec<LeanPluginCallsRouteCase>,
    pub plugin_job_admission_cases: Vec<LeanPluginJobAdmissionCase>,
    pub plugin_job_adoption_cases: Vec<LeanPluginJobAdoptionCase>,
    pub plugin_job_key_cases: Vec<LeanPluginJobKeyCase>,
    pub plugin_job_control_cases: Vec<LeanPluginJobControlCase>,
    pub plugin_job_authority_cases: Vec<LeanPluginJobAuthorityCase>,
    pub plugin_job_wire_cases: Vec<LeanPluginJobWireCase>,
    pub plugin_job_lineage_cases: Vec<LeanPluginJobLineageCase>,
    pub plugin_job_recovery_cases: Vec<LeanPluginJobRecoveryCase>,
    pub plugin_job_wake_cases: Vec<LeanPluginJobWakeCase>,
    pub mailbox_notification_cases: Vec<LeanMailboxNotificationCase>,
    pub mailbox_reply_cases: Vec<LeanMailboxReplyCase>,
    pub mailbox_question_cases: Vec<LeanMailboxQuestionCase>,
    pub mailbox_handoff_cases: Vec<LeanMailboxHandoffCase>,
    pub mailbox_sweep_cases: Vec<LeanMailboxSweepCase>,
    pub inference_observation_cases: Vec<LeanInferenceObservationCase>,
    pub inference_visibility_cases: Vec<LeanInferenceVisibilityCase>,
    pub inference_route_cases: Vec<LeanInferenceRouteCase>,
    pub identity_publication_cases: Vec<LeanIdentityPublicationCase>,
    pub identity_grant_recipient_cases: Vec<LeanGrantRecipientCase>,
    pub identity_delegation_cases: Vec<LeanDelegationCase>,
    pub identity_agent_baseline_cases: Vec<LeanAgentBaselineCase>,
    pub identity_agent_creator_cases: Vec<serde_json::Value>,
    pub identity_agent_block_signing_cases: Vec<LeanAgentBlockSigningCase>,
    pub identity_private_journal_policy: LeanPrivateJournalPolicy,
    pub mailbox_runtime_notice_case: serde_json::Value,
    pub mailbox_claimed_recipient_cases: Vec<LeanMailboxClaimedRecipientCase>,
    pub workspace_agent_selection_cases: Vec<serde_json::Value>,
    pub workspace_liveness_cases: Vec<LeanWorkspaceLivenessCase>,
    pub workspace_child_cases: Vec<LeanWorkspaceChildCase>,
    pub workspace_quickstart_selection_cases: Vec<LeanWorkspaceQuickstartSelectionCase>,
    pub pack_placement_reader_cases: Vec<LeanPackPlacementReaderCase>,
    pub native_route_cases: Vec<LeanNativeRouteCase>,
    pub workspace_datastore_write_cases: Vec<LeanWorkspaceDatastoreWriteCase>,
    pub workspace_root_access_cases: Vec<LeanWorkspaceRootAccessCase>,
    pub workspace_child_revocation_cases: Vec<LeanWorkspaceChildRevocationCase>,
    pub workspace_admission_cases: Vec<LeanWorkspaceAdmissionCase>,
    pub workspace_cleanup_cases: Vec<LeanWorkspaceCleanupCase>,
    pub workspace_publication_cases: LeanWorkspacePublicationCases,
    pub workspace_quickstart_cases: Vec<LeanWorkspaceQuickstartCase>,
    pub mailbox_native_policy: serde_json::Value,
    pub session_input_edit_native_discovery_cases: Vec<
        LeanSessionInputEditNativeDiscoveryCase,
    >,
    pub session_input_edit_native_claim_cases: Vec<serde_json::Value>,
    pub client_optimistic_send_cases: serde_json::Value,
    pub identity_client_agent_recovery_cases: Vec<LeanIdentityClientAgentRecoveryCase>,
    pub client_publication_protected_collection: String,
    pub watcher_actor_cases: Vec<LeanWatcherActorCase>,
    pub watcher_scoped_scan_cases: Vec<serde_json::Value>,
    pub identity_bootstrap_cases: Vec<LeanIdentityBootstrapCase>,
    pub identity_bootstrap_reservation_cases: Vec<LeanIdentityBootstrapReservationCase>,
    pub identity_runtime_nac_cases: Vec<LeanIdentityRuntimeNacCase>,
    pub identity_parent_publication_cases: Vec<LeanIdentityParentPublicationCase>,
    pub identity_callback_parent_actor_cases: Vec<serde_json::Value>,
    pub identity_eval_record_cases: Vec<LeanIdentityEvalRecordCase>,
    pub identity_optimization_promotion_cases: Vec<serde_json::Value>,
    pub identity_readiness_preflight_cases: Vec<serde_json::Value>,
    pub identity_secret_publication_cases: Vec<serde_json::Value>,
    pub identity_deferred_operation_cases: Vec<serde_json::Value>,
    pub identity_pairing_partition_cases: Vec<LeanIdentityPairingPartitionCase>,
    pub identity_desired_publisher_cases: Vec<serde_json::Value>,
    pub identity_pairing_reader_cases: Vec<LeanIdentityPairingReaderCase>,
    pub identity_pairing_reader_policy_cases: Vec<serde_json::Value>,
    pub identity_local_registry_writer_cases: Vec<LeanIdentityLocalRegistryWriterCase>,
    pub identity_discovery_policy_cases: Vec<LeanIdentityDiscoveryPolicyCase>,
    pub identity_client_transport_cases: Vec<LeanIdentityClientTransportCase>,
    pub identity_replica_grant_cases: Vec<LeanIdentityReplicaGrantCase>,
    pub identity_enrollment_policy_cases: Vec<LeanIdentityEnrollmentPolicyCase>,
    pub identity_readiness_recovery_cases: Vec<serde_json::Value>,
    pub identity_readiness_observation_cases: Vec<serde_json::Value>,
    pub identity_readiness_settled_cases: Vec<LeanIdentityReadinessSettledCase>,
    pub identity_startup_readiness_cases: Vec<LeanIdentityStartupReadinessCase>,
    pub goal_link_cases: Vec<LeanGoalLinkCase>,
    pub remote_agent_delivery_cases: Vec<LeanRemoteAgentDeliveryCase>,
    pub remote_agent_delivery_recovery_cases: Vec<serde_json::Value>,
    pub remote_agent_delivery_response_cases: Vec<serde_json::Value>,
    pub remote_agent_delivery_claim_cases: Vec<LeanRemoteAgentDeliveryClaimCase>,
    pub cross_actor_authorization_cases: Vec<LeanCrossActorAuthorizationCase>,
    pub client_telemetry_cases: Vec<serde_json::Value>,
    pub client_settings_cases: LeanClientSettingsCases,
    pub client_data_publication_cases: LeanClientDataPublicationCases,
    pub client_request_patch_publication_cases: LeanClientRequestPatchPublicationCases,
    pub client_direct_session_read_cases: Vec<LeanClientDirectSessionReadCase>,
    pub storage_failure_cases: Vec<serde_json::Value>,
    pub client_request_closure_cases: Vec<serde_json::Value>,
    pub client_request_cache_identity_cases: Vec<LeanClientRequestCacheIdentityCase>,
    pub client_session_queue_order_cases: Vec<serde_json::Value>,
    pub request_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub process_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub provider_eof_cases: Vec<LeanProviderEofCase>,
    pub request_execution_lease_cases: Vec<LeanRequestExecutionLeaseCase>,
    pub request_execution_lease_observed_cases: Vec<LeanRequestExecutionObservedCase>,
    pub request_execution_lease_trace_cases: Vec<LeanRequestExecutionLeaseTraceCase>,
    pub trigger_dispatch_case_count: usize,
    pub trigger_dispatch_cases: Vec<LeanTriggerDispatchCase>,
    pub event_group_case_count: usize,
    pub event_group_cases: Vec<LeanEventGroupCase>,
    pub event_group_clock_case_count: usize,
    pub event_group_clock_cases: Vec<serde_json::Value>,
    pub event_group_capture_case_count: usize,
    pub event_group_capture_cases: Vec<serde_json::Value>,
    pub callback_transition_case_count: usize,
    pub callback_transition_cases: Vec<LeanCallbackTransitionCase>,
    pub session_document_starter_verification_cases: Vec<serde_json::Value>,
    pub session_document_archive_cases: Vec<serde_json::Value>,
    pub session_document_reference_encoding: String,
    pub session_document_selection_cases: Vec<serde_json::Value>,
    pub session_document_projection_cases: Vec<serde_json::Value>,
    pub session_document_retry_cases: Vec<serde_json::Value>,
    pub session_document_fork_cases: Vec<serde_json::Value>,
    pub session_document_edit_anchor_cases: Vec<serde_json::Value>,
    pub session_document_edit_fork_binding_cases: Vec<serde_json::Value>,
    pub plugin_job_opening_cases: Vec<LeanPluginJobOpeningCase>,
    pub session_reply_preview_cases: Vec<LeanSessionReplyPreviewCase>,
    pub request_input_cases: Vec<serde_json::Value>,
    pub background_wake_row_cases: Vec<serde_json::Value>,
    pub discovery_scope_cases: Vec<serde_json::Value>,
    pub discovery_guidance_cases: Vec<LeanDiscoveryGuidanceCase>,
    pub budget_rehydration_cases: Vec<LeanBudgetRehydrationCase>,
    pub goal_claimed_readiness_cases: Vec<LeanGoalClaimedReadinessCase>,
    pub goal_create_cases: Vec<LeanGoalCreateCase>,
    pub goal_client_delivery_cases: Vec<serde_json::Value>,
    pub goal_client_control_cases: Vec<LeanGoalClientControlCase>,
    pub goal_client_start_cases: Vec<serde_json::Value>,
    pub goal_client_receipt_cases: Vec<LeanGoalClientReceiptCase>,
    pub goal_observation_cases: Vec<serde_json::Value>,
    pub goal_session_observation_cases: Vec<serde_json::Value>,
    pub goal_session_fold_cases: Vec<serde_json::Value>,
    pub workspace_command_grant_answers: Vec<serde_json::Value>,
    pub workspace_command_grant_lookups: Vec<serde_json::Value>,
    pub workspace_command_grant_revocations: Vec<serde_json::Value>,
    pub workspace_permission_write_cases: Vec<LeanWorkspacePermissionWriteCase>,
    pub workspace_permission_cases: Vec<serde_json::Value>,
    pub operator_path_cases: Vec<serde_json::Value>,
    pub access_review_task_cases: Vec<LeanAccessReviewTaskCase>,
    pub plugin_approval_cases: Vec<LeanPluginApprovalCase>,
    pub goal_objective_whitespace_codepoints: Vec<u32>,
    pub task_goal_publication_cases: Vec<LeanTaskGoalPublicationCase>,
    pub file_inventory_closure_cases: Vec<LeanSessionClosureCase>,
    pub enrollment_cases: Vec<LeanEnrollmentCase>,
    pub enrollment_durable_projection_cases: Vec<LeanEnrollmentDurableProjectionCase>,
    pub enrollment_pending_request_cases: Vec<LeanEnrollmentPendingRequestCase>,
    pub enrollment_absent_peer_cases: Vec<LeanEnrollmentAbsentPeerCase>,
    pub client_chat_readiness_cases: Vec<LeanClientChatReadinessCase>,
    pub client_route_pairing_legs: Vec<LeanClientRoutePairingLegsItem>,
    pub client_chat_readiness_causes: Vec<LeanClientChatReadinessCausesItem>,
    pub client_sync_health_cases: Vec<LeanClientSyncHealthCase>,
    pub enrollment_route_readiness_cases: Vec<LeanEnrollmentRouteReadinessCase>,
    pub enrollment_startup_cases: Vec<LeanEnrollmentStartupCase>,
    pub enrollment_encoding_cases: Vec<LeanEnrollmentEncodingCase>,
    pub enrollment_digest_cases: Vec<LeanEnrollmentDigestCase>,
    pub enrollment_auto_renewal_cases: Vec<LeanEnrollmentAutoRenewalCase>,
    pub enrollment_renewal_intent_cases: Vec<LeanEnrollmentRenewalIntentCase>,
    pub agent_request_admission_cases: Vec<LeanAgentRequestAdmissionCase>,
    pub causal_hop_default_max_request_hop: Option<u32>,
    pub causal_hop_cap_observation_cases: Vec<LeanCapObservationCase>,
    pub causal_hop_step_cases: Vec<LeanCausalHopStepCase>,
    pub causal_hop_chain_cases: Vec<LeanCausalHopChainCase>,
    pub causal_hop_call_cases: Vec<LeanCausalHopCallCase>,
    pub causal_hop_interrupt_cases: Vec<LeanAgentInterruptCase>,
    pub causal_hop_creation_cases: Vec<LeanSessionCreationCase>,
    pub causal_hop_creation_signer_cases: Vec<LeanSessionCreationSignerCase>,
    pub causal_hop_job_creation_cases: Vec<LeanJobCreationCase>,
    pub causal_hop_deferred_creation_cases: Vec<LeanDeferredCreationCase>,
    pub causal_hop_write_cases: Vec<LeanSessionMessageWriteCase>,
    pub causal_hop_session_target_cases: Vec<LeanSessionTargetCase>,
    pub causal_hop_session_list_cases: Vec<LeanSessionListCase>,
    pub scope_collection_rules: std::collections::BTreeMap<
        String,
        Vec<LeanScopeCollectionRule>,
    >,
    pub scope_node_route_cases: Vec<LeanScopeNodeRouteCase>,
    pub scope_template_cases: Vec<LeanScopeTemplateCase>,
    pub title_request_admission_cases: Vec<LeanTitleRequestAdmissionCase>,
    pub title_request_purpose_wire_cases: Vec<LeanTitleRequestPurposeWireCase>,
    pub title_text_cases: LeanTitleTextCases,
    pub title_usage_cases: Vec<LeanTitleUsageCase>,
    pub provider_usage_cases: Vec<serde_json::Value>,
    pub title_admission_join_cases: Vec<LeanTitleAdmissionJoinCase>,
    pub frontend_client_shell_case_count: usize,
    pub frontend_client_shell_cases: Vec<LeanClientShellCase>,
    pub desktop_client_shell_case_count: usize,
    pub desktop_client_shell_cases: Vec<LeanClientShellCase>,
    pub client_session_turn_cases: Vec<LeanSessionTurnCase>,
    pub request_lifecycle_operator_ui_cases: Vec<LeanClientShellCase>,
    pub client_live_delta_cases: Vec<LeanClientLiveDeltaCase>,
    pub runtime_session_selection_cases: Vec<LeanRuntimeSessionSelectionCase>,
    pub runtime_configuration_admission_cases: Vec<
        LeanRuntimeConfigurationAdmissionCase,
    >,
    pub runtime_reconcile_cases: Vec<LeanRuntimeReconcileCase>,
    pub client_agent_readiness_cases: Vec<LeanClientAgentReadinessCase>,
    pub startup_readiness_cases: Vec<LeanStartupReadinessCase>,
    pub snapshot_demotion_cases: Vec<LeanSnapshotDemotionCase>,
    pub plugin_reference_cases: Vec<serde_json::Value>,
    pub plugin_pin_cases: Vec<serde_json::Value>,
    pub pack_wire_lock_cases: Vec<serde_json::Value>,
    pub pack_wire_target_cases: Vec<serde_json::Value>,
    pub pack_finality_cases: Vec<serde_json::Value>,
    pub hosted_plugin_cases: Vec<serde_json::Value>,
    pub readiness_publication_cases: Vec<LeanReadinessPublicationCase>,
    pub eval_outcome_cases: Vec<LeanEvalOutcomeCase>,
    pub eval_document_capture_cases: Vec<LeanEvalDocumentCaptureCase>,
    pub optimization_cases: LeanOptimizationCases,
    pub tool_policy_cases: Vec<LeanToolPolicyCase>,
    pub application_write_cases: Vec<LeanApplicationWriteCase>,
    pub field_read_cases: Vec<LeanFieldReadCase>,
    pub write_input_cases: Vec<serde_json::Value>,
    pub invocation_correlation_cases: Vec<LeanInvocationCorrelationCase>,
    pub goal_capability_resolution_cases: Vec<LeanGoalCapabilityResolutionCase>,
    pub lsp_action_cases: Vec<LeanLspActionCase>,
    pub self_config_field_tables: Vec<LeanSelfConfigFieldTable>,
    pub self_config_resource_read_exposure_cases: Vec<
        LeanSelfConfigResourceReadExposureCase,
    >,
    pub self_config_scope_cases: LeanSelfConfigScopeCases,
    pub policy_resource_exposure_cases: Vec<LeanPolicyResourceExposureCase>,
    pub policy_grant_bound_cases: Vec<LeanPolicyGrantBoundCase>,
    pub policy_lockout_cases: LeanPolicyLockoutCases,
    pub self_config_cases: Vec<LeanSelfConfigCase>,
    pub agent_decision_cases: Vec<LeanAgentDecisionCase>,
    pub agent_materialization_cases: Vec<LeanAgentMaterializationCase>,
    pub sibling_tools_cases: Vec<LeanSiblingToolsCase>,
    pub self_config_selection_cases: LeanSelfConfigSelectionCases,
    pub session_recovery_cases: Vec<LeanSessionRecoveryCase>,
    pub inference_slot_accounting_cases: Vec<LeanInferenceSlotAccountingCase>,
    pub fleet_slot_accounting_cases: Vec<LeanFleetSlotAccountingCase>,
    pub persistence_failure_policy_cases: Vec<LeanPersistenceFailurePolicyCase>,
    pub storage_observation_runtime_cases: Vec<LeanStorageObservationRuntimeCase>,
    pub backend_health_admission_cases: Vec<LeanBackendHealthAdmissionCase>,
    pub native_filesystem_boundary_cases: Vec<LeanNativeFilesystemBoundaryCase>,
    pub managed_exec_tool_boundary_cases: Vec<LeanManagedExecToolBoundaryCase>,
    pub pairing_reconcile_shutdown_boundary_cases: Vec<
        LeanPairingReconcileShutdownBoundaryCase,
    >,
    pub pairing_reconcile_sweep_retry_boundary_cases: Vec<
        LeanPairingReconcileSweepRetryBoundaryCase,
    >,
    pub pairing_reconcile_sweep_scheduling_cases: Vec<
        LeanPairingReconcileSweepSchedulingCase,
    >,
    pub managed_exec_liveness_cases: Vec<LeanManagedExecLivenessCase>,
    pub process_stop_cases: Vec<LeanProcessStopCase>,
    pub tool_preflight_cases: Vec<LeanToolPreflightCase>,
    pub tool_retry_cases: Vec<LeanToolRetryCase>,
    pub completion_retry_cases: Vec<LeanCompletionRetryCase>,
    pub backend_wake_cases: Vec<LeanBackendWakeCase>,
    pub backend_capture_scope_cases: Vec<LeanBackendCaptureScopeCase>,
    pub backend_health_wake_cases: Vec<LeanBackendHealthWakeCase>,
    pub backend_generation_mark_cases: Vec<LeanBackendGenerationMarkCase>,
    pub backend_resume_admission_cases: Vec<LeanBackendResumeAdmissionCase>,
    pub backend_repark_cases: Vec<LeanBackendReparkCase>,
    pub backend_parked_finalize_cases: Vec<LeanBackendParkedFinalizeCase>,
    pub backend_resumed_turn_cases: Vec<LeanBackendResumedTurnCase>,
    pub mcp_health_cases: Vec<LeanMcpHealthCase>,
    pub backend_health_cases: Vec<LeanBackendHealthCase>,
    pub backend_probe_schedule_cases: Vec<LeanBackendProbeScheduleCase>,
    pub boundaries: Vec<LeanBoundary>,
    pub deviations: Vec<LeanDeviation>,
    pub command_policy_cases: Vec<LeanCommandPolicyCase>,
    pub command_isolation_cases: Vec<LeanCommandIsolationCase>,
    pub artifact_mode_meet_cases: Vec<serde_json::Value>,
    pub artifact_admission_cases: Vec<serde_json::Value>,
    pub artifact_spawn_cases: Vec<serde_json::Value>,
    pub command_env_cases: Vec<LeanCommandEnvCase>,
    pub live_overlay_cases: Vec<LeanLiveOverlayCase>,
    pub request_progress_cases: Vec<LeanRequestProgressCase>,
    pub pending_user_turn_cases: Vec<LeanPendingUserTurnCase>,
    pub queued_steering_trace_cases: Vec<LeanQueuedSteeringTraceCase>,
    pub queued_steering_guard_cases: Vec<LeanQueuedSteeringGuardCase>,
    pub agent_delivery_cases: Vec<LeanAgentDeliveryCase>,
    pub interrupt_delivery_cases: Vec<LeanInterruptDeliveryCase>,
    pub agent_steer_disposition_cases: Vec<LeanAgentSteerDispositionCase>,
    pub agent_steer_target_cases: Vec<LeanAgentSteerTargetCase>,
    pub agent_steering_boundary_cases: Vec<LeanAgentSteeringBoundaryCase>,
    pub agent_steer_reachability_cases: Vec<LeanAgentSteerReachabilityCase>,
    pub agent_message_outcome_cases: Vec<LeanAgentMessageOutcomeCase>,
    pub agent_folded_binding_cases: Vec<LeanAgentFoldedBindingCase>,
    pub queue_deadline_conformance_cases: Vec<LeanQueueDeadlineConformanceCase>,
    pub recovery_sweep_cases: Vec<LeanRecoverySweepCase>,
    pub session_message_kill_cases: Vec<LeanKillCase>,
    pub session_message_kill_observation_cases: Vec<LeanKillObservationCase>,
    pub restart_disposition_cases: Vec<LeanRestartDispositionCase>,
    pub r4c_background_work_cases: Vec<LeanR4cBackgroundWorkCase>,
    pub tool_output_paging_cases: Vec<LeanToolOutputPagingCase>,
    pub interrupt_disposition_cases: Vec<LeanInterruptDispositionCase>,
    pub r6_backgrounding_cases: Vec<LeanR6BackgroundingCase>,
    pub composed_invariant_witnesses: Vec<LeanComposedInvariantWitness>,
    pub r6_background_theorem_witnesses: Vec<LeanBackgroundTheoremWitness>,
    pub transcript_conformance_cases: Vec<LeanTranscriptCase>,
    pub canonical_output_projection_cases: Vec<LeanCanonicalOutputProjectionCase>,
    pub reasoning_audit_cases: Vec<LeanReasoningAuditCase>,
    pub reasoning_signature_cases: Vec<LeanReasoningSignatureCase>,
    pub reasoning_delta_cases: Vec<LeanReasoningDeltaCase>,
    pub auxiliary_output_cases: Vec<LeanAuxiliaryOutputCase>,
    pub canonical_execution_gate_cases: Vec<LeanCanonicalExecutionCase>,
    pub canonical_dispatch_observation_cases: Vec<LeanDispatchObservationCase>,
    pub canonical_spawned_target_rejection_cases: Vec<LeanSpawnedTargetRejectionCase>,
    pub interrupt_queue_cases: Vec<LeanInterruptQueueCase>,
    pub session_control_cases: Vec<serde_json::Value>,
    pub fold_queue_cases: Vec<LeanFoldQueueCase>,
    pub fold_turn_input_cases: Vec<LeanFoldTurnInputCase>,
    pub handover_fold_cases: Vec<LeanHandoverFoldCase>,
    pub fold_publication_cases: Vec<LeanFoldPublicationCase>,
    pub steering_publication_cases: Vec<LeanFoldPublicationCase>,
    pub queue_management_cases: Vec<LeanQueueManagementCase>,
    pub session_input_edit_cases: Vec<LeanSessionInputEditCase>,
    pub session_input_edit_client_observation_cases: Vec<
        LeanSessionInputEditClientObservationCase,
    >,
    pub retry_selection_cases: Vec<LeanRetrySelectionCase>,
    pub canonical_worker_capacity_cases: Vec<LeanWorkerCapacityCase>,
    pub canonical_payload_presentation_cases: Vec<LeanPayloadPresentationCase>,
    pub terminal_diagnostic_presentation_cases: Vec<
        LeanTerminalDiagnosticPresentationCase,
    >,
    pub terminal_diagnostic_replay_cases: Vec<LeanTerminalDiagnosticReplayCase>,
    pub compaction_reducer_cases: Vec<LeanCompactionReducerCase>,
    pub compaction_cursor_cases: Vec<LeanCompactionCursorCase>,
    pub prompt_assembly_sanitize_cases: Vec<LeanPromptAssemblySanitizeCase>,
    pub prompt_assembly_assistant_order_cases: Vec<LeanPromptAssemblyAssistantOrderCase>,
    pub provider_tool_identity_cases: Vec<serde_json::Value>,
    pub prompt_assembly_mode_sanitize_cases: Vec<LeanPromptAssemblyModeSanitizeCase>,
    pub routing_affinity_cases: Vec<LeanRoutingAffinityCase>,
    pub retry_frontier_cases: Vec<LeanRetryFrontierCase>,
    pub retry_entry_cases: Vec<LeanRetryEntryCase>,
    pub current_input_cases: Vec<LeanCurrentInputCase>,
    pub wake_input_cases: Vec<LeanWakeInputCase>,
    pub prompt_assembly_layer_cases: Vec<LeanPromptAssemblyLayerCase>,
    pub prompt_assembly_repair_cases: Vec<LeanPromptAssemblyRepairCase>,
    pub prompt_assembly_budget_cases: Vec<LeanPromptAssemblyBudgetCase>,
    pub prompt_assembly_turn_budget_cases: Vec<LeanPromptAssemblyTurnBudgetCase>,
    pub prompt_assembly_retention_cases: Vec<LeanPromptAssemblyRetentionCase>,
    pub prompt_assembly_compaction_trigger_cases: Vec<
        LeanPromptAssemblyCompactionTriggerCase,
    >,
    pub prompt_assembly_claude_map_cases: Vec<LeanPromptAssemblyClaudeMapCase>,
    pub prompt_assembly_claude_body_cases: Vec<LeanPromptAssemblyClaudeBodyCase>,
    pub prompt_assembly_claude_stream_cases: Vec<LeanPromptAssemblyClaudeStreamCase>,
    pub prompt_assembly_claude_thinking_stream_cases: Vec<
        LeanPromptAssemblyClaudeThinkingStreamCase,
    >,
    pub prompt_assembly_service_tier_cases: Vec<LeanPromptAssemblyServiceTierCase>,
    pub prompt_assembly_responses_storage_cases: Vec<
        LeanPromptAssemblyResponsesStorageCase,
    >,
    pub prompt_assembly_responses_effort_cases: Vec<
        LeanPromptAssemblyResponsesEffortCase,
    >,
    pub prompt_assembly_claude_replay_cases: Vec<LeanPromptAssemblyClaudeReplayCase>,
    pub prompt_assembly_claude_checkpoint_cases: Vec<
        LeanPromptAssemblyClaudeCheckpointCase,
    >,
    pub prompt_assembly_reasoning_suffix_cases: Vec<
        LeanPromptAssemblyReasoningSuffixCase,
    >,
    pub prompt_assembly_replay_shape_cases: Vec<LeanPromptAssemblyReplayShapeCase>,
    pub prompt_assembly_replay_prefix_cases: Vec<LeanPromptAssemblyReplayPrefixCase>,
    pub protected_replay_compaction_cases: Vec<LeanProtectedReplayCompactionCase>,
    pub rendered_capture_cases: Vec<LeanRenderedCaptureCase>,
    pub durable_reduction_cases: Vec<LeanDurableReductionCase>,
    pub compaction_projection_join_cases: Vec<LeanCompactionProjectionJoinCase>,
    pub compaction_canonical_projection_cases: Vec<LeanCanonicalCompactionCase>,
    pub repaired_projection_admission_cases: Vec<LeanRepairedProjectionCase>,
    pub rendered_capture_key_cases: Vec<LeanRenderedCaptureKeyCase>,
    pub auxiliary_session_child_binding_cases: Vec<LeanAuxiliarySessionChildBindingCase>,
    pub client_request_publication_cases: Vec<serde_json::Value>,
    pub capture_scope_cases: Vec<LeanCaptureScopeCase>,
    pub capture_order_cases: Vec<LeanCaptureOrderCase>,
    pub aggregate_token_budget_cases: Vec<LeanAggregateTokenBudgetCase>,
    pub task_hook_admission_cases: Vec<LeanTaskHookAdmissionCase>,
    pub native_completion_cases: Vec<LeanNativeCompletionCase>,
    pub native_claim_lifetime_cases: Vec<LeanNativeClaimLifetimeCase>,
    pub families: BTreeMap<String, LeanFamily>,
    pub family_vocabularies: BTreeMap<String, serde_json::Value>,
    pub task_hook_run_cases: Vec<LeanTaskHookRunCase>,
    pub task_hook_recovery_cases: Vec<LeanTaskHookRecoveryCase>,
    pub lifecycle_hook_effect_cases: Vec<LeanLifecycleHookEffectCase>,
    pub lifecycle_hook_stage_cases: Vec<LeanLifecycleHookStageCase>,
    pub lifecycle_hook_must_commit_cases: Vec<LeanLifecycleHookMustCommitCase>,
    pub lifecycle_hook_rerun_cases: Vec<LeanLifecycleHookRerunCase>,
    pub lifecycle_hook_goal_resume_cases: Vec<LeanLifecycleHookGoalResumeCase>,
    pub lifecycle_hook_combine_cases: Vec<LeanLifecycleHookCombineCase>,
    pub lifecycle_hook_gate_cases: Vec<LeanLifecycleHookGateCase>,
    pub lifecycle_hook_queue_scope_cases: Vec<LeanLifecycleHookQueueScopeCase>,
    pub lifecycle_hook_delivery_cases: Vec<LeanLifecycleHookDeliveryCase>,
    pub lifecycle_hook_owed_delivery_cases: LeanLifecycleHookOwedDeliveryCases,
    pub lifecycle_hook_admit_cases: Vec<LeanLifecycleHookAdmitCase>,
    pub lifecycle_hook_grant_cases: Vec<LeanLifecycleHookGrantCase>,
    pub lifecycle_hook_supersede_cases: Vec<LeanLifecycleHookSupersedeCase>,
    pub lifecycle_hook_goal_halt_cases: Vec<LeanLifecycleHookGoalHaltCase>,
    pub lifecycle_hook_history_cases: Vec<LeanLifecycleHookHistoryCase>,
    pub follow_up_hooks: Vec<String>,
    pub coverage_ledger: Vec<LeanCoverageEntry>,
    pub feature_surface_requirements: Vec<LeanFeatureSurfaceRequirement>,
    pub feature_matrix: LeanFeatureMatrix,
    pub identity_permission_cases: Vec<LeanIdentityPermissionCase>,
    pub identity_household_cases: Vec<LeanIdentityHouseholdCase>,
    pub identity_task_session_publication_cases: Vec<serde_json::Value>,
    pub identity_client_session_reservation_cases: Vec<serde_json::Value>,
    pub identity_atomic_client_request_cases: Vec<serde_json::Value>,
    pub identity_derivation_cases: Vec<LeanIdentityDerivationCase>,
    pub identity_contracts: Vec<LeanIdentityContract>,
    pub pack_eval_subject_cases: Vec<LeanPackEvalSubjectCase>,
    pub workspace_cases: Vec<LeanWorkspaceCase>,
    pub workspace_binding_cases: Vec<LeanWorkspaceBindingCase>,
    pub callback_cases: Vec<LeanCallbackCase>,
    pub callback_retry_cases: Vec<LeanCallbackRetryCase>,
    pub callback_recovery_cases: Vec<LeanCallbackRecoveryCase>,
    pub event_delivery_transition_case_count: usize,
    pub event_delivery_transition_cases: Vec<LeanEventDeliveryTransitionCase>,
    pub event_delivery_source_instances: Vec<LeanEventDeliverySourceInstance>,
    pub event_delivery_convergence_traces: Vec<LeanEventDeliveryConvergenceTrace>,
    pub pack_operator_wire_cases: Vec<LeanPackOperatorWireCase>,
    pub recovery_sweep_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanCostRow {
    pub name: String,
    pub baseline_tokens: u64,
    pub baseline_trials: u64,
    pub candidate_tokens: u64,
    pub candidate_trials: u64,
    pub cost_ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanCoverageEntry {
    pub category: String,
    pub domain: String,
    pub consumed: bool,
    pub accepted_boundary: String,
    pub accepted_follow_up: String,
    pub feature: String,
    pub surfaces: Vec<LeanSurface>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCrossActorAuthorizationCase {
    pub name: String,
    pub intent: LeanCrossActorIntent,
    pub observation: LeanCrossActorObservation,
    pub before_session_changes: Vec<LeanPermissionChange>,
    pub session_changes: Vec<LeanPermissionChange>,
    pub invoke_changes: Vec<LeanPermissionChange>,
    pub node_changes: Vec<LeanPermissionChange>,
    pub native_committed: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub prior_effect: Option<String>,
    pub prior_lifecycle: String,
    #[serde(deserialize_with = "required_nullable")]
    pub prior_terminal_output: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub prior_failure_reason: Option<String>,
    pub decision: LeanCrossActorDecision,
    #[serde(deserialize_with = "required_nullable")]
    pub effect: Option<String>,
    pub lifecycle: String,
    #[serde(deserialize_with = "required_nullable")]
    pub terminal_output: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanCrossActorDecision {
    Blocked,
    Unavailable,
    Denied,
    Conflict,
    Claimed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCrossActorIntent {
    pub document: String,
    pub session: String,
    pub agent_document: String,
    pub author: String,
    pub selected_agent: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCrossActorObservation {
    pub supported: bool,
    pub visible: bool,
    pub targets_visible: bool,
    pub transient_failure: bool,
    pub document: String,
    pub session: String,
    pub agent_document: String,
    #[serde(deserialize_with = "required_nullable")]
    pub owner: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub authenticated_author: Option<String>,
    pub executing_agent: String,
    pub signature_valid: bool,
    pub session_permission: bool,
    pub agent_invoke: bool,
    pub node_permission_enforced: bool,
    pub requester_node_permission: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCurrentInputCase {
    pub name: String,
    pub current_request: String,
    pub headers: Vec<LeanCurrentInputHeader>,
    pub retained_indices: Vec<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCurrentInputHeader {
    pub request: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanDecisionRow {
    pub mode: String,
    pub sufficient: bool,
    pub no_case_regression: bool,
    pub cost_ok: bool,
    pub improves: bool,
    pub decision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDeferredCreationCase {
    pub kind: bool,
    pub receipt: bool,
    pub opening: bool,
    pub cause: bool,
    pub expected: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDelegatedFinalityCase {
    pub actions: Vec<LeanDelegatedFinalityCaseActionsItem>,
    pub expected_attempts: u64,
    pub expected_knowledge: String,
    pub expected_native: String,
    pub may_report_failure: bool,
    pub may_retry: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDelegatedFinalityCaseActionsItem {
    pub kind: String,
    pub valid: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDelegationCase {
    pub name: String,
    pub holder: String,
    pub relation: String,
    pub delete: bool,
    pub expected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDependencyDenial {
    pub root_close_id: u64,
    pub denied_doc_id: u64,
}

#[derive(Debug, Deserialize)]
pub struct LeanDeviation {
    pub id: String,
    pub domain: String,
    pub subject: String,
    pub statement: String,
    pub accepted_failure_mode: Option<String>,
    pub accepted_follow_up: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDiscoveryGuidanceCase {
    pub enabled: bool,
    pub expected: bool,
    pub name: String,
    pub presented: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchObservationCase {
    pub name: String,
    pub inputs: Vec<LeanDispatchObservationInput>,
    pub expected: Vec<LeanDispatchObservationResult>,
    pub completion_probe_outcome: String,
    pub completion_probe_accepted: bool,
    pub parent_outcome: String,
    pub expected_after_parent_failure: LeanDispatchParentFailure,
    pub expected_after_policy_settlement: Option<LeanDispatchPolicySettlement>,
    pub expected_after_parent_recovery: Option<LeanDispatchParentRecovery>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchObservationInput {
    pub acknowledged: bool,
    pub policy_allows: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchObservationResult {
    pub observation: String,
    pub may_invoke: bool,
    pub running: bool,
    pub in_flight: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchParentFailure {
    pub running: bool,
    pub needs_recovery: bool,
    pub message_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchParentRecovery {
    pub state: String,
    pub dispatchable: bool,
    pub terminalized: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchPolicySettlement {
    pub failed: bool,
    pub running: bool,
    pub started: bool,
    pub failure_class: Option<String>,
    pub completion_accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDurableObservationCase {
    pub cancelled: bool,
    pub change: String,
    pub deadline_at: u64,
    pub expected_feed_open: bool,
    pub expected_rescan_at: u64,
    pub expected_wake: String,
    pub feed_open: bool,
    pub now: u64,
    pub rescan_at: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanDurableReductionCase {
    pub name: String,
    pub request_doc_id: u64,
    pub turn_index: usize,
    pub ordinal: usize,
    pub checkpoint: u64,
    pub claim_commit: u64,
    pub prior_checkpoint: Option<u64>,
    pub prior_claim_commit: Option<u64>,
    pub pair_closed: bool,
    pub inference_cites: bool,
    pub inference_supported: bool,
    pub title_cites: bool,
    pub outcome: String,
    pub durable_after: bool,
    pub send_permitted: bool,
    pub consumed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDurableUsageRow {
    pub request_doc_id: String,
    pub kind: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentAbsentPeerCase {
    pub expected_present: bool,
    pub expected_ready: bool,
    pub initial_ready: bool,
    pub managed: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentAutoRenewalCase {
    pub allowed: bool,
    pub current: LeanEnrollmentAutoRenewalCaseCurrent,
    pub name: String,
    pub now: u64,
    pub projection_allowed: bool,
    pub proposal: LeanEnrollmentAutoRenewalCaseProposal,
    pub protocol_max: u64,
    pub window: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentAutoRenewalCaseCurrent {
    pub authority_conflict: bool,
    pub enabled: bool,
    pub expires_at: u64,
    pub lease_seconds: u64,
    pub member: String,
    pub network: String,
    pub next_sequence: u64,
    pub owner: String,
    pub peer: String,
    pub profile: String,
    pub renewal_limit: Option<u64>,
    pub revoked: bool,
    pub sequence: u64,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentAutoRenewalCaseProposal {
    pub member: String,
    pub member_signed: bool,
    pub network: String,
    pub offer_fresh: bool,
    pub owner: String,
    pub peer: String,
    pub predecessor_sequence: u64,
    pub profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentCase {
    pub name: String,
    pub steps: Vec<LeanEnrollmentTraceStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentDigestCase {
    pub name: String,
    pub fields: Vec<String>,
    pub expected_payload: String,
    pub actual_payload: String,
    pub expected_digest: String,
    pub actual_digest: String,
    pub payload_matches: bool,
    pub digest_matches: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentDurableProjectionCase {
    pub name: String,
    pub documents: Vec<LeanEnrollmentTraceStep>,
    pub expected_current_approval: bool,
    pub expected_current_route_receipt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentEncodingCase {
    pub name: String,
    pub value: String,
    pub expected_frame: String,
    pub actual_frame: String,
    pub frame_matches: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentPendingRequestCase {
    pub current: LeanEnrollmentPendingRequestCaseCurrent,
    pub current_approval: bool,
    pub expected_satisfied: bool,
    pub expected_visible: bool,
    pub has_decision: bool,
    pub name: String,
    pub request_fresh: bool,
    pub requested: LeanEnrollmentPendingRequestCaseRequested,
    pub route_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentPendingRequestCaseCurrent {
    pub admin_did: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub network_id: String,
    pub owner_node: String,
    pub profile: String,
    pub server_peer: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentPendingRequestCaseRequested {
    pub admin_did: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub network_id: String,
    pub owner_node: String,
    pub profile: String,
    pub server_peer: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentRenewalIntentCase {
    pub allowed: bool,
    pub current: LeanEnrollmentRenewalIntentCaseCurrent,
    pub intent: LeanEnrollmentRenewalIntentCaseIntent,
    pub max_validity: u64,
    pub name: String,
    pub now: u64,
    pub projection_allowed: bool,
    pub window: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentRenewalIntentCaseCurrent {
    pub conflict: bool,
    pub enabled: bool,
    pub expires_at: u64,
    pub member: String,
    pub network: String,
    pub next_sequence: u64,
    pub peer: String,
    pub revoked: bool,
    pub sequence: u64,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentRenewalIntentCaseIntent {
    pub expires_at: u64,
    pub issued_at: u64,
    pub member: String,
    pub member_signed: bool,
    pub network: String,
    pub peer: String,
    pub predecessor_sequence: u64,
    pub transport_authenticated: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentRouteReadinessCase {
    pub digest: bool,
    pub enrollment: bool,
    pub expected_ready: bool,
    pub lease_fresh: bool,
    pub name: String,
    pub network: bool,
    pub operator_endpoint: bool,
    pub route_applied: bool,
    pub sequence_fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanEnrollmentStartupCase {
    pub expected_authority_unchanged: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentTraceStep {
    pub action: String,
    pub peer_admission_did: String,
    pub offer_id: String,
    pub offer_challenge: String,
    pub offer_network_id: String,
    pub offer_admin_did: String,
    pub offer_server_peer: String,
    pub offer_owner_node: String,
    pub offer_profile: String,
    pub challenge: String,
    pub request_id: String,
    pub request_digest: String,
    pub request_offer_id: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub server_ticket_peer: String,
    pub resolved_server_did: String,
    pub profile: String,
    pub schema_compatible: bool,
    pub offer_admin_signed: bool,
    pub offer_fresh: bool,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub observed_candidate_peer: String,
    pub resolved_candidate_did: String,
    pub candidate_ticket_peer: String,
    pub owner_node: String,
    pub client_nonce: String,
    pub issued_at: String,
    pub expires_at: String,
    pub candidate_signed: bool,
    pub request_fresh: bool,
    pub decision_authorization_sequence: usize,
    pub decision_authorization_expires_at: String,
    pub decision_signer_did: String,
    pub decision_kind: String,
    pub decision_request_id: String,
    pub decision_request_digest: String,
    pub decision_network_id: String,
    pub decision_admin_did: String,
    pub decision_candidate_did: String,
    pub decision_candidate_peer: String,
    pub decision_owner_node: String,
    pub decision_admin_signed: bool,
    pub decision_fresh: bool,
    pub revision_kind: String,
    pub revision_sequence: usize,
    pub revision_authorization_expires_at: String,
    pub revision_signer_did: String,
    pub revision_request_id: String,
    pub revision_request_digest: String,
    pub revision_network_id: String,
    pub revision_admin_did: String,
    pub revision_member_did: String,
    pub revision_member_peer: String,
    pub revision_owner_node: String,
    pub revision_admin_signed: bool,
    pub receipt_request_id: String,
    pub receipt_request_digest: String,
    pub receipt_network_id: String,
    pub receipt_admin_did: String,
    pub receipt_member_did: String,
    pub receipt_member_peer: String,
    pub receipt_server_peer: String,
    pub receipt_owner_node: String,
    pub receipt_authorization_sequence: usize,
    pub receipt_authorization_expires_at: String,
    pub receipt_direction: String,
    pub receipt_signer_did: String,
    pub receipt_admin_signed: bool,
    pub receipt_applied: bool,
    pub observed_offer_count: usize,
    pub admin_pin_count: usize,
    pub challenge_binding_count: usize,
    pub request_binding_count: usize,
    pub request_count: usize,
    pub decision_count: usize,
    pub authorization_count: usize,
    pub membership_count: usize,
    pub receipt_count: usize,
    pub route_count: usize,
    pub request_accepted: bool,
    pub decision_recorded: bool,
    pub authorization_recorded: bool,
    pub revision_recorded: bool,
    pub receipt_recorded: bool,
    pub membership_present: bool,
    pub client_route_present: bool,
    pub server_route_present: bool,
    pub admin_pin_present: bool,
    pub admin_pin_conflict: bool,
    pub challenge_binding_conflict: bool,
    pub request_binding_conflict: bool,
    pub current_approval: bool,
    pub peer_admitted: bool,
    pub ready: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanEvalDocumentCaptureCase {
    pub collection: String,
    pub fields: Vec<String>,
    pub filter_fields: Vec<String>,
    pub filter: serde_json::Value,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEvalOutcomeCase {
    pub kind: String,
    pub provider_reason: Option<String>,
    pub class: String,
    pub subject_causable: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanEventDeliveryAction {
    Persist { doc: String },
    Depersist { doc: String },
    Enqueue { doc: String },
    Drop { doc: String },
    DeliverFromQueue { doc: String },
    RescanTick,
    Handle { doc: String },
    Release { doc: String },
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanEventDeliveryConvergenceTrace {
    pub name: String,
    pub instance_name: String,
    pub initial_world: LeanEventDeliveryWorld,
    pub actions: Vec<LeanEventDeliveryAction>,
    pub final_world: LeanEventDeliveryWorld,
    pub status: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanEventDeliverySourceInstance {
    pub name: String,
    pub dedupe_policy: String,
    pub rescan_bounded_by: u64,
    pub deviation: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanEventDeliveryTransitionCase {
    pub name: String,
    pub pre: LeanEventDeliveryWorld,
    pub action: LeanEventDeliveryAction,
    pub post: LeanEventDeliveryWorld,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct LeanEventDeliveryWorld {
    pub persistent_set: Vec<String>,
    pub subscription_queue: Vec<String>,
    pub processed_set: Vec<String>,
    pub handled: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEventGroupCase {
    pub name: String,
    pub candidate: LeanEventGroupKeyContract,
    pub actual_count: usize,
    pub expected_count: Option<usize>,
    pub minimum_count: usize,
    pub timed_out: bool,
    pub well_formed: bool,
    pub prior_markers: Vec<LeanEventGroupKeyContract>,
    pub eligible: bool,
    pub materialized: bool,
    pub marker_count_after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEventGroupKeyContract {
    pub node_did: String,
    pub consumer: serde_json::Value,
    pub consumer_config_key: String,
    pub correlation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanExactSeed {
    pub case_id: String,
    pub stage_id: String,
    pub collection: String,
    pub document: serde_json::Value,
    pub rendered: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFamily {
    pub input: Value,
    pub output: Value,
    pub cases: Vec<LeanFamilyCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFamilyCase {
    pub input: Value,
    pub expected: Value,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureMatrixCell {
    pub coverage_strength: String,
    pub row_count: usize,
    pub pending_follow_ups: usize,
    pub deferred_note: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureSurfaceDeferral {
    pub surface: LeanSurface,
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureSurfaceRequirement {
    pub feature: String,
    pub required: Vec<LeanSurface>,
    pub deferred: Vec<LeanFeatureSurfaceDeferral>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFieldReadCase {
    pub granted: bool,
    #[serde(rename = "hashOk")]
    pub hash_ok: bool,
    pub next: Option<u64>,
    pub offset: u64,
    pub present: bool,
    pub widths: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFileReviewRouteCase {
    pub demand_collection: String,
    pub direction: String,
    pub expected: Option<serde_json::Value>,
    pub image_collection: String,
    pub image_filter_installed: bool,
    pub images_granted: bool,
    pub owner: String,
    pub requester: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFileReviewSubscriptionCase {
    pub collection: String,
    pub replicated: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanFleetSlotAccountingCase {
    pub name: String,
    pub property: String,
    pub backend_id: String,
    pub request_state: String,
    pub admission_state: String,
    pub contribution: usize,
    pub expected_contribution: usize,
    pub active_count: usize,
    pub scheduler_running: usize,
    pub slot_count: usize,
    pub row_states: Vec<String>,
    pub row_backend_ids: Vec<String>,
    pub reconstructed_running_count: usize,
    pub max_concurrent: usize,
    pub bounded_by_max_concurrent: bool,
    pub aggregate_reconstructed_not_persisted: bool,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanFoldAuthoredKey {
    Context,
    Prompt,
    Folded { request_id: u64 },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldPublicationCase {
    pub name: String,
    pub head: u64,
    pub selected: u64,
    pub steps: Vec<LeanFoldPublicationStep>,
    pub expected: Vec<LeanFoldPublicationObservation>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldPublicationObservation {
    pub accepted: bool,
    pub active: Option<u64>,
    pub pending: Vec<u64>,
    pub folding: Vec<u64>,
    pub terminal: Vec<u64>,
    pub authored_keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanFoldPublicationStep {
    ObserveDeadline { now: u64, deadline: u64 },
    EnqueueSteering { request_id: u64 },
    Intake { generation: u64, safe_boundary: bool },
    FinishOrIntake { generation: u64, safe_boundary: bool },
    CancelFirstPending,
    PublishPrompt { generation: u64 },
    PublishChangedPrompt { generation: u64 },
    PublishFolded { generation: u64, request_id: u64 },
    AcceptTurn { generation: u64 },
    Recover { expected: u64, fresh: u64 },
    Terminalize { generation: u64 },
    Finish,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldQueueCase {
    pub name: String,
    pub node_id: u64,
    pub session_id: u64,
    pub inputs: Vec<LeanFoldQueueInput>,
    pub expected: LeanFoldQueueObservation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldQueueClaim {
    pub head: u64,
    pub folded: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldQueueEntry {
    pub request_id: u64,
    pub execution_origin: String,
    pub delivery: String,
    pub order_key: u64,
    pub source: String,
    pub policy: String,
    pub queued_after: Option<u64>,
    pub requester_id: Option<u64>,
    pub turn_context: u64,
    pub fresh: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanFoldQueueInput {
    Enqueue { entry: LeanFoldQueueEntry },
    Claim { admitted: Vec<u64> },
    Consume,
    Finish,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldQueueObservation {
    pub active: Option<u64>,
    pub pending: Vec<u64>,
    pub folding: Vec<u64>,
    pub terminal: Vec<u64>,
    pub claims: Vec<LeanFoldQueueClaim>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldTurnInputAuthored {
    pub key: LeanFoldAuthoredKey,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldTurnInputCase {
    pub name: String,
    pub context: Option<String>,
    pub head: String,
    pub folded: Vec<LeanFoldTurnInputFolded>,
    pub authored: Vec<LeanFoldTurnInputAuthored>,
    pub provider_input: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldTurnInputFolded {
    pub request_id: u64,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGateRow {
    pub name: String,
    pub cases_match: bool,
    pub cases: Vec<LeanCasePairs>,
    pub keys: u64,
    pub dropped_baseline: u64,
    pub dropped_candidate: u64,
    pub sufficient: bool,
    pub no_case_regression: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalBackedRuntimeCase {
    pub goal: bool,
    pub grants: bool,
    pub name: String,
    pub request: bool,
    pub session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalCapabilityResolutionCase {
    pub name: String,
    pub explicit_goal_tools: Option<bool>,
    pub explicit_goal_create: Option<bool>,
    pub expected_goal_tools: bool,
    pub expected_goal_create: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedIdentityCase {
    pub actor: String,
    pub allowed: bool,
    pub candidate_allowed: bool,
    pub parent: String,
    pub selected: String,
    pub signer: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCase {
    pub before: LeanGoalClaimedPublicationCaseBefore,
    pub commit: bool,
    pub expected: LeanGoalClaimedPublicationCaseExpected,
    pub name: String,
    pub observation: LeanGoalClaimedPublicationCaseObservation,
    pub outcome: String,
    pub request: LeanGoalClaimedPublicationCaseRequest,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseBefore {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalClaimedPublicationCaseBeforeChildrenItem>,
    pub last_continued_from: u64,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseBeforeChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseExpected {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalClaimedPublicationCaseExpectedChildrenItem>,
    pub last_continued_from: u64,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseExpectedChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseObservation {
    pub backgrounds: Option<
        Vec<LeanGoalClaimedPublicationCaseObservationBackgroundsItem>,
    >,
    pub storage_failed: bool,
    pub waits: Vec<LeanGoalClaimedPublicationCaseObservationWaitsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseObservationBackgroundsItem {
    pub doc_id: u64,
    pub handle: String,
    pub origin: LeanGoalClaimedPublicationCaseObservationBackgroundsItemOrigin,
    pub owner: String,
    pub requester: String,
    pub session: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseObservationBackgroundsItemOrigin {
    pub kind: String,
    pub parent_tool_doc: Option<u64>,
    pub stable_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseObservationWaitsItem {
    pub accepted_handle: String,
    pub accepted_tool: String,
    pub doc_id: u64,
    pub owner: String,
    pub parent_request_doc: u64,
    pub replied: bool,
    pub reply: String,
    pub reply_handle: String,
    pub requester: String,
    pub session: String,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseRequest {
    pub authorized: bool,
    pub binding: LeanGoalClaimedPublicationCaseRequestBinding,
    pub expected_last_continued_from: u64,
    pub expected_sequence: u64,
    pub expected_status: String,
    pub parent_belongs_to_goal: bool,
    pub requester: String,
    pub session_idle: bool,
    pub terminal_parent: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedPublicationCaseRequestBinding {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedReadinessCase {
    pub before: LeanGoalClaimedReadinessCaseBefore,
    pub child_exists: bool,
    pub decision: String,
    pub expected: LeanGoalClaimedReadinessCaseExpected,
    pub name: String,
    pub newer_than_terminal: bool,
    pub observation: String,
    pub settled: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedReadinessCaseBefore {
    pub blocked_audits: u64,
    pub continuation_sequence: u64,
    pub infrastructure_retries: u64,
    pub last_failure: String,
    pub parent: String,
    pub status: String,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedReadinessCaseExpected {
    pub blocked_audits: u64,
    pub continuation_sequence: u64,
    pub infrastructure_retries: u64,
    pub last_failure: String,
    pub parent: String,
    pub status: String,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClaimedTargetCase {
    pub agent: String,
    pub commit: bool,
    pub committed: bool,
    pub fresh: bool,
    pub grants: bool,
    pub name: String,
    pub observed_agent: String,
    pub observed_session: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCase {
    pub before: LeanGoalClientControlCaseBefore,
    pub expected: Option<LeanGoalClientControlCaseExpected>,
    pub expected_revision: LeanGoalClientControlCaseExpectedRevision_,
    pub facts: LeanGoalClientControlCaseFacts,
    pub name: String,
    pub next_composite: String,
    pub operation: LeanGoalClientControlCaseOperation,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseBefore {
    pub execution: LeanGoalClientControlCaseBeforeExecution,
    pub objective: String,
    pub revision: LeanGoalClientControlCaseBeforeRevision,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseBeforeExecution {
    pub blocked_audits: u64,
    pub children: Vec<serde_json::Value>,
    pub last_continued_from: Option<serde_json::Value>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseBeforeRevision {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseExpected {
    pub execution: LeanGoalClientControlCaseExpectedExecution,
    pub objective: String,
    pub revision: LeanGoalClientControlCaseExpectedRevision,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseExpectedExecution {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalClientControlCaseExpectedExecutionChildrenItem>,
    pub last_continued_from: Option<u64>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseExpectedExecutionChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseExpectedRevision {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseExpectedRevision_ {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseFacts {
    pub authorized: bool,
    pub resume: Option<LeanGoalClientControlCaseFactsResume>,
    pub unfinished_request: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseFactsResume {
    pub authorized: bool,
    pub binding: LeanGoalClientControlCaseFactsResumeBinding,
    pub expected_sequence: u64,
    pub expected_status: String,
    pub parent_belongs_to_goal: bool,
    pub session_idle: bool,
    pub terminal_parent: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseFactsResumeBinding {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientControlCaseOperation {
    pub budget: Option<i64>,
    pub kind: String,
    pub objective: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCase {
    pub before: LeanGoalClientReceiptCaseBefore,
    pub command: LeanGoalClientReceiptCaseCommand,
    pub commit: bool,
    pub expected: LeanGoalClientReceiptCaseExpected,
    pub facts: LeanGoalClientReceiptCaseFacts,
    pub history: Vec<LeanGoalClientReceiptCaseHistoryItem>,
    pub initial: LeanGoalClientReceiptCaseInitial,
    pub name: String,
    pub next_composite: String,
    pub now: u64,
    pub receipt: LeanGoalClientReceiptCaseReceipt,
    pub recovered: Option<LeanGoalClientReceiptCaseRecovered>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseBefore {
    pub goal: LeanGoalClientReceiptCaseBeforeGoal,
    pub receipts: Vec<LeanGoalClientReceiptCaseBeforeReceiptsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseBeforeGoal {
    pub execution: LeanGoalClientReceiptCaseBeforeGoalExecution,
    pub objective: String,
    pub revision: LeanGoalClientReceiptCaseBeforeGoalRevision,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseBeforeGoalExecution {
    pub blocked_audits: u64,
    pub children: Vec<serde_json::Value>,
    pub last_continued_from: Option<serde_json::Value>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseBeforeGoalRevision {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseBeforeReceiptsItem {
    pub command_id: u64,
    pub digest: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseCommand {
    pub digest: u64,
    pub expected: LeanGoalClientReceiptCaseCommandExpected,
    pub expires_at: u64,
    pub has_peer: bool,
    pub id: u64,
    pub issued_at: u64,
    pub operation: LeanGoalClientReceiptCaseCommandOperation,
    pub session_document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseCommandExpected {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseCommandOperation {
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseExpected {
    pub goal: LeanGoalClientReceiptCaseExpectedGoal,
    pub receipts: Vec<LeanGoalClientReceiptCaseExpectedReceiptsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseExpectedGoal {
    pub execution: LeanGoalClientReceiptCaseExpectedGoalExecution,
    pub objective: String,
    pub revision: LeanGoalClientReceiptCaseExpectedGoalRevision,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseExpectedGoalExecution {
    pub blocked_audits: u64,
    pub children: Vec<serde_json::Value>,
    pub last_continued_from: Option<serde_json::Value>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseExpectedGoalRevision {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseExpectedReceiptsItem {
    pub command_id: u64,
    pub digest: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseFacts {
    pub authority: LeanGoalClientReceiptCaseFactsAuthority,
    pub command_identity_unique: bool,
    pub control: LeanGoalClientReceiptCaseFactsControl,
    pub session_document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseFactsAuthority {
    pub enrollment_fresh: bool,
    pub route_applied: bool,
    pub session_owned: bool,
    pub signature_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseFactsControl {
    pub authorized: bool,
    pub resume: Option<serde_json::Value>,
    pub unfinished_request: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseHistoryItem {
    pub command: Option<LeanGoalClientReceiptCaseHistoryItemCommand>,
    pub commit: Option<bool>,
    pub facts: Option<LeanGoalClientReceiptCaseHistoryItemFacts>,
    pub kind: String,
    pub next_composite: Option<String>,
    pub now: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseHistoryItemCommand {
    pub digest: u64,
    pub expected: LeanGoalClientReceiptCaseHistoryItemCommandExpected,
    pub expires_at: u64,
    pub has_peer: bool,
    pub id: u64,
    pub issued_at: u64,
    pub operation: LeanGoalClientReceiptCaseHistoryItemCommandOperation,
    pub session_document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseHistoryItemCommandExpected {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseHistoryItemCommandOperation {
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseHistoryItemFacts {
    pub authority: LeanGoalClientReceiptCaseHistoryItemFactsAuthority,
    pub command_identity_unique: bool,
    pub control: LeanGoalClientReceiptCaseHistoryItemFactsControl,
    pub session_document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseHistoryItemFactsAuthority {
    pub enrollment_fresh: bool,
    pub route_applied: bool,
    pub session_owned: bool,
    pub signature_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseHistoryItemFactsControl {
    pub authorized: bool,
    pub resume: Option<serde_json::Value>,
    pub unfinished_request: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseInitial {
    pub goal: LeanGoalClientReceiptCaseInitialGoal,
    pub receipts: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseInitialGoal {
    pub execution: LeanGoalClientReceiptCaseInitialGoalExecution,
    pub objective: String,
    pub revision: LeanGoalClientReceiptCaseInitialGoalRevision,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseInitialGoalExecution {
    pub blocked_audits: u64,
    pub children: Vec<serde_json::Value>,
    pub last_continued_from: Option<serde_json::Value>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseInitialGoalRevision {
    pub composite: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseReceipt {
    pub command_id: u64,
    pub digest: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalClientReceiptCaseRecovered {
    pub command_id: u64,
    pub digest: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalConfigReactivationCase {
    pub allowed: bool,
    pub current: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalContinuationAssignmentCase {
    pub current: Option<String>,
    pub edges: Vec<LeanGoalContinuationAssignmentCaseEdgesItem>,
    pub expected: Option<LeanGoalContinuationAssignmentCaseExpected>,
    pub goal: String,
    pub name: String,
    pub parent: Option<LeanGoalContinuationAssignmentCaseParent>,
    pub parent_doc: String,
    pub receipt_doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalContinuationAssignmentCaseEdgesItem {
    pub accepted: bool,
    pub child: Option<LeanGoalContinuationAssignmentCaseEdgesItemChild>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalContinuationAssignmentCaseEdgesItemChild {
    pub goal_id: String,
    pub root_request_doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalContinuationAssignmentCaseExpected {
    pub goal_id: String,
    pub root_request_doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalContinuationAssignmentCaseParent {
    pub goal_id: String,
    pub root_request_doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalCreateCase {
    pub name: String,
    pub caller: String,
    pub current_session: String,
    pub requested_owner: String,
    pub requested_session: String,
    pub objective: String,
    pub objective_nonempty: bool,
    pub token_budget: Option<i128>,
    pub goal_tools: bool,
    pub goal_create: bool,
    pub existing: bool,
    pub existing_matches: bool,
    pub expected: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalFirstRequestBindingCase {
    pub accepted: bool,
    pub commit: bool,
    pub current: Option<String>,
    pub current_task_goal: Option<String>,
    pub expected_root: Option<String>,
    pub expected_task_goal: String,
    pub goal: String,
    pub initial_task_goal: String,
    pub name: String,
    pub observed_task_goal: Option<String>,
    pub reference: Option<LeanGoalFirstRequestBindingCaseReference>,
    pub request_doc: String,
    pub task_binding_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalFirstRequestBindingCaseReference {
    pub goal_id: String,
    pub root_request_doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalFreshSessionInputCase {
    pub expected: LeanGoalFreshSessionInputCaseExpected,
    pub input: LeanGoalFreshSessionInputCaseInput,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalFreshSessionInputCaseExpected {
    pub authored: u64,
    pub continuation: bool,
    pub creation: Option<serde_json::Value>,
    pub queued: bool,
    pub reference: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalFreshSessionInputCaseInput {
    pub authored: u64,
    pub continuation: bool,
    pub creation: Option<String>,
    pub queued: bool,
    pub reference: Option<LeanGoalFreshSessionInputCaseInputReference>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalFreshSessionInputCaseInputReference {
    pub goal: u64,
    pub root: Option<u64>,
}

/// An RFC 3339 text with the instant, in nanoseconds since the epoch, the
/// model compares; `None` where the text does not parse.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalHaltStamp {
    pub text: String,
    pub instant: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanGoalHaltVerdict {
    Inactive,
    ChangedAfter,
    UnreadableHalt,
    Pause { status: String },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalInputBindingCase {
    pub captured: Option<LeanGoalInputBindingCaseCaptured>,
    pub expected: LeanGoalInputBindingCaseExpected,
    pub name: String,
    pub observation_verified: bool,
    pub observed: Option<LeanGoalInputBindingCaseObserved>,
    pub session_readable: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalInputBindingCaseCaptured {
    pub goal: u64,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalInputBindingCaseExpected {
    pub kind: String,
    pub reference: Option<LeanGoalInputBindingCaseExpectedReference>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalInputBindingCaseExpectedReference {
    pub goal: u64,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalInputBindingCaseObserved {
    pub goal: u64,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalLinkCase {
    pub collection: String,
    pub actor: String,
    pub mode: String,
    pub commit: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub owner: Option<String>,
    pub initial: LeanGoalLinkState,
    pub steps: Vec<LeanGoalLinkStep>,
    pub after_revocation: LeanGoalLinkState,
    pub repair_allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalLinkState {
    pub parent: bool,
    pub worker: bool,
    pub linked: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalLinkStep {
    pub parent_ok: bool,
    pub worker_ok: bool,
    pub mark_ok: bool,
    pub after: LeanGoalLinkState,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeCase {
    pub before: LeanGoalOperatorResumeCaseBefore,
    pub commit: bool,
    pub expected: LeanGoalOperatorResumeCaseExpected,
    pub goal_status: Option<String>,
    pub name: String,
    pub outcome: String,
    pub request: LeanGoalOperatorResumeCaseRequest,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeCaseBefore {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalOperatorResumeCaseBeforeChildrenItem>,
    pub last_continued_from: Option<u64>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeCaseBeforeChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeCaseExpected {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalOperatorResumeCaseExpectedChildrenItem>,
    pub last_continued_from: Option<u64>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeCaseExpectedChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeCaseRequest {
    pub authorized: bool,
    pub binding: LeanGoalOperatorResumeCaseRequestBinding,
    pub expected_sequence: u64,
    pub expected_status: String,
    pub parent_belongs_to_goal: bool,
    pub session_idle: bool,
    pub terminal_parent: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeCaseRequestBinding {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeOnCase {
    pub before: LeanGoalOperatorResumeOnCaseBefore,
    pub expected: LeanGoalOperatorResumeOnCaseExpected,
    pub name: String,
    pub outcome: String,
    pub target_available: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeOnCaseBefore {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalOperatorResumeOnCaseBeforeChildrenItem>,
    pub last_continued_from: Option<u64>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeOnCaseBeforeChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeOnCaseExpected {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalOperatorResumeOnCaseExpectedChildrenItem>,
    pub last_continued_from: Option<u64>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalOperatorResumeOnCaseExpectedChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalQueueAssignmentCase {
    pub candidate_reference: Option<LeanGoalQueueAssignmentCaseCandidateReference>,
    pub expected_fold: bool,
    pub expected_steer: bool,
    pub head_reference: Option<LeanGoalQueueAssignmentCaseHeadReference>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalQueueAssignmentCaseCandidateReference {
    pub goal_id: String,
    pub root_request_doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalQueueAssignmentCaseHeadReference {
    pub goal_id: String,
    pub root_request_doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAncestryCase {
    pub current: u64,
    pub expected_entry: Option<u64>,
    pub expected_goal: Option<u64>,
    pub name: String,
    pub rows: Vec<LeanGoalRequestAncestryCaseRowsItem>,
    pub scope: LeanGoalRequestAncestryCaseScope,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAncestryCaseRowsItem {
    pub deterministic_identity: bool,
    pub doc: u64,
    pub goal: Option<u64>,
    pub input_disposition: String,
    pub owner: u64,
    pub parent_doc: Option<u64>,
    pub parent_request: Option<u64>,
    pub purpose: String,
    pub receipt_valid: bool,
    pub request: u64,
    pub sequence: u64,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAncestryCaseScope {
    pub goal: u64,
    pub owner: u64,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAssignmentCase {
    pub assignment: LeanGoalRequestAssignmentCaseAssignment,
    pub entry: LeanGoalRequestAssignmentCaseEntry,
    pub expected_member: bool,
    pub expected_observation: LeanGoalRequestAssignmentCaseExpectedObservation,
    pub expected_queue_scope: Option<LeanGoalRequestAssignmentCaseExpectedQueueScope>,
    pub name: String,
    pub observed: Vec<LeanGoalRequestAssignmentCaseObservedItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAssignmentCaseAssignment {
    pub goal: u64,
    pub owner: u64,
    pub root: Option<u64>,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAssignmentCaseEntry {
    pub doc: u64,
    pub owner: u64,
    pub reference: Option<LeanGoalRequestAssignmentCaseEntryReference>,
    pub session: u64,
    pub signature_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAssignmentCaseEntryReference {
    pub goal: u64,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAssignmentCaseExpectedObservation {
    pub goal: Option<u64>,
    pub kind: String,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAssignmentCaseExpectedQueueScope {
    pub entry: Option<u64>,
    pub goal: Option<u64>,
    pub kind: String,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalRequestAssignmentCaseObservedItem {
    pub goal: u64,
    pub owner: u64,
    pub root: Option<u64>,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCase {
    pub before: LeanGoalResetResumeCaseBefore,
    pub commit: bool,
    pub expected: LeanGoalResetResumeCaseExpected,
    pub facts: LeanGoalResetResumeCaseFacts,
    pub name: String,
    pub outcome: String,
    pub request: LeanGoalResetResumeCaseRequest,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCaseBefore {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalResetResumeCaseBeforeChildrenItem>,
    pub last_continued_from: Option<u64>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCaseBeforeChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCaseExpected {
    pub blocked_audits: u64,
    pub children: Vec<LeanGoalResetResumeCaseExpectedChildrenItem>,
    pub last_continued_from: Option<u64>,
    pub latest_request: u64,
    pub sequence: u64,
    pub status: String,
    pub token_budget: u64,
    pub tokens_used: u64,
    pub wrapup_completed: bool,
    pub wrapup_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCaseExpectedChildrenItem {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCaseFacts {
    pub account_enabled: bool,
    pub backend_enabled: bool,
    pub limit_started_at: u64,
    pub now: u64,
    pub opted_in: bool,
    pub profile_names_account: bool,
    pub reset_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCaseRequest {
    pub authorized: bool,
    pub binding: LeanGoalResetResumeCaseRequestBinding,
    pub expected_sequence: u64,
    pub expected_status: String,
    pub parent_belongs_to_goal: bool,
    pub session_idle: bool,
    pub terminal_parent: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResetResumeCaseRequestBinding {
    pub child: u64,
    pub correlation: String,
    pub goal: u64,
    pub owner: String,
    pub predecessor: u64,
    pub predecessor_doc: u64,
    pub semantic_fingerprint: String,
    pub sequence: u64,
    pub session: String,
    pub source_document: String,
    pub trigger_context: String,
    pub workspace_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResumeCreate {
    pub id: u64,
    pub trigger: Option<u64>,
    pub reference: Option<LeanGoalResumeReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanGoalResumeDecision {
    Refuse { reason: String },
    Proceed {
        goal: Option<LeanGoalResumeGoal>,
        #[serde(default)]
        create: Option<LeanGoalResumeCreate>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResumeGoal {
    pub goal: u64,
    pub status: LeanGoalResumeStatus,
    pub root: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResumePublicationCase {
    pub name: String,
    pub pending: bool,
    pub authorized: bool,
    pub granted: bool,
    pub expected_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResumeReference {
    pub goal: u64,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResumeRequest {
    pub id: u64,
    pub terminal: bool,
    pub trigger: Option<u64>,
    pub reference: Option<LeanGoalResumeReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanGoalResumeResult {
    Ok { goals: Vec<LeanGoalResumeGoal>, requests: Vec<LeanGoalResumeRequest> },
    Error { refusal: LeanLifecycleHookRefusal },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanGoalResumeStatus {
    Active,
    Paused,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCase {
    pub can_create: bool,
    pub can_update: bool,
    pub current: Option<LeanGoalToolContextCaseCurrent>,
    pub entry: LeanGoalToolContextCaseEntry,
    pub name: String,
    pub observation: LeanGoalToolContextCaseObservation,
    pub read_only: bool,
    pub retire_documents: Vec<u64>,
    pub versions: Vec<LeanGoalToolContextCaseVersionsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCaseCurrent {
    pub document: u64,
    pub goal: u64,
    pub root: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCaseEntry {
    pub doc: u64,
    pub owner: u64,
    pub reference: Option<LeanGoalToolContextCaseEntryReference>,
    pub session: u64,
    pub signature_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCaseEntryReference {
    pub goal: u64,
    pub root: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCaseObservation {
    pub identity: Option<LeanGoalToolContextCaseObservationIdentity>,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCaseObservationIdentity {
    pub document: u64,
    pub goal: u64,
    pub root: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCaseVersionsItem {
    pub assignment: LeanGoalToolContextCaseVersionsItemAssignment,
    pub commit: u64,
    pub document: u64,
    pub height: u64,
    pub parents: Vec<u64>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalToolContextCaseVersionsItemAssignment {
    pub goal: u64,
    pub owner: u64,
    pub root: Option<u64>,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeAncestryCase {
    pub current: u64,
    pub expected_entry: Option<u64>,
    pub expected_goal: Option<serde_json::Value>,
    pub expected_invocation_entry: Option<u64>,
    pub name: String,
    pub rows: Vec<LeanGoalWakeAncestryCaseRowsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeAncestryCaseRowsItem {
    pub background_wake: bool,
    pub opening_goals: Vec<u64>,
    pub request: LeanGoalWakeAncestryCaseRowsItemRequest,
    pub runtime_wake_receipt: bool,
    pub tagged: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeAncestryCaseRowsItemRequest {
    pub deterministic_identity: bool,
    pub doc: u64,
    pub goal: Option<u64>,
    pub input_disposition: String,
    pub owner: u64,
    pub parent_doc: Option<u64>,
    pub parent_request: Option<u64>,
    pub purpose: String,
    pub receipt_valid: bool,
    pub request: u64,
    pub sequence: u64,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCase {
    pub captured: Vec<u64>,
    pub claim: LeanGoalWakeReceiptCaseClaim,
    pub current: Vec<u64>,
    pub name: String,
    pub notifications: Vec<LeanGoalWakeReceiptCaseNotificationsItem>,
    pub prior: Vec<LeanGoalWakeReceiptCasePriorItem>,
    pub retry_parent: Option<u64>,
    pub superseded_origins: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCaseClaim {
    pub cutoff: u64,
    pub scope: LeanGoalWakeReceiptCaseClaimScope,
    pub wake: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCaseClaimScope {
    pub goal: Option<u64>,
    pub kind: String,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCaseNotificationsItem {
    pub message: u64,
    pub scope: LeanGoalWakeReceiptCaseNotificationsItemScope,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCaseNotificationsItemScope {
    pub goal: Option<u64>,
    pub kind: String,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCasePriorItem {
    pub claim: LeanGoalWakeReceiptCasePriorItemClaim,
    pub create_goal: bool,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCasePriorItemClaim {
    pub cutoff: u64,
    pub scope: LeanGoalWakeReceiptCasePriorItemClaimScope,
    pub wake: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalWakeReceiptCasePriorItemClaimScope {
    pub goal: Option<u64>,
    pub kind: String,
    pub root: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGrantRecipientCase {
    pub recipient: String,
    pub direct: Vec<String>,
    pub projected: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanHandoverFoldCase {
    pub name: String,
    pub pending: Vec<LeanFoldQueueEntry>,
    pub admitted: Vec<u64>,
    pub expected: LeanHandoverFoldObservation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanHandoverFoldObservation {
    pub active: Option<u64>,
    pub folding: Vec<u64>,
    pub pending: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanHeldClientSignerCase {
    pub actor: String,
    pub bearer: String,
    pub signer: String,
    pub has_private: bool,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanHistoryRead {
    pub collection: String,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanHookAttempt {
    pub hook_id: String,
    pub result: LeanCommandResult,
    pub succeeded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanHookInvocation {
    Hook { hook_id: String },
    Work,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanIdentityAction {
    Grant { relation: LeanIdentityRelation },
    Revoke { relation: LeanIdentityRelation },
    GrantAs { actor: String, relation: LeanIdentityRelation, expected: bool },
    RevokeAs { actor: String, relation: LeanIdentityRelation, expected: bool },
    Check {
        actor: String,
        resource: String,
        id: String,
        permission: String,
        expected: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityAgent {
    pub label: String,
    pub did: String,
    pub creation_nonce: String,
    pub node: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityBootstrapCase {
    pub authenticated_client: bool,
    pub configuration_published: bool,
    pub exclusive: bool,
    pub expected_may_grant: bool,
    pub fresh_evidence: bool,
    pub matching_runtime: bool,
    pub nac_enabled: bool,
    pub name: String,
    pub node_absent_observed: bool,
    pub reservation_durable: bool,
    pub store_empty_observed: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityBootstrapReservationCase {
    pub created: bool,
    pub directory_synced: bool,
    pub expected: bool,
    pub file_synced: bool,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanIdentityClientAgentJournalStatus {
    Fresh,
    Pending,
    Succeeded,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityClientAgentRecoveryCase {
    pub name: String,
    pub actor_matches: bool,
    pub record_present: bool,
    pub binding_matches: bool,
    pub commit: LeanIdentityClientAgentJournalStatus,
    pub parent: LeanIdentityClientAgentJournalStatus,
    pub executor: LeanIdentityClientAgentJournalStatus,
    pub expected: LeanIdentityClientAgentRecoveryDisposition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanIdentityClientAgentRecoveryDisposition {
    Proceed,
    BindingMismatch,
    CommitUnknown,
    ExplicitRepair,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityClientTransportCase {
    pub allowed: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityContract {
    pub node_local_collections: Vec<String>,
    pub replicated_protocol_collections: Vec<String>,
    pub schema_actor: String,
    pub runtime_actor: String,
    pub name: String,
    pub statement: String,
    pub enforced: bool,
    pub tracked_by: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityDerivationCase {
    pub name: String,
    pub left: LeanIdentityDerivationInput,
    pub right: LeanIdentityDerivationInput,
    pub expected_same_did: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityDerivationInput {
    pub node_key: String,
    pub creation_nonce: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityDiscoveryPolicyCase {
    pub allowed: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityEnrollmentPolicyCase {
    pub actor: String,
    pub allowed: bool,
    pub document: String,
    pub name: String,
    pub permission: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityEvalRecordCase {
    pub commit_succeeded: bool,
    pub creator: String,
    pub expected_created: bool,
    pub expected_creator_update: bool,
    pub expected_stranger_read: bool,
    pub expected_user_read: bool,
    pub name: String,
    pub node_visible: bool,
    pub relationship_succeeded: bool,
    pub resource: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityHouseholdCase {
    pub name: String,
    pub policy_yaml: String,
    pub objects: Vec<LeanIdentityObject>,
    pub relations: Vec<LeanIdentityRelation>,
    pub enrolled: Vec<String>,
    pub policy_registrations: Vec<LeanIdentityPolicyRegistration>,
    #[serde(rename = "managed_client_handoff_cases")]
    pub _managed_client_handoff_cases: Vec<serde_json::Value>,
    #[serde(rename = "desktop_reset_cases")]
    pub _desktop_reset_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub task_checkpoint_cases: Vec<LeanTaskCheckpointCase>,
    pub self_config_actions: Vec<LeanIdentityAction>,
    #[serde(default)]
    pub invoke_session_actions: Vec<LeanIdentityAction>,
    pub self_config_lookup_cases: Vec<LeanSelfConfigLookupCase>,
    pub actions: Vec<LeanIdentityAction>,
    #[serde(default)]
    pub configuration_signed_create_collections: Vec<
        LeanConfigurationSignedCreateCollection,
    >,
    #[serde(default)]
    pub configuration_signed_delete_collections: Vec<serde_json::Value>,
    #[serde(default)]
    pub client_goal_transaction_actor_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub client_goal_graph_handoff_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_signed_write_retry_cases: Vec<
        LeanConfigurationSignedWriteRetryCase,
    >,
    #[serde(default)]
    pub configuration_immutable_cases: Vec<LeanConfigurationImmutableCase>,
    #[serde(default)]
    pub held_client_signer_cases: Vec<LeanHeldClientSignerCase>,
    #[serde(default)]
    pub ordinary_client_mutation_cases: Vec<LeanOrdinaryClientMutationCase>,
    #[serde(default)]
    pub configuration_atomic_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_node_binding_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_provider_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "backend_credential_cases")]
    pub _backend_credential_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub runtime_default_account_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub runtime_default_choice_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub fresh_backend_admission_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "fresh_backend_origin_cases")]
    pub _fresh_backend_origin_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub fresh_backend_reopen_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "backend_credential_native_cases")]
    pub _backend_credential_native_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "backend_credential_publication_cases")]
    pub _backend_credential_publication_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_plugin_backend_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub backend_discovery_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub backend_probe_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub backend_removal_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub backend_genesis_distinct: bool,
    #[serde(default)]
    pub backend_observation_authority_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_inference_provider_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_backend_metadata_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_userset_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_inference_self_config_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_skill_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_compaction_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_tools_selection_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_task_target_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_tool_reference_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_plugin_profile_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_selection_manager_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_node_manager_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_node_manager_link_revocation_case: serde_json::Value,
    #[serde(default)]
    pub agent_node_manager_invoke_case: serde_json::Value,
    #[serde(default)]
    pub agent_node_manager_link_revocation_case: serde_json::Value,
    #[serde(default)]
    pub mcp_service_use_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_schedule_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_schedule_write_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_pack_installation_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_pack_removal_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_pack_cleanup_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_datastore_tool_use_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_datastore_write_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_ux_datastore_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_ux_create_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_affected_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_removal_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_configuration_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_cursor_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_cursor_rebind_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_cursor_move_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_stale_claim_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_generation_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_execution_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_rebound_checkpoint_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_rebound_visibility_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_module_reader_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub event_group_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub task_receipt_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub outcome_reader_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_projection_cases")]
    pub _tool_grant_projection_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_group_cases")]
    pub _tool_grant_group_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_hierarchy_cases")]
    pub _tool_grant_hierarchy_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_rebinding_cases")]
    pub _tool_grant_rebinding_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "session_tool_access_cases")]
    pub _session_tool_access_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grantor_cases")]
    pub _tool_grantor_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub own_tool_edit_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub dangling_selection_repair_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub destructive_decision_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub tool_source_change_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub tool_operation_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub plugin_delete_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub managed_tool_identity_cases: Vec<LeanManagedToolIdentityCase>,
    #[serde(default)]
    pub projection_binding_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_configuration_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_generation_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_module_claim_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_recovery_configuration_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_placement_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_child_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub pairing_overlay_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub unattended_agent_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "unattended_receipt_cases")]
    pub _unattended_receipt_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub callback_checkpoint_claim_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub ordinary_receipt_visibility_cases: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityLocalRegistryWriterCase {
    pub creator: String,
    pub name: String,
    pub names_runtime: bool,
    pub writer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityNode {
    pub did: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityObject {
    pub resource: String,
    pub id: String,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPairingPartitionCase {
    pub name: String,
    pub owner: String,
    pub peer: String,
    pub selected: bool,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPairingReaderCase {
    pub before_generation: Option<u64>,
    pub before_granted: bool,
    pub committed: bool,
    pub entitled: bool,
    pub expected_generation: Option<u64>,
    pub expected_granted: bool,
    pub generation: u64,
    pub name: String,
    pub owner_authorized: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityParentPublicationCase {
    pub name: String,
    pub initial: LeanIdentityParentPublicationState,
    pub steps: Vec<LeanIdentityParentPublicationStep>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityParentPublicationState {
    pub committed: bool,
    pub parent_granted: bool,
    pub parent_linked: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityParentPublicationStep {
    #[serde(deserialize_with = "required_nullable")]
    pub native_owner: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub claimed_creator: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub authenticated_creator: Option<String>,
    pub grant_ok: bool,
    pub mark_ok: bool,
    pub revoke_parent: bool,
    pub after: LeanIdentityParentPublicationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityPermissionCase {
    pub name: String,
    pub nodes: Vec<LeanIdentityNode>,
    pub agents: Vec<LeanIdentityAgent>,
    pub grants: Vec<LeanIdentityPermissionGrant>,
    pub permission: String,
    pub row_owner: String,
    pub actor_node: String,
    pub actor_did: String,
    pub peer_node: String,
    pub peer_did: String,
    pub expected_actor_did: Option<String>,
    pub expected_peer_did: Option<String>,
    pub expected_actor_allowed: bool,
    pub expected_peer_allowed: bool,
    pub same_node: bool,
    pub expected_decisions_equal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityPermissionGrant {
    pub actor: String,
    pub permission: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPolicyRegistration {
    pub name: String,
    pub candidate_yaml: String,
    pub same_id: bool,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPublicationCase {
    pub initial: LeanIdentityPublicationCaseInitial,
    pub name: String,
    pub steps: Vec<LeanIdentityPublicationCaseStepsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPublicationCaseInitial {
    pub committed: bool,
    pub parent_granted: bool,
    pub pending: bool,
    pub reader_granted: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPublicationCaseStepsItem {
    pub after: LeanIdentityPublicationCaseStepsItemAfter,
    pub parent_ok: bool,
    pub reader_ok: bool,
    pub revoke_parent: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPublicationCaseStepsItemAfter {
    pub committed: bool,
    pub parent_granted: bool,
    pub pending: bool,
    pub reader_granted: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityReadinessSettledCase {
    pub expected: bool,
    pub idle: bool,
    pub live_present: bool,
    pub name: String,
    pub persisted_settled: bool,
    pub result_error: bool,
    pub result_known: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityRelation {
    pub resource: String,
    pub id: String,
    pub relation: String,
    pub subject: LeanIdentitySubject,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityReplicaGrantCase {
    pub agent_read: bool,
    pub app_read: bool,
    pub name: String,
    pub outsider_read: bool,
    pub pending: bool,
    pub runtime_read: bool,
    pub user_read: bool,
    pub user_write: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityRuntimeNacCase {
    pub may_start: bool,
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanIdentityStartupJoin {
    Pending,
    Clean,
    Failed { message: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanIdentityStartupOutcome {
    Wait,
    AwaitJoin,
    Ready,
    Timeout,
    Premature,
    Failed { message: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityStartupReadinessCase {
    pub name: String,
    pub join: LeanIdentityStartupJoin,
    pub shutting_down: bool,
    pub readiness: bool,
    pub deadline: bool,
    pub expected: LeanIdentityStartupOutcome,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum LeanIdentitySubject {
    Actor { actor: String },
    EntitySet { resource: String, id: String, target_relation: String },
    Object { resource: String, id: String },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInferenceObservationCase {
    pub actions: Vec<LeanInferenceObservationCaseActionsItem>,
    pub expected: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInferenceObservationCaseActionsItem {
    pub epoch: Option<u64>,
    pub kind: String,
    pub max_age: Option<u64>,
    pub now: Option<u64>,
    pub observation: Option<LeanInferenceObservationCaseActionsItemObservation>,
    pub target: Option<LeanInferenceObservationCaseActionsItemTarget>,
    pub terminal: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInferenceObservationCaseActionsItemObservation {
    pub activity: String,
    pub attempt: u64,
    pub observed_at: u64,
    pub sequence: u64,
    pub target: LeanInferenceObservationCaseActionsItemObservationTarget,
    pub turn: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInferenceObservationCaseActionsItemObservationTarget {
    pub generation: String,
    pub node: String,
    pub request: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInferenceObservationCaseActionsItemTarget {
    pub generation: String,
    pub node: String,
    pub request: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInferenceRouteCase {
    pub active: bool,
    pub expected: String,
    pub holds_node_read: bool,
    pub requests: u64,
    pub sessions: u64,
}

#[derive(Debug, Deserialize)]
pub struct LeanInferenceSlotAccountingCase {
    pub name: String,
    pub property: String,
    pub backend_id: String,
    pub pre_state: String,
    pub post_state: String,
    pub contribution: usize,
    pub expected_contribution: usize,
    pub pre_contribution: usize,
    pub post_contribution: usize,
    pub released_slot: bool,
    pub permit_drop_terminalization: bool,
    pub row_states: Vec<String>,
    pub row_backend_ids: Vec<String>,
    pub reconstructed_running_count: usize,
    pub max_concurrent: usize,
    pub bounded_by_max_concurrent: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInferenceVisibilityCase {
    pub active: bool,
    pub expected: bool,
    pub requests: u64,
    pub sessions: u64,
}

#[derive(Debug, Deserialize)]
pub struct LeanInterruptDeliveryCase {
    pub goal_paused: bool,
    pub planned: Option<u64>,
    pub active: Option<u64>,
    pub expected: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanInterruptDispositionCase {
    pub name: String,
    pub state: String,
    pub await_mode: String,
    pub disposition: String,
    pub post_state: String,
    pub post_await_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInterruptQueueCase {
    pub name: String,
    pub node_id: u64,
    pub requester_id: Option<u64>,
    pub session_id: u64,
    pub active_request_id: Option<u64>,
    pub inputs: Vec<LeanInterruptQueueInput>,
    pub expected: LeanInterruptQueueObservation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInterruptQueueEntry {
    pub request_id: u64,
    pub created_at: u64,
    pub execution_origin: String,
    pub source: String,
    pub policy: String,
    pub queue_key: Option<u64>,
    pub queued_after: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanInterruptQueueInput {
    Interrupt,
    CaptureInterrupt,
    CommitInterrupt,
    Enqueue { entry: LeanInterruptQueueEntry },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInterruptQueueObservation {
    pub pending: Vec<u64>,
    pub terminal: Vec<u64>,
    pub latched: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInvalidToolProgressCase {
    pub composed_actions: Vec<String>,
    pub composed_ending: Option<String>,
    pub composed_invalid_used: u64,
    pub name: String,
    pub outcomes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanInvocationCorrelationCase {
    pub expected: Option<String>,
    pub request: Option<String>,
    pub supplied: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanJobCreationCase {
    pub parent: bool,
    pub job: bool,
    pub key: bool,
    pub spawn: bool,
    pub expected: bool,
}

/// A self-config write of a Tools `integrations` value (Lean `JobGuardRow`):
/// `writer` is `"model"` or `"operator"`, `stored_integrations` is null when
/// the stored Tools document has no `integrations`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanJobGuardCase {
    pub name: String,
    pub writer: String,
    pub selected_before: bool,
    pub native_authorized: bool,
    pub native_source_authorized: bool,
    pub add_schema_tool: bool,
    #[serde(deserialize_with = "super::required_nullable")]
    pub stored_integrations: Option<serde_json::Value>,
    pub candidate_integrations: serde_json::Value,
    pub accepted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanKillCase {
    pub observation: String,
    pub action: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanKillObservationCase {
    pub caused: Option<LeanCausedFacts>,
    pub observation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanLifecycleHookAccess {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookAdmitCase {
    pub name: String,
    pub authorized: bool,
    pub cancelled: bool,
    pub current: LeanLifecycleHookBinding,
    pub prepared: Option<LeanLifecycleHookPrepared>,
    pub result: LeanLifecycleHookAdmitResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookAdmitResult {
    Ok { payload: String },
    Error { stale: LeanLifecycleHookStale },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookAgedOwedRecord {
    pub record: LeanLifecycleHookOwedRecord,
    pub age: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookBinding {
    pub actor: String,
    pub point: String,
    pub subject: String,
    pub inputs: String,
    pub configuration: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanLifecycleHookCarrier {
    Context,
    Sourced,
    Transparent,
    Stringified,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookCombineCase {
    pub name: String,
    pub verdicts: Vec<LeanLifecycleHookVerdict>,
    pub combined: LeanLifecycleHookVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDeliveryCase {
    pub name: String,
    pub initial: LeanLifecycleHookDeliveryRecord,
    pub trace: Vec<LeanLifecycleHookDeliveryTraceStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDeliveryRecord {
    pub terminal: Option<String>,
    pub owed: Vec<String>,
    pub delivered: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookDeliveryStep {
    Publish { outcome: String, keys: Vec<String>, commit: bool },
    Deliver { key: String, applied: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDeliveryTraceStep {
    pub step: LeanLifecycleHookDeliveryStep,
    pub state: LeanLifecycleHookDeliveryRecord,
    pub pending: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDenial {
    pub access: LeanLifecycleHookAccess,
    pub collection: String,
    pub doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDerivedHalt {
    pub session: String,
    pub at: String,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookEffect {
    Create { collection: String, doc: String, value: String },
    Update { collection: String, doc: String, expected: u64, value: String },
    Delete { collection: String, doc: String, expected: u64 },
    Core { op: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookEffectCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub manifest: LeanLifecycleHookManifest,
    pub effects: Vec<LeanLifecycleHookEffect>,
    pub valid: bool,
    pub committed: Option<LeanLifecycleHookState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGateCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<LeanLifecycleHookVerdict>>,
    pub result: LeanLifecycleHookGateResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookGateResult {
    Ok { verdicts: Vec<LeanLifecycleHookVerdict>, combined: LeanLifecycleHookVerdict },
    Error { refusal: LeanLifecycleHookRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGoalHaltCase {
    pub name: String,
    pub status: Option<String>,
    pub updated: Option<LeanGoalHaltStamp>,
    pub halt: LeanGoalHaltStamp,
    pub verdict: LeanGoalHaltVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGoalResumeCase {
    pub name: String,
    pub goal: LeanGoalResumeGoal,
    pub requests: Vec<LeanGoalResumeRequest>,
    pub next: u64,
    pub restage: LeanGoalResumeDecision,
    pub bind: Option<LeanGoalResumeDecision>,
    pub result: LeanGoalResumeResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGrant {
    pub target: LeanLifecycleHookRef,
    pub relation: String,
    pub subject: LeanLifecycleHookSubject,
}

/// `Hooks.grantsValid` on one result: the declared grantable relations, the
/// transaction's actor, what the event names and what core showed, the
/// result's effects and grants.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGrantCase {
    pub name: String,
    pub declared: Vec<LeanLifecycleHookGrantable>,
    pub actor: String,
    pub named_actors: Vec<String>,
    pub named_docs: Vec<LeanLifecycleHookRef>,
    pub effects: Vec<LeanLifecycleHookEffect>,
    pub grants: Vec<LeanLifecycleHookGrant>,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGrantable {
    pub collection: String,
    pub relation: String,
    pub subject: LeanLifecycleHookSubjectKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookHaltedSession {
    pub session: String,
    pub owner: Option<String>,
    pub latched_at: Option<String>,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookHistoryCase {
    pub name: String,
    pub owns: Vec<String>,
    pub history: Vec<String>,
    pub named: Vec<LeanLifecycleHookRef>,
    pub read: LeanHistoryRead,
    pub permitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookManifest {
    pub name: String,
    pub owns: Vec<String>,
    pub bound: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookMustCommitCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<String>>,
    pub transition: String,
    pub transition_refused: bool,
    pub read_failed: Vec<String>,
    pub committed: Option<LeanLifecycleHookMustCommitted>,
    pub owed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookMustCommitted {
    pub state: LeanLifecycleHookState,
    pub dropped: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedAttempt {
    Applied { writes: Vec<LeanLifecycleHookOwedWrite> },
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedDeliveryCases {
    pub traces: Vec<LeanLifecycleHookOwedTraceCase>,
    pub derive: Vec<LeanLifecycleHookOwedDeriveCase>,
    pub prune: Vec<LeanLifecycleHookOwedPruneCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedDeriveCase {
    pub name: String,
    pub bound: usize,
    pub sessions: Vec<LeanLifecycleHookHaltedSession>,
    pub derived: Vec<LeanLifecycleHookDerivedHalt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedKey {
    pub point: String,
    pub plugin: String,
    pub subject: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedKind {
    Staging,
    OnBehalf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedPruneCase {
    pub name: String,
    pub retention: u64,
    pub records: Vec<LeanLifecycleHookAgedOwedRecord>,
    pub kept: Vec<LeanLifecycleHookAgedOwedRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedRecord {
    pub key: LeanLifecycleHookOwedKey,
    pub kind: LeanLifecycleHookOwedKind,
    pub actor: Option<String>,
    pub runner: String,
    pub attempts: u64,
    pub last_error: Option<String>,
    pub settled: Option<LeanLifecycleHookOwedSettlement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedSettlement {
    Delivered,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedStep {
    Owe {
        point: String,
        actor: Option<String>,
        owner: Option<String>,
        signable: Vec<String>,
        keys: Vec<LeanLifecycleHookOwedKey>,
        commit: bool,
    },
    Redrive {
        key: LeanLifecycleHookOwedKey,
        authorized: bool,
        current: bool,
        attempt: LeanLifecycleHookOwedAttempt,
        refused: Vec<LeanLifecycleHookOwedWrite>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedTraceCase {
    pub name: String,
    pub trace: Vec<LeanLifecycleHookOwedTraceStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedTraceStep {
    pub step: LeanLifecycleHookOwedStep,
    pub runner: Option<String>,
    pub records: Vec<LeanLifecycleHookOwedRecord>,
    pub performed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedWrite {
    pub subject: String,
    pub doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookPartition {
    Ok { partition: Vec<LeanLifecycleHookScopeKey> },
    Error { refusal: LeanLifecycleHookRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookPrepared {
    pub binding: LeanLifecycleHookBinding,
    pub verdict: LeanLifecycleHookPreparedVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookPreparedVerdict {
    Allow { payload: String },
    Deny { reason: String },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookQueueScopeCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<Vec<LeanLifecycleHookScopeAnswer>>>,
    pub asked: Vec<String>,
    pub result: LeanLifecycleHookQueueScopeResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookQueueScopeResult {
    Ok { scopes: Vec<LeanLifecycleHookPartition> },
    Error { refusal: LeanLifecycleHookRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookRef {
    pub collection: String,
    pub doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookRefusal {
    Core,
    Refused { plugin: String, reason: String },
    Invalid { plugin: String },
    Conflict { plugin: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookRerunCase {
    pub name: String,
    pub path: Vec<LeanLifecycleHookCarrier>,
    pub rerun: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookRow {
    pub collection: String,
    pub doc: String,
    pub revision: u64,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookScopeAnswer {
    Unanswered,
    Keyed { key: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookScopeKey {
    pub plugin: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookStageCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<String>>,
    pub result: LeanLifecycleHookStageResult,
    pub settled: LeanLifecycleHookState,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookStageResult {
    Ok { state: LeanLifecycleHookState, values: Vec<String> },
    Error { refusal: LeanLifecycleHookRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookStale {
    Unauthorized,
    Cancelled,
    Missing,
    Stale,
    Denied { reason: String },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookState {
    pub core: Vec<String>,
    pub rows: Vec<LeanLifecycleHookRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookSubject {
    Actor { did: String },
    Document { collection: String, doc: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookSubjectKind {
    Actor,
    Document { resource: String, collection: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookSuccessor {
    pub request_id: String,
    pub request_doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookSupersedeCase {
    pub name: String,
    pub selector: LeanSupersedeSelector,
    pub rows: Vec<LeanSupersedeRow>,
    pub parents: Vec<LeanSupersedeParent>,
    pub stale: Vec<LeanSupersedeStale>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookVerdict {
    Allow,
    LeaveQueued,
    Retire { reason: String, successor: LeanLifecycleHookSuccessor },
    Reject { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookWorld {
    pub rows: Vec<LeanLifecycleHookRow>,
    pub core: Vec<String>,
    pub denied: Vec<LeanLifecycleHookDenial>,
    pub failing_ops: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanLifecycleTransitionCase {
    pub stopped_before_running: bool,
    pub name: String,
    pub domain: String,
    pub from: String,
    pub to: String,
    pub classification: String,
    pub action: Option<String>,
    pub boundary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLiveOverlayCase {
    pub name: String,
    #[serde(rename = "liveOutputAvailable")]
    pub live_output_available: bool,
    #[serde(rename = "hasDurableOwner")]
    pub has_durable_owner: bool,
    #[serde(rename = "precedingToolCalls")]
    pub preceding_tool_calls: u64,
    #[serde(rename = "turnTerminal")]
    pub turn_terminal: bool,
    #[serde(rename = "turnLabel")]
    pub turn_label: String,
    #[serde(rename = "hasContent")]
    pub has_content: bool,
    #[serde(rename = "hasReasoning")]
    pub has_reasoning: bool,
    #[serde(rename = "expectOverlay")]
    pub expect_overlay: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLogicalOutputObligationCase {
    pub authenticated_child: bool,
    pub automated_root: bool,
    pub count_valid: bool,
    pub expected_count: Option<u64>,
    pub expected_decision: String,
    pub minimum: u64,
    pub name: String,
    pub request_scope: bool,
    pub writes: Vec<LeanLogicalOutputObligationCaseWritesItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLogicalOutputObligationCaseWritesItem {
    pub call_doc: u64,
    pub completed: bool,
    pub request_doc: u64,
    pub tool: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanLspActionCase {
    pub name: String,
    pub lsp: bool,
    pub file_rank: u8,
    pub action: String,
    pub mutates: bool,
    pub source: String,
    pub advertised: bool,
    pub action_authorized: bool,
    pub apply_authorized: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxClaimedRecipientCase {
    pub agent_doc: Option<String>,
    pub committed: bool,
    pub present: bool,
    pub request_doc: Option<String>,
    pub session_doc: Option<String>,
    pub signer: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxHandoffCase {
    pub variant: String,
    pub kind: String,
    pub action: String,
    pub content: String,
    pub request_id: u64,
    pub producer_request_id: u64,
    pub tool_request_id: u64,
    pub producer_node_did: String,
    pub producer_requester_did: String,
    pub question_node_did: String,
    pub question_requester_did: String,
    pub session_id: String,
    pub request_doc_id: String,
    pub mailbox_doc_id: String,
    pub reply_request_doc_id: String,
    pub reply_source_doc_id: String,
    pub reply_session_id: String,
    pub reply_bound_session_id: Option<String>,
    pub handoff_accepted: bool,
    pub stored_open_receipt: bool,
    pub linked_reply_accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxNotificationCase {
    pub mode: String,
    pub open_exists: bool,
    pub outcome: String,
    pub same_content: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxQuestionCase {
    pub accepted: bool,
    pub dismissible: bool,
    pub free_text: bool,
    pub option_count: u64,
    pub optional: bool,
    pub presentations: Vec<LeanMailboxQuestionCasePresentationsItem>,
    pub receipt_versions: Vec<LeanMailboxQuestionCaseReceiptVersionsItem>,
    pub runtimes: Vec<LeanMailboxQuestionCaseRuntimesItem>,
    pub selected: bool,
    pub skip: bool,
    pub text: bool,
    pub valid: bool,
    pub version: u64,
    pub waits: Vec<LeanMailboxQuestionCaseWaitsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxQuestionCasePresentationsItem {
    pub has_prompt: bool,
    pub multi_select: bool,
    pub nonempty_prompt: bool,
    pub order: Option<Vec<u64>>,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxQuestionCaseReceiptVersionsItem {
    pub current: bool,
    pub version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxQuestionCaseRuntimesItem {
    pub reply_version: Option<u64>,
    pub supported: bool,
    pub version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxQuestionCaseWaitsItem {
    pub completed: bool,
    pub open: bool,
    pub same_parent: bool,
    pub waiting: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxReplyCase {
    pub accepted: bool,
    pub agent_did: String,
    pub authenticated: bool,
    pub bound_session_id: Option<String>,
    pub deadline_valid: bool,
    pub handling: String,
    pub interactive: bool,
    pub mailbox_doc_id: String,
    pub node_did: String,
    pub post_resolved_doc_id: Option<String>,
    pub post_status: Option<String>,
    pub question_current: bool,
    pub recipient_authorized: bool,
    pub request_doc_id: String,
    pub requester_did: String,
    pub resolved_doc_id: String,
    pub session_id: String,
    pub source_doc_id: String,
    pub status: String,
    pub target_agent_did: String,
    pub target_node_did: String,
    pub variant: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxSweepCase {
    pub actions: Vec<LeanMailboxSweepCaseActionsItem>,
    pub budget: u64,
    pub case: String,
    pub node_did: String,
    pub page_size: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxSweepCaseActionsItem {
    pub cursor: Option<u64>,
    pub next_cursor: Option<u64>,
    pub post_rows: Vec<LeanMailboxSweepCaseActionsItemPostRowsItem>,
    pub rows: Vec<LeanMailboxSweepCaseActionsItemRowsItem>,
    pub selected: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxSweepCaseActionsItemPostRowsItem {
    pub doc_id: u64,
    pub due: bool,
    pub node_did: String,
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMailboxSweepCaseActionsItemRowsItem {
    pub doc_id: u64,
    pub due: bool,
    pub node_did: String,
    pub open: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanManagedExecLivenessCase {
    pub name: String,
    pub trigger: String,
    pub pre_exec_state: String,
    pub pre_tool_state: String,
    pub expected_exec_state: String,
    pub expected_tool_state: String,
    pub max_steps: usize,
    pub kill_signal_required: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanManagedExecToolBoundaryCase {
    pub name: String,
    pub tool_name: String,
    pub work_class: String,
    pub boundary: String,
    pub kill_scope: String,
    pub timeout_requires_kill: bool,
    pub cancel_requires_kill: bool,
    pub descendants_in_termination_scope: bool,
    pub capture_drain_bounded: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanManagedToolIdentityCase {
    pub claimed: Option<LeanClaimedDid>,
    pub node_key: String,
    pub creation_nonce: Option<String>,
    pub allowed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanMcpHealthCase {
    pub name: String,
    pub start_state: String,
    pub start_count: usize,
    pub event: String,
    pub threshold_k: usize,
    pub next_state: Option<String>,
    pub next_count: Option<usize>,
    pub rust_projection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanMediaKind {
    Image,
    Audio,
    Video,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanMessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanNamedTransition {
    pub name: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeClaimLifetimeCase {
    pub events: Vec<String>,
    pub initial: LeanNativeClaimLifetimeCaseInitial,
    pub name: String,
    pub observations: Vec<LeanNativeClaimLifetimeCaseObservationsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeClaimLifetimeCaseInitial {
    pub access_owner_alive: bool,
    pub caller: String,
    pub claim_attached: bool,
    pub claim_held: bool,
    pub cleanup_pending: bool,
    pub lane: String,
    pub lifecycle_guard_held: bool,
    pub mutation_gate_held: bool,
    pub native_outstanding: u64,
    pub next_renewal_started: bool,
    pub poisoned: bool,
    pub renewal_attempt_returned: bool,
    pub retry_started: bool,
    pub rollback_returned: bool,
    pub transaction_owner_alive: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeClaimLifetimeCaseObservationsItem {
    pub access_owner_alive: bool,
    pub caller: String,
    pub claim_attached: bool,
    pub claim_held: bool,
    pub cleanup_pending: bool,
    pub lane: String,
    pub lifecycle_guard_held: bool,
    pub mutation_gate_held: bool,
    pub native_outstanding: u64,
    pub next_renewal_started: bool,
    pub poisoned: bool,
    pub renewal_attempt_returned: bool,
    pub retry_started: bool,
    pub rollback_returned: bool,
    pub transaction_owner_alive: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeCompletionCase {
    pub events: Vec<String>,
    pub initial: LeanNativeCompletionCaseInitial,
    pub name: String,
    pub observations: Vec<LeanNativeCompletionCaseObservationsItem>,
    pub periodic_renewal: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeCompletionCaseInitial {
    pub cleanup_required: bool,
    pub gate_held: bool,
    pub native_outstanding: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeCompletionCaseObservationsItem {
    pub caller: String,
    pub gate_held: bool,
    pub native_outstanding: u64,
    pub next_renewal_started: bool,
    pub poisoned: bool,
    pub registry_abandoned: bool,
    pub renewal_attempt_returned: bool,
    pub retry_started: bool,
    pub rollback_returned: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanNativeFilesystemBoundaryCase {
    pub name: String,
    pub tool_name: String,
    pub work_class: String,
    pub boundary: String,
    pub inner_poll_blocks: bool,
    pub request_deadline_ms: usize,
    pub blocker_ms: usize,
    pub expected_terminal: String,
    pub expected_failure_class: Option<String>,
    pub queue_advances_before_blocker_returns: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeRouteCase {
    pub admitted: bool,
    pub delivered: LeanNativeRouteCaseDelivered,
    pub emit_outcome: bool,
    pub input: LeanNativeRouteCaseInput,
    pub name: String,
    pub stamped: LeanNativeRouteCaseStamped,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeRouteCaseDelivered {
    pub attempt: Option<u64>,
    pub handoff_id: Option<String>,
    pub reply_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeRouteCaseInput {
    pub attempt: Option<u64>,
    pub handoff_id: Option<String>,
    pub reply_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanNativeRouteCaseStamped {
    pub attempt: Option<u64>,
    pub handoff_id: Option<String>,
    pub reply_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanOptimizationCases {
    pub params: LeanOptimizationParams,
    pub decisions: Vec<LeanDecisionRow>,
    pub gates: Vec<LeanGateRow>,
    pub costs: Vec<LeanCostRow>,
    pub seed_coverage: Vec<LeanSeedCoverageRow>,
    pub seed_rendering: Vec<LeanSeedRenderingRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanOptimizationParams {
    pub min_pairs: u64,
    pub max_not_evidence_bp: u64,
    pub max_asymmetry_bp: u64,
    pub case_tolerance_bp: u64,
    pub alpha_ppm: u64,
    pub max_rounds: u32,
    pub max_token_increase_bp: u64,
    pub alpha_effective_ppm: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanOrdinaryClientMutationCase {
    pub intent: bool,
    pub mutation: bool,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanOutcome {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanOutputOwner {
    pub current_request: Option<(u64, u64)>,
    pub live_tools: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanOutputTarget {
    pub coordinate: LeanCanonicalCoordinate,
    pub writer: LeanCanonicalWriter,
    pub message_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackEvalSubjectCase {
    pub name: String,
    pub selected: Option<LeanPackIdentityBinding>,
    pub slot: String,
    pub expected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackIdentityBinding {
    pub slot: String,
    pub creation_nonce: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackOperatorWireCase {
    pub allowed: bool,
    pub home: bool,
    pub operator: bool,
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackPlacementReaderCase {
    pub declared: Vec<String>,
    pub readers: Vec<LeanPackPlacementReaderCaseReadersItem>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackPlacementReaderCaseReadersItem {
    pub actor: String,
    pub read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPairingReconcileCase {
    pub name: String,
    pub action: String,
    pub before: LeanPairingReconcileSnapshot,
    pub after: LeanPairingReconcileSnapshot,
}

#[derive(Debug, Deserialize)]
pub struct LeanPairingReconcileShutdownBoundaryCase {
    pub name: String,
    pub supervisor: String,
    pub work_class: String,
    pub boundary: String,
    pub per_admin_call_timeout_ms: usize,
    pub cancellation_observed_inside_sweep: bool,
    pub current_admin_future_dropped: bool,
    pub remaining_peers_skipped: bool,
    pub shutdown_join_bounded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPairingReconcileSnapshot {
    pub connected: bool,
    pub desired_replicators: Vec<String>,
    pub actual_replicators: Vec<String>,
    pub converged: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanPairingReconcileSweepRetryBoundaryCase {
    pub name: String,
    pub supervisor: String,
    pub work_class: String,
    pub boundary: String,
    pub failure_scope: String,
    pub failure_terminal: bool,
    pub retry_trigger: String,
    pub cancellation_prioritized: bool,
    pub convergence_retried: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanPairingReconcileSweepSchedulingCase {
    pub name: String,
    pub supervisor: String,
    pub work_class: String,
    pub boundary: String,
    pub max_concurrent_peer_preparations: usize,
    pub peer_preparation_bounded: bool,
    pub topology_mutation_serialized: bool,
    pub stale_peer_blocks_ready_peer: bool,
    pub every_peer_result_accounted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPayloadKind {
    Text,
    Reasoning,
    Signature,
    ReasoningStop,
    Summary,
    Encrypted,
    Redacted,
    Arguments,
    ToolOutput,
    Media,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPayloadLengths {
    pub stored_payload_bytes: u64,
    pub presented_payload_bytes: u64,
}

/// Payload presentation before provider-specific projection. These byte counts
/// exclude metadata, escaping, media URLs and request structure; they are neither
/// serialized request sizes nor token estimates. The existing provider_input
/// owner must still project and estimate the complete request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPayloadPresentationCase {
    pub name: String,
    pub segments: Vec<LeanCanonicalSegment>,
    pub message: LeanCanonicalMessage<LeanPayloadSpec>,
    /// `None` when the message cannot be reconstructed.
    pub expected: Option<LeanPayloadLengths>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPayloadRef {
    pub close_id: u64,
    pub stream: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPayloadSpec {
    pub reference: LeanPayloadRef,
    pub presentation: LeanPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPendingUserTurnCase {
    pub name: String,
    #[serde(rename = "hasDurableUserOwner")]
    pub has_durable_user_owner: bool,
    #[serde(rename = "retiredByAuthor")]
    pub retired_by_author: bool,
    #[serde(rename = "unrelatedUserTurns")]
    pub unrelated_user_turns: u64,
    #[serde(rename = "expectPendingTurn")]
    pub expect_pending_turn: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPermissionChange {
    Revoke,
    Grant,
}

#[derive(Debug, Deserialize)]
pub struct LeanPersistenceFailurePolicyCase {
    pub name: String,
    pub policy: String,
    pub action: String,
    pub pre_persistence: String,
    pub post_persistence: String,
    pub post_storage_observation: String,
    pub hook_decision: String,
    pub records_failure: bool,
    pub records_success: bool,
    pub external_durability_claimed: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCase {
    pub directory: bool,
    pub initial: LeanPluginApprovalCaseInitial,
    pub scoped: Option<LeanPluginApprovalCaseScoped>,
    pub steps: Vec<LeanPluginApprovalCaseStepsItem>,
    pub write: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseInitial {
    pub decision: Option<serde_json::Value>,
    pub folder_grant: Option<bool>,
    pub path_grant: Option<bool>,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScoped {
    pub available: Vec<LeanPluginApprovalCaseScopedAvailableItem>,
    pub deliveries: Vec<LeanPluginApprovalCaseScopedDeliveriesItem>,
    pub expected: LeanPluginApprovalCaseScopedExpected,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScopedAvailableItem {
    pub revision: u64,
    pub workspace: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScopedDeliveriesItem {
    pub available: Vec<LeanPluginApprovalCaseScopedDeliveriesItemAvailableItem>,
    pub expected: Option<bool>,
    pub received: LeanPluginApprovalCaseScopedDeliveriesItemReceived,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScopedDeliveriesItemAvailableItem {
    pub revision: u64,
    pub workspace: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScopedDeliveriesItemReceived {
    pub action: u64,
    pub scope: LeanPluginApprovalCaseScopedDeliveriesItemReceivedScope,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScopedDeliveriesItemReceivedScope {
    pub revision: u64,
    pub workspace: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScopedExpected {
    pub action: u64,
    pub scope: LeanPluginApprovalCaseScopedExpectedScope,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseScopedExpectedScope {
    pub revision: u64,
    pub workspace: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseStepsItem {
    pub accepted: bool,
    pub answer: Option<String>,
    pub expected: LeanPluginApprovalCaseStepsItemExpected,
    pub id: Option<String>,
    pub kind: String,
    pub now: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginApprovalCaseStepsItemExpected {
    pub decision: Option<bool>,
    pub folder_grant: Option<bool>,
    pub path_grant: Option<bool>,
    pub present: bool,
}

/// `Conformance.PluginResources.routeCasesJson`: `PluginCalls.childAdmissible`
/// over `routeOf staticHookOwned jobBound`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginCallsRouteCase {
    pub static_hook_owned: bool,
    pub job_bound: bool,
    pub selected: bool,
    pub admitted: bool,
    pub expected: bool,
}

/// `Conformance.PluginResources.dataCasesJson`: `PluginData.admitted` and
/// `refusal` over one write, `queryAdmitted`, `substituteCallerDid` and
/// `insertNote`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginDataCases {
    pub writes: Vec<LeanPluginDataWriteCase>,
    pub queries: Vec<LeanPluginDataQueryCase>,
    pub filters: Vec<LeanPluginDataFilterCase>,
    pub notes: Vec<LeanPluginDataNoteCase>,
}

/// A filter maps each field to a literal or the `$caller.did` placeholder.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginDataFilterCase {
    pub caller_did: String,
    pub filter: BTreeMap<String, String>,
    pub expected: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginDataInsertOutcome {
    Inserted,
    Unchanged,
    AbortStep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginDataInvocation {
    ToolCall,
    JobStep,
    Callback,
    Ux,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginDataNoteCase {
    #[serde(deserialize_with = "required_nullable")]
    pub existing: Option<String>,
    pub new: String,
    pub on_conflict: LeanPluginDataOnConflict,
    pub expected: LeanPluginDataInsertOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginDataOnConflict {
    Ignore,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginDataOperation {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginDataQueryCase {
    pub limit: u32,
    pub in_own_sdl: bool,
    pub runtime_or_protected: bool,
    pub query_granted: bool,
    pub expected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPluginDataRefusal {
    StampUnavailable {
        stamp: LeanPluginDataStamp,
        invocation: LeanPluginDataInvocation,
    },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginDataStamp {
    CallerDid,
    SessionId,
    RequestDocId,
    ToolCallDocId,
    JobRowDocId,
    ChainRootDocId,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginDataWriteCase {
    pub op: LeanPluginDataOperation,
    pub in_own_sdl: bool,
    pub runtime_or_protected: bool,
    pub actor_is_invoker: bool,
    pub op_granted: bool,
    pub stamped_supplied_by_guest: bool,
    pub invocation: LeanPluginDataInvocation,
    /// The manifest's `data.stamped` map: field to host fact.
    #[serde(deserialize_with = "ordered_pairs")]
    pub declared_stamps: Vec<(String, LeanPluginDataStamp)>,
    pub collection_fields: Vec<String>,
    pub write: LeanPluginDataWriteFacts,
    /// `applicableStamps`: the declared stamps of the written collection's fields.
    #[serde(deserialize_with = "ordered_pairs")]
    pub applicable: Vec<(String, LeanPluginDataStamp)>,
    pub admitted: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub refusal: Option<LeanPluginDataRefusal>,
}

/// `ToolPolicy.ApplicationWrite.Observation`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginDataWriteFacts {
    pub granted: bool,
    pub application: bool,
    pub blocked: bool,
    pub credential: bool,
    pub bounded: bool,
    pub targets: u32,
    pub limit: u32,
    pub preview: bool,
    pub digest_matches: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobAddressExpected {
    pub split: Option<LeanPluginJobAddressSplit>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobAddressSplit {
    pub session_id: String,
    pub tool_call_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobAdmissibleExpected {
    pub admissible: bool,
}

/// `Conformance.PluginJob.admissionCases`: one `admitStep` row, its context,
/// job and step output as the model holds them, and the verdict
/// `settleStep` computes from them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobAdmissionCase {
    pub name: String,
    pub context: LeanPluginJobContext,
    pub job: LeanPluginJobRow,
    pub output: LeanPluginJobOutput,
    pub expected: LeanPluginJobAdmissionExpected,
    pub wire_trace: Option<LeanPluginJobWireRecoveryTrace>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobAdmissionExpected {
    pub admitted: bool,
    pub dynamic_code_disabled: bool,
    pub code: Option<String>,
    pub job: Option<LeanPluginJobRow>,
    pub row_state: String,
    pub kill: Vec<LeanPluginJobKill>,
}

/// `Conformance.PluginJob.adoptionCasesJson`: the step-0 adoption rows and the
/// pure pin, handle and address rows, tagged by `kind`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPluginJobAdoptionCase {
    Step0 {
        name: String,
        tool_call: Box<LeanPluginJobToolCall>,
        context: Box<LeanPluginJobContext>,
        job: Box<LeanPluginJobRow>,
        output: Box<LeanPluginJobOutput>,
        expected: Box<LeanPluginJobStep0Expected>,
    },
    HostPin {
        name: String,
        claimed: Box<LeanPluginJobPin>,
        facts: Box<LeanPluginJobHostFacts>,
        expected: Box<LeanPluginJobHostPinExpected>,
    },
    Handle {
        name: String,
        taken: Vec<String>,
        handle: String,
        expected: Box<LeanPluginJobHandleExpected>,
    },
    Address {
        name: String,
        address: String,
        expected: Box<LeanPluginJobAddressExpected>,
    },
}

/// `Conformance.PluginJob.authorityCasesJson`, tagged by `kind`: provenance
/// reads, install records, call origin, pack-shipped job
/// blocks, `Policy.valid` and `wireTargetsFrom`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPluginJobAuthorityCase {
    Provenance {
        name: String,
        pack_kind: String,
        record: Box<LeanPluginJobRecord>,
        expected: Box<LeanPluginJobProvenanceExpected>,
    },
    Install {
        name: String,
        prior: Box<LeanPluginJobRecord>,
        pack_kind: String,
        explicit: bool,
        via: String,
        expected: Box<LeanPluginJobInstallExpected>,
    },
    Installer {
        name: String,
        via: String,
        pack_authority: bool,
        expected: Box<LeanPluginJobProvenanceExpected>,
    },
    Origin {
        name: String,
        call: LeanPluginJobCall,
        expected: Box<LeanPluginJobOriginExpected>,
    },
    PackJobBlock {
        name: String,
        via: String,
        closure: Vec<String>,
        block: Box<LeanPluginJobPackBlock>,
        expected: Box<LeanPluginJobAdmissibleExpected>,
    },
    PolicyValid {
        name: String,
        agent_targets: Vec<String>,
        policy: Box<LeanPluginJobPolicy>,
        expected: Box<LeanPluginJobValidExpected>,
    },
    ConsentedDynamic {
        name: String,
        consent: bool,
        mode: String,
        expected: LeanPluginJobModeExpected,
    },
    PublishedRead {
        name: String,
        publication_in_progress: bool,
        expected: LeanPluginJobPublishedReadExpected,
    },
    WireTargetsFrom {
        name: String,
        pack: String,
        pack_targets: Vec<LeanPluginJobTarget>,
        policy: Box<LeanPluginJobPolicy>,
        job_targets: Vec<LeanPluginJobTarget>,
        caller_targets: Vec<LeanPluginJobTarget>,
        expected: Box<LeanPluginJobWireTargetsExpected>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCall {
    pub argv: Vec<String>,
    pub args: Option<String>,
    pub script: Option<String>,
    pub agent_budget: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCeiling {
    pub targets: Vec<String>,
    pub max_live: u32,
    pub max_spawns: u32,
    pub max_messages_per_slot: u32,
    pub result_bytes: u64,
    pub max_wall_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobChild {
    pub retry_key: String,
    pub slot: String,
    pub attempt: u32,
    pub target: LeanPluginJobTarget,
    pub state: String,
    pub messages: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCase {
    pub cause: String,
    pub committed: LeanPluginJobCommitKillCaseCommitted,
    pub initial: LeanPluginJobCommitKillCaseInitial,
    pub kills: Vec<LeanPluginJobCommitKillCaseKillsItem>,
    pub output: LeanPluginJobCommitKillCaseOutput,
    pub row_state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseCommitted {
    pub call: LeanPluginJobCommitKillCaseCommittedCall,
    pub caller_cwd: String,
    pub caller_hop: u64,
    pub caller_session: String,
    pub children: Vec<LeanPluginJobCommitKillCaseCommittedChildrenItem>,
    pub consecutive_failures: u64,
    pub handle: String,
    pub observed_terminal: u64,
    pub pin: LeanPluginJobCommitKillCaseCommittedPin,
    pub resume: Option<serde_json::Value>,
    pub root: String,
    pub root_call: Option<serde_json::Value>,
    pub row_doc_id: String,
    pub spent: u64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseCommittedCall {
    pub agent_budget: Option<serde_json::Value>,
    pub args: Option<serde_json::Value>,
    pub argv: Vec<String>,
    pub script: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseCommittedChildrenItem {
    pub attempt: u64,
    pub messages: u64,
    pub retry_key: String,
    pub slot: String,
    pub state: String,
    pub target: LeanPluginJobCommitKillCaseCommittedChildrenItemTarget,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseCommittedChildrenItemTarget {
    pub agent: String,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseCommittedPin {
    pub adopted_at: u64,
    pub agent_budget: u64,
    pub args_digest: String,
    pub ceiling_at_pin: LeanPluginJobCommitKillCaseCommittedPinCeilingAtPin,
    pub chain_root: String,
    pub cwd: String,
    pub engine_digest: String,
    pub handle: String,
    pub origin: String,
    pub plugin_ref: String,
    pub replayed_from: Option<serde_json::Value>,
    pub resume_of: Option<serde_json::Value>,
    pub script_id: String,
    pub source_digest: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseCommittedPinCeilingAtPin {
    pub max_live: u64,
    pub max_messages_per_slot: u64,
    pub max_spawns: u64,
    pub max_wall_secs: u64,
    pub result_bytes: u64,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseInitial {
    pub call: LeanPluginJobCommitKillCaseInitialCall,
    pub caller_cwd: String,
    pub caller_hop: u64,
    pub caller_session: String,
    pub children: Vec<serde_json::Value>,
    pub consecutive_failures: u64,
    pub handle: String,
    pub observed_terminal: u64,
    pub pin: LeanPluginJobCommitKillCaseInitialPin,
    pub resume: Option<serde_json::Value>,
    pub root: String,
    pub root_call: Option<serde_json::Value>,
    pub row_doc_id: String,
    pub spent: u64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseInitialCall {
    pub agent_budget: Option<serde_json::Value>,
    pub args: Option<serde_json::Value>,
    pub argv: Vec<String>,
    pub script: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseInitialPin {
    pub adopted_at: u64,
    pub agent_budget: u64,
    pub args_digest: String,
    pub ceiling_at_pin: LeanPluginJobCommitKillCaseInitialPinCeilingAtPin,
    pub chain_root: String,
    pub cwd: String,
    pub engine_digest: String,
    pub handle: String,
    pub origin: String,
    pub plugin_ref: String,
    pub replayed_from: Option<serde_json::Value>,
    pub resume_of: Option<serde_json::Value>,
    pub script_id: String,
    pub source_digest: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseInitialPinCeilingAtPin {
    pub max_live: u64,
    pub max_messages_per_slot: u64,
    pub max_spawns: u64,
    pub max_wall_secs: u64,
    pub result_bytes: u64,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseKillsItem {
    pub action: String,
    pub slot: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseOutput {
    pub extras: LeanPluginJobCommitKillCaseOutputExtras,
    pub interrupts: Vec<serde_json::Value>,
    pub kind: String,
    pub messages: Vec<serde_json::Value>,
    pub pin: Option<serde_json::Value>,
    pub resume: Option<serde_json::Value>,
    pub spawns: Vec<LeanPluginJobCommitKillCaseOutputSpawnsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseOutputExtras {
    pub progress: bool,
    pub scratch: Vec<serde_json::Value>,
    pub writes: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseOutputSpawnsItem {
    pub attempt: u64,
    pub cwd: Option<serde_json::Value>,
    pub slot: String,
    pub target: LeanPluginJobCommitKillCaseOutputSpawnsItemTarget,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobCommitKillCaseOutputSpawnsItemTarget {
    pub agent: String,
    pub node: String,
}

/// The host facts one step reads, tabulated over the documents and working
/// directories the case names.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobContext {
    pub live: LeanPluginJobCeiling,
    pub bindings: Vec<LeanPluginJobWireBinding>,
    pub available: bool,
    pub caller_targets: LeanPluginJobScope,
    pub job_targets: LeanPluginJobScope,
    pub policy: LeanPluginJobPolicy,
    pub engine_digest: String,
    pub engine_provenance_operator: bool,
    pub max_hop: Option<u32>,
    pub target_hops: Vec<LeanPluginJobTargetHop>,
    pub live_background: u32,
    pub started_by_job: bool,
    pub cwd_allowed: Vec<String>,
    pub within: Vec<LeanPluginJobWithin>,
    #[serde(default)]
    pub script_digest: Option<String>,
    pub adoption_facts: LeanPluginJobHostFacts,
    pub caller_scope: LeanPluginJobProcessScope,
    pub control_targets: Vec<LeanPluginJobControlTarget>,
    pub resume_targets: Vec<LeanPluginJobResumeTarget>,
    pub resume_facts: LeanPluginJobHostFacts,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControl {
    pub row_doc_id: String,
    pub control: String,
}

/// `Conformance.PluginJob.controlCasesJson`, tagged by `kind`: kills, resumes,
/// `job_calls.result` reads and offers, and step failures.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPluginJobControlCase {
    Kill {
        name: String,
        job: Box<LeanPluginJobRow>,
        observations: Vec<LeanPluginJobObservation>,
        control: String,
        expected: Box<LeanPluginJobKillExpected>,
    },
    Resume {
        name: String,
        caller: Box<LeanPluginJobProcessScope>,
        owner: Box<LeanPluginJobProcessScope>,
        same_plugin: bool,
        root: Box<LeanPluginJobRow>,
        root_call_args: Box<LeanPluginJobCall>,
        prior: Box<LeanPluginJobRow>,
        job: Box<LeanPluginJobRow>,
        call_args: Box<LeanPluginJobCall>,
        guest_pin: Box<LeanPluginJobGuestPin>,
        facts: Box<LeanPluginJobHostFacts>,
        resume: Box<LeanPluginJobResume>,
        expected: Box<LeanPluginJobResumeExpected>,
    },
    ResultRead {
        name: String,
        caller: Box<LeanPluginJobProcessScope>,
        owner: Box<LeanPluginJobProcessScope>,
        same_plugin: bool,
        terminal: bool,
        expected: Box<LeanPluginJobResultReadExpected>,
    },
    ResultOffer {
        name: String,
        invocation: String,
        expected: Box<LeanPluginJobResultOfferExpected>,
    },
    StepFailure {
        name: String,
        job: Box<LeanPluginJobRow>,
        consecutive_failures: u32,
        observations: Vec<LeanPluginJobObservation>,
        expected: Box<LeanPluginJobStepFailureExpected>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControlEcho {
    pub kind: String,
    pub payload: Option<LeanPluginJobResume>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControlOwner {
    pub owner: LeanPluginJobProcessScope,
    pub same_plugin: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControlTarget {
    pub row_doc_id: String,
    pub target: Option<LeanPluginJobControlOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobDecodedKey {
    pub root: String,
    pub slot: String,
    pub attempt: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobDurableRead {
    pub session_stopped: bool,
    pub now_millis: u64,
    pub available: bool,
    pub progress_revision: u64,
    pub step_in_flight: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobExtras {
    pub writes: u32,
    pub scratch: Vec<String>,
    pub progress: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobGuestPin {
    pub script_id: String,
    pub source_digest: String,
    pub handle: String,
    pub agent_budget: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobHandleExpected {
    pub handle: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobHostFacts {
    pub engine_digest: String,
    pub plugin_ref: String,
    pub cwd: String,
    pub args_digest: String,
    pub chain_root: String,
    pub origin: String,
    pub adopted_at: u64,
    pub resume_of: Option<String>,
    pub replayed_from: Option<String>,
    pub ceiling_at_pin: LeanPluginJobCeiling,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobHostPinExpected {
    pub pin: LeanPluginJobPin,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobInstallExpected {
    pub record: LeanPluginJobRecord,
    pub provenance: String,
}

/// `Conformance.PluginJob.keyCasesJson`: one spawn key, the message numbers
/// probed, and the strings the Lean encoders and `decodeRetryKey?` produce.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobKeyCase {
    pub name: String,
    pub root: String,
    pub slot: String,
    pub attempt: u32,
    pub messages: Vec<u32>,
    pub expected: LeanPluginJobKeyExpected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobKeyExpected {
    pub retry_key: String,
    pub session_id: String,
    pub message_keys: Vec<LeanPluginJobMessageKey>,
    pub decoded: Option<LeanPluginJobDecodedKey>,
    pub message_keys_decode: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobKill {
    pub retry_key: String,
    pub action: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobKillExpected {
    pub row_state: String,
    pub won: bool,
    pub job_state: String,
    pub kill: Vec<LeanPluginJobKill>,
}

/// `Conformance.PluginJob.lineageCasesJson`: request rows and the
/// `DurableLineage.openingRequest` each `(cause, session)` query selects.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobLineageCase {
    pub name: String,
    pub rows: Vec<LeanPluginJobRequestRow>,
    pub queries: Vec<LeanPluginJobLineageQuery>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobLineageQuery {
    pub cause_request_doc_id: String,
    pub session_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_opening_doc_id: Option<String>,
    pub listed_start: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobMessageKey {
    pub n: u32,
    pub key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobModeExpected {
    pub mode: String,
}

/// What a kill observes about the request one child caused
/// (`Recovery.KillObservation`'s contract spelling).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobObservation {
    pub retry_key: String,
    pub observation: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobOpeningCase {
    pub commits: bool,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobOriginExpected {
    pub origin: String,
}

/// A step output, tagged by `kind`; the sibling riders follow the variant.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPluginJobOutput {
    Result {
        extras: LeanPluginJobExtras,
        control: Vec<LeanPluginJobControl>,
        pin: Option<LeanPluginJobGuestPin>,
        resume: Option<LeanPluginJobResume>,
    },
    Fail { extras: LeanPluginJobExtras },
    Suspend {
        suspend: String,
        reason: Option<String>,
        extras: LeanPluginJobExtras,
        pin: Option<LeanPluginJobGuestPin>,
        resume: Option<LeanPluginJobResume>,
    },
    Effects {
        pin: Option<LeanPluginJobGuestPin>,
        resume: Option<LeanPluginJobResume>,
        spawns: Vec<LeanPluginJobSpawn>,
        messages: Vec<String>,
        interrupts: Vec<String>,
        extras: LeanPluginJobExtras,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobPackBlock {
    pub targets_from: Vec<String>,
    pub trusted: Vec<String>,
    pub dynamic: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobPin {
    pub script_id: String,
    pub source_digest: String,
    pub engine_digest: String,
    pub plugin_ref: String,
    pub handle: String,
    pub cwd: String,
    pub args_digest: String,
    pub chain_root: String,
    pub origin: String,
    pub agent_budget: u32,
    pub adopted_at: u64,
    pub resume_of: Option<String>,
    pub replayed_from: Option<String>,
    pub ceiling_at_pin: LeanPluginJobCeiling,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobPolicy {
    pub mode: String,
    pub dynamic_targets: Vec<String>,
    pub trusted_packs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobProcessScope {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobProvenanceExpected {
    pub provenance: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobPublishedReadExpected {
    pub deferred: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobReceiptDecisions {
    pub terminal_decision: String,
    pub late_decision: String,
}

/// `kind` is `absent`, `legacy` or `written`; only `written` carries `via`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRecord {
    pub kind: String,
    #[serde(default)]
    pub via: Option<String>,
}

/// `Conformance.PluginJob.recoveryCasesJson`: plugin-job rows, the registered
/// sweep's staleness verdict and measures, and the row after one tick
/// (`pluginJobRecover ∘ pluginJobRecover`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRecoveryCase {
    pub name: String,
    pub sweep_id: String,
    pub rust_function: String,
    pub row: LeanPluginJobRecoveryRow,
    pub stale: bool,
    pub measure_before: u32,
    pub measure_after: u32,
    pub step_commit_allowed: bool,
    pub step_invocation_allowed: bool,
    pub expected_row: LeanPluginJobRecoveryRow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRecoveryRow {
    pub running: bool,
    pub session_stopped: bool,
    pub terminal_children: u32,
    pub observed: u32,
    pub live: u32,
    pub step_new: u32,
    pub deadline_expired: bool,
    pub available: bool,
    pub stepped: bool,
    pub step_in_flight: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRequestRow {
    pub created_at: u64,
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub session_id: String,
    pub is_public: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub caused_by_parent_request_doc_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub opening_request_doc_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResultOfferExpected {
    pub offered: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResultReadExpected {
    pub authorized: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResume {
    pub row_doc_id: String,
    pub engine_current: bool,
    pub answer: Option<String>,
    pub budget: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResumeExpected {
    pub valid: bool,
    pub pin: Option<LeanPluginJobPin>,
    pub control: LeanPluginJobControlEcho,
    pub root_call: LeanPluginJobRootCall,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResumeOwner {
    pub owner: LeanPluginJobProcessScope,
    pub same_plugin: bool,
    pub prior: LeanPluginJobRow,
    pub root: LeanPluginJobRow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResumeTarget {
    pub row_doc_id: String,
    pub is_head: bool,
    pub target: Option<LeanPluginJobResumeOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRootCall {
    pub argv: Vec<String>,
    pub args: Option<String>,
    pub agent_budget: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRow {
    pub root: String,
    pub row_doc_id: String,
    pub caller_session: String,
    pub caller_cwd: String,
    pub handle: String,
    pub caller_hop: u32,
    pub call: LeanPluginJobCall,
    pub root_call: Option<LeanPluginJobRootCall>,
    pub resume: Option<LeanPluginJobResume>,
    pub pin: Option<LeanPluginJobPin>,
    pub children: Vec<LeanPluginJobChild>,
    pub spent: u32,
    pub observed_terminal: u32,
    pub consecutive_failures: u32,
    pub state: String,
}

/// `kind` is `none`, `all` or `only`; only `only` carries `targets`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobScope {
    pub kind: String,
    #[serde(default)]
    pub targets: Vec<LeanPluginJobTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobSpawn {
    pub slot: String,
    pub attempt: u32,
    pub target: LeanPluginJobTarget,
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobStep0Expected {
    pub admitted: bool,
    pub code: Option<String>,
    pub job: Option<LeanPluginJobRow>,
    pub tool_call: LeanPluginJobToolCall,
    pub delivery: Option<String>,
    pub continuation_appended: bool,
    pub receipt: Option<LeanPluginJobReceiptDecisions>,
    pub next_resume_valid: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobStepFailureExpected {
    pub row_state: String,
    pub consecutive_failures: u32,
    pub kill: Vec<LeanPluginJobKill>,
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobTarget {
    pub node: String,
    pub agent: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobTargetHop {
    pub target: LeanPluginJobTarget,
    pub hop: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobToolCall {
    pub state: String,
    pub operation: String,
    pub await_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobValidExpected {
    pub valid: bool,
}

/// `Conformance.PluginJob.wakeCasesJson`: the step in flight on a row, a wake
/// or immediate step of a row, and `ExecutionIdentity.wakeOutcome`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWakeCase {
    pub name: String,
    pub running: LeanPluginJobWork,
    pub wake: LeanPluginJobWork,
    pub expected: LeanPluginJobWakeExpected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWakeExpected {
    pub outcome: LeanPluginJobWakeOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginJobWakeOutcome {
    Independent,
    Coalesced,
    Refused,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWireBinding {
    pub wire: String,
    pub target: LeanPluginJobTarget,
}

/// `Conformance.PluginJob.wireCasesJson`: one sample in the protocol's own
/// spelling, which the consumer decodes strictly into the `kind`'s type.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWireCase {
    pub name: String,
    pub kind: LeanPluginJobWireKind,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::enum_variant_names,
    reason = "variants mirror the Lean wire kind names"
)]
pub enum LeanPluginJobWireKind {
    JobStepInput,
    JobStepOutput,
    JobView,
    JobOutcome,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWireRecoveryTrace {
    pub spawn_wires: Vec<String>,
    pub interruption: String,
    pub read: LeanPluginJobDurableRead,
    pub before: LeanPluginJobRecoveryRow,
    pub after: LeanPluginJobRecoveryRow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWireTargetsExpected {
    pub policy: LeanPluginJobPolicy,
    pub job_targets: Vec<LeanPluginJobTarget>,
    pub caller_targets: Vec<LeanPluginJobTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWithin {
    pub base: String,
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWork {
    pub document: String,
    pub agent: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginTargetResolutionCase {
    pub bindings: Vec<LeanPluginTargetResolutionCaseBindingsItem>,
    pub expected_guest_targets: Vec<String>,
    pub expected_permitted: bool,
    pub expected_resolved: Option<Vec<String>>,
    pub ids: Vec<String>,
    pub target: Vec<String>,
    pub wire: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginTargetResolutionCaseBindingsItem {
    pub agent: String,
    pub node: String,
    pub wire: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPolicyGrantBoundCase {
    pub accepted: bool,
    pub candidate_policy: bool,
    pub held_policy: bool,
    pub stored_policy: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPolicyLockoutCases {
    pub protected: Vec<String>,
    pub rows: Vec<LeanPolicyLockoutCasesRowsItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPolicyLockoutCasesRowsItem {
    pub delete: bool,
    pub is_self: bool,
    pub kept: bool,
    pub no_lockout: bool,
    pub permission: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPolicyResourceExposureCase {
    pub categories: Vec<String>,
    pub exposed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPresentation {
    Full,
    Composed { parts: Vec<LeanPresentationPart> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPresentationPart {
    Range { start: u64, end: u64 },
    Literal { bytes: Vec<u8> },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPrivateJournalCheck {
    pub role: String,
    pub action: String,
    pub expected: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPrivateJournalPolicy {
    pub policy_yaml: String,
    pub checks: Vec<LeanPrivateJournalCheck>,
    pub signer_checks: Vec<LeanPrivateJournalSignerCheck>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPrivateJournalSignerCheck {
    pub role: String,
    pub expected: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanProcessStopCase {
    pub name: String,
    pub live_evidence: String,
    pub worker_released: bool,
    pub settle_completed: bool,
    pub settle_cancelled: bool,
    pub settle_timed_out: bool,
    pub settle_failed: bool,
    pub settle_panicked: bool,
    pub retire_before_commit: bool,
    pub retire_after_commit: bool,
    pub preserve_timeout: bool,
    pub preserve_failure: bool,
    pub pending_failure_beats_cancel: bool,
    pub pending_failure_resolves: bool,
    pub retained_completion_notified: bool,
    pub retired_completion_notified: bool,
    pub before: String,
    pub after: String,
    pub may_terminate: bool,
    pub retain_record: bool,
    pub outcome: String,
    pub cancel_reply: String,
    pub durable_state: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyAssistantOrderCase {
    pub name: String,
    pub order_mode: LeanAssistantOrderMode,
    pub input: Vec<LeanPromptAssemblyItem>,
    pub expected: Vec<LeanPromptAssemblyItem>,
    pub expected_twice: Vec<LeanPromptAssemblyItem>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyBudgetCase {
    pub name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub threshold_basis_points: usize,
    pub configured_threshold_budget: usize,
    pub prompt_tokens: usize,
    pub request_tokens: usize,
    pub effective_input_budget: usize,
    pub compaction_input_budget: usize,
    pub effective_output_tokens: usize,
    pub should_compact: bool,
    pub provider_safe: bool,
    pub can_dispatch: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeBodyCase {
    pub name: String,
    pub model: String,
    pub first_user_text: String,
    pub fingerprint_placeholder: String,
    pub prompt_id: String,
    pub attribution: String,
    pub preamble: Option<String>,
    pub rows: Vec<String>,
    pub tools: Vec<String>,
    pub system: Vec<String>,
    pub system_marked: Vec<bool>,
    pub tools_present: bool,
    pub supported_efforts: Option<Vec<String>>,
    pub effort: Option<String>,
    pub selected_effort: Option<String>,
    pub facts: LeanPromptAssemblyClaudeFieldFacts,
    pub request: LeanPromptAssemblyClaudeFieldRequest,
    pub max_tokens: u64,
    pub thinking: LeanPromptAssemblyClaudeFieldThinking,
    pub temperature: Option<u64>,
    pub context_management: bool,
    pub body_effort: Option<String>,
    pub diagnostics: LeanPromptAssemblyClaudeFieldDiagnostics,
    pub per_turn_effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeCheckpointCase {
    pub name: String,
    pub required: Vec<LeanCanonicalCoordinate>,
    pub rows: Vec<LeanClaudeTaggedReplayRow>,
    pub split: usize,
    pub carrier_ids: Vec<String>,
    pub resolutions: Vec<LeanClaudeCheckpointResolution>,
    pub outcome: String,
    pub prefix_rows: Vec<LeanClaudeTaggedReplayRow>,
    pub retained: Vec<LeanClaudeTaggedReplayRow>,
    pub replay: Vec<Vec<LeanClaudeReplayBlock>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldDiagnostics {
    pub present: bool,
    pub previous_message_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldFacts {
    pub adaptive: bool,
    pub always_thinks: bool,
    pub thinking_supported: bool,
    pub rejects_disabled: bool,
    pub accepts_temperature: bool,
    pub effort_capable: bool,
    pub max_effort: bool,
    pub xhigh_effort: bool,
    pub caps_effort_when_disabled: bool,
    pub per_turn_effort: bool,
    pub output_default: u64,
    pub output_upper: u64,
    pub default_effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldRequest {
    pub thinking: LeanPromptAssemblyClaudeFieldThinkingConfig,
    pub max_tokens_override: Option<u64>,
    pub requested_effort: Option<String>,
    pub first_party: bool,
    pub first_party_default_url: bool,
    pub experimental_betas_disabled: bool,
    pub agentic: bool,
    pub fork: bool,
    pub display_updates: bool,
    pub context_management_beta: bool,
    pub per_turn_control_beta: bool,
    pub previous_message_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldThinking {
    pub kind: String,
    pub budget: Option<u64>,
    pub updates: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldThinkingConfig {
    pub kind: String,
    pub mechanical: bool,
    pub budget: Option<u64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeMapCase {
    pub name: String,
    pub surface: Vec<String>,
    pub blocks: Vec<String>,
    pub outcome: String,
    pub ids: Vec<u64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeReplayCase {
    pub name: String,
    pub blocks: Vec<LeanClaudeReplayInputBlock>,
    pub outcome: String,
    pub replay: Vec<LeanClaudeReplayBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeStreamCase {
    pub name: String,
    pub surface: Vec<String>,
    pub events: Vec<String>,
    pub outcome: String,
    pub calls: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeThinkingStreamCase {
    pub name: String,
    pub surface: Vec<String>,
    pub events: Vec<LeanClaudeStreamEvent>,
    pub outcome: String,
    pub steps: Vec<LeanClaudeContentStep>,
    pub content: Vec<LeanClaudeStreamBlock>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyCompactionTriggerCase {
    pub name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub threshold_basis_points: u64,
    pub prior_basis_points: u64,
    pub seeded_observations: Vec<u64>,
    pub observed: Vec<(u64, usize)>,
    pub anchor: Option<LeanContextAnchor>,
    pub invalidate: bool,
    pub raw_input_tokens: usize,
    pub ratio_basis_points: u64,
    pub calibrated_tokens: usize,
    pub anchored_tokens: Option<usize>,
    pub context_tokens: usize,
    pub compaction_input_budget: usize,
    pub admission: Option<LeanReductionAdmission>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyItem {
    pub item: String,
    pub value: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyLayerCase {
    pub name: String,
    pub skill_count: usize,
    pub summary_count: usize,
    pub conversation_len: usize,
    pub slots: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyModeSanitizeCase {
    pub name: String,
    pub order_mode: LeanAssistantOrderMode,
    pub input: Vec<LeanPromptAssemblyRow>,
    pub expected: Vec<LeanPromptAssemblyRow>,
    pub expected_twice: Vec<LeanPromptAssemblyRow>,
    pub association: Vec<LeanPromptAssemblyRow>,
    pub association_indices: Vec<Vec<usize>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyReasoningSuffixCase {
    pub name: String,
    pub issuer_family: String,
    pub issuer_endpoint: String,
    pub wire: LeanReasoningReplayWire,
    pub rows: Vec<LeanClaudeTaggedReplayRow>,
    pub resolutions: Vec<LeanClaudeCheckpointResolution>,
    pub stage: Vec<LeanClaudeTaggedReplayRow>,
    pub assembled: Vec<LeanReplayFlatItem>,
    pub replay: Vec<LeanClaudeTaggedReplayRow>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyRepairCase {
    pub name: String,
    pub input: String,
    pub expected: String,
    pub expected_twice: String,
    pub payload_only: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyReplayPrefixCase {
    pub name: String,
    pub captured: Vec<LeanReplayFlatItem>,
    pub current: Vec<LeanReplayFlatItem>,
    pub ordinary_equal: bool,
    pub reasoning_suffix: bool,
    pub leading_removal: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyReplayShapeCase {
    pub name: String,
    pub source: LeanClaudeTaggedReplayRow,
    pub retained_indices: Vec<usize>,
    pub outcome: String,
    pub shaped: Option<LeanClaudeTaggedReplayRow>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyResponsesEffortCase {
    pub name: String,
    pub endpoint: String,
    #[serde(deserialize_with = "required_nullable")]
    pub advertised: Option<Vec<String>>,
    #[serde(deserialize_with = "required_nullable")]
    pub requested: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub expected: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyResponsesStorageCase {
    pub name: String,
    pub family: String,
    pub wire: String,
    pub endpoint: String,
    #[serde(deserialize_with = "required_nullable")]
    pub store: Option<bool>,
    pub encrypted_include: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyRetentionCase {
    pub name: String,
    pub configured_keep_recent: usize,
    pub effective_input_budget: usize,
    pub fixed_input: usize,
    pub available_input: usize,
    pub retention_target: usize,
    pub summary_max_output: usize,
    pub effective_summary_output: usize,
    pub rolling_summary_input_budget: usize,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyRow {
    pub role: String,
    pub kind: String,
    pub call_ids: Vec<u64>,
    pub content: Vec<LeanPromptAssemblyItem>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblySanitizeCase {
    pub name: String,
    pub input: Vec<LeanPromptAssemblyRow>,
    pub expected: Vec<LeanPromptAssemblyRow>,
    pub expected_twice: Vec<LeanPromptAssemblyRow>,
    pub splits: Vec<LeanPromptAssemblySplit>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyServiceTierCase {
    pub family: String,
    #[serde(deserialize_with = "required_nullable")]
    pub requested: Option<String>,
    pub allowed: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub wire: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblySplit {
    pub index: usize,
    pub expected: Vec<LeanPromptAssemblyRow>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyTurnBudgetCase {
    pub name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub threshold_basis_points: usize,
    pub configured_threshold_budget: usize,
    pub effective_input_budget: usize,
    pub compaction_input_budget: usize,
    pub turn_input_tokens: Vec<usize>,
    pub turn_output_tokens: Vec<usize>,
    pub turn_should_compact: Vec<bool>,
    pub turn_can_dispatch: Vec<bool>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LeanProtectedReplayCompactionCase {
    pub name: String,
    pub message_count: usize,
    pub raw_index: usize,
    pub required: Vec<LeanCanonicalCoordinate>,
    pub rows: Vec<LeanClaudeTaggedReplayRow>,
    pub ceiling_error: String,
    #[serde(deserialize_with = "required_nullable")]
    pub ceiling: Option<usize>,
    #[serde(deserialize_with = "required_nullable")]
    pub selected_split: Option<usize>,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanProviderEofCase {
    pub saw_explicit_final: bool,
    pub expected_failure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanQueueDeadlineConformanceCase {
    pub name: String,
    pub group: String,
    pub action: String,
    pub session_id: usize,
    pub legal: bool,
    pub pre_active_request_id: Option<usize>,
    pub post_active_request_id: Option<usize>,
    pub pre_pending_request_ids: Vec<usize>,
    pub post_pending_request_ids: Vec<usize>,
    pub claimed_request_id: Option<usize>,
    pub blocked_by_active: bool,
    pub superseded_request_ids: Vec<usize>,
    pub queue_key: Option<String>,
    pub post_coalesced_pending_count: usize,
    pub automated_drained_request_ids: Vec<usize>,
    pub preserved_user_pending_request_ids: Vec<usize>,
    pub preserved_foreign_requester_request_ids: Vec<usize>,
    pub preserved_foreign_owner_request_ids: Vec<usize>,
    pub post_terminal_request_ids: Vec<usize>,
    pub pre_request_deadline: Option<usize>,
    pub synthesized_claim_deadline: Option<usize>,
    pub post_deadline: Option<usize>,
    pub explicit_deadline_preserved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueueManagementCase {
    pub name: String,
    pub before: LeanQueueManagementState,
    pub operation: LeanQueueManagementOperation,
    pub expected: Option<LeanQueueManagementState>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanQueueManagementOperation {
    Intake { active: LeanFoldQueueEntry, admitted: Vec<u64>, safe_boundary: bool },
    Replace {
        caller: Option<u64>,
        expected: Vec<u64>,
        offset: usize,
        count: usize,
        replacements: Vec<LeanFoldQueueEntry>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueueManagementState {
    pub active: Option<u64>,
    pub pending: Vec<LeanFoldQueueEntry>,
    pub folding: Vec<LeanFoldQueueEntry>,
    pub terminal: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanQueuedSteeringAction {
    Enqueue,
    ClaimWithoutBegin,
    ClaimAndBegin,
    LatchInterrupt,
    InterruptBeforeClaim,
    AdmissionReject,
    FailBeforeStream,
    DedupLose,
    Expire,
    InterruptClaimed,
    InterruptProcessing,
    Fail,
    Finish,
    BindWorkspace,
    Claim,
    BeginInference,
    ContinueProcessing,
    Publish,
    PrepareFails,
    Capture,
    Send,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueuedSteeringCandidate {
    pub closing: LeanCanonicalSegment,
    pub message: super::canonical_output::LeanCanonicalMessage<LeanPayloadSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueuedSteeringCapture {
    #[serde(rename = "nodeDid")]
    pub node_did: u64,
    #[serde(rename = "sessionId")]
    pub session_id: u64,
    #[serde(rename = "requestDocId")]
    pub request_doc_id: u64,
    #[serde(rename = "turnIndex")]
    pub turn_index: u64,
    pub attempt: u64,
    #[serde(rename = "bodyToken")]
    pub body_token: u64,
    #[serde(rename = "priorBodyToken")]
    pub prior_body_token: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueuedSteeringEntry {
    #[serde(rename = "requestId")]
    pub request_id: u64,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    pub source: String,
    pub policy: String,
    #[serde(rename = "queueKey")]
    pub queue_key: Option<u64>,
    #[serde(rename = "queuedAfter")]
    pub queued_after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueuedSteeringGuardCase {
    pub name: String,
    #[serde(rename = "prefixAdmitted")]
    pub prefix_admitted: bool,
    pub admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueuedSteeringTraceCase {
    pub name: String,
    pub actions: Vec<LeanQueuedSteeringAction>,
    #[serde(rename = "requestId")]
    pub request_id: u64,
    #[serde(rename = "requestDocId")]
    pub request_doc_id: u64,
    #[serde(rename = "contentToken")]
    pub content_token: u64,
    pub entry: LeanQueuedSteeringEntry,
    #[serde(rename = "interruptAt")]
    pub interrupt_at: Option<u64>,
    #[serde(rename = "preparedCandidate")]
    pub prepared_candidate: Option<LeanQueuedSteeringCandidate>,
    pub capture: LeanQueuedSteeringCapture,
    #[serde(rename = "lifecycleState")]
    pub lifecycle_state: String,
    #[serde(rename = "acceptedInput")]
    pub accepted_input: bool,
    #[serde(rename = "queueActive")]
    pub queue_active: Option<u64>,
    #[serde(rename = "admissionVisible")]
    pub admission_visible: bool,
    #[serde(rename = "canonicalAuthoredCount")]
    pub canonical_authored_count: u64,
    #[serde(rename = "providerSendPermitted")]
    pub provider_send_permitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "witness", deny_unknown_fields)]
pub enum LeanR4cBackgroundWorkCase {
    #[serde(rename = "r4c.read_tool_output.canonical_source_reconstruction")]
    ReadToolOutputCanonicalSourceReconstruction {
        tool_call_id: String,
        canonical_source: String,
        cases: Vec<LeanToolOutputProjectionCase>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanR6BackgroundingCase {
    pub name: String,
    pub group: String,
    pub action: String,
    pub legal: bool,
    pub pre_live_count: usize,
    pub max_backgrounded: usize,
    pub await_mode: String,
    pub terminal_state: String,
    pub result: Option<String>,
    pub reason: Option<String>,
    pub error_code: Option<String>,
    pub queue_source: Option<String>,
    pub queue_key: Option<String>,
    #[serde(default)]
    pub retry_count: Option<usize>,
    #[serde(default)]
    pub max_retries: Option<usize>,
    #[serde(default)]
    pub post_retry_count: Option<usize>,
    #[serde(default)]
    pub redrive_source_request_id: Option<u64>,
    #[serde(default)]
    pub pre_depth: Option<u64>,
    #[serde(default)]
    pub post_depth: Option<u64>,
    #[serde(default)]
    pub pre_parent_request_id: Option<u64>,
    #[serde(default)]
    pub post_parent_request_id: Option<u64>,
    #[serde(default)]
    pub pre_execution_deadline: Option<u64>,
    #[serde(default)]
    pub post_execution_deadline: Option<u64>,
    #[serde(default)]
    pub retry_delay_seconds: Option<usize>,
    #[serde(default)]
    pub is_latest: Option<bool>,
    #[serde(default)]
    pub goal_status: Option<String>,
    #[serde(default)]
    pub notification_persisted: Option<bool>,
    #[serde(default)]
    pub wake_created: Option<bool>,
    #[serde(default)]
    pub redrive_allowed: Option<bool>,
    #[serde(default)]
    pub operation: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanReadinessPublicationCase {
    pub states: Vec<u64>,
    pub publishes: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanReasoningAuditCase {
    pub name: String,
    pub input: LeanCanonicalOutputObservation,
    pub expected: LeanReasoningAuditResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanReasoningAuditResult {
    Ok { streams: Vec<LeanCanonicalStream> },
    Loading,
    Conflicted,
    Invalid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanReasoningDeltaCase {
    pub name: String,
    pub before: Vec<LeanReasoningDeltaPart>,
    pub id: Option<usize>,
    pub fragment: Vec<u8>,
    pub expected: Vec<LeanReasoningDeltaPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanReasoningDeltaPart {
    Reasoning { id: Option<usize>, signature: Option<usize>, bytes: Vec<u8> },
    Text { bytes: Vec<u8> },
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeanReasoningReplayWire {
    ClaudeMessages,
    Responses,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanReasoningSignatureCase {
    pub name: String,
    pub records: Vec<LeanCanonicalSegment>,
    pub payload: LeanPayloadSpec,
    pub signature: Option<String>,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRebuiltProjectionInput {
    pub checkpoint: usize,
    pub retained_suffix: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanReconstructedMessage {
    pub role: LeanMessageRole,
    pub native_id: Option<String>,
    pub native_request_id: Option<String>,
    pub blocks: Vec<LeanMessageBlock<Vec<u8>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanRecoverySweepCase {
    pub name: String,
    pub sweep_id: String,
    pub collection: String,
    pub rust_function: String,
    pub cadence: String,
    pub implementation_status: String,
    pub pre_state: String,
    pub terminal_state: String,
    pub measure_before: usize,
    pub measure_after: usize,
    pub deadline_expired: Option<bool>,
    pub parent_live: Option<bool>,
    pub parent_interrupted: Option<bool>,
    pub parent_terminal: Option<bool>,
    pub task_hook_observation: Option<String>,
    pub execution_registered: Option<bool>,
    pub process_outcome: Option<String>,
    pub owner_task_deleted: Option<bool>,
    pub owner_session_stopped: Option<bool>,
    pub recovery_cause: Option<String>,
    pub notification_reason: Option<String>,
    pub deadline_audit_ref: String,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct LeanReductionAdmission {
    pub input_tokens: usize,
    pub input_budget: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRehydratedBudget {
    pub limit: u64,
    pub used: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryCase {
    pub confirmed: bool,
    pub create_succeeded: bool,
    pub existing: Option<LeanRemoteAgentDeliveryCaseExisting>,
    pub intent: LeanRemoteAgentDeliveryCaseIntent,
    pub name: String,
    pub persisted: bool,
    pub response_verified: bool,
    pub result: String,
    pub working_grant: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryCaseExisting {
    pub intent: LeanRemoteAgentDeliveryCaseExistingIntent,
    pub owner: String,
    pub request_doc_id: String,
    pub working_agent: String,
    pub working_grant: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryCaseExistingIntent {
    pub digest: String,
    pub key: String,
    pub sender: String,
    pub target: String,
    pub target_agent_doc_id: String,
    pub target_session_doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryCaseIntent {
    pub digest: String,
    pub key: String,
    pub sender: String,
    pub target: String,
    pub target_agent_doc_id: String,
    pub target_session_doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryClaimCase {
    pub atomic_requester_authorization: bool,
    pub claim: bool,
    pub commit_agent_invoke: bool,
    pub commit_node_permission: bool,
    pub commit_session_permission: bool,
    pub decision: LeanCrossActorDecision,
    pub document_visible: bool,
    pub name: String,
    pub native_committed: bool,
    pub observation: LeanRemoteAgentDeliveryClaimCaseObservation,
    pub record: LeanRemoteAgentDeliveryClaimCaseRecord,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryClaimCaseObservation {
    pub agent_document: String,
    pub agent_invoke: bool,
    pub authenticated_author: String,
    pub document: String,
    pub executing_agent: String,
    pub node_permission_enforced: bool,
    pub owner: String,
    pub requester_node_permission: bool,
    pub session: String,
    pub session_permission: bool,
    pub signature_valid: bool,
    pub supported: bool,
    pub targets_visible: bool,
    pub transient_failure: bool,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryClaimCaseRecord {
    pub intent: LeanRemoteAgentDeliveryClaimCaseRecordIntent,
    pub owner: String,
    pub request_doc_id: String,
    pub working_agent: String,
    pub working_grant: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRemoteAgentDeliveryClaimCaseRecordIntent {
    pub digest: String,
    pub key: String,
    pub sender: String,
    pub target: String,
    pub target_agent_doc_id: String,
    pub target_session_doc_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanRenderedCaptureCase {
    pub name: String,
    pub node_did: u64,
    pub session_id: u64,
    pub request_id: u64,
    pub turn_index: usize,
    pub attempt: u32,
    pub request: u64,
    pub prior_binding: Option<u64>,
    pub capture_outcome: String,
    pub capture_durable: bool,
    pub post_stage: String,
    pub send_permitted: bool,
    pub provider_requests_observed: usize,
    pub durable_after: Option<u64>,
    pub final_stage: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanRenderedCaptureKeyCase {
    pub name: String,
    pub left_node_did: u64,
    pub left_session_id: u64,
    pub left_request_id: u64,
    pub left_turn_index: usize,
    pub left_attempt: u32,
    pub right_node_did: u64,
    pub right_session_id: u64,
    pub right_request_id: u64,
    pub right_turn_index: usize,
    pub right_attempt: u32,
    pub same_fact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRenderedKind {
    Text,
    Reasoning,
    Summary,
    Arguments,
    ToolOutput,
    Media,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRepairedProjectionCase {
    pub name: String,
    pub input: LeanRepairedProjectionInput,
    pub expected: LeanRepairedProjectionObservation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRepairedProjectionInput {
    pub source: Vec<u64>,
    pub project_succeeds: bool,
    pub estimate_tokens: Option<u64>,
    pub effective_input_budget: u64,
    pub context_window: u64,
    pub configured_max_output_tokens: u64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRepairedProjectionObservation {
    pub result: String,
    pub output_tokens: Option<u64>,
    pub projected_source: Option<Vec<u64>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanReplayFlatItem {
    pub kind: LeanReplayFlatKind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeanReplayFlatKind {
    Ordinary,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanRequestExecutionAction {
    Claim {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        duration: u64,
        explicit_deadline: u64,
    },
    Begin { boundary: LeanRequestExecutionBoundary, generation: u64 },
    AppendOutput { boundary: LeanRequestExecutionBoundary, generation: u64 },
    Renew {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        expected_deadline: u64,
    },
    AuthorizeProducerDecision {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        decision: LeanRequestExecutionProducerDecision,
    },
    SocketTraffic { generation: u64 },
    NoOp { generation: u64 },
    AdvanceTime { now: u64 },
    Drop { boundary: LeanRequestExecutionBoundary, generation: u64 },
    RecoverExpired {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        duration: u64,
        explicit_deadline: u64,
    },
    RecoverDropped {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        duration: u64,
        explicit_deadline: u64,
    },
    Finalize {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    PolicyRevoke {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    RecoverExpiredAndFail {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
    },
    RecoverExpiredTerminal {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    RecoverDroppedAndFail {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionBoundary {
    MutationWriteGate,
    ValidatedTransaction,
    ObservingReplica,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionLease {
    pub status: LeanRequestExecutionLeaseStatus,
    pub generation: Option<u64>,
    pub duration: Option<u64>,
    pub explicit_deadline: Option<u64>,
    pub outcome: Option<LeanRequestExecutionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionLeaseCase {
    pub name: String,
    pub pre: LeanRequestExecutionWorld,
    pub action: LeanRequestExecutionAction,
    pub expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionLeaseStatus {
    Vacant,
    Active,
    Recoverable,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionLeaseTraceCase {
    pub name: String,
    pub pre: LeanRequestExecutionWorld,
    pub actions: Vec<LeanRequestExecutionAction>,
    pub expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionObservedCase {
    pub name: String,
    pub pre: LeanRequestExecutionWorld,
    pub action: LeanRequestExecutionAction,
    pub intervening: Vec<LeanRequestExecutionAction>,
    pub expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionOutcome {
    Completed,
    Failed,
    Interrupted,
    Dead,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionProducerDecision {
    CloseOrRetract,
    AcceptAndPublish,
    Dispatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionWorld {
    pub request: LeanRequestExecutionRequestPhase,
    pub lease: LeanRequestExecutionLease,
    pub used_generations: Vec<u64>,
    pub now: u64,
    pub effective_expiry: u64,
    pub continuation_required: bool,
    pub token_charge_required: bool,
    pub continuation_count: u64,
    pub token_charge_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanRequestProgressCase {
    pub name: String,
    #[serde(rename = "lifecycleState")]
    pub lifecycle_state: String,
    pub label: String,
    pub animated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum LeanRequestPurpose {
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "title-audit")]
    TitleAudit,
    #[serde(rename = "access-review")]
    AccessReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanRestartDispositionCase {
    pub name: String,
    pub rust_function: String,
    pub await_mode: String,
    pub session_message: bool,
    pub kind: LeanRestartRowKind,
    pub parent_observation: String,
    pub deadline_expired: bool,
    pub process_outcome: String,
    pub disposition: String,
    pub cause: Option<String>,
    pub terminal_state: Option<String>,
    pub notification_reason: Option<String>,
    pub queue_source: Option<String>,
    pub queue_key_prefix: Option<String>,
    #[allow(dead_code)]
    pub theorem: String,
}

/// `Recovery.RowKind`: which owner a running tool-call row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum LeanRestartRowKind {
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "sessionMessage")]
    SessionMessage,
    #[serde(rename = "pluginJob")]
    PluginJob,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRetryEntryCase {
    pub authored: u64,
    pub context: Option<u64>,
    pub expected: Vec<u64>,
    pub history: Vec<u64>,
    pub publish: bool,
    pub resume: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRetryFrontierCase {
    pub background: bool,
    pub expected: Option<bool>,
    pub name: String,
    pub published: bool,
    pub running: bool,
    pub same_requester: bool,
    pub same_session: bool,
    pub scoped_terminal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRetrySelectionCase {
    pub name: String,
    pub parent_published: bool,
    pub selected: Vec<u64>,
    pub resume: bool,
    pub answered: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRootAdmissionCase {
    pub name: String,
    pub operation: String,
    pub authored: String,
    pub blank: bool,
    pub stored: bool,
    pub observation: String,
    pub configured: bool,
    pub ceiling: Option<LeanCanonicalPath>,
    pub enabled: Vec<LeanCanonicalPath>,
    pub published: Vec<LeanCanonicalPath>,
    pub candidate: Option<LeanCanonicalPath>,
    pub expected: bool,
    pub effective: Option<LeanCanonicalPath>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRoutingAffinityCase {
    pub codex: bool,
    pub expected_session: Option<String>,
    pub expected_token: Option<String>,
    pub main: bool,
    pub name: String,
    pub responses: Vec<Option<String>>,
    pub session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRuntimeConfigurationAdmissionCase {
    pub active: u64,
    pub deferred_at: u64,
    pub expected: String,
    pub name: String,
    pub retries: bool,
    pub visible: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanRuntimeReconcileCase {
    pub requested_agent: Option<usize>,
    pub pre_default_agent: usize,
    pub pre_session_agent: Option<usize>,
    pub pre_runnable: Vec<usize>,
    pub name: String,
    pub action: String,
    pub legal: bool,
    pub pre_phase: String,
    pub post_phase: String,
    pub pre_active_generation: usize,
    pub post_active_generation: usize,
    pub pre_router_generation: usize,
    pub post_router_generation: usize,
    pub pre_ready_generation_count: usize,
    pub post_ready_generation_count: usize,
    pub pre_live_generation_count: usize,
    pub post_live_generation_count: usize,
    pub pre_in_flight_count: usize,
    pub post_in_flight_count: usize,
    pub tracked_request_id: usize,
    pub tracked_session_id: usize,
    pub tracked_request_generation: usize,
    pub tracked_request_session: usize,
    pub tracked_request_agent: usize,
    pub tracked_session_agent: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRuntimeSessionSelectionCase {
    pub expected: String,
    pub name: String,
    pub requested: u64,
    pub visible: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSchemaArgumentRepairCase {
    pub expected: Option<serde_json::Value>,
    pub input: Option<serde_json::Value>,
    pub name: String,
    pub native_expected: LeanSchemaArgumentRepairCaseNativeExpected,
    pub native_input: LeanSchemaArgumentRepairCaseNativeInput,
    pub native_paths: Vec<String>,
    pub native_schema: LeanSchemaArgumentRepairCaseNativeSchema,
    pub parses: Vec<LeanSchemaArgumentRepairCaseParsesItem>,
    pub paths: Vec<String>,
    pub schema: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSchemaArgumentRepairCaseNativeExpected {
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSchemaArgumentRepairCaseNativeInput {
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSchemaArgumentRepairCaseNativeSchema {
    #[serde(rename = "additionalProperties")]
    pub additional_properties: bool,
    pub properties: LeanSchemaArgumentRepairCaseNativeSchemaProperties,
    pub required: Vec<String>,
    pub r#type: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSchemaArgumentRepairCaseNativeSchemaProperties {
    pub value: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSchemaArgumentRepairCaseParsesItem {
    pub accepted: bool,
    pub parsed: Option<serde_json::Value>,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSchemaPublicationCase {
    pub accepted: bool,
    pub artifact: bool,
    pub compatible: bool,
    pub grant: bool,
    pub native_allowed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScopeCollectionRule {
    pub collection: String,
    pub field: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScopeNodeRouteCase {
    pub allowed: bool,
    pub direction: String,
    pub node_did: Option<serde_json::Value>,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScopeTemplateCase {
    pub collections: Vec<String>,
    pub delivery: String,
    pub direction: String,
    pub filters: Vec<LeanScopeTemplateCaseFiltersItem>,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScopeTemplateCaseFiltersItem {
    pub clauses: Vec<LeanScopeTemplateCaseFiltersItemClausesItem>,
    pub collection: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScopeTemplateCaseFiltersItemClausesItem {
    pub field: String,
    pub operator: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScriptedHookResult {
    pub hook_id: String,
    pub result: LeanCommandResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedCase {
    pub split: String,
    pub seed_collections: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedCoverageRow {
    pub name: String,
    pub target_task: Option<String>,
    pub task_enabled: bool,
    pub triggers: Vec<LeanSeedTrigger>,
    pub sources: Vec<LeanSeedSource>,
    pub cases: Vec<LeanSeedCase>,
    pub missing_splits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedRenderingRow {
    pub name: String,
    pub target_task: Option<String>,
    pub task_enabled: bool,
    pub triggers: Vec<LeanSeedTrigger>,
    pub sources: Vec<LeanSeedSource>,
    pub text: String,
    pub seeds: Vec<LeanExactSeed>,
    pub failed_stages: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedSource {
    pub event_source_id: String,
    pub source_collection: String,
    pub event_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedTrigger {
    pub task_id: String,
    pub event_source_id: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigBackend {
    pub backend_id: String,
    pub provider_kind: String,
    pub auth: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigCase {
    pub name: String,
    pub collection: String,
    pub category: String,
    pub guarded: bool,
    pub validates: bool,
    pub held_grants: LeanSelfConfigGrants,
    #[serde(deserialize_with = "super::required_nullable")]
    pub selected_before: Option<Vec<LeanSelfConfigFieldValue>>,
    #[serde(deserialize_with = "super::required_nullable")]
    pub selected_after: Option<Vec<LeanSelfConfigFieldValue>>,
    pub doc: Vec<LeanSelfConfigFieldValue>,
    pub patch: Vec<LeanSelfConfigPatchEntry>,
    pub admissible: bool,
    pub accepted: bool,
    pub result: Vec<LeanSelfConfigFieldValue>,
    pub protected_preserved: bool,
    pub containment_holds: bool,
    pub unchanged_on_reject: bool,
    pub control_kept_after_accept: bool,
    #[serde(default)]
    pub backends: Vec<LeanSelfConfigBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigFieldTable {
    pub collection: String,
    pub unique_field: String,
    pub category: String,
    pub all_fields: Vec<String>,
    pub writable_fields: Vec<String>,
    pub protected_fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigFieldValue {
    pub field: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigGrants {
    pub pack_install: bool,
    pub policy: bool,
    pub pack_authority: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigGroupCase {
    pub name: String,
    pub enabled: bool,
    pub categories: Option<Vec<String>>,
    pub effective: Vec<String>,
    pub may_create_agent: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSelfConfigLookupCase {
    pub name: String,
    pub present: bool,
    pub visible: bool,
    pub same_generation: bool,
    pub native_allows: bool,
    pub expected: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigPatchEntry {
    pub field: String,
    pub action: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSelfConfigResourceReadExposureCase {
    pub categories: Vec<String>,
    pub exposed: bool,
    pub resource: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigScopeCases {
    pub categories: Vec<String>,
    pub default_categories: Vec<String>,
    pub edit: Vec<LeanSelfConfigScopeEditCase>,
    pub groups: Vec<LeanSelfConfigGroupCase>,
    pub scope_of: Vec<LeanSelfConfigScopeOfCase>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigScopeEditCase {
    pub name: String,
    pub categories: Vec<String>,
    pub category: String,
    pub scope: String,
    pub allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigScopeOfCase {
    pub name: String,
    pub invoking_agent: String,
    pub target_agent: String,
    pub shared: bool,
    pub scope: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSelfConfigSelectionCases {
    pub tool_flag_cases: Vec<LeanToolFlagCase>,
    pub job_guard_cases: Vec<LeanJobGuardCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureAccess {
    pub key: LeanSessionClosureDocumentKey,
    pub state: String,
    pub requester: String,
    pub node: String,
    pub session: String,
    pub native_session: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionClosureCase {
    pub name: String,
    pub closure_input: LeanSessionClosureInput,
    pub expected_closure_documents: Option<Vec<LeanSessionClosureDocumentKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureDocumentKey {
    pub collection: String,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureEffect {
    pub id: u64,
    pub parent: Option<u64>,
    pub ordinal: Option<u32>,
    pub spawned: bool,
    pub tool_call_id: String,
    pub terminal: Option<LeanSessionClosurePayloadRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureInput {
    pub request: LeanSessionClosureRequest,
    pub roots: Vec<u64>,
    pub messages: Vec<LeanSessionClosureMessage>,
    pub segments: Vec<LeanSessionClosureSegment>,
    pub access: Vec<LeanSessionClosureAccess>,
    pub denied_headers: Vec<u64>,
    pub denied_segments: Vec<u64>,
    pub bases: Vec<LeanSessionClosureDocumentKey>,
    pub observed_effects: Vec<LeanSessionClosureEffect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureMessage {
    pub id: u64,
    pub session: u64,
    pub request: Option<u64>,
    pub origin: Option<u64>,
    pub tool_delivery: Option<u64>,
    pub refs: Vec<LeanSessionClosurePayloadRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureOwner {
    pub kind: String,
    #[serde(default)]
    pub owner: Option<u64>,
    #[serde(default)]
    pub scope: Option<u64>,
    #[serde(default)]
    pub turn: Option<u64>,
    #[serde(default)]
    pub attempt: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosurePayloadRef {
    pub close_id: u64,
    pub stream: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureRequest {
    pub key: String,
    pub requester: String,
    pub node: String,
    pub session: String,
    pub native_session: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureSegment {
    pub id: u64,
    pub kind: String,
    pub tool: Option<LeanSessionClosureTool>,
    pub request: u64,
    pub source: LeanSessionClosureOwner,
    pub writer: LeanSessionClosureOwner,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureTool {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionCreationCase {
    pub name: String,
    pub signature_valid: bool,
    pub scope_matches: bool,
    pub physical_matches: bool,
    pub cause_matches: bool,
    pub expected: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionCreationSignerCase {
    pub agent_new: bool,
    pub has_requester: bool,
    pub signer: String,
    pub expected: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditAuthority {
    pub signature_valid: bool,
    pub replacements_valid: bool,
    pub command_identity_unique: bool,
    pub session_owned: bool,
    pub editor: LeanSessionInputEditor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditCase {
    pub name: String,
    pub initial: LeanSessionInputEditState,
    pub history: Vec<LeanSessionInputEditStep>,
    pub before: LeanSessionInputEditState,
    pub now: u64,
    pub command: LeanSessionInputEditCommand,
    pub authority: LeanSessionInputEditAuthority,
    pub after: LeanSessionInputEditState,
    pub receipt: Option<LeanSessionInputEditReceipt>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditClientObservationCase {
    pub command_outcome: String,
    pub name: String,
    pub projection_fresh: bool,
    pub projection_refresh_succeeded: bool,
    pub receipt_outcome: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditCommand {
    pub id: u64,
    pub digest: u64,
    pub caller: Option<u64>,
    pub issued_at: u64,
    pub expires_at: u64,
    pub expected: Vec<u64>,
    pub offset: usize,
    pub count: usize,
    pub replacements: Vec<LeanFoldQueueEntry>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditNativeDiscoveryCase {
    pub actor: String,
    pub selected: bool,
    pub visible: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditReceipt {
    pub command_id: u64,
    pub digest: u64,
    pub outcome: String,
    pub request_ids: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditState {
    pub queue: LeanQueueManagementState,
    pub receipts: Vec<LeanSessionInputEditReceipt>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanSessionInputEditStep {
    Apply {
        now: u64,
        authority: LeanSessionInputEditAuthority,
        command: LeanSessionInputEditCommand,
    },
    LoseReceipts,
    OtherSession { session: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanSessionInputEditor {
    Requester,
    WorkingAgent,
    Admin,
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionListCase {
    pub same_requester: bool,
    pub same_node: bool,
    pub caused: bool,
    pub allowed_target: bool,
    pub verified_starter_receipt: bool,
    pub interrupt_allowed: bool,
    pub expected_listed: bool,
    pub expected_can_message: bool,
    pub expected_can_interrupt: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionMessageGoalInputCase {
    pub caller_reference: LeanSessionMessageGoalInputCaseCallerReference,
    pub expected: LeanSessionMessageGoalInputCaseExpected,
    pub name: String,
    pub observation_verified: bool,
    pub observed: Option<LeanSessionMessageGoalInputCaseObserved>,
    pub owner: u64,
    pub session: u64,
    pub session_readable: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionMessageGoalInputCaseCallerReference {
    pub goal: u64,
    pub root: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionMessageGoalInputCaseExpected {
    pub kind: String,
    pub reference: Option<LeanSessionMessageGoalInputCaseExpectedReference>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionMessageGoalInputCaseExpectedReference {
    pub goal: u64,
    pub root: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionMessageGoalInputCaseObserved {
    pub goal: u64,
    pub owner: u64,
    pub root: Option<u64>,
    pub session: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionMessageWriteCase {
    pub name: String,
    pub delivery: String,
    pub caller_hop: u32,
    pub own_hop: u32,
    pub expected_hop: u32,
    pub names_caller_tool_call: bool,
    pub queued_after_active: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanSessionRecoveryCase {
    pub name: String,
    pub action: String,
    pub legal: bool,
    pub pre_latest_state: String,
    pub pre_failed_state: String,
    pub post_latest_state: String,
    pub post_failed_state: String,
    pub post_new_state: String,
    pub pre_latest_admission: String,
    pub post_latest_admission: String,
    pub pre_failed_admission: String,
    pub post_failed_admission: String,
    pub post_new_admission: String,
    pub pre_origin: String,
    pub post_new_origin: String,
    pub failed_id: usize,
    pub new_id: usize,
    pub pre_latest_id: usize,
    pub post_latest_id: usize,
    pub pre_session_id: usize,
    pub post_session_id: usize,
    pub pre_agent_did: usize,
    pub post_agent_did: usize,
    pub pre_request_count: usize,
    pub post_request_count: usize,
    pub pre_retry_count: usize,
    pub post_retry_count: usize,
    pub max_retries: usize,
    pub pre_deadline_exceeded: bool,
    pub post_deadline_exceeded: bool,
    pub pre_failed_is_latest: bool,
    pub post_failed_is_latest: bool,
    pub post_new_is_latest: bool,
    pub pre_request_ids: Vec<usize>,
    pub pre_failed_exists: bool,
    pub pre_latest_exists: bool,
    pub pre_new_request_exists: bool,
    pub old_request_retained: bool,
    pub new_request_inserted: bool,
    pub origin_preserved: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionReplyPreviewCase {
    pub doc: u64,
    pub preview: Option<String>,
    pub reply: Option<String>,
    pub request: u64,
    pub state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionTargetCase {
    pub same_requester: bool,
    pub same_node: bool,
    pub caused: bool,
    pub allowed_target: bool,
    pub expected_allowed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionTurnCase {
    pub name: String,
    pub rows: Vec<LeanSessionTurnRow>,
    pub selected_doc: usize,
    pub page_request_docs: Vec<usize>,
    pub seed_docs: Vec<usize>,
    pub native_completion_premise: bool,
    pub expected_display_turn_doc: usize,
    pub expected_read_docs: Vec<usize>,
    pub expected_turn_doc: usize,
    pub expected_answer_doc: usize,
    pub expected_queued_docs: Vec<usize>,
    pub expected_folded_requests: Vec<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionTurnRow {
    pub doc: usize,
    pub request: usize,
    pub requester: usize,
    pub state: String,
    pub folded_into: Option<usize>,
    pub replaced_by: Option<usize>,
    pub queued_after: Option<usize>,
    pub retry_parent: Option<usize>,
    pub retry_parent_doc: Option<usize>,
    pub completed: bool,
    pub public_input: bool,
    pub background_wake: bool,
    pub authored_input_materialized: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSiblingToolsCase {
    pub owner_matches: bool,
    pub shared_context: bool,
    pub shared_tools: bool,
    pub requested_network: Option<String>,
    pub existing_network: String,
    pub admitted: bool,
    pub result_network: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSnapshotDemotionCase {
    pub default_agent: u64,
    pub demotions: Vec<u64>,
    pub post_default_agent: u64,
    pub post_runnable: Vec<u64>,
    pub post_unavailable: Vec<u64>,
    pub runnable: Vec<u64>,
    pub unavailable: Vec<u64>,
    pub well_formed: bool,
    pub witness: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSpawnedTargetRejection {
    pub failed: bool,
    pub started: bool,
    pub failure_class: Option<String>,
    pub spawned_admitted: bool,
    pub completion_accepted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSpawnedTargetRejectionCase {
    pub name: String,
    pub parent_tool: String,
    pub completion_probe_outcome: String,
    pub expected: LeanSpawnedTargetRejection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanStartupReadinessCase {
    pub witness: String,
    pub lean_theorems: Vec<String>,
    pub budget: u64,
    pub outcomes: Vec<String>,
    pub retired_after: bool,
    pub post_standing: String,
    pub blocks_ready: bool,
    pub requires_restart: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanStateMachineContract {
    pub domain: String,
    pub states: Vec<String>,
    pub state_count: usize,
    pub terminal_states: Vec<String>,
    pub nonterminal_states: Vec<String>,
    pub actions: Vec<String>,
    pub legal_transitions: Vec<LeanTransitionPair>,
    pub illegal_transitions: Vec<LeanTransitionPair>,
    pub named_transitions: Vec<LeanNamedTransition>,
}

#[derive(Debug, Deserialize)]
pub struct LeanStorageObservationRuntimeCase {
    pub name: String,
    pub policy: String,
    pub action: String,
    pub pre_observation: String,
    pub mutation_result: String,
    pub post_observation: String,
    pub post_persistence: String,
    pub hook_result: String,
    pub records_failure: bool,
    pub records_success: bool,
    pub terminal_write_observed: bool,
    pub external_visibility_claimed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanSupersedeAncestry {
    Ordinary,
    Broken { link: LeanSupersedeLink },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanSupersedeLink {
    Unsigned,
    OtherGoal,
    NoParent,
    ParentAbsent,
    ParentOutsideSession,
    ParentUnsigned,
    Diverges,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeParent {
    pub doc: String,
    pub in_session: bool,
    pub signed: bool,
    pub goal: Option<String>,
    pub reference: Option<LeanSupersedeReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeReference {
    pub goal: String,
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeRow {
    pub doc: String,
    pub request: String,
    pub pending: bool,
    pub goal_continuation: bool,
    pub trigger: Option<String>,
    pub reference: Option<LeanSupersedeReference>,
    pub signed: bool,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeSelector {
    pub goal: String,
    pub root: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeStale {
    pub doc: String,
    pub request: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
    pub ancestry: LeanSupersedeAncestry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanSurface {
    AgentFacing,
    OperatorCli,
    OperatorUi,
    Api,
    RuntimeInternal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskCheckpointCase {
    pub actor: String,
    pub cursor: u64,
    pub through: u64,
    pub receipt: bool,
    pub expected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanTaskGoalPublicationCase {
    pub name: String,
    pub node_did: String,
    pub task_id: String,
    pub fire_key: String,
    pub goal_objective: Option<String>,
    pub goal_token_budget: Option<i128>,
    pub declaration_valid: bool,
    pub expected_mode: String,
    pub expected_published: bool,
    pub expected_runnable_request: bool,
    pub expected_durable_goal: bool,
    pub expected_session_id: Option<String>,
    pub expected_request_id: Option<String>,
    pub expected_retry_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHook {
    pub hook_id: String,
    pub phase: String,
    pub command: Vec<String>,
    pub timeout_secs: Option<i64>,
    pub skip_exit_codes: Vec<i64>,
    pub effective_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHookAdmissionCase {
    pub name: String,
    pub hooks: Vec<LeanTaskHook>,
    pub expected_admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHookRecoveryCase {
    pub name: String,
    pub interrupt_requested: bool,
    pub started: bool,
    pub hooks: Vec<LeanTaskHook>,
    pub observed: Vec<LeanHookAttempt>,
    pub script: Vec<LeanScriptedHookResult>,
    pub expected_remaining: Vec<String>,
    pub attempted: Vec<LeanHookAttempt>,
    pub expected_request_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHookRunCase {
    pub name: String,
    pub hooks: Vec<LeanTaskHook>,
    pub script: Vec<LeanScriptedHookResult>,
    pub revoked: Vec<String>,
    pub refused_before_launch: Vec<String>,
    pub agent: String,
    pub expected_agent_ran: bool,
    pub before_attempted: Vec<LeanHookAttempt>,
    pub after_success_attempted: Vec<LeanHookAttempt>,
    pub after_failure_attempted: Vec<LeanHookAttempt>,
    pub finally_attempted: Vec<LeanHookAttempt>,
    pub invocation_trace: Vec<LeanHookInvocation>,
    pub cleanup_errors: Vec<String>,
    pub expected_outcome: LeanTaskOutcome,
    pub expected_final_outcome: LeanTaskOutcome,
    pub expected_request_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanTaskOutcome {
    Parked,
    Success,
    Failure { error: LeanTaskPrimaryError },
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanTaskPrimaryError {
    Hook { hook_id: String },
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTerminalDiagnosticPresentationCase {
    pub name: String,
    pub raw: Vec<u8>,
    pub cause: Vec<u8>,
    pub tail_budget: u64,
    pub expected: LeanTerminalDiagnosticPresentationExpected,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanTerminalDiagnosticPresentationExpected {
    Ok { presentation: LeanPresentation, rendered: Vec<u8> },
    InvalidUtf8,
    UnexpectedError { error: String },
}

/// `Conformance.TerminalDiagnosticReplayContracts`: whether a stored
/// interrupted-call diagnostic replays under a (possibly changed) budget.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTerminalDiagnosticReplayCase {
    pub name: String,
    pub raw: Vec<u8>,
    pub cause: Vec<u8>,
    pub configured_budget: u64,
    pub stored: LeanPresentation,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanTerminalSelection {
    Message { id: u64 },
    NoMessage,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleAdmission {
    pub kind: String,
    pub signer_did: String,
    pub issuer_did: String,
    pub source_request_id: String,
    pub runtime_source_kind: String,
    pub signature_valid: bool,
    pub model_signed_fields_hex: Vec<String>,
    pub model_expected_fields_hex: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleAdmissionJoinCase {
    pub name: String,
    pub admission_case: String,
    pub observation_available: bool,
    pub row: LeanTitleJoinRow,
    pub world: LeanTitleJoinWorld,
    pub generation: u64,
    pub duration: u64,
    pub deadline: u64,
    pub now: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_activation: Option<LeanTitleJoinBinding>,
    pub expected_claimed: bool,
    pub expected_lease_active: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_claimed_binding: Option<LeanTitleJoinBinding>,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_queue_active: Option<String>,
    pub expected_queue_unchanged: bool,
    pub expected_own_physical: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleGoalContinuation {
    pub sequence: u64,
    pub wrapup: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleInitialTitle {
    pub text: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleJoinBinding {
    pub purpose: LeanRequestPurpose,
    pub physical_request: String,
    pub logical_request: String,
    pub parent_physical: String,
    pub parent_logical: String,
    pub node: String,
    pub session: String,
    pub authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleJoinRow {
    pub physical_request: String,
    pub logical_request: String,
    pub model_signed_fields_hex: Vec<String>,
    pub physical_binding_current: bool,
    pub branch_fields_exact: bool,
    pub pending_deadline_absent: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleJoinWorld {
    pub own_physical: String,
    pub model_own_physical: String,
    pub purpose: LeanRequestPurpose,
    pub session: String,
    pub node_did: String,
    pub queue_scope_node: String,
    pub queue_scope_session: String,
    #[serde(deserialize_with = "required_nullable")]
    pub queue_active: Option<String>,
    pub lease_pending: bool,
    pub lease_vacant: bool,
    pub lease_now: u64,
    pub claimed_absent: bool,
    pub terminal_selection_absent: bool,
    pub gate_actor: u64,
    pub gate_phase: String,
    pub gate_independent: bool,
    pub gate_sibling_waiting: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleParentEvidence {
    pub request_id: String,
    pub document_id: String,
    pub node_did: String,
    pub session_id: String,
    pub agent_did: String,
    pub logical_binding_current: bool,
    pub physical_binding_current: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleQueueInput {
    pub delivery: LeanTitleQueueDelivery,
    #[serde(deserialize_with = "required_nullable")]
    pub position: Option<LeanTitleQueuePosition>,
    pub source: String,
    pub policy: String,
    #[serde(deserialize_with = "required_nullable")]
    pub key: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub queued_after_request_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub interrupted_request_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub background_completion_wake_version: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequest {
    pub hop: u32,
    pub request_id: String,
    pub purpose: LeanRequestPurpose,
    pub target_node: String,
    pub requester_did: String,
    pub agent_did: String,
    pub session_id: String,
    pub content: String,
    pub input: LeanTitleRequestInput,
    pub model_input_fields_hex: Vec<String>,
    pub created_at: String,
    pub trigger_config_document_id: String,
    pub model_retry_fields_hex: Vec<String>,
    pub model_trigger_fields_hex: Vec<String>,
    pub model_parent_fields_hex: Vec<String>,
    pub model_workspace_fields_hex: Vec<String>,
    pub model_semantic_fields_hex: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequestAdmissionCase {
    pub name: String,
    pub parent_observed_state: String,
    pub observation_available: bool,
    pub branch_fields_exact: bool,
    pub pending_deadline_absent: bool,
    pub request: LeanTitleRequest,
    pub admission: LeanTitleAdmission,
    #[serde(deserialize_with = "required_nullable")]
    pub runtime_evidence: Option<LeanTitleRuntimeEvidence>,
    pub session_agent: String,
    pub expected_admitted: bool,
    pub expected_claimable: bool,
    pub expected_disposition: String,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_pending_state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequestInput {
    pub selected_skill_ids: Vec<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub cwd: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub initial_title: Option<LeanTitleInitialTitle>,
    #[serde(deserialize_with = "required_nullable")]
    pub queue: Option<LeanTitleQueueInput>,
    #[serde(deserialize_with = "required_nullable")]
    pub goal_continuation: Option<LeanTitleGoalContinuation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequestPurposeWireCase {
    pub name: String,
    #[serde(deserialize_with = "required_nullable")]
    pub wire: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_decoded: Option<LeanRequestPurpose>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRuntimeEvidence {
    pub source_kind: String,
    pub issuer_did: String,
    pub source_request_id: String,
    pub target_node: String,
    pub target_runtime_attestation_valid: bool,
    pub source_binding_current: bool,
    pub source_document_binding_current: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub title_parent: Option<LeanTitleParentEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleSignedPurpose {
    pub physical: u64,
    pub purpose: LeanRequestPurpose,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleTextCases {
    pub cases: Vec<LeanTitleTextCasesCase>,
    pub preamble: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleTextCasesCase {
    pub captures: Vec<LeanTitleTextCasesCaseCapturesItem>,
    pub expected_display: String,
    pub expected_generated: String,
    pub expected_input: String,
    pub expected_prompt: String,
    pub fallback: String,
    pub generated_source: bool,
    pub name: String,
    pub raw: String,
    pub recent: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleTextCasesCaseCapturesItem {
    #[serde(rename = "capturedAt")]
    pub captured_at: String,
    pub content: String,
    pub kind: String,
    pub range: Option<serde_json::Value>,
    pub source: String,
    #[serde(rename = "terminalName")]
    pub terminal_name: Option<serde_json::Value>,
    pub unsaved: bool,
    pub uri: Option<serde_json::Value>,
    pub version: Option<serde_json::Value>,
    #[serde(rename = "workspaceUri")]
    pub workspace_uri: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageAction {
    pub call_id: u64,
    pub usage: LeanTitleUsage,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageBinding {
    pub purpose: LeanRequestPurpose,
    pub physical: u64,
    pub logical: u64,
    pub parent_physical: u64,
    pub parent_logical: u64,
    pub node: u64,
    pub session: u64,
    pub authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageBindingFact {
    pub physical: u64,
    pub binding: LeanTitleUsageBinding,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageCase {
    pub name: String,
    pub parent: LeanTitleUsageParent,
    pub rows: Vec<LeanTitleUsageRow>,
    pub signed_purposes: Vec<LeanTitleSignedPurpose>,
    pub title_bindings: Vec<LeanTitleUsageBindingFact>,
    #[serde(deserialize_with = "required_nullable")]
    pub usage_action: Option<LeanTitleUsageAction>,
    pub expected_before: LeanTitleUsageResult,
    pub expected_after: LeanTitleUsageResult,
    pub budget_before: LeanUsageBudgetResult,
    pub budget_after: LeanUsageBudgetResult,
    pub goal_before: LeanUsageBudgetResult,
    pub goal_after: LeanUsageBudgetResult,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageParent {
    pub physical: u64,
    pub logical: u64,
    pub node: u64,
    pub session: u64,
    pub state: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanTitleUsageResult {
    Ok { normal_public: LeanTitleUsage, parent_inclusive_audit: LeanTitleUsage },
    Error { error: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageRow {
    pub key: u64,
    pub call_id: u64,
    pub request_physical: u64,
    pub request_logical: u64,
    pub backend: String,
    pub state: String,
    #[serde(deserialize_with = "required_nullable")]
    pub terminal_stamp: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub usage: Option<LeanTitleUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolAdmissionHandoffAction {
    pub kind: String,
    pub call: Option<u64>,
    pub agent: Option<String>,
    pub decision: Option<String>,
    pub reason: Option<String>,
    pub now: Option<u64>,
    pub generation: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolAdmissionHandoffCase {
    pub name: String,
    pub published: bool,
    pub request: u64,
    pub session: u64,
    pub call: u64,
    pub generation: u64,
    pub selected_agent: String,
    pub actions: Vec<LeanToolAdmissionHandoffAction>,
    pub expected_phase: String,
    pub expected_replies: u64,
    pub expected_decision: Option<String>,
    pub expected_dispatch: String,
    pub expected_native_dispatch_committed: Option<bool>,
    pub expected_effect: String,
    pub expected_invoked: bool,
    pub expected_author: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanToolFlagCase {
    pub existing: bool,
    pub requested: Option<bool>,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolIdentity {
    pub id: String,
    pub call_id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolOutputPagingCase {
    pub name: String,
    pub first_offset: u64,
    pub retained_len: u64,
    pub total_bytes: u64,
    pub offset: u64,
    pub max_bytes: u64,
    pub start: u64,
    pub slice_len: u64,
    pub next_offset: u64,
    pub first_available_offset: u64,
    pub total_bytes_out: u64,
    pub has_more: bool,
    #[allow(dead_code)]
    pub theorem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolOutputProjectionCase {
    pub name: String,
    pub document: u64,
    pub segments: Vec<LeanCanonicalSegment>,
    pub expected_state: Option<String>,
    pub expected_payload: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolPolicyCase {
    pub name: String,
    pub agent: LeanToolPolicySurfaceView,
    pub ceiling: LeanToolPolicySurfaceView,
    pub runtime: LeanToolPolicySurfaceView,
    pub expected: LeanToolPolicySurfaceView,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolPolicySurfaceView {
    pub file_rank: u8,
    pub goal_tools: bool,
    pub goal_create: bool,
    pub defra_query: bool,
    pub self_config: bool,
    pub schema_management: bool,
    pub p2p_read: bool,
    pub p2p_mutate: bool,
    pub p2p_mutation_allowed: bool,
    pub session_history: bool,
    pub context_budget: bool,
    pub session_messages: bool,
    pub skills: bool,
    pub lsp: bool,
    pub bash_mode: u8,
    pub bash_net: u8,
    pub bash_sandbox: bool,
    pub bash_allowed_kind: String,
    pub bash_allowed_prefixes: Vec<Vec<String>>,
    pub bash_forbidden: Vec<Vec<String>>,
    pub bash_read_only_kind: String,
    pub bash_read_only_keys: Vec<String>,
    pub cli_scope_kind: String,
    pub cli_keys: Vec<String>,
    pub mcp_probe: String,
    pub mcp_scope_kind: String,
    pub mcp_services: Vec<String>,
    pub mcp_permits: bool,
    pub defra_collections_scope_kind: String,
    pub defra_collections_keys: Vec<String>,
    pub self_config_categories_scope_kind: String,
    pub self_config_categories_keys: Vec<String>,
    pub agent_targets_scope_kind: String,
    pub agent_targets_keys: Vec<String>,
    pub background_tools_scope_kind: String,
    pub background_tools_keys: Vec<String>,
    pub write_probe_tool: String,
    pub write_probe_collection: String,
    pub write_scope_kind: String,
    pub write_grants: Vec<LeanToolPolicyWriteGrant>,
    pub write_fields: Vec<String>,
    pub query_probe_tool: String,
    pub query_probe_collection: String,
    pub query_scope_kind: String,
    pub query_grants: Vec<LeanToolPolicyWriteGrant>,
    pub query_fields: Vec<String>,
    pub plugin_tools_kind: String,
    pub plugin_tools_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolPolicyWriteGrant {
    pub tool: String,
    pub collection: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanToolPreflightCase {
    pub name: String,
    pub health: String,
    pub schema_status: String,
    pub decision: String,
    pub failure_class: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanToolRetryCase {
    pub name: String,
    pub operation: String,
    pub idempotency: String,
    pub failure_class: String,
    pub disposition: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCases {
    pub background: Vec<LeanToolTimeoutCasesBackgroundItem>,
    pub cli: Vec<LeanToolTimeoutCasesCliItem>,
    pub foreground: Vec<LeanToolTimeoutCasesForegroundItem>,
    pub lsp: Vec<LeanToolTimeoutCasesLspItem>,
    pub wait: Vec<LeanToolTimeoutCasesWaitItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesBackgroundItem {
    pub background_timeout_secs: Option<i64>,
    pub expected: Option<u64>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesCliItem {
    pub expected: Option<u64>,
    pub host_default: u64,
    pub host_maximum: u64,
    pub name: String,
    pub registration_secs: u64,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesForegroundItem {
    pub expected: Option<LeanToolTimeoutCasesForegroundItemExpected>,
    pub host_default: u64,
    pub host_maximum: u64,
    pub max_timeout_secs: Option<u64>,
    pub name: String,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesForegroundItemExpected {
    pub default: u64,
    pub maximum: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesLspItem {
    pub expected: Option<LeanToolTimeoutCasesLspItemExpected>,
    pub max_timeout_secs: Option<u64>,
    pub name: String,
    pub requests: Vec<LeanToolTimeoutCasesLspItemRequestsItem>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesLspItemExpected {
    pub default: u64,
    pub maximum: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesLspItemRequestsItem {
    pub effective: Option<u64>,
    pub requested: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesWaitItem {
    pub expected: Option<LeanToolTimeoutCasesWaitItemExpected>,
    pub max_timeout_secs: Option<u64>,
    pub name: String,
    pub requests: Vec<LeanToolTimeoutCasesWaitItemRequestsItem>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesWaitItemExpected {
    pub default: u64,
    pub maximum: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolTimeoutCasesWaitItemRequestsItem {
    pub effective: Option<u64>,
    pub requested: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct LeanTranscriptCase {
    pub name: String,
    pub group: String,
    pub action: String,
    pub action_call_ids: Vec<usize>,
    pub action_logical_result_ids: Vec<usize>,
    pub action_payload_hashes: Vec<usize>,
    pub legal: bool,
    pub pre_message_count: usize,
    pub post_message_count: usize,
    pub pre_tool_call_count: usize,
    pub post_tool_call_count: usize,
    pub pre_in_flight_count: usize,
    pub post_in_flight_count: usize,
    pub assistant_sequence: usize,
    pub result_sequence: usize,
    pub logical_result_id: usize,
    pub payload_hash: usize,
    pub expected_pair_closed: bool,
    pub expected_ordered: bool,
    pub expected_duplicate_reused_sequence: bool,
    pub expected_strong_drain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanTransitionPair {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanTriggerDispatchCase {
    pub name: String,
    pub trigger_id: Option<String>,
    pub trigger_kind: String,
    pub concurrency: String,
    pub active_schedule_ids: Vec<String>,
    pub active_event_trigger_ids: Vec<String>,
    pub intent_task_id: String,
    pub selected_task_id: Option<String>,
    pub prior_nonterminal_keys: Vec<String>,
    pub expected_result: String,
    pub expected_skip_reason: Option<String>,
    pub expected_materialize_trigger_id: Option<String>,
    pub expected_materialize_trigger_kind: Option<String>,
    pub expected_request_caused_by_id: Option<String>,
    pub expected_request_caused_by_kind: Option<String>,
    pub expected_execution_origin: Option<String>,
    pub expected_supersede_call_keys: Vec<String>,
    pub superseded_prior_ids: Vec<String>,
    pub target_nonterminal_count_after: Option<usize>,
    pub request_count_before: usize,
    pub request_count_after: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanUsageBudgetResult {
    Ok { used: u64 },
    Error { error: String },
}

#[derive(Debug, Deserialize)]
pub struct LeanVocabularyContract {
    pub domain: String,
    pub values: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWakeInputCase {
    pub name: String,
    pub claimed: Vec<String>,
    pub acknowledged: Vec<String>,
    pub rows: Vec<LeanWakeInputRow>,
    pub notifications: Vec<LeanWakeInputRow>,
    pub current_keys: Vec<String>,
    pub expected_order: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWakeInputRow {
    pub header: String,
    pub notification_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWatcherActorCase {
    pub candidates: Vec<LeanWatcherActorCaseCandidatesItem>,
    pub expected: Vec<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWatcherActorCaseCandidatesItem {
    pub available: bool,
    pub configured: bool,
    pub did: String,
    pub identity_resolved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkerCapacityCase {
    pub name: String,
    pub pre: LeanWorkerCapacityState,
    pub operation: LeanWorkerCapacityOperation,
    pub expected: Option<LeanWorkerCapacityState>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanWorkerCapacityOperation {
    Acquire { ticket: [u64; 2] },
    Release { ticket: [u64; 2] },
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkerCapacityState {
    pub active_limit: usize,
    pub active: Vec<[u64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceAdmissionCase {
    pub admitted: bool,
    pub complete: bool,
    pub occupied: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanWorkspaceBindingCase {
    pub name: String,
    pub workspace_id: String,
    pub workspace_state: String,
    pub workspace_seal_hash: Option<String>,
    pub owner_node_did: String,
    pub creation_policy: String,
    pub existing: Vec<LeanWorkspaceBindingRef>,
    pub candidate: LeanWorkspaceBindingRef,
    pub git_metadata_write: bool,
    pub agent_command_mode: String,
    pub legal: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanWorkspaceBindingRef {
    pub binding_id: String,
    pub workspace_id: String,
    pub request_id: String,
    pub authority: String,
    pub node_did: String,
    pub seal_hash: Option<String>,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanWorkspaceCase {
    pub name: String,
    pub from: String,
    pub to: String,
    pub seal_hash: Option<String>,
    pub legal: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceChildCase {
    pub actor: String,
    pub expected: bool,
    pub permission: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceChildRevocationCase {
    pub actor: String,
    pub read: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceCleanupCase {
    pub active: bool,
    pub allowed: bool,
    pub begins: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceDatastoreWriteCase {
    pub allowed: bool,
    pub collection: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceLivenessCase {
    pub may_release: bool,
    pub terminal: bool,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathAliasCase {
    pub base_paths: Vec<String>,
    pub base_prefixes: Vec<Vec<String>>,
    pub changed_paths: Vec<String>,
    pub changed_prefixes: Vec<Vec<String>>,
    pub expected: bool,
    pub name: String,
    pub tree_paths: Vec<String>,
    pub tree_prefixes: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCase {
    pub before: LeanWorkspacePathCapabilityCaseBefore,
    pub disposition: String,
    pub evidence: LeanWorkspacePathCapabilityCaseEvidence,
    pub expected: LeanWorkspacePathCapabilityCaseExpected,
    pub name: String,
    pub operation: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseBefore {
    pub base: String,
    pub capability: LeanWorkspacePathCapabilityCaseBeforeCapability,
    pub integrator: Option<LeanWorkspacePathCapabilityCaseBeforeIntegrator>,
    pub owner: String,
    pub seal: Option<String>,
    pub state: String,
    pub trunk_effects: u64,
    pub workspace_id: String,
    pub writer: Option<LeanWorkspacePathCapabilityCaseBeforeWriter>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseBeforeCapability {
    pub mode: String,
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseBeforeIntegrator {
    pub base: String,
    pub capability: LeanWorkspacePathCapabilityCaseBeforeIntegratorCapability,
    pub owner: String,
    pub tree: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseBeforeIntegratorCapability {
    pub mode: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseBeforeWriter {
    pub base: String,
    pub capability: LeanWorkspacePathCapabilityCaseBeforeWriterCapability,
    pub owner: String,
    pub tree: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseBeforeWriterCapability {
    pub mode: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseEvidence {
    pub applied_snapshot_matches: bool,
    pub captured_base_matches: bool,
    pub checkout_present: bool,
    pub delta: Vec<LeanWorkspacePathCapabilityCaseEvidenceDeltaItem>,
    pub existing_identity_matches: bool,
    pub expected: LeanWorkspacePathCapabilityCaseEvidenceExpected,
    pub manifest_canonical: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseEvidenceDeltaItem {
    pub canonical: bool,
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseEvidenceExpected {
    pub base: String,
    pub capability: LeanWorkspacePathCapabilityCaseEvidenceExpectedCapability,
    pub owner: String,
    pub tree: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseEvidenceExpectedCapability {
    pub mode: String,
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseExpected {
    pub base: String,
    pub capability: LeanWorkspacePathCapabilityCaseExpectedCapability,
    pub integrator: Option<LeanWorkspacePathCapabilityCaseExpectedIntegrator>,
    pub owner: String,
    pub seal: Option<String>,
    pub state: String,
    pub trunk_effects: u64,
    pub workspace_id: String,
    pub writer: Option<LeanWorkspacePathCapabilityCaseExpectedWriter>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseExpectedCapability {
    pub mode: String,
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseExpectedIntegrator {
    pub base: String,
    pub capability: LeanWorkspacePathCapabilityCaseExpectedIntegratorCapability,
    pub owner: String,
    pub tree: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseExpectedIntegratorCapability {
    pub mode: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseExpectedWriter {
    pub base: String,
    pub capability: LeanWorkspacePathCapabilityCaseExpectedWriterCapability,
    pub owner: String,
    pub tree: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePathCapabilityCaseExpectedWriterCapability {
    pub mode: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePermissionWriteCase {
    pub accepted: bool,
    pub changed: bool,
    pub expected: u64,
    pub mode: String,
    pub next: Option<LeanWorkspacePermissionWriteCaseNext>,
    pub previous: Option<LeanWorkspacePermissionWriteCasePrevious>,
    #[serde(rename = "requestedTask")]
    pub requested_task: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePermissionWriteCaseNext {
    pub mode: String,
    #[serde(rename = "reviewTask")]
    pub review_task: Option<u64>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePermissionWriteCasePrevious {
    pub mode: String,
    #[serde(rename = "reviewTask")]
    pub review_task: Option<u64>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspacePublicationCases {
    pub aborted_document_visible: bool,
    pub effect_after_failed_reservation: bool,
    pub revoked_reader_restored: bool,
    pub revoked_writer_restored: bool,
    pub stale_publication_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceQuickstartCase {
    pub aborted_document_visible: bool,
    pub actor: String,
    pub after_revoke: bool,
    pub before: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceQuickstartSelectionCase {
    pub actor: String,
    pub after_revocation: bool,
    pub before: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkspaceRootAccessCase {
    pub aborted_document: bool,
    pub actor: String,
    pub after_integrator_revocation: bool,
    pub after_parent_revocation: bool,
    pub after_writer_revocation: bool,
    pub expected: bool,
    pub may_publish: bool,
    pub permission: String,
    pub writer_execution: bool,
    pub writer_execution_after_revocation: bool,
}
