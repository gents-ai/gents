#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::Deserialize;

pub fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// A JSON object read as its pairs in document order, which a `BTreeMap` would sort.
pub fn ordered_pairs<'de, D, T>(deserializer: D) -> Result<Vec<(String, T)>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Pairs<T>(std::marker::PhantomData<T>);
    impl<'de, T> serde::de::Visitor<'de> for Pairs<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<(String, T)>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON object")
        }
        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut pairs = Vec::new();
            while let Some(pair) = map.next_entry()? {
                pairs.push(pair);
            }
            Ok(pairs)
        }
    }
    deserializer.deserialize_map(Pairs(std::marker::PhantomData))
}

pub type LeanFeatureMatrix = BTreeMap<String, BTreeMap<String, LeanFeatureMatrixCell>>;

#[derive(Debug, Clone, Copy)]
pub struct LeanVocabulary<'a> {
    pub lean_file: &'a str,
    pub model: &'a str,
    pub namespace: &'a str,
    pub rust_source: &'a str,
    pub rust_values: &'a [&'a str],
}

#[derive(Debug, Clone, Copy)]
pub struct LeanContractVocabulary<'a> {
    pub domain: &'a str,
    pub rust_source: &'a str,
    pub rust_values: &'a [&'a str],
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentDeliveryCase {
    pub delivery: Option<String>,
    pub interrupt: Option<bool>,
    pub expected: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanInterruptDeliveryCase {
    pub goal_paused: bool,
    pub planned: Option<u64>,
    pub active: Option<u64>,
    pub expected: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct LeanAgentSteerDispositionCase {
    pub active: bool,
    pub compatible: bool,
    pub expected: String,
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
pub struct LeanAgentMessageOutcomeCase {
    pub folded: bool,
    pub caused: String,
    pub head: String,
    pub expected: String,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanContractSnapshot {
    pub tool_admission_handoff_cases: Vec<LeanToolAdmissionHandoffCase>,
    pub delegated_publication_cases: Vec<serde_json::Value>,
    pub delegated_finality_cases: Vec<serde_json::Value>,
    pub claude_output_config_cases: Vec<serde_json::Value>,
    pub checkpoint_fallback_cases: Vec<serde_json::Value>,
    pub checkpoint_list_fields: Vec<String>,
    pub generated_by: String,
    pub schema_publication_cases: Vec<serde_json::Value>,
    pub root_admission_cases: Vec<LeanRootAdmissionCase>,
    pub vocabularies: Vec<LeanVocabularyContract>,
    pub state_machines: Vec<LeanStateMachineContract>,
    pub pairing_reconcile_cases: Vec<LeanPairingReconcileCase>,
    pub goal_claimed_publication_cases: Vec<serde_json::Value>,
    pub goal_claimed_identity_cases: Vec<serde_json::Value>,
    pub goal_claimed_target_cases: Vec<serde_json::Value>,
    pub goal_backed_runtime_cases: Vec<serde_json::Value>,
    pub goal_wake_coalescing_cases: Vec<serde_json::Value>,
    pub goal_wake_ancestry_cases: Vec<serde_json::Value>,
    pub goal_request_scope_cases: Vec<serde_json::Value>,
    pub goal_request_ancestry_cases: Vec<serde_json::Value>,
    pub goal_queue_assignment_cases: Vec<serde_json::Value>,
    pub goal_first_request_binding_cases: Vec<serde_json::Value>,
    pub goal_continuation_assignment_cases: Vec<serde_json::Value>,
    pub plugin_job_commit_kill_cases: Vec<serde_json::Value>,
    pub client_process_claim_cases: Vec<serde_json::Value>,
    pub client_process_receipt_cases: Vec<serde_json::Value>,
    pub client_control_grant_cases: Vec<serde_json::Value>,
    pub client_signed_control_cases: Vec<serde_json::Value>,
    pub process_stop_action_cases: Vec<serde_json::Value>,
    pub background_process_identity_cases: Vec<serde_json::Value>,
    pub durable_observation_cases: Vec<serde_json::Value>,
    pub plugin_target_resolution_cases: Vec<serde_json::Value>,
    pub goal_request_head_cases: Vec<serde_json::Value>,
    pub goal_request_assignment_cases: Vec<serde_json::Value>,
    pub goal_wake_receipt_cases: Vec<serde_json::Value>,
    pub goal_tool_context_cases: Vec<serde_json::Value>,
    pub goal_input_binding_cases: Vec<serde_json::Value>,
    pub goal_fresh_session_input_cases: Vec<serde_json::Value>,
    pub session_message_goal_input_cases: Vec<serde_json::Value>,
    pub goal_delivery_input_cases: Vec<serde_json::Value>,
    pub goal_assignment_history_cases: Vec<serde_json::Value>,
    pub goal_operator_resume_cases: Vec<serde_json::Value>,
    pub goal_resume_publication_cases: Vec<LeanGoalResumePublicationCase>,
    pub goal_operator_resume_on_cases: Vec<serde_json::Value>,
    pub goal_config_reactivation_cases: Vec<serde_json::Value>,
    pub goal_reset_resume_cases: Vec<serde_json::Value>,
    pub trigger_delivery: serde_json::Value,
    pub operator_base_freeze_cases: Vec<serde_json::Value>,
    pub workspace_path_capability_cases: Vec<serde_json::Value>,
    pub file_inventory_direct_reader_cases: Vec<serde_json::Value>,
    pub file_inventory_fork_cases: Vec<serde_json::Value>,
    pub file_inventory_reader_cases: Vec<serde_json::Value>,
    pub file_inventory_source_projection_cases: Vec<serde_json::Value>,
    pub file_inventory_order_window_cases: Vec<serde_json::Value>,
    pub file_inventory_order_resolution_cases: Vec<serde_json::Value>,
    pub file_inventory_order_comparison_cases: Vec<serde_json::Value>,
    pub file_inventory_order_entry_cases: Vec<serde_json::Value>,
    pub file_review_route_cases: Vec<serde_json::Value>,
    pub file_review_subscription_cases: Vec<serde_json::Value>,
    pub workspace_path_alias_cases: Vec<serde_json::Value>,
    pub logical_output_obligation_cases: Vec<serde_json::Value>,
    pub count_carrier_cases: serde_json::Value,
    pub schema_argument_repair_cases: Vec<serde_json::Value>,
    pub invalid_tool_progress_cases: Vec<serde_json::Value>,
    pub repeated_tool_failure_cases: Vec<serde_json::Value>,
    pub tool_timeout_cases: serde_json::Value,
    pub plugin_resource_cases: serde_json::Value,
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
    pub mailbox_notification_cases: Vec<serde_json::Value>,
    pub mailbox_reply_cases: Vec<serde_json::Value>,
    pub mailbox_question_cases: Vec<serde_json::Value>,
    pub mailbox_handoff_cases: Vec<LeanMailboxHandoffCase>,
    pub mailbox_sweep_cases: Vec<serde_json::Value>,
    pub client_installation_cases: Vec<serde_json::Value>,
    pub client_installation_compatibility_cases: Vec<serde_json::Value>,
    pub runtime_service_replacement_cases: Vec<serde_json::Value>,
    pub update_handoff_cases: Vec<serde_json::Value>,
    pub client_observation_scope_cases: Vec<serde_json::Value>,
    pub inference_observation_cases: Vec<serde_json::Value>,
    pub inference_visibility_cases: Vec<serde_json::Value>,
    pub inference_route_cases: Vec<serde_json::Value>,
    pub identity_publication_cases: Vec<serde_json::Value>,
    pub identity_grant_set_cases: Vec<LeanGrantSetCase>,
    pub identity_grant_recipient_cases: Vec<LeanGrantRecipientCase>,
    pub identity_delegation_cases: Vec<LeanDelegationCase>,
    pub identity_agent_baseline_cases: Vec<LeanAgentBaselineCase>,
    pub identity_enrollment_grant_cases: serde_json::Value,
    pub identity_agent_creator_cases: Vec<serde_json::Value>,
    pub identity_agent_creation_exposure_cases: Vec<serde_json::Value>,
    pub identity_created_agent_access_cases: Vec<serde_json::Value>,
    pub identity_client_agent_creation_cases: Vec<LeanIdentityClientAgentCreationCase>,
    pub identity_client_document_creation_cases: Vec<serde_json::Value>,
    pub identity_agent_block_signing_cases: Vec<LeanAgentBlockSigningCase>,
    pub node_file_request_access_cases: Vec<serde_json::Value>,
    pub identity_private_journal_policy: LeanPrivateJournalPolicy,
    pub mailbox_runtime_notice_case: serde_json::Value,
    pub mailbox_sweep_actor_cases: Vec<serde_json::Value>,
    pub mailbox_claimed_recipient_cases: Vec<serde_json::Value>,
    pub workspace_root_completeness_cases: Vec<serde_json::Value>,
    pub workspace_completion_actor_cases: Vec<serde_json::Value>,
    pub workspace_completion_access_cases: Vec<serde_json::Value>,
    pub workspace_agent_selection_cases: Vec<serde_json::Value>,
    pub workspace_liveness_cases: Vec<serde_json::Value>,
    pub workspace_child_cases: Vec<serde_json::Value>,
    pub workspace_quickstart_selection_cases: Vec<serde_json::Value>,
    pub workspace_callback_selection_cases: Vec<serde_json::Value>,
    pub pack_placement_reader_cases: Vec<serde_json::Value>,
    pub agent_target_access_cases: Vec<serde_json::Value>,
    pub native_route_cases: Vec<serde_json::Value>,
    pub workspace_datastore_write_cases: Vec<serde_json::Value>,
    pub workspace_root_access_cases: Vec<serde_json::Value>,
    pub workspace_child_revocation_cases: Vec<serde_json::Value>,
    pub workspace_admission_cases: Vec<serde_json::Value>,
    pub workspace_cleanup_cases: Vec<serde_json::Value>,
    pub workspace_publication_cases: serde_json::Value,
    pub workspace_quickstart_cases: Vec<serde_json::Value>,
    pub mailbox_native_policy: serde_json::Value,
    pub callback_workspace_actor_cases: Vec<serde_json::Value>,
    pub session_input_edit_native_discovery_cases: Vec<serde_json::Value>,
    pub session_input_edit_native_claim_cases: Vec<serde_json::Value>,
    pub client_optimistic_send_cases: serde_json::Value,
    pub identity_client_agent_journal_cases: Vec<LeanIdentityClientAgentJournalCase>,
    pub identity_client_agent_recovery_cases: Vec<LeanIdentityClientAgentRecoveryCase>,
    pub client_publication_protected_collection: String,
    pub watcher_actor_cases: Vec<serde_json::Value>,
    pub watcher_scoped_scan_cases: Vec<serde_json::Value>,
    pub identity_bootstrap_cases: Vec<serde_json::Value>,
    pub identity_bootstrap_reservation_cases: Vec<serde_json::Value>,
    pub identity_bootstrap_startup_cases: Vec<serde_json::Value>,
    pub identity_runtime_nac_cases: Vec<serde_json::Value>,
    pub identity_parent_publication_cases: Vec<LeanIdentityParentPublicationCase>,
    pub identity_callback_parent_actor_cases: Vec<serde_json::Value>,
    pub identity_eval_record_cases: Vec<serde_json::Value>,
    pub identity_optimization_promotion_cases: Vec<serde_json::Value>,
    pub identity_owner_registration_cases: Vec<serde_json::Value>,
    pub identity_readiness_preflight_cases: Vec<serde_json::Value>,
    pub identity_secret_publication_cases: Vec<serde_json::Value>,
    pub identity_deferred_operation_cases: Vec<serde_json::Value>,
    pub identity_pairing_partition_cases: Vec<serde_json::Value>,
    pub identity_desired_publisher_cases: Vec<serde_json::Value>,
    pub identity_receiver_grant_cases: Vec<serde_json::Value>,
    pub identity_pairing_reader_cases: Vec<serde_json::Value>,
    pub identity_pairing_reader_policy_cases: Vec<serde_json::Value>,
    pub identity_relationship_replay_cases: Vec<serde_json::Value>,
    pub identity_relationship_replay_producer_cases: Vec<serde_json::Value>,
    pub identity_local_registry_writer_cases: Vec<serde_json::Value>,
    pub identity_discovery_policy_cases: Vec<serde_json::Value>,
    pub identity_client_transport_cases: Vec<serde_json::Value>,
    pub identity_replica_grant_cases: Vec<serde_json::Value>,
    pub identity_network_protocol_cases: Vec<serde_json::Value>,
    pub identity_enrollment_policy_cases: Vec<serde_json::Value>,
    pub identity_readiness_recovery_cases: Vec<serde_json::Value>,
    pub identity_readiness_observation_cases: Vec<serde_json::Value>,
    pub identity_readiness_settled_cases: Vec<serde_json::Value>,
    pub identity_directory_publication_cases: Vec<serde_json::Value>,
    pub identity_startup_readiness_cases: Vec<LeanIdentityStartupReadinessCase>,
    pub goal_link_cases: Vec<LeanGoalLinkCase>,
    pub remote_agent_delivery_cases: Vec<serde_json::Value>,
    pub remote_agent_delivery_recovery_cases: Vec<serde_json::Value>,
    pub remote_agent_delivery_response_cases: Vec<serde_json::Value>,
    pub remote_agent_delivery_claim_cases: Vec<serde_json::Value>,
    pub cross_actor_authorization_cases: Vec<LeanCrossActorAuthorizationCase>,
    pub client_telemetry_cases: Vec<serde_json::Value>,
    pub client_settings_cases: serde_json::Value,
    pub client_data_publication_cases: serde_json::Value,
    pub client_request_patch_publication_cases: serde_json::Value,
    pub client_direct_session_read_cases: Vec<serde_json::Value>,
    pub storage_failure_cases: Vec<serde_json::Value>,
    pub client_request_closure_cases: Vec<serde_json::Value>,
    pub client_request_cache_identity_cases: Vec<serde_json::Value>,
    pub client_session_queue_order_cases: Vec<serde_json::Value>,
    pub request_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub process_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub provider_eof_cases: Vec<LeanProviderEofCase>,
    pub request_execution_lease_cases: Vec<LeanRequestExecutionLeaseCase>,
    pub request_execution_lease_observed_cases: Vec<LeanRequestExecutionObservedCase>,
    pub request_execution_lease_trace_cases: Vec<LeanRequestExecutionLeaseTraceCase>,
    pub inference_registry_cases: Vec<serde_json::Value>,
    pub trigger_dispatch_case_count: usize,
    pub trigger_dispatch_cases: Vec<LeanTriggerDispatchCase>,
    pub event_group_case_count: usize,
    pub event_group_cases: Vec<LeanEventGroupCase>,
    pub event_group_clock_case_count: usize,
    pub event_group_clock_cases: Vec<serde_json::Value>,
    pub event_group_capture_case_count: usize,
    pub event_group_capture_cases: Vec<serde_json::Value>,
    pub callback_transition_case_count: usize,
    pub callback_transition_cases: Vec<serde_json::Value>,
    pub session_document_cases: LeanSessionDocumentCases,
    pub plugin_job_opening_cases: Vec<serde_json::Value>,
    pub session_reply_preview_cases: Vec<serde_json::Value>,
    pub request_input_cases: Vec<serde_json::Value>,
    pub background_wake_row_cases: Vec<serde_json::Value>,
    pub configuration_scope_cases: serde_json::Value,
    pub initial_inference_cases: Vec<serde_json::Value>,
    pub discovery_scope_cases: Vec<serde_json::Value>,
    pub discovery_guidance_cases: Vec<serde_json::Value>,
    pub budget_rehydration_cases: Vec<LeanBudgetRehydrationCase>,
    pub goal_decision_cases: Vec<LeanGoalDecisionCase>,
    pub goal_readiness_gate_cases: Vec<LeanGoalReadinessGateCase>,
    pub goal_claimed_readiness_cases: Vec<serde_json::Value>,
    pub goal_transition_cases: Vec<LeanGoalTransitionCase>,
    pub goal_create_cases: Vec<LeanGoalCreateCase>,
    pub goal_edit_cases: Vec<serde_json::Value>,
    pub goal_client_delivery_cases: Vec<serde_json::Value>,
    pub goal_client_control_cases: Vec<serde_json::Value>,
    pub goal_client_start_cases: Vec<serde_json::Value>,
    pub goal_client_receipt_cases: Vec<serde_json::Value>,
    pub goal_observation_cases: Vec<serde_json::Value>,
    pub goal_session_observation_cases: Vec<serde_json::Value>,
    pub goal_session_fold_cases: Vec<serde_json::Value>,
    pub workspace_command_grant_answers: Vec<serde_json::Value>,
    pub workspace_command_grant_lookups: Vec<serde_json::Value>,
    pub workspace_command_grant_revocations: Vec<serde_json::Value>,
    pub workspace_permission_write_cases: Vec<serde_json::Value>,
    pub workspace_permission_cases: Vec<serde_json::Value>,
    pub workspace_command_root_cases: Vec<serde_json::Value>,
    pub operator_path_cases: Vec<serde_json::Value>,
    pub access_review_cases: Vec<serde_json::Value>,
    pub access_review_task_cases: Vec<serde_json::Value>,
    pub plugin_approval_cases: Vec<serde_json::Value>,
    pub goal_objective_whitespace_codepoints: Vec<u32>,
    pub task_goal_publication_cases: Vec<LeanTaskGoalPublicationCase>,
    pub task_goal_recovery_cases: Vec<LeanTaskGoalRecoveryCase>,
    pub goal_submission_cases: Vec<LeanGoalSubmissionCase>,
    pub goal_continuation_materialization_cases: Vec<LeanGoalContinuationMaterializationCase>,
    pub file_inventory_closure_cases: Vec<LeanSessionClosureCase>,
    pub enrollment_cases: Vec<LeanEnrollmentCase>,
    pub enrollment_durable_projection_cases: Vec<LeanEnrollmentDurableProjectionCase>,
    pub enrollment_pending_request_cases: Vec<serde_json::Value>,
    pub enrollment_absent_peer_cases: Vec<serde_json::Value>,
    pub client_chat_readiness_cases: Vec<serde_json::Value>,
    pub client_route_pairing_legs: Vec<serde_json::Value>,
    pub client_chat_readiness_causes: Vec<serde_json::Value>,
    pub client_sync_health_cases: Vec<serde_json::Value>,
    pub enrollment_route_readiness_cases: Vec<serde_json::Value>,
    pub enrollment_startup_cases: Vec<serde_json::Value>,
    pub enrollment_candidate_name_cases: Vec<serde_json::Value>,
    pub enrollment_encoding_cases: Vec<LeanEnrollmentEncodingCase>,
    pub enrollment_digest_cases: Vec<LeanEnrollmentDigestCase>,
    pub enrollment_auto_renewal_cases: Vec<serde_json::Value>,
    pub enrollment_renewal_intent_cases: Vec<serde_json::Value>,
    pub agent_request_admission_cases: Vec<LeanAgentRequestAdmissionCase>,
    pub causal_hop_contract: LeanCausalHopContract,
    pub scope_collection_rules: std::collections::BTreeMap<String, Vec<LeanScopeCollectionRule>>,
    pub scope_node_route_cases: Vec<serde_json::Value>,
    pub scope_template_cases: Vec<serde_json::Value>,
    pub title_request_admission_cases: Vec<LeanTitleRequestAdmissionCase>,
    pub title_request_purpose_wire_cases: Vec<LeanTitleRequestPurposeWireCase>,
    pub title_text_cases: serde_json::Value,
    pub title_usage_cases: Vec<LeanTitleUsageCase>,
    pub provider_usage_cases: Vec<serde_json::Value>,
    pub provider_limit_read_cases: Vec<serde_json::Value>,
    pub title_admission_join_cases: Vec<LeanTitleAdmissionJoinCase>,
    pub frontend_client_shell_case_count: usize,
    pub frontend_client_shell_cases: Vec<LeanClientShellCase>,
    pub desktop_client_shell_case_count: usize,
    pub desktop_client_shell_cases: Vec<LeanClientShellCase>,
    pub client_session_turn_cases: Vec<LeanSessionTurnCase>,
    pub request_lifecycle_operator_ui_cases: Vec<LeanClientShellCase>,
    pub client_live_delta_cases: Vec<serde_json::Value>,
    pub runtime_session_selection_cases: Vec<serde_json::Value>,
    pub runtime_configuration_admission_cases: Vec<serde_json::Value>,
    pub runtime_reconcile_cases: Vec<LeanRuntimeReconcileCase>,
    pub client_agent_readiness_cases: Vec<LeanClientAgentReadinessCase>,
    pub startup_readiness_cases: Vec<LeanStartupReadinessCase>,
    pub snapshot_demotion_cases: Vec<serde_json::Value>,
    pub catalog_refresh_cases: Vec<serde_json::Value>,
    pub claude_platform_cases: Vec<serde_json::Value>,
    pub plugin_reference_cases: Vec<serde_json::Value>,
    pub plugin_pin_cases: Vec<serde_json::Value>,
    pub pack_document_ownership_cases: Vec<serde_json::Value>,
    pub pack_wire_lock_cases: Vec<serde_json::Value>,
    pub pack_wire_target_cases: Vec<serde_json::Value>,
    pub pack_finality_cases: Vec<serde_json::Value>,
    pub hosted_pack_evidence_cases: Vec<serde_json::Value>,
    pub hosted_plugin_cases: Vec<serde_json::Value>,
    pub hosted_pack_schema_cases: Vec<serde_json::Value>,
    pub hosted_pack_home_cases: Vec<serde_json::Value>,
    pub readiness_publication_cases: Vec<LeanReadinessPublicationCase>,
    pub apply_reconcile_cases: Vec<LeanApplyReconcileCase>,
    pub eval_outcome_cases: Vec<LeanEvalOutcomeCase>,
    pub eval_document_capture_cases: Vec<LeanEvalDocumentCaptureCase>,
    pub optimization_cases: LeanOptimizationCases,
    pub publish_if_cases: Vec<LeanPublishIfCase>,
    pub tool_policy_cases: Vec<LeanToolPolicyCase>,
    pub application_write_cases: Vec<serde_json::Value>,
    pub field_read_cases: Vec<serde_json::Value>,
    pub write_input_cases: Vec<serde_json::Value>,
    pub invocation_correlation_cases: Vec<serde_json::Value>,
    pub goal_capability_resolution_cases: Vec<LeanGoalCapabilityResolutionCase>,
    pub lsp_action_cases: Vec<LeanLspActionCase>,
    pub self_config_field_tables: Vec<LeanSelfConfigFieldTable>,
    pub self_config_resource_read_exposure_cases: Vec<serde_json::Value>,
    pub self_config_scope_cases: LeanSelfConfigScopeCases,
    pub policy_resource_exposure_cases: Vec<serde_json::Value>,
    pub policy_grant_bound_cases: Vec<serde_json::Value>,
    pub policy_lockout_cases: serde_json::Value,
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
    pub pairing_reconcile_shutdown_boundary_cases: Vec<LeanPairingReconcileShutdownBoundaryCase>,
    pub pairing_reconcile_sweep_retry_boundary_cases:
        Vec<LeanPairingReconcileSweepRetryBoundaryCase>,
    pub pairing_reconcile_sweep_scheduling_cases: Vec<LeanPairingReconcileSweepSchedulingCase>,
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
    pub account_rotation_select_cases: Vec<LeanAccountRotationSelectCase>,
    pub account_rotation_attempt_cases: Vec<LeanAccountRotationAttemptCase>,
    pub account_rotation_observe_cases: Vec<LeanAccountRotationObserveCase>,
    pub mcp_health_cases: Vec<LeanMcpHealthCase>,
    pub backend_health_cases: Vec<LeanBackendHealthCase>,
    pub backend_probe_schedule_cases: Vec<LeanBackendProbeScheduleCase>,
    pub boundaries: Vec<LeanBoundary>,
    pub deviations: Vec<LeanDeviation>,
    pub command_policy_cases: Vec<LeanCommandPolicyCase>,
    pub command_sandbox_cases: Vec<LeanCommandSandboxCase>,
    pub command_isolation_cases: Vec<serde_json::Value>,
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
    pub agent_steer_target_cases: Vec<serde_json::Value>,
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
    pub request_terminal_selection_cases: Vec<serde_json::Value>,
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
    pub session_input_edit_client_observation_cases: Vec<serde_json::Value>,
    pub retry_selection_cases: Vec<LeanRetrySelectionCase>,
    pub canonical_worker_capacity_cases: Vec<LeanWorkerCapacityCase>,
    pub canonical_payload_presentation_cases: Vec<LeanPayloadPresentationCase>,
    pub terminal_diagnostic_presentation_cases: Vec<LeanTerminalDiagnosticPresentationCase>,
    pub terminal_diagnostic_replay_cases: Vec<LeanTerminalDiagnosticReplayCase>,
    pub compaction_reducer_cases: Vec<LeanCompactionReducerCase>,
    pub compaction_cursor_cases: Vec<LeanCompactionCursorCase>,
    pub prompt_assembly_sanitize_cases: Vec<LeanPromptAssemblySanitizeCase>,
    pub prompt_assembly_assistant_order_cases: Vec<LeanPromptAssemblyAssistantOrderCase>,
    pub provider_tool_identity_cases: Vec<serde_json::Value>,
    pub prompt_assembly_mode_sanitize_cases: Vec<LeanPromptAssemblyModeSanitizeCase>,
    pub routing_affinity_cases: Vec<serde_json::Value>,
    pub retry_frontier_cases: Vec<serde_json::Value>,
    pub retry_entry_cases: Vec<serde_json::Value>,
    pub current_input_cases: Vec<LeanCurrentInputCase>,
    pub wake_input_cases: Vec<LeanWakeInputCase>,
    pub prompt_assembly_layer_cases: Vec<LeanPromptAssemblyLayerCase>,
    pub prompt_assembly_repair_cases: Vec<LeanPromptAssemblyRepairCase>,
    pub prompt_assembly_budget_cases: Vec<LeanPromptAssemblyBudgetCase>,
    pub prompt_assembly_turn_budget_cases: Vec<LeanPromptAssemblyTurnBudgetCase>,
    pub prompt_assembly_retention_cases: Vec<LeanPromptAssemblyRetentionCase>,
    pub prompt_assembly_compaction_trigger_cases: Vec<LeanPromptAssemblyCompactionTriggerCase>,
    pub prompt_assembly_claude_map_cases: Vec<LeanPromptAssemblyClaudeMapCase>,
    pub prompt_assembly_claude_body_cases: Vec<LeanPromptAssemblyClaudeBodyCase>,
    pub prompt_assembly_claude_attribution_cases: Vec<LeanPromptAssemblyClaudeAttributionCase>,
    pub prompt_assembly_claude_cache_cases: Vec<LeanPromptAssemblyClaudeCacheCase>,
    pub prompt_assembly_claude_origin_cases: Vec<LeanPromptAssemblyClaudeOriginCase>,
    pub prompt_assembly_claude_field_cases: Vec<LeanPromptAssemblyClaudeFieldCase>,
    pub prompt_assembly_claude_field_row_cases: Vec<LeanPromptAssemblyClaudeFieldRowCase>,
    pub prompt_assembly_claude_stream_cases: Vec<LeanPromptAssemblyClaudeStreamCase>,
    pub prompt_assembly_claude_thinking_stream_cases:
        Vec<LeanPromptAssemblyClaudeThinkingStreamCase>,
    pub prompt_assembly_claude_wire_start_cases: Vec<LeanPromptAssemblyClaudeWireStartCase>,
    pub prompt_assembly_service_tier_cases: Vec<LeanPromptAssemblyServiceTierCase>,
    pub prompt_assembly_responses_storage_cases: Vec<LeanPromptAssemblyResponsesStorageCase>,
    pub prompt_assembly_responses_effort_cases: Vec<LeanPromptAssemblyResponsesEffortCase>,
    pub prompt_assembly_claude_replay_cases: Vec<LeanPromptAssemblyClaudeReplayCase>,
    pub prompt_assembly_claude_checkpoint_cases: Vec<LeanPromptAssemblyClaudeCheckpointCase>,
    pub prompt_assembly_reasoning_suffix_cases: Vec<LeanPromptAssemblyReasoningSuffixCase>,
    pub prompt_assembly_replay_shape_cases: Vec<LeanPromptAssemblyReplayShapeCase>,
    pub prompt_assembly_replay_prefix_cases: Vec<LeanPromptAssemblyReplayPrefixCase>,
    pub protected_replay_compaction_cases: Vec<LeanProtectedReplayCompactionCase>,
    pub rendered_capture_cases: Vec<LeanRenderedCaptureCase>,
    pub rendered_capture_storage_cases: Vec<LeanRenderedCaptureStorageCase>,
    pub durable_reduction_cases: Vec<LeanDurableReductionCase>,
    pub rolling_compaction_cases: Vec<LeanRollingCompactionCase>,
    pub reduction_engine_cases: Vec<LeanReductionEngineCase>,
    pub compaction_projection_join_cases: Vec<LeanCompactionProjectionJoinCase>,
    pub compaction_canonical_projection_cases: Vec<LeanCanonicalCompactionCase>,
    pub repaired_projection_admission_cases: Vec<LeanRepairedProjectionCase>,
    pub rendered_capture_key_cases: Vec<LeanRenderedCaptureKeyCase>,
    pub capture_block_scope_cases: Vec<serde_json::Value>,
    pub session_child_cases: Vec<serde_json::Value>,
    pub auxiliary_session_child_binding_cases: Vec<serde_json::Value>,
    pub local_self_publication_cases: Vec<serde_json::Value>,
    pub client_request_publication_cases: Vec<serde_json::Value>,
    pub capture_scope_cases: Vec<LeanCaptureScopeCase>,
    pub capture_order_cases: Vec<LeanCaptureOrderCase>,
    pub aggregate_token_budget_cases: Vec<LeanAggregateTokenBudgetCase>,
    pub task_hook_admission_cases: Vec<LeanTaskHookAdmissionCase>,
    pub store_encryption_upgrade_cases: Vec<serde_json::Value>,
    pub task_hook_run_cases: Vec<LeanTaskHookRunCase>,
    pub task_hook_recovery_cases: Vec<LeanTaskHookRecoveryCase>,
    pub lifecycle_hook_effect_cases: Vec<LeanLifecycleHookEffectCase>,
    pub lifecycle_hook_stage_cases: Vec<LeanLifecycleHookStageCase>,
    pub lifecycle_hook_must_commit_cases: Vec<LeanLifecycleHookMustCommitCase>,
    pub lifecycle_hook_combine_cases: Vec<LeanLifecycleHookCombineCase>,
    pub lifecycle_hook_gate_cases: Vec<LeanLifecycleHookGateCase>,
    pub lifecycle_hook_queue_scope_cases: Vec<LeanLifecycleHookQueueScopeCase>,
    pub lifecycle_hook_delivery_cases: Vec<LeanLifecycleHookDeliveryCase>,
    pub lifecycle_hook_owed_delivery_cases: Vec<LeanLifecycleHookOwedDeliveryCase>,
    pub lifecycle_hook_admit_cases: Vec<LeanLifecycleHookAdmitCase>,
    pub lifecycle_hook_grant_cases: Vec<LeanLifecycleHookGrantCase>,
    pub follow_up_hooks: Vec<String>,
    pub coverage_ledger: Vec<LeanCoverageEntry>,
    pub feature_surface_requirements: Vec<LeanFeatureSurfaceRequirement>,
    pub feature_matrix: LeanFeatureMatrix,
    pub identity_structural_cases: Vec<LeanIdentityStructuralCase>,
    pub identity_permission_cases: Vec<LeanIdentityPermissionCase>,
    pub identity_household_cases: Vec<LeanIdentityHouseholdCase>,
    pub identity_task_session_publication_cases: Vec<serde_json::Value>,
    pub identity_client_session_reservation_cases: Vec<serde_json::Value>,
    pub identity_atomic_client_request_cases: Vec<serde_json::Value>,
    pub identity_derivation_cases: Vec<LeanIdentityDerivationCase>,
    pub identity_contracts: Vec<LeanIdentityContract>,
    pub pack_identity_cases: Vec<LeanPackIdentityCase>,
    pub pack_identity_reference_cases: Vec<LeanPackIdentityReferenceCase>,
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
pub struct LeanPrivateJournalCheck {
    pub role: String,
    pub action: String,
    pub expected: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityClientAgentJournalCase {
    pub name: String,
    pub initial: LeanIdentityClientAgentJournalState,
    pub steps: Vec<LeanIdentityClientAgentJournalStep>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityClientAgentJournalStep {
    pub action: LeanIdentityClientAgentJournalAction,
    pub before: LeanIdentityClientAgentJournalState,
    pub after: LeanIdentityClientAgentJournalState,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityClientAgentJournalState {
    pub client: String,
    pub agent_did: String,
    pub node_did: String,
    pub agent_doc: String,
    pub node_doc: String,
    pub commit: LeanIdentityClientAgentJournalStatus,
    pub parent: LeanIdentityClientAgentJournalStatus,
    pub executor: LeanIdentityClientAgentJournalStatus,
    pub permit: Option<LeanIdentityClientAgentJournalStage>,
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
#[serde(rename_all = "snake_case")]
pub enum LeanIdentityClientAgentJournalStage {
    Commit,
    Parent,
    Executor,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanIdentityClientAgentJournalAction {
    Record {
        stage: LeanIdentityClientAgentJournalStage,
        durable: bool,
    },
    Dispatch {
        stage: LeanIdentityClientAgentJournalStage,
    },
    Finish {
        stage: LeanIdentityClientAgentJournalStage,
        success: bool,
        confirmed_failure: bool,
    },
    Restart,
    SaveUnrelated,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityClientAgentCreationCase {
    pub name: String,
    pub client_intent: bool,
    pub agent_collection: bool,
    pub create: bool,
    pub http: bool,
    pub signer_available: bool,
    pub supported: bool,
    pub signer_matches_bearer: bool,
    pub input_matches_graph: bool,
    pub trusted_schema: bool,
    pub graph_verified: bool,
    pub transport_ready: bool,
    pub native_authorized: bool,
    pub commit: String,
    pub grants: String,
    pub expected_route: String,
    pub expected_publication: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGrantSetCase {
    pub set: String,
    pub permissions: Vec<String>,
    pub native_admin: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGrantRecipientCase {
    pub recipient: String,
    pub direct: Vec<String>,
    pub projected: Vec<String>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalResumePublicationCase {
    pub name: String,
    pub pending: bool,
    pub authorized: bool,
    pub granted: bool,
    pub expected_pending: bool,
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
pub struct LeanPluginJobWireRecoveryTrace {
    pub spawn_wires: Vec<String>,
    pub interruption: String,
    pub read: LeanPluginJobDurableRead,
    pub before: LeanPluginJobRecoveryRow,
    pub after: LeanPluginJobRecoveryRow,
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
pub struct LeanPluginJobWireBinding {
    pub wire: String,
    pub target: LeanPluginJobTarget,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobKill {
    pub retry_key: String,
    pub action: String,
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

impl LeanPluginJobAdoptionCase {
    pub fn name(&self) -> &str {
        match self {
            Self::Step0 { name, .. }
            | Self::HostPin { name, .. }
            | Self::Handle { name, .. }
            | Self::Address { name, .. } => name,
        }
    }
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
pub struct LeanPluginJobReceiptDecisions {
    pub terminal_decision: String,
    pub late_decision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobHostPinExpected {
    pub pin: LeanPluginJobPin,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobHandleExpected {
    pub handle: String,
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
pub struct LeanPluginJobMessageKey {
    pub n: u32,
    pub key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobDecodedKey {
    pub root: String,
    pub slot: String,
    pub attempt: u32,
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

impl LeanPluginJobControlCase {
    pub fn name(&self) -> &str {
        match self {
            Self::Kill { name, .. }
            | Self::Resume { name, .. }
            | Self::ResultRead { name, .. }
            | Self::ResultOffer { name, .. }
            | Self::StepFailure { name, .. } => name,
        }
    }
}

/// What a kill observes about the request one child caused
/// (`Recovery.KillObservation`'s contract spelling).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobObservation {
    pub retry_key: String,
    pub observation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobKillExpected {
    pub row_state: String,
    pub won: bool,
    pub job_state: String,
    pub kill: Vec<LeanPluginJobKill>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobControlEcho {
    pub kind: String,
    pub payload: Option<LeanPluginJobResume>,
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
pub struct LeanPluginJobResultReadExpected {
    pub authorized: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobResultOfferExpected {
    pub offered: bool,
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
pub struct LeanPluginJobGuestPin {
    pub script_id: String,
    pub source_digest: String,
    pub handle: String,
    pub agent_budget: u32,
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
pub struct LeanPluginJobChild {
    pub retry_key: String,
    pub slot: String,
    pub attempt: u32,
    pub target: LeanPluginJobTarget,
    pub state: String,
    pub messages: u32,
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
pub struct LeanPluginJobExtras {
    pub writes: u32,
    pub scratch: Vec<String>,
    pub progress: bool,
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
pub struct LeanPluginJobControl {
    pub row_doc_id: String,
    pub control: String,
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
    Fail {
        extras: LeanPluginJobExtras,
    },
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
pub struct LeanPluginJobPolicy {
    pub mode: String,
    pub dynamic_targets: Vec<String>,
    pub trusted_packs: Vec<String>,
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

impl LeanPluginJobAuthorityCase {
    pub fn name(&self) -> &str {
        match self {
            Self::Provenance { name, .. }
            | Self::Install { name, .. }
            | Self::Installer { name, .. }
            | Self::Origin { name, .. }
            | Self::PackJobBlock { name, .. }
            | Self::PolicyValid { name, .. }
            | Self::ConsentedDynamic { name, .. }
            | Self::WireTargetsFrom { name, .. }
            | Self::PublishedRead { name, .. } => name,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobPublishedReadExpected {
    pub deferred: bool,
}

/// `kind` is `absent`, `legacy` or `written`; only `written` carries `via`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobRecord {
    pub kind: String,
    #[serde(default)]
    pub via: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobProvenanceExpected {
    pub provenance: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobInstallExpected {
    pub record: LeanPluginJobRecord,
    pub provenance: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobOriginExpected {
    pub origin: String,
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
pub struct LeanPluginJobAdmissibleExpected {
    pub admissible: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobModeExpected {
    pub mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobValidExpected {
    pub valid: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWireTargetsExpected {
    pub policy: LeanPluginJobPolicy,
    pub job_targets: Vec<LeanPluginJobTarget>,
    pub caller_targets: Vec<LeanPluginJobTarget>,
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

/// `Conformance.PluginJob.lineageCasesJson`: request rows and the
/// `DurableLineage.openingRequest` each `(cause, session)` query selects.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobLineageCase {
    pub name: String,
    pub rows: Vec<LeanPluginJobRequestRow>,
    pub queries: Vec<LeanPluginJobLineageQuery>,
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
pub struct LeanPluginJobWork {
    pub document: String,
    pub agent: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginDataOperation {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPluginDataInvocation {
    ToolCall,
    JobStep,
    Callback,
    Ux,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanPluginDataRefusal {
    StampUnavailable {
        stamp: LeanPluginDataStamp,
        invocation: LeanPluginDataInvocation,
    },
    Other,
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

/// A filter maps each field to a literal or the `$caller.did` placeholder.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginDataFilterCase {
    pub caller_did: String,
    pub filter: BTreeMap<String, String>,
    pub expected: BTreeMap<String, String>,
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
pub enum LeanPluginDataInsertOutcome {
    Inserted,
    Unchanged,
    AbortStep,
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

/// `plugin_resource_cases.host_import`: whether the `gents` imports are
/// linked for a plugin and what `run_script` answers.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginHostImportCase {
    pub declared: bool,
    pub call_has_script: bool,
    pub linked: bool,
    pub answer: Option<String>,
}

/// `plugin_resource_cases.scratch`: `scratchAdmitted`. `handle` is not an
/// owner input; rows that differ only in it share a path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginScratchCase {
    pub cwd: String,
    pub cwd_allowed: bool,
    pub tool_call_id: String,
    pub handle: String,
    pub name: String,
    pub size: u64,
    pub count: u32,
    #[serde(deserialize_with = "required_nullable")]
    pub expected: Option<String>,
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
pub struct LeanPluginJobLineageQuery {
    pub cause_request_doc_id: String,
    pub session_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_opening_doc_id: Option<String>,
    pub listed_start: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobTargetHop {
    pub target: LeanPluginJobTarget,
    pub hop: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobWithin {
    pub base: String,
    pub cwd: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalPath {
    pub anchor: String,
    pub components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPairingReconcileSnapshot {
    pub connected: bool,
    pub desired_replicators: Vec<String>,
    pub actual_replicators: Vec<String>,
    pub converged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPairingReconcileCase {
    pub name: String,
    pub action: String,
    pub before: LeanPairingReconcileSnapshot,
    pub after: LeanPairingReconcileSnapshot,
}

#[derive(Debug, Deserialize)]
pub struct LeanVocabularyContract {
    pub domain: String,
    pub values: Vec<String>,
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
pub struct LeanWorkspaceCase {
    pub name: String,
    pub from: String,
    pub to: String,
    pub seal_hash: Option<String>,
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
#[serde(deny_unknown_fields)]
pub struct LeanCallbackRetryCase {
    pub name: String,
    pub state: String,
    pub journal: Vec<String>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub allowed: bool,
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
pub struct LeanNamedTransition {
    pub name: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanCoverageEntry {
    pub category: String,
    pub domain: String,
    pub consumer: String,
    pub accepted_boundary: String,
    pub accepted_follow_up: String,
    pub feature: String,
    pub surfaces: Vec<LeanSurface>,
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

impl LeanSurface {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentFacing => "agentFacing",
            Self::OperatorCli => "operatorCli",
            Self::OperatorUi => "operatorUi",
            Self::Api => "api",
            Self::RuntimeInternal => "runtimeInternal",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureSurfaceRequirement {
    pub feature: String,
    pub required: Vec<LeanSurface>,
    pub deferred: Vec<LeanFeatureSurfaceDeferral>,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureSurfaceDeferral {
    pub surface: LeanSurface,
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureMatrixCell {
    pub coverage_strength: String,
    pub row_count: usize,
    pub pending_follow_ups: usize,
    pub deferred_note: String,
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
pub struct LeanDeviation {
    pub id: String,
    pub domain: String,
    pub subject: String,
    pub statement: String,
    pub accepted_failure_mode: Option<String>,
    pub accepted_follow_up: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanTransitionPair {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalDecisionCase {
    pub name: String,
    pub status: String,
    pub terminal: String,
    pub session_idle: bool,
    pub child_exists: bool,
    pub budget_reached: bool,
    pub has_activity: bool,
    pub request_is_wrapup: bool,
    pub infrastructure_retries: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
    pub expected_decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalReadinessGateCase {
    pub name: String,
    pub observation: String,
    pub newer_than_terminal: bool,
    pub settled: bool,
    pub cause: String,
    pub status: String,
    pub terminal: String,
    pub session_idle: bool,
    pub child_exists: bool,
    pub budget_reached: bool,
    pub has_activity: bool,
    pub request_is_wrapup: bool,
    pub infrastructure_retries: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
    pub expected_readiness: String,
    pub expected_gate: String,
    pub expected_retries: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalTransitionCase {
    pub name: String,
    pub pre_status: String,
    pub pre_blocked_audits: i64,
    pub pre_wrapup_requested: bool,
    pub pre_wrapup_completed: bool,
    pub action: String,
    pub accepted: bool,
    pub expected_status: String,
    pub expected_blocked_audits: i64,
    pub expected_wrapup_requested: bool,
    pub expected_wrapup_completed: bool,
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
pub struct LeanTaskGoalRecoveryCase {
    pub name: String,
    pub node_did: String,
    pub agent_did: String,
    pub task_id: String,
    pub fire_key: String,
    pub request_present: bool,
    pub request_binding_matches: bool,
    pub observed_node_did: Option<String>,
    pub observed_agent_did: Option<String>,
    pub observed_session_id: Option<String>,
    pub observed_request_id: Option<String>,
    pub observed_retry_key: Option<String>,
    pub durable_goal_present: bool,
    pub creation_claim_present: bool,
    pub expected_disposition: String,
    pub expected_recovered_request_id: Option<String>,
    pub expected_checkpointable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalSubmissionCase {
    pub name: String,
    pub durable_goal: bool,
    pub runnable_request: bool,
    pub staged_goal: bool,
    pub staged_request: bool,
    pub action: String,
    pub expected_durable_goal: bool,
    pub expected_runnable_request: bool,
    pub expected_staged_goal: bool,
    pub expected_staged_request: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalContinuationMaterializationCase {
    pub name: String,
    pub phase: String,
    pub action: String,
    pub expected_phase: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosureDocumentKey {
    pub collection: String,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSessionClosurePayloadRef {
    pub close_id: u64,
    pub stream: u64,
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
pub struct LeanSessionClosureTool {
    pub id: String,
    pub name: String,
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
pub struct LeanSessionClosureAccess {
    pub key: LeanSessionClosureDocumentKey,
    pub state: String,
    pub requester: String,
    pub node: String,
    pub session: String,
    pub native_session: u64,
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
#[serde(deny_unknown_fields)]
pub struct LeanSessionClosureCase {
    pub name: String,
    pub closure_input: LeanSessionClosureInput,
    pub expected_closure_documents: Option<Vec<LeanSessionClosureDocumentKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentCase {
    pub name: String,
    pub steps: Vec<LeanEnrollmentTraceStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentDurableProjectionCase {
    pub name: String,
    pub documents: Vec<LeanEnrollmentTraceStep>,
    pub expected_current_approval: bool,
    pub expected_current_route_receipt: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEnrollmentEncodingCase {
    pub name: String,
    pub value: String,
    pub expected_frame: String,
    pub actual_frame: String,
    pub frame_matches: bool,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScopeCollectionRule {
    pub collection: String,
    pub field: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanKillCase {
    pub observation: String,
    pub action: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCausedFacts {
    pub terminal: bool,
    pub this_runtime: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanKillObservationCase {
    pub caused: Option<LeanCausedFacts>,
    pub observation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCausalHopContract {
    pub default_max_request_hop: Option<u32>,
    pub cap_observation_cases: Vec<LeanCapObservationCase>,
    pub step_cases: Vec<LeanCausalHopStepCase>,
    pub chain_cases: Vec<LeanCausalHopChainCase>,
    pub call_cases: Vec<LeanCausalHopCallCase>,
    pub interrupt_cases: Vec<LeanAgentInterruptCase>,
    pub creation_cases: Vec<LeanSessionCreationCase>,
    pub creation_signer_cases: Vec<LeanSessionCreationSignerCase>,
    pub job_creation_cases: Vec<LeanJobCreationCase>,
    pub deferred_creation_cases: Vec<LeanDeferredCreationCase>,
    pub write_cases: Vec<LeanSessionMessageWriteCase>,
    pub session_target_cases: Vec<LeanSessionTargetCase>,
    pub session_list_cases: Vec<LeanSessionListCase>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanJobCreationCase {
    pub parent: bool,
    pub job: bool,
    pub key: bool,
    pub spawn: bool,
    pub expected: bool,
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
pub struct LeanAgentInterruptCase {
    pub name: String,
    pub caller_session: String,
    pub target_session: String,
    pub target_origin_cause: Option<String>,
    pub same_node: bool,
    pub task_triggered: bool,
    pub expected_allowed: bool,
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
pub struct LeanCausalHopChainStep {
    pub kind: String,
    pub own_predecessor_hop: Option<u32>,
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
pub struct LeanCausalHopCallCase {
    pub name: String,
    pub max_request_hop: Option<u32>,
    pub events: Vec<String>,
    pub expected_hops: Vec<u32>,
    pub expected_admitted: Vec<bool>,
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

mod account_rotation;
mod background_transcript;
mod canonical_execution;
mod canonical_output;
pub mod canonical_presentation;
mod client_session;
mod command_identity_queue;
mod compaction_canonical_projection;
mod compaction_projection_join;
mod composed_invariants;
mod durable_reduction;
mod eval;
mod event_delivery;
mod hooks;
mod optimization;
mod prompt_assembly;
mod publish_if;
mod reduction_engine;
mod rendered_capture;
mod request_execution_lease;
mod rolling_compaction;
mod self_config;
mod session_documents;
mod slot_persistence_health;
mod task_hooks;
mod title;
mod tool_policy;
mod triggers_runtime_apply;

pub use account_rotation::*;
pub use background_transcript::*;
pub use canonical_execution::*;
pub use canonical_output::*;
pub use canonical_presentation::native_adapter::native_presentation;
pub use canonical_presentation::*;
pub use client_session::*;
pub use command_identity_queue::*;
pub use compaction_canonical_projection::*;
pub use compaction_projection_join::*;
pub use composed_invariants::*;
pub use durable_reduction::*;
pub use eval::*;
pub use event_delivery::*;
pub use hooks::*;
pub use optimization::*;
pub use prompt_assembly::*;
pub use publish_if::*;
pub use reduction_engine::*;
pub use rendered_capture::*;
pub use request_execution_lease::*;
pub use rolling_compaction::*;
pub use self_config::*;
pub use session_documents::*;
pub use slot_persistence_health::*;
pub use task_hooks::*;
pub use title::*;
pub use tool_policy::*;
pub use triggers_runtime_apply::*;

#[cfg(test)]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LeanVocabularyParseError<'a> {
    MissingNamespace,
    MissingToDefraDB,
    EmptyToDefraDB,
    MalformedArm {
        line_number: usize,
        line: &'a str,
        reason: &'static str,
    },
}

static LEAN_CONTRACT_SNAPSHOT: OnceLock<LeanContractSnapshot> = OnceLock::new();

pub fn lean_contract_snapshot() -> &'static LeanContractSnapshot {
    LEAN_CONTRACT_SNAPSHOT.get_or_init(load_lean_contract_snapshot)
}

pub fn lean_plugin_job_admission_cases() -> &'static [LeanPluginJobAdmissionCase] {
    &lean_contract_snapshot().plugin_job_admission_cases
}

pub fn lean_plugin_job_adoption_cases() -> &'static [LeanPluginJobAdoptionCase] {
    &lean_contract_snapshot().plugin_job_adoption_cases
}

pub fn lean_plugin_job_control_cases() -> &'static [LeanPluginJobControlCase] {
    &lean_contract_snapshot().plugin_job_control_cases
}

pub fn lean_plugin_job_key_cases() -> &'static [LeanPluginJobKeyCase] {
    &lean_contract_snapshot().plugin_job_key_cases
}

pub fn lean_plugin_job_authority_cases() -> &'static [LeanPluginJobAuthorityCase] {
    &lean_contract_snapshot().plugin_job_authority_cases
}

pub fn lean_plugin_job_wire_cases() -> &'static [LeanPluginJobWireCase] {
    &lean_contract_snapshot().plugin_job_wire_cases
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

pub fn lean_plugin_data_cases() -> &'static LeanPluginDataCases {
    &lean_contract_snapshot().plugin_data_cases
}

pub fn lean_plugin_calls_route_cases() -> &'static [LeanPluginCallsRouteCase] {
    &lean_contract_snapshot().plugin_calls_route_cases
}

/// One strictly decoded group of `plugin_resource_cases`.
fn lean_plugin_resource_group<T>(group: &str) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(lean_contract_snapshot().plugin_resource_cases[group].clone())
        .unwrap_or_else(|error| panic!("plugin_resource_cases.{group}: {error}"))
}

pub fn lean_plugin_host_import_cases() -> &'static [LeanPluginHostImportCase] {
    static CASES: OnceLock<Vec<LeanPluginHostImportCase>> = OnceLock::new();
    CASES.get_or_init(|| lean_plugin_resource_group("host_import"))
}

pub fn lean_plugin_scratch_cases() -> &'static [LeanPluginScratchCase] {
    static CASES: OnceLock<Vec<LeanPluginScratchCase>> = OnceLock::new();
    CASES.get_or_init(|| lean_plugin_resource_group("scratch"))
}

pub fn lean_workspace_cases() -> &'static [LeanWorkspaceCase] {
    &lean_contract_snapshot().workspace_cases
}

pub fn lean_workspace_binding_cases() -> &'static [LeanWorkspaceBindingCase] {
    &lean_contract_snapshot().workspace_binding_cases
}

pub fn lean_callback_retry_cases() -> &'static [LeanCallbackRetryCase] {
    &lean_contract_snapshot().callback_retry_cases
}

pub fn lean_callback_recovery_cases() -> &'static [LeanCallbackRecoveryCase] {
    &lean_contract_snapshot().callback_recovery_cases
}

pub fn lean_callback_cases() -> &'static [LeanCallbackCase] {
    &lean_contract_snapshot().callback_cases
}

pub fn lean_vocabulary_contract(domain: &str) -> &'static LeanVocabularyContract {
    lean_contract_snapshot()
        .vocabularies
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean vocabulary contract {domain:?} was not emitted"))
}

pub fn lean_state_machine_contract(domain: &str) -> &'static LeanStateMachineContract {
    lean_contract_snapshot()
        .state_machines
        .iter()
        .find(|contract| contract.domain == domain)
        .unwrap_or_else(|| panic!("Lean state-machine contract {domain:?} was not emitted"))
}

pub fn lean_feature_surface_requirements() -> &'static [LeanFeatureSurfaceRequirement] {
    &lean_contract_snapshot().feature_surface_requirements
}

pub fn lean_feature_matrix() -> &'static LeanFeatureMatrix {
    &lean_contract_snapshot().feature_matrix
}

pub fn lean_request_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().request_transition_cases
}

pub fn lean_process_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().process_transition_cases
}

pub fn lean_provider_eof_cases() -> &'static [LeanProviderEofCase] {
    &lean_contract_snapshot().provider_eof_cases
}

pub fn lean_task_hook_admission_cases() -> &'static [LeanTaskHookAdmissionCase] {
    &lean_contract_snapshot().task_hook_admission_cases
}

pub fn lean_task_hook_run_cases() -> &'static [LeanTaskHookRunCase] {
    &lean_contract_snapshot().task_hook_run_cases
}

pub fn lean_task_hook_recovery_cases() -> &'static [LeanTaskHookRecoveryCase] {
    &lean_contract_snapshot().task_hook_recovery_cases
}

pub fn lean_lifecycle_hook_effect_cases() -> &'static [LeanLifecycleHookEffectCase] {
    &lean_contract_snapshot().lifecycle_hook_effect_cases
}

pub fn lean_lifecycle_hook_stage_cases() -> &'static [LeanLifecycleHookStageCase] {
    &lean_contract_snapshot().lifecycle_hook_stage_cases
}

pub fn lean_lifecycle_hook_must_commit_cases() -> &'static [LeanLifecycleHookMustCommitCase] {
    &lean_contract_snapshot().lifecycle_hook_must_commit_cases
}

pub fn lean_lifecycle_hook_combine_cases() -> &'static [LeanLifecycleHookCombineCase] {
    &lean_contract_snapshot().lifecycle_hook_combine_cases
}

pub fn lean_lifecycle_hook_gate_cases() -> &'static [LeanLifecycleHookGateCase] {
    &lean_contract_snapshot().lifecycle_hook_gate_cases
}

pub fn lean_lifecycle_hook_queue_scope_cases() -> &'static [LeanLifecycleHookQueueScopeCase] {
    &lean_contract_snapshot().lifecycle_hook_queue_scope_cases
}

pub fn lean_lifecycle_hook_delivery_cases() -> &'static [LeanLifecycleHookDeliveryCase] {
    &lean_contract_snapshot().lifecycle_hook_delivery_cases
}

pub fn lean_lifecycle_hook_owed_delivery_cases() -> &'static [LeanLifecycleHookOwedDeliveryCase] {
    &lean_contract_snapshot().lifecycle_hook_owed_delivery_cases
}

pub fn lean_lifecycle_hook_admit_cases() -> &'static [LeanLifecycleHookAdmitCase] {
    &lean_contract_snapshot().lifecycle_hook_admit_cases
}

pub fn lean_lifecycle_hook_grant_cases() -> &'static [LeanLifecycleHookGrantCase] {
    &lean_contract_snapshot().lifecycle_hook_grant_cases
}

pub fn lean_request_execution_lease_cases() -> &'static [LeanRequestExecutionLeaseCase] {
    &lean_contract_snapshot().request_execution_lease_cases
}

pub fn lean_request_execution_lease_observed_cases() -> &'static [LeanRequestExecutionObservedCase]
{
    &lean_contract_snapshot().request_execution_lease_observed_cases
}

pub fn lean_request_execution_lease_trace_cases() -> &'static [LeanRequestExecutionLeaseTraceCase] {
    &lean_contract_snapshot().request_execution_lease_trace_cases
}

pub fn lean_startup_readiness_cases() -> &'static [LeanStartupReadinessCase] {
    &lean_contract_snapshot().startup_readiness_cases
}

pub fn lean_readiness_publication_cases() -> &'static [LeanReadinessPublicationCase] {
    &lean_contract_snapshot().readiness_publication_cases
}

pub fn lean_runtime_reconcile_cases() -> &'static [LeanRuntimeReconcileCase] {
    &lean_contract_snapshot().runtime_reconcile_cases
}

pub fn lean_client_agent_readiness_cases() -> &'static [LeanClientAgentReadinessCase] {
    &lean_contract_snapshot().client_agent_readiness_cases
}

pub fn lean_runtime_reconcile_case(name: &str) -> &'static LeanRuntimeReconcileCase {
    lean_contract_snapshot()
        .runtime_reconcile_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean runtime-reconcile case {name:?} was not emitted"))
}

pub fn lean_apply_reconcile_cases() -> &'static [LeanApplyReconcileCase] {
    &lean_contract_snapshot().apply_reconcile_cases
}

pub fn lean_apply_reconcile_case(name: &str) -> &'static LeanApplyReconcileCase {
    lean_contract_snapshot()
        .apply_reconcile_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean apply-reconcile case {name:?} was not emitted"))
}

pub fn lean_eval_outcome_cases() -> &'static [LeanEvalOutcomeCase] {
    &lean_contract_snapshot().eval_outcome_cases
}

pub fn lean_eval_document_capture_cases() -> &'static [LeanEvalDocumentCaptureCase] {
    &lean_contract_snapshot().eval_document_capture_cases
}

pub fn lean_optimization_cases() -> &'static LeanOptimizationCases {
    &lean_contract_snapshot().optimization_cases
}

pub fn lean_publish_if_cases() -> &'static [LeanPublishIfCase] {
    &lean_contract_snapshot().publish_if_cases
}

pub fn lean_tool_policy_cases() -> &'static [LeanToolPolicyCase] {
    &lean_contract_snapshot().tool_policy_cases
}

pub fn lean_write_input_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().write_input_cases
}

pub fn lean_goal_capability_resolution_cases() -> &'static [LeanGoalCapabilityResolutionCase] {
    &lean_contract_snapshot().goal_capability_resolution_cases
}

pub fn lean_lsp_action_cases() -> &'static [LeanLspActionCase] {
    &lean_contract_snapshot().lsp_action_cases
}

pub fn lean_self_config_field_tables() -> &'static [LeanSelfConfigFieldTable] {
    &lean_contract_snapshot().self_config_field_tables
}

pub fn lean_self_config_cases() -> &'static [LeanSelfConfigCase] {
    &lean_contract_snapshot().self_config_cases
}

pub fn lean_agent_decision_cases() -> &'static [LeanAgentDecisionCase] {
    &lean_contract_snapshot().agent_decision_cases
}

pub fn lean_agent_materialization_cases() -> &'static [LeanAgentMaterializationCase] {
    &lean_contract_snapshot().agent_materialization_cases
}

pub fn lean_self_config_selection_cases() -> &'static LeanSelfConfigSelectionCases {
    &lean_contract_snapshot().self_config_selection_cases
}

pub fn lean_sibling_tools_cases() -> &'static [LeanSiblingToolsCase] {
    &lean_contract_snapshot().sibling_tools_cases
}

pub fn lean_tool_policy_case(name: &str) -> &'static LeanToolPolicyCase {
    lean_tool_policy_cases()
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean tool-policy case {name:?} was not emitted"))
}

pub fn lean_session_recovery_case(name: &str) -> &'static LeanSessionRecoveryCase {
    lean_contract_snapshot()
        .session_recovery_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean session-recovery case {name:?} was not emitted"))
}

pub fn lean_client_shell_case(name: &str) -> &'static LeanClientShellCase {
    lean_contract_snapshot()
        .frontend_client_shell_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean ClientShell case {name:?} was not emitted"))
}

pub fn lean_desktop_client_shell_cases() -> &'static [LeanClientShellCase] {
    &lean_contract_snapshot().desktop_client_shell_cases
}

pub fn lean_client_session_turn_cases() -> &'static [LeanSessionTurnCase] {
    &lean_contract_snapshot().client_session_turn_cases
}

pub fn lean_client_session_queue_order_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().client_session_queue_order_cases
}

pub fn lean_request_lifecycle_operator_ui_cases() -> &'static [LeanClientShellCase] {
    &lean_contract_snapshot().request_lifecycle_operator_ui_cases
}

pub fn lean_inference_slot_accounting_cases() -> &'static [LeanInferenceSlotAccountingCase] {
    &lean_contract_snapshot().inference_slot_accounting_cases
}

pub fn lean_inference_slot_accounting_case(name: &str) -> &'static LeanInferenceSlotAccountingCase {
    lean_contract_snapshot()
        .inference_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean inference slot-accounting case {name:?} was not emitted"))
}

pub fn lean_fleet_slot_accounting_case(name: &str) -> &'static LeanFleetSlotAccountingCase {
    lean_contract_snapshot()
        .fleet_slot_accounting_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean fleet slot-accounting case {name:?} was not emitted"))
}

pub fn lean_persistence_failure_policy_cases() -> &'static [LeanPersistenceFailurePolicyCase] {
    &lean_contract_snapshot().persistence_failure_policy_cases
}

pub fn lean_storage_observation_runtime_cases() -> &'static [LeanStorageObservationRuntimeCase] {
    &lean_contract_snapshot().storage_observation_runtime_cases
}

pub fn lean_backend_health_admission_cases() -> &'static [LeanBackendHealthAdmissionCase] {
    &lean_contract_snapshot().backend_health_admission_cases
}

pub fn lean_native_filesystem_boundary_cases() -> &'static [LeanNativeFilesystemBoundaryCase] {
    &lean_contract_snapshot().native_filesystem_boundary_cases
}

pub fn lean_managed_exec_tool_boundary_cases() -> &'static [LeanManagedExecToolBoundaryCase] {
    &lean_contract_snapshot().managed_exec_tool_boundary_cases
}

pub fn lean_pairing_reconcile_cases() -> &'static [LeanPairingReconcileCase] {
    &lean_contract_snapshot().pairing_reconcile_cases
}

pub fn lean_pairing_reconcile_shutdown_boundary_cases(
) -> &'static [LeanPairingReconcileShutdownBoundaryCase] {
    &lean_contract_snapshot().pairing_reconcile_shutdown_boundary_cases
}

pub fn lean_pairing_reconcile_sweep_retry_boundary_cases(
) -> &'static [LeanPairingReconcileSweepRetryBoundaryCase] {
    &lean_contract_snapshot().pairing_reconcile_sweep_retry_boundary_cases
}

pub fn lean_pairing_reconcile_sweep_scheduling_cases(
) -> &'static [LeanPairingReconcileSweepSchedulingCase] {
    &lean_contract_snapshot().pairing_reconcile_sweep_scheduling_cases
}

pub fn lean_process_stop_cases() -> &'static [LeanProcessStopCase] {
    &lean_contract_snapshot().process_stop_cases
}

pub fn lean_managed_exec_liveness_cases() -> &'static [LeanManagedExecLivenessCase] {
    &lean_contract_snapshot().managed_exec_liveness_cases
}

pub fn lean_tool_preflight_cases() -> &'static [LeanToolPreflightCase] {
    &lean_contract_snapshot().tool_preflight_cases
}

pub fn lean_completion_retry_cases() -> &'static [LeanCompletionRetryCase] {
    &lean_contract_snapshot().completion_retry_cases
}

pub fn lean_queue_deadline_cases() -> &'static [LeanQueueDeadlineConformanceCase] {
    &lean_contract_snapshot().queue_deadline_conformance_cases
}

pub fn lean_queue_deadline_case(name: &str) -> &'static LeanQueueDeadlineConformanceCase {
    lean_contract_snapshot()
        .queue_deadline_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean queue/deadline case {name:?} was not emitted"))
}

pub fn lean_causal_hop_contract() -> &'static LeanCausalHopContract {
    &lean_contract_snapshot().causal_hop_contract
}

pub fn lean_recovery_sweep_cases() -> &'static [LeanRecoverySweepCase] {
    &lean_contract_snapshot().recovery_sweep_cases
}

pub fn lean_recovery_sweep_case(name: &str) -> &'static LeanRecoverySweepCase {
    lean_contract_snapshot()
        .recovery_sweep_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean recovery sweep case {name:?} was not emitted"))
}

pub fn lean_restart_disposition_cases() -> &'static [LeanRestartDispositionCase] {
    &lean_contract_snapshot().restart_disposition_cases
}

pub fn lean_r4c_background_work_cases() -> &'static [LeanR4cBackgroundWorkCase] {
    &lean_contract_snapshot().r4c_background_work_cases
}

pub fn lean_tool_output_paging_cases() -> &'static [LeanToolOutputPagingCase] {
    &lean_contract_snapshot().tool_output_paging_cases
}

pub fn lean_interrupt_disposition_cases() -> &'static [LeanInterruptDispositionCase] {
    &lean_contract_snapshot().interrupt_disposition_cases
}

pub fn lean_r4c_background_work_case(witness: &str) -> &'static LeanR4cBackgroundWorkCase {
    lean_contract_snapshot()
        .r4c_background_work_cases
        .iter()
        .find(|case| case.witness() == witness)
        .unwrap_or_else(|| panic!("Lean R4c background-work witness {witness:?} was not emitted"))
}

pub fn lean_r6_backgrounding_cases() -> &'static [LeanR6BackgroundingCase] {
    &lean_contract_snapshot().r6_backgrounding_cases
}

pub fn lean_r6_backgrounding_case(name: &str) -> &'static LeanR6BackgroundingCase {
    lean_contract_snapshot()
        .r6_backgrounding_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean R6 backgrounding case {name:?} was not emitted"))
}

pub fn lean_composed_invariant_witnesses() -> &'static [LeanComposedInvariantWitness] {
    &lean_contract_snapshot().composed_invariant_witnesses
}

pub fn lean_composed_invariant_witness(
    theorem_name: &str,
) -> &'static LeanComposedInvariantWitness {
    lean_contract_snapshot()
        .composed_invariant_witnesses
        .iter()
        .find(|witness| witness.theorem_name == theorem_name)
        .unwrap_or_else(|| {
            panic!("Lean composed invariant witness {theorem_name:?} was not emitted")
        })
}

pub fn lean_composed_invariant_witness_by_scenario(
    scenario: &str,
) -> &'static LeanComposedInvariantWitness {
    lean_contract_snapshot()
        .composed_invariant_witnesses
        .iter()
        .find(|witness| witness.scenario == scenario)
        .unwrap_or_else(|| {
            panic!("Lean composed invariant witness scenario {scenario:?} was not emitted")
        })
}

pub fn lean_r6_background_theorem_witnesses() -> &'static [LeanBackgroundTheoremWitness] {
    &lean_contract_snapshot().r6_background_theorem_witnesses
}

pub fn lean_r6_background_theorem_witness(
    theorem_name: &str,
) -> &'static LeanBackgroundTheoremWitness {
    lean_contract_snapshot()
        .r6_background_theorem_witnesses
        .iter()
        .find(|witness| witness.theorem_name == theorem_name)
        .unwrap_or_else(|| {
            panic!("Lean R6 background theorem witness {theorem_name:?} was not emitted")
        })
}

pub fn lean_transcript_cases() -> &'static [LeanTranscriptCase] {
    &lean_contract_snapshot().transcript_conformance_cases
}

pub fn lean_transcript_case(name: &str) -> &'static LeanTranscriptCase {
    lean_contract_snapshot()
        .transcript_conformance_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean transcript case {name:?} was not emitted"))
}

pub fn lean_canonical_output_projection_cases() -> &'static [LeanCanonicalOutputProjectionCase] {
    &lean_contract_snapshot().canonical_output_projection_cases
}

pub fn lean_reasoning_audit_cases() -> &'static [LeanReasoningAuditCase] {
    &lean_contract_snapshot().reasoning_audit_cases
}

pub fn lean_reasoning_signature_cases() -> &'static [LeanReasoningSignatureCase] {
    &lean_contract_snapshot().reasoning_signature_cases
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanReasoningDeltaPart {
    Reasoning {
        id: Option<usize>,
        signature: Option<usize>,
        bytes: Vec<u8>,
    },
    Text {
        bytes: Vec<u8>,
    },
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

pub fn lean_reasoning_delta_cases() -> &'static [LeanReasoningDeltaCase] {
    &lean_contract_snapshot().reasoning_delta_cases
}

pub fn lean_auxiliary_output_cases() -> &'static [LeanAuxiliaryOutputCase] {
    &lean_contract_snapshot().auxiliary_output_cases
}

pub fn lean_canonical_execution_gate_cases() -> &'static [LeanCanonicalExecutionCase] {
    &lean_contract_snapshot().canonical_execution_gate_cases
}

pub fn lean_canonical_dispatch_observation_cases() -> &'static [LeanDispatchObservationCase] {
    &lean_contract_snapshot().canonical_dispatch_observation_cases
}

pub fn lean_canonical_spawned_target_rejection_cases() -> &'static [LeanSpawnedTargetRejectionCase]
{
    &lean_contract_snapshot().canonical_spawned_target_rejection_cases
}

pub fn lean_interrupt_queue_cases() -> &'static [LeanInterruptQueueCase] {
    &lean_contract_snapshot().interrupt_queue_cases
}

pub fn lean_session_control_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().session_control_cases
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanInterruptQueueInput {
    Interrupt,
    CaptureInterrupt,
    CommitInterrupt,
    Enqueue { entry: LeanInterruptQueueEntry },
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
#[serde(deny_unknown_fields)]
pub struct LeanInterruptQueueObservation {
    pub pending: Vec<u64>,
    pub terminal: Vec<u64>,
    pub latched: bool,
}

pub fn lean_fold_queue_cases() -> &'static [LeanFoldQueueCase] {
    &lean_contract_snapshot().fold_queue_cases
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanFoldQueueInput {
    Enqueue { entry: LeanFoldQueueEntry },
    Claim { admitted: Vec<u64> },
    Consume,
    Finish,
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
#[serde(deny_unknown_fields)]
pub struct LeanFoldQueueObservation {
    pub active: Option<u64>,
    pub pending: Vec<u64>,
    pub folding: Vec<u64>,
    pub terminal: Vec<u64>,
    pub claims: Vec<LeanFoldQueueClaim>,
}

pub fn lean_fold_turn_input_cases() -> &'static [LeanFoldTurnInputCase] {
    &lean_contract_snapshot().fold_turn_input_cases
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldTurnInputAuthored {
    pub key: LeanFoldAuthoredKey,
    pub content: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanFoldAuthoredKey {
    Context,
    Prompt,
    Folded { request_id: u64 },
}

pub fn lean_retry_selection_cases() -> &'static [LeanRetrySelectionCase] {
    &lean_contract_snapshot().retry_selection_cases
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

pub fn lean_queue_management_cases() -> &'static [LeanQueueManagementCase] {
    &lean_contract_snapshot().queue_management_cases
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
#[serde(deny_unknown_fields)]
pub struct LeanQueueManagementState {
    pub active: Option<u64>,
    pub pending: Vec<LeanFoldQueueEntry>,
    pub folding: Vec<LeanFoldQueueEntry>,
    pub terminal: Vec<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct QueueImage {
    pub active: Option<String>,
    pub folding: Vec<String>,
    pub pending: Vec<String>,
    pub terminal: BTreeSet<String>,
}

impl LeanQueueManagementState {
    pub fn image(&self, native: impl Fn(u64) -> String) -> QueueImage {
        let ids = |entries: &[LeanFoldQueueEntry]| {
            entries
                .iter()
                .map(|entry| native(entry.request_id))
                .collect()
        };
        QueueImage {
            active: self.active.map(&native),
            folding: ids(&self.folding),
            pending: ids(&self.pending),
            terminal: self.terminal.iter().copied().map(&native).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanQueueManagementOperation {
    Intake {
        active: LeanFoldQueueEntry,
        admitted: Vec<u64>,
        safe_boundary: bool,
    },
    Replace {
        caller: Option<u64>,
        expected: Vec<u64>,
        offset: usize,
        count: usize,
        replacements: Vec<LeanFoldQueueEntry>,
    },
}

pub fn lean_steering_publication_cases() -> &'static [LeanFoldPublicationCase] {
    &lean_contract_snapshot().steering_publication_cases
}

pub fn lean_fold_publication_cases() -> &'static [LeanFoldPublicationCase] {
    &lean_contract_snapshot().fold_publication_cases
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

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanFoldPublicationStep {
    ObserveDeadline {
        now: u64,
        deadline: u64,
    },
    EnqueueSteering {
        request_id: u64,
    },
    Intake {
        generation: u64,
        safe_boundary: bool,
    },
    FinishOrIntake {
        generation: u64,
        safe_boundary: bool,
    },
    CancelFirstPending,
    PublishPrompt {
        generation: u64,
    },
    PublishChangedPrompt {
        generation: u64,
    },
    PublishFolded {
        generation: u64,
        request_id: u64,
    },
    AcceptTurn {
        generation: u64,
    },
    Recover {
        expected: u64,
        fresh: u64,
    },
    Terminalize {
        generation: u64,
    },
    Finish,
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

pub fn lean_handover_fold_cases() -> &'static [LeanHandoverFoldCase] {
    &lean_contract_snapshot().handover_fold_cases
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFoldQueueClaim {
    pub head: u64,
    pub folded: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkerCapacityCase {
    pub name: String,
    pub pre: LeanWorkerCapacityState,
    pub operation: LeanWorkerCapacityOperation,
    pub expected: Option<LeanWorkerCapacityState>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkerCapacityState {
    pub active_limit: usize,
    pub active: Vec<[u64; 2]>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanWorkerCapacityOperation {
    Acquire { ticket: [u64; 2] },
    Release { ticket: [u64; 2] },
}

pub fn lean_canonical_worker_capacity_cases() -> &'static [LeanWorkerCapacityCase] {
    &lean_contract_snapshot().canonical_worker_capacity_cases
}

pub fn lean_canonical_payload_presentation_cases() -> &'static [LeanPayloadPresentationCase] {
    &lean_contract_snapshot().canonical_payload_presentation_cases
}

pub fn lean_terminal_diagnostic_presentation_cases(
) -> &'static [LeanTerminalDiagnosticPresentationCase] {
    &lean_contract_snapshot().terminal_diagnostic_presentation_cases
}

pub fn lean_terminal_diagnostic_replay_cases() -> &'static [LeanTerminalDiagnosticReplayCase] {
    &lean_contract_snapshot().terminal_diagnostic_replay_cases
}

pub fn lean_compaction_reducer_cases() -> &'static [LeanCompactionReducerCase] {
    &lean_contract_snapshot().compaction_reducer_cases
}

pub fn lean_compaction_cursor_cases() -> &'static [LeanCompactionCursorCase] {
    &lean_contract_snapshot().compaction_cursor_cases
}

pub fn lean_prompt_assembly_sanitize_cases() -> &'static [LeanPromptAssemblySanitizeCase] {
    &lean_contract_snapshot().prompt_assembly_sanitize_cases
}

pub fn lean_prompt_assembly_assistant_order_cases(
) -> &'static [LeanPromptAssemblyAssistantOrderCase] {
    &lean_contract_snapshot().prompt_assembly_assistant_order_cases
}

pub fn lean_prompt_assembly_mode_sanitize_cases() -> &'static [LeanPromptAssemblyModeSanitizeCase] {
    &lean_contract_snapshot().prompt_assembly_mode_sanitize_cases
}

pub fn lean_prompt_assembly_layer_cases() -> &'static [LeanPromptAssemblyLayerCase] {
    &lean_contract_snapshot().prompt_assembly_layer_cases
}

pub fn lean_prompt_assembly_repair_cases() -> &'static [LeanPromptAssemblyRepairCase] {
    &lean_contract_snapshot().prompt_assembly_repair_cases
}

pub fn lean_prompt_assembly_budget_cases() -> &'static [LeanPromptAssemblyBudgetCase] {
    &lean_contract_snapshot().prompt_assembly_budget_cases
}

pub fn lean_prompt_assembly_turn_budget_cases() -> &'static [LeanPromptAssemblyTurnBudgetCase] {
    &lean_contract_snapshot().prompt_assembly_turn_budget_cases
}

pub fn lean_prompt_assembly_compaction_trigger_cases(
) -> &'static [LeanPromptAssemblyCompactionTriggerCase] {
    &lean_contract_snapshot().prompt_assembly_compaction_trigger_cases
}

pub fn lean_prompt_assembly_retention_cases() -> &'static [LeanPromptAssemblyRetentionCase] {
    &lean_contract_snapshot().prompt_assembly_retention_cases
}

pub fn lean_prompt_assembly_claude_map_cases() -> &'static [LeanPromptAssemblyClaudeMapCase] {
    &lean_contract_snapshot().prompt_assembly_claude_map_cases
}

pub fn lean_prompt_assembly_claude_body_cases() -> &'static [LeanPromptAssemblyClaudeBodyCase] {
    &lean_contract_snapshot().prompt_assembly_claude_body_cases
}

pub fn lean_prompt_assembly_claude_attribution_cases(
) -> &'static [LeanPromptAssemblyClaudeAttributionCase] {
    &lean_contract_snapshot().prompt_assembly_claude_attribution_cases
}

pub fn lean_prompt_assembly_claude_cache_cases() -> &'static [LeanPromptAssemblyClaudeCacheCase] {
    &lean_contract_snapshot().prompt_assembly_claude_cache_cases
}

pub fn lean_prompt_assembly_claude_stream_cases() -> &'static [LeanPromptAssemblyClaudeStreamCase] {
    &lean_contract_snapshot().prompt_assembly_claude_stream_cases
}

pub fn lean_prompt_assembly_claude_thinking_stream_cases(
) -> &'static [LeanPromptAssemblyClaudeThinkingStreamCase] {
    &lean_contract_snapshot().prompt_assembly_claude_thinking_stream_cases
}

pub fn lean_prompt_assembly_claude_wire_start_cases(
) -> &'static [LeanPromptAssemblyClaudeWireStartCase] {
    &lean_contract_snapshot().prompt_assembly_claude_wire_start_cases
}

pub fn lean_prompt_assembly_responses_storage_cases(
) -> &'static [LeanPromptAssemblyResponsesStorageCase] {
    &lean_contract_snapshot().prompt_assembly_responses_storage_cases
}

pub fn lean_prompt_assembly_responses_effort_cases(
) -> &'static [LeanPromptAssemblyResponsesEffortCase] {
    &lean_contract_snapshot().prompt_assembly_responses_effort_cases
}

pub fn lean_prompt_assembly_claude_replay_cases() -> &'static [LeanPromptAssemblyClaudeReplayCase] {
    &lean_contract_snapshot().prompt_assembly_claude_replay_cases
}

pub fn lean_prompt_assembly_claude_checkpoint_cases(
) -> &'static [LeanPromptAssemblyClaudeCheckpointCase] {
    &lean_contract_snapshot().prompt_assembly_claude_checkpoint_cases
}

pub fn lean_prompt_assembly_reasoning_suffix_cases(
) -> &'static [LeanPromptAssemblyReasoningSuffixCase] {
    &lean_contract_snapshot().prompt_assembly_reasoning_suffix_cases
}

pub fn lean_prompt_assembly_replay_shape_cases() -> &'static [LeanPromptAssemblyReplayShapeCase] {
    &lean_contract_snapshot().prompt_assembly_replay_shape_cases
}

pub fn lean_prompt_assembly_replay_prefix_cases() -> &'static [LeanPromptAssemblyReplayPrefixCase] {
    &lean_contract_snapshot().prompt_assembly_replay_prefix_cases
}

pub fn lean_protected_replay_compaction_cases() -> &'static [LeanProtectedReplayCompactionCase] {
    &lean_contract_snapshot().protected_replay_compaction_cases
}

pub fn lean_rendered_capture_cases() -> &'static [LeanRenderedCaptureCase] {
    &lean_contract_snapshot().rendered_capture_cases
}

pub fn lean_rendered_capture_storage_cases() -> &'static [LeanRenderedCaptureStorageCase] {
    &lean_contract_snapshot().rendered_capture_storage_cases
}

pub fn lean_durable_reduction_cases() -> &'static [LeanDurableReductionCase] {
    &lean_contract_snapshot().durable_reduction_cases
}

pub fn lean_rolling_compaction_cases() -> &'static [LeanRollingCompactionCase] {
    &lean_contract_snapshot().rolling_compaction_cases
}

pub fn lean_reduction_engine_cases() -> &'static [LeanReductionEngineCase] {
    &lean_contract_snapshot().reduction_engine_cases
}

pub fn lean_compaction_projection_join_cases() -> &'static [LeanCompactionProjectionJoinCase] {
    &lean_contract_snapshot().compaction_projection_join_cases
}

pub fn lean_compaction_canonical_projection_cases() -> &'static [LeanCanonicalCompactionCase] {
    &lean_contract_snapshot().compaction_canonical_projection_cases
}

pub fn lean_repaired_projection_admission_cases() -> &'static [LeanRepairedProjectionCase] {
    &lean_contract_snapshot().repaired_projection_admission_cases
}

pub fn lean_rendered_capture_key_cases() -> &'static [LeanRenderedCaptureKeyCase] {
    &lean_contract_snapshot().rendered_capture_key_cases
}

pub fn lean_capture_scope_cases() -> &'static [LeanCaptureScopeCase] {
    &lean_contract_snapshot().capture_scope_cases
}

pub fn lean_capture_order_cases() -> &'static [LeanCaptureOrderCase] {
    &lean_contract_snapshot().capture_order_cases
}

pub fn lean_aggregate_token_budget_cases() -> &'static [LeanAggregateTokenBudgetCase] {
    &lean_contract_snapshot().aggregate_token_budget_cases
}

pub fn lean_compaction_reducer_case(name: &str) -> &'static LeanCompactionReducerCase {
    lean_contract_snapshot()
        .compaction_reducer_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean compaction-reducer case {name:?} was not emitted"))
}

pub fn lean_event_delivery_transition_cases() -> &'static [LeanEventDeliveryTransitionCase] {
    &lean_contract_snapshot().event_delivery_transition_cases
}

pub fn lean_event_delivery_source_instances() -> &'static [LeanEventDeliverySourceInstance] {
    &lean_contract_snapshot().event_delivery_source_instances
}

pub fn lean_event_delivery_convergence_traces() -> &'static [LeanEventDeliveryConvergenceTrace] {
    &lean_contract_snapshot().event_delivery_convergence_traces
}

pub fn lean_tool_retry_case(name: &str) -> &'static LeanToolRetryCase {
    lean_contract_snapshot()
        .tool_retry_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean tool retry case {name:?} was not emitted"))
}

pub fn lean_mcp_health_cases() -> &'static [LeanMcpHealthCase] {
    &lean_contract_snapshot().mcp_health_cases
}

pub fn lean_backend_health_cases() -> &'static [LeanBackendHealthCase] {
    &lean_contract_snapshot().backend_health_cases
}

pub fn lean_backend_probe_schedule_cases() -> &'static [LeanBackendProbeScheduleCase] {
    &lean_contract_snapshot().backend_probe_schedule_cases
}

pub fn lean_command_policy_cases() -> &'static [LeanCommandPolicyCase] {
    &lean_contract_snapshot().command_policy_cases
}

pub fn lean_command_policy_case(name: &str) -> &'static LeanCommandPolicyCase {
    lean_contract_snapshot()
        .command_policy_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command policy case {name:?} was not emitted"))
}

pub fn lean_command_sandbox_cases() -> &'static [LeanCommandSandboxCase] {
    &lean_contract_snapshot().command_sandbox_cases
}

pub fn lean_command_sandbox_case(name: &str) -> &'static LeanCommandSandboxCase {
    lean_contract_snapshot()
        .command_sandbox_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command sandbox case {name:?} was not emitted"))
}

pub fn lean_command_env_cases() -> &'static [LeanCommandEnvCase] {
    &lean_contract_snapshot().command_env_cases
}

pub fn lean_command_env_case(name: &str) -> &'static LeanCommandEnvCase {
    lean_contract_snapshot()
        .command_env_cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("Lean command env case {name:?} was not emitted"))
}

pub fn lean_identity_structural_cases() -> &'static [LeanIdentityStructuralCase] {
    &lean_contract_snapshot().identity_structural_cases
}

pub fn lean_identity_permission_cases() -> &'static [LeanIdentityPermissionCase] {
    &lean_contract_snapshot().identity_permission_cases
}

pub fn lean_identity_household_cases() -> &'static [LeanIdentityHouseholdCase] {
    &lean_contract_snapshot().identity_household_cases
}

pub fn lean_identity_derivation_cases() -> &'static [LeanIdentityDerivationCase] {
    &lean_contract_snapshot().identity_derivation_cases
}

pub fn lean_identity_contracts() -> &'static [LeanIdentityContract] {
    &lean_contract_snapshot().identity_contracts
}

pub fn lean_live_overlay_cases() -> &'static [LeanLiveOverlayCase] {
    &lean_contract_snapshot().live_overlay_cases
}

pub fn lean_request_progress_cases() -> &'static [LeanRequestProgressCase] {
    &lean_contract_snapshot().request_progress_cases
}

pub fn lean_pending_user_turn_cases() -> &'static [LeanPendingUserTurnCase] {
    &lean_contract_snapshot().pending_user_turn_cases
}

pub fn lean_queued_steering_trace_cases() -> &'static [LeanQueuedSteeringTraceCase] {
    &lean_contract_snapshot().queued_steering_trace_cases
}

pub fn lean_queued_steering_guard_cases() -> &'static [LeanQueuedSteeringGuardCase] {
    &lean_contract_snapshot().queued_steering_guard_cases
}

pub fn lean_vocabulary_values(domain: &str) -> Vec<&'static str> {
    lean_vocabulary_contract(domain)
        .values
        .iter()
        .map(String::as_str)
        .collect()
}

pub fn lean_trigger_dispatch_cases() -> &'static [LeanTriggerDispatchCase] {
    &lean_contract_snapshot().trigger_dispatch_cases
}

pub fn lean_goal_decision_cases() -> &'static [LeanGoalDecisionCase] {
    &lean_contract_snapshot().goal_decision_cases
}

pub fn lean_goal_readiness_gate_cases() -> &'static [LeanGoalReadinessGateCase] {
    &lean_contract_snapshot().goal_readiness_gate_cases
}

pub fn lean_goal_transition_cases() -> &'static [LeanGoalTransitionCase] {
    &lean_contract_snapshot().goal_transition_cases
}

pub fn lean_goal_create_cases() -> &'static [LeanGoalCreateCase] {
    &lean_contract_snapshot().goal_create_cases
}

pub fn lean_task_goal_publication_cases() -> &'static [LeanTaskGoalPublicationCase] {
    &lean_contract_snapshot().task_goal_publication_cases
}

pub fn lean_task_goal_recovery_cases() -> &'static [LeanTaskGoalRecoveryCase] {
    &lean_contract_snapshot().task_goal_recovery_cases
}

pub fn lean_goal_submission_cases() -> &'static [LeanGoalSubmissionCase] {
    &lean_contract_snapshot().goal_submission_cases
}

pub fn lean_goal_continuation_materialization_cases(
) -> &'static [LeanGoalContinuationMaterializationCase] {
    &lean_contract_snapshot().goal_continuation_materialization_cases
}

pub fn lean_file_inventory_closure_cases() -> &'static [LeanSessionClosureCase] {
    &lean_contract_snapshot().file_inventory_closure_cases
}

pub fn lean_enrollment_cases() -> &'static [LeanEnrollmentCase] {
    &lean_contract_snapshot().enrollment_cases
}

pub fn lean_enrollment_durable_projection_cases() -> &'static [LeanEnrollmentDurableProjectionCase]
{
    &lean_contract_snapshot().enrollment_durable_projection_cases
}

pub fn lean_enrollment_encoding_cases() -> &'static [LeanEnrollmentEncodingCase] {
    &lean_contract_snapshot().enrollment_encoding_cases
}

pub fn lean_enrollment_digest_cases() -> &'static [LeanEnrollmentDigestCase] {
    &lean_contract_snapshot().enrollment_digest_cases
}

pub fn lean_agent_request_admission_cases() -> &'static [LeanAgentRequestAdmissionCase] {
    &lean_contract_snapshot().agent_request_admission_cases
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

pub fn lean_title_admission_join_cases() -> &'static [LeanTitleAdmissionJoinCase] {
    &lean_contract_snapshot().title_admission_join_cases
}

pub fn lean_trigger_dispatch_case_count() -> usize {
    lean_contract_snapshot().trigger_dispatch_case_count
}

pub fn lean_event_group_cases() -> &'static [LeanEventGroupCase] {
    &lean_contract_snapshot().event_group_cases
}

pub fn lean_event_group_case_count() -> usize {
    lean_contract_snapshot().event_group_case_count
}

pub fn assert_lean_contract_vocabulary_matches(spec: LeanContractVocabulary<'_>) {
    let lean_values = lean_vocabulary_values(spec.domain);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        spec.rust_values == lean_values.as_slice()
            && missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean vocabulary contract mismatch\n  Lean contract domain: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.domain,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

pub fn assert_lean_contract_vocabulary_set_matches(spec: LeanContractVocabulary<'_>) {
    let lean_values = lean_vocabulary_values(spec.domain);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean vocabulary contract set mismatch\n  Lean contract domain: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.domain,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

pub fn assert_lean_transition_is_legal(domain: &str, from: &str, to: &str) {
    let machine = lean_state_machine_contract(domain);
    assert!(
        machine
            .legal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} does not allow transition {from:?} -> {to:?}\n  legal transitions: {:?}",
        machine.legal_transitions
    );
    assert!(
        !machine
            .illegal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} marks transition {from:?} -> {to:?} as both legal and illegal"
    );
}

pub fn assert_lean_transition_is_illegal(domain: &str, from: &str, to: &str) {
    let machine = lean_state_machine_contract(domain);
    assert!(
        machine
            .illegal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} does not mark transition {from:?} -> {to:?} illegal\n  illegal transitions: {:?}",
        machine.illegal_transitions
    );
    assert!(
        !machine
            .legal_transitions
            .iter()
            .any(|pair| pair.from == from && pair.to == to),
        "Lean state-machine contract {domain:?} marks transition {from:?} -> {to:?} as both legal and illegal"
    );
}

pub fn assert_state_machine_contract_is_complete(domain: &str) {
    let machine = lean_state_machine_contract(domain);
    let duplicate_states = duplicate_string_values(&machine.states);
    let duplicate_actions = duplicate_string_values(&machine.actions);
    let duplicate_legal_pairs = duplicate_transition_pairs(&machine.legal_transitions);
    let duplicate_illegal_pairs = duplicate_transition_pairs(&machine.illegal_transitions);
    let expected_pairs = machine.state_count * machine.state_count;
    let actual_pairs = machine.legal_transitions.len() + machine.illegal_transitions.len();

    assert!(
        machine.state_count == machine.states.len()
            && duplicate_states.is_empty()
            && duplicate_actions.is_empty()
            && duplicate_legal_pairs.is_empty()
            && duplicate_illegal_pairs.is_empty()
            && actual_pairs == expected_pairs
            && machine
                .legal_transitions
                .iter()
                .all(|pair| !machine.illegal_transitions.contains(pair))
            && machine.legal_transitions.iter().all(
                |pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to)
            )
            && machine.illegal_transitions.iter().all(
                |pair| machine.states.contains(&pair.from) && machine.states.contains(&pair.to)
            ),
        "Lean state-machine contract {domain:?} is incomplete or malformed\n  state_count: {}\n  states: {:?}\n  actions: {:?}\n  legal transitions: {:?}\n  illegal transitions: {:?}\n  duplicate states: {:?}\n  duplicate actions: {:?}\n  duplicate legal pairs: {:?}\n  duplicate illegal pairs: {:?}\n  expected pair partition size: {}\n  actual pair partition size: {}",
        machine.state_count,
        machine.states,
        machine.actions,
        machine.legal_transitions,
        machine.illegal_transitions,
        duplicate_states,
        duplicate_actions,
        duplicate_legal_pairs,
        duplicate_illegal_pairs,
        expected_pairs,
        actual_pairs
    );
}

pub fn assert_lifecycle_transition_cases_partition(
    domain: &str,
    states: &[&str],
    cases: &[LeanLifecycleTransitionCase],
) {
    let expected_pairs = states
        .iter()
        .flat_map(|from| {
            states
                .iter()
                .map(move |to| ((*from).to_string(), (*to).to_string()))
        })
        .collect::<BTreeSet<_>>();
    let mut actual_pairs = BTreeSet::new();
    let mut invalid_cases = Vec::new();

    for case in cases {
        if case.domain != domain {
            invalid_cases.push(format!("{} has wrong domain {:?}", case.name, case.domain));
        }
        if !matches!(
            case.classification.as_str(),
            "legal" | "illegal" | "recoveryReachable"
        ) {
            invalid_cases.push(format!(
                "{} has invalid classification {:?}",
                case.name, case.classification
            ));
        }
        if case.classification == "legal" && case.action.is_none() {
            invalid_cases.push(format!("{} legal case missing action", case.name));
        }
        if case.classification != "legal" && case.action.is_some() {
            invalid_cases.push(format!("{} non-legal case has action", case.name));
        }
        let requires_boundary = case.classification == "recoveryReachable";
        if requires_boundary && case.boundary.is_none() {
            invalid_cases.push(format!(
                "{} {} case missing boundary",
                case.name, case.classification
            ));
        }
        if !requires_boundary && case.boundary.is_some() {
            invalid_cases.push(format!("{} reachable case has boundary", case.name));
        }
        if !expected_pairs.contains(&(case.from.clone(), case.to.clone())) {
            invalid_cases.push(format!(
                "{} has pair outside {domain} vocabulary: {:?} -> {:?}",
                case.name, case.from, case.to
            ));
        }
        if !actual_pairs.insert((case.from.clone(), case.to.clone())) {
            invalid_cases.push(format!(
                "{} duplicates pair {:?} -> {:?}",
                case.name, case.from, case.to
            ));
        }
    }

    let missing = expected_pairs
        .difference(&actual_pairs)
        .cloned()
        .collect::<Vec<_>>();
    let extra = actual_pairs
        .difference(&expected_pairs)
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        invalid_cases.is_empty() && missing.is_empty() && extra.is_empty(),
        "Lean lifecycle transition cases for {domain:?} do not form a state^2 partition\n  invalid cases: {:?}\n  missing pairs: {:?}\n  extra pairs: {:?}\n  cases: {:?}",
        invalid_cases,
        missing,
        extra,
        cases
    );
}

pub fn assert_lean_to_defradb_vocabulary_matches(spec: LeanVocabulary<'_>) {
    let lean_values = lean_to_defradb_values(spec.lean_file, spec.model, spec.namespace);
    let missing_from_lean = values_missing_from(spec.rust_values, &lean_values);
    let extra_in_lean = values_missing_from(&lean_values, spec.rust_values);
    let duplicate_rust_values = duplicate_values(spec.rust_values);
    let duplicate_lean_values = duplicate_values(&lean_values);

    assert!(
        spec.rust_values == lean_values.as_slice()
            && missing_from_lean.is_empty()
            && extra_in_lean.is_empty()
            && duplicate_rust_values.is_empty()
            && duplicate_lean_values.is_empty(),
        "Rust/Lean toDefraDB vocabulary mismatch\n  Lean file: {}\n  namespace: {}\n  Rust vocabulary source: {}\n  missing Lean values (present in Rust): {:?}\n  extra Lean values (absent from Rust): {:?}\n  duplicate Rust values: {:?}\n  duplicate Lean values: {:?}\n  Rust values: {:?}\n  Lean values: {:?}",
        spec.lean_file,
        spec.namespace,
        spec.rust_source,
        missing_from_lean,
        extra_in_lean,
        duplicate_rust_values,
        duplicate_lean_values,
        spec.rust_values,
        lean_values
    );
}

/// Mirrors the conformance build target and generator of `gents_lean_contract`.
/// The proofs directory is anchored at compile time: the runtime
/// `CARGO_MANIFEST_DIR` lookup in `gents_lean_contract::proofs_dir` is absent
/// when a test binary runs outside cargo. The anchor names the `gents` crate,
/// which holds the proofs, as a sibling of this crate's manifest.
const LEAN_CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const LEAN_CONTRACT_FILE: &str = "Proofs/Conformance/Contracts.lean";

fn load_lean_contract_snapshot() -> LeanContractSnapshot {
    let proofs_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../gents/proofs"));
    let stdout = gents_lean_contract::run_prepared_generator(
        proofs_dir,
        LEAN_CONTRACT_TARGET,
        LEAN_CONTRACT_FILE,
    )
    .unwrap_or_else(|error| panic!("failed to load Lean conformance contract JSON: {error:#}"));
    let json = gents_lean_contract::extract_contract_json(&stdout)
        .unwrap_or_else(|error| panic!("failed to load Lean conformance contract JSON: {error:#}"))
        .to_owned();

    serde_json::from_str(&json).unwrap_or_else(|error| {
        let excerpt = json_excerpt_at(&json, error.line(), error.column(), 512);
        panic!(
            "failed to parse Lean conformance contract JSON ({} bytes) at line {}, column {}: {error}\nnear error: {excerpt}",
            json.len(),
            error.line(),
            error.column(),
        )
    })
}

fn json_excerpt_at(json: &str, line: usize, column: usize, max_chars: usize) -> String {
    let source_line = json.lines().nth(line.saturating_sub(1)).unwrap_or_default();
    let error_char = column.saturating_sub(1);
    let start = error_char.saturating_sub(max_chars / 2);
    let excerpt = source_line
        .chars()
        .skip(start)
        .take(max_chars)
        .collect::<String>();
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if source_line.chars().count() > start + excerpt.chars().count() {
        "…"
    } else {
        ""
    };
    format!("{prefix}{excerpt}{suffix}")
}

pub fn lean_to_defradb_values<'a>(
    lean_file: &str,
    model: &'a str,
    namespace: &str,
) -> Vec<&'a str> {
    parse_lean_to_defradb_values(model, namespace)
        .unwrap_or_else(|error| panic!("{}", error.message(lean_file, namespace)))
}

pub(crate) fn parse_lean_to_defradb_values<'a>(
    model: &'a str,
    namespace: &str,
) -> Result<Vec<&'a str>, LeanVocabularyParseError<'a>> {
    let namespace_start = format!("namespace {namespace}");
    let namespace_end = format!("end {namespace}");
    let mut found_namespace = false;
    let mut found_to_defradb = false;
    let mut in_namespace = false;
    let mut in_to_defradb = false;
    let mut values = Vec::new();

    for (index, line) in model.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();

        if !in_namespace {
            if trimmed == namespace_start {
                found_namespace = true;
                in_namespace = true;
            }
            continue;
        }

        if trimmed == namespace_end {
            break;
        }

        if !in_to_defradb {
            if trimmed.starts_with("def toDefraDB") {
                found_to_defradb = true;
                in_to_defradb = true;
            }
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        if trimmed.starts_with('|') {
            values.push(parse_to_defradb_arm(trimmed, line_number)?);
            continue;
        }

        if values.is_empty() {
            return Err(LeanVocabularyParseError::MalformedArm {
                line_number,
                line: trimmed,
                reason: "expected a toDefraDB pattern arm starting with `| .`",
            });
        }

        break;
    }

    if !found_namespace {
        return Err(LeanVocabularyParseError::MissingNamespace);
    }
    if !found_to_defradb {
        return Err(LeanVocabularyParseError::MissingToDefraDB);
    }
    if values.is_empty() {
        return Err(LeanVocabularyParseError::EmptyToDefraDB);
    }

    Ok(values)
}

fn parse_to_defradb_arm<'a>(
    trimmed: &'a str,
    line_number: usize,
) -> Result<&'a str, LeanVocabularyParseError<'a>> {
    let Some(rest) = trimmed.strip_prefix("| .") else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line: trimmed,
            reason: "expected a toDefraDB pattern arm starting with `| .`",
        });
    };
    let Some((_constructor, value)) = rest.split_once("=>") else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line: trimmed,
            reason: "missing `=>`",
        });
    };

    parse_string_literal(value.trim(), line_number, trimmed)
}

fn parse_string_literal<'a>(
    value: &'a str,
    line_number: usize,
    line: &'a str,
) -> Result<&'a str, LeanVocabularyParseError<'a>> {
    let Some(after_opening_quote) = value.strip_prefix('"') else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "expected a string literal after `=>`",
        });
    };
    let Some(end_index) = after_opening_quote.find('"') else {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "string literal is missing a closing quote",
        });
    };
    let literal = &after_opening_quote[..end_index];
    let trailing = after_opening_quote[end_index + 1..].trim();
    if !trailing.is_empty() && !trailing.starts_with("--") {
        return Err(LeanVocabularyParseError::MalformedArm {
            line_number,
            line,
            reason: "expected only optional comment text after the string literal",
        });
    }

    Ok(literal)
}

fn values_missing_from<'a>(expected: &[&'a str], actual: &[&str]) -> Vec<&'a str> {
    expected
        .iter()
        .copied()
        .filter(|value| !actual.contains(value))
        .collect()
}

fn duplicate_values<'a>(values: &[&'a str]) -> Vec<&'a str> {
    let mut seen = Vec::new();
    let mut duplicates = Vec::new();
    for value in values {
        if seen.contains(value) {
            if !duplicates.contains(value) {
                duplicates.push(*value);
            }
        } else {
            seen.push(*value);
        }
    }
    duplicates
}

fn duplicate_string_values(values: &[String]) -> Vec<String> {
    let refs = values.iter().map(String::as_str).collect::<Vec<_>>();
    duplicate_values(&refs)
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn duplicate_transition_pairs(values: &[LeanTransitionPair]) -> Vec<LeanTransitionPair> {
    let mut seen = Vec::new();
    let mut duplicates = Vec::new();
    for value in values {
        if seen.contains(value) {
            if !duplicates.contains(value) {
                duplicates.push(value.clone());
            }
        } else {
            seen.push(value.clone());
        }
    }
    duplicates
}

impl LeanVocabularyParseError<'_> {
    fn message(&self, lean_file: &str, namespace: &str) -> String {
        match self {
            Self::MissingNamespace => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: namespace block was not found"
            ),
            Self::MissingToDefraDB => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: def toDefraDB was not found in the namespace"
            ),
            Self::EmptyToDefraDB => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  reason: def toDefraDB has no parsed string-valued arms"
            ),
            Self::MalformedArm {
                line_number,
                line,
                reason,
            } => format!(
                "Lean toDefraDB vocabulary parse failed\n  Lean file: {lean_file}\n  namespace: {namespace}\n  line: {line_number}\n  reason: {reason}\n  source: {line}"
            ),
        }
    }
}

pub fn lean_budget_rehydration_cases() -> &'static [LeanBudgetRehydrationCase] {
    &lean_contract_snapshot().budget_rehydration_cases
}

pub fn lean_application_write_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().application_write_cases
}

pub fn lean_session_input_edit_cases() -> &'static [LeanSessionInputEditCase] {
    &lean_contract_snapshot().session_input_edit_cases
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
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanSessionInputEditStep {
    Apply {
        now: u64,
        authority: LeanSessionInputEditAuthority,
        command: LeanSessionInputEditCommand,
    },
    LoseReceipts,
    OtherSession {
        session: u64,
    },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditState {
    pub queue: LeanQueueManagementState,
    pub receipts: Vec<LeanSessionInputEditReceipt>,
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
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionInputEditAuthority {
    pub signature_valid: bool,
    pub replacements_valid: bool,
    pub command_identity_unique: bool,
    pub session_owned: bool,
    pub editor: LeanSessionInputEditor,
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
pub struct LeanSessionInputEditReceipt {
    pub command_id: u64,
    pub digest: u64,
    pub outcome: String,
    pub request_ids: Vec<u64>,
}

pub fn lean_prompt_assembly_service_tier_cases() -> &'static [LeanPromptAssemblyServiceTierCase] {
    &lean_contract_snapshot().prompt_assembly_service_tier_cases
}

pub fn lean_snapshot_demotion_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().snapshot_demotion_cases
}

pub fn lean_plugin_reference_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().plugin_reference_cases
}

pub fn lean_catalog_refresh_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().catalog_refresh_cases
}

pub fn lean_request_terminal_selection_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().request_terminal_selection_cases
}

pub fn lean_claude_platform_cases() -> &'static [serde_json::Value] {
    &lean_contract_snapshot().claude_platform_cases
}

pub fn lean_pack_identity_cases() -> &'static [LeanPackIdentityCase] {
    &lean_contract_snapshot().pack_identity_cases
}

pub fn lean_pack_identity_reference_cases() -> &'static [LeanPackIdentityReferenceCase] {
    &lean_contract_snapshot().pack_identity_reference_cases
}

pub fn lean_pack_eval_subject_cases() -> &'static [LeanPackEvalSubjectCase] {
    &lean_contract_snapshot().pack_eval_subject_cases
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendWakeCase {
    pub now_ms: i64,
    pub deadline_ms: i64,
    pub attempts: u32,
    pub wake_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendCaptureScopeCase {
    pub persisted: Vec<u64>,
    pub next: u64,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendHealthWakeCase {
    pub started_ms: i64,
    pub now_ms: i64,
    pub observed_ms: Option<i64>,
    pub fresh: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendGenerationMarkCase {
    pub marked: Option<u64>,
    pub observed: Option<u64>,
    pub applies: bool,
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
pub struct LeanBackendParkedFinalizeCase {
    pub request: gents_protocol::request_lifecycle::RequestLifecycleState,
    pub outcome: gents_protocol::request_lifecycle::RequestLifecycleState,
    pub allowed: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeanBackendResumedTurnCase {
    pub completed: Vec<u32>,
    pub next: usize,
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
