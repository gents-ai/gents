use super::*;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCrossActorAuthorizationCase {
    pub(crate) name: String,
    pub(crate) intent: LeanCrossActorIntent,
    pub(crate) observation: LeanCrossActorObservation,
    pub(crate) before_session_changes: Vec<LeanPermissionChange>,
    pub(crate) session_changes: Vec<LeanPermissionChange>,
    pub(crate) invoke_changes: Vec<LeanPermissionChange>,
    pub(crate) node_changes: Vec<LeanPermissionChange>,
    pub(crate) native_committed: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) prior_effect: Option<String>,
    pub(crate) prior_lifecycle: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) prior_terminal_output: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) prior_failure_reason: Option<String>,
    pub(crate) decision: LeanCrossActorDecision,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) effect: Option<String>,
    pub(crate) lifecycle: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) terminal_output: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) failure_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCrossActorIntent {
    pub(crate) document: String,
    pub(crate) session: String,
    pub(crate) agent_document: String,
    pub(crate) author: String,
    pub(crate) selected_agent: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCrossActorObservation {
    pub(crate) supported: bool,
    pub(crate) visible: bool,
    pub(crate) targets_visible: bool,
    pub(crate) transient_failure: bool,
    pub(crate) document: String,
    pub(crate) session: String,
    pub(crate) agent_document: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) owner: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) authenticated_author: Option<String>,
    pub(crate) executing_agent: String,
    pub(crate) signature_valid: bool,
    pub(crate) session_permission: bool,
    pub(crate) agent_invoke: bool,
    pub(crate) node_permission_enforced: bool,
    pub(crate) requester_node_permission: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanPermissionChange {
    Revoke,
    Grant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanCrossActorDecision {
    Blocked,
    Unavailable,
    Denied,
    Conflict,
    Claimed,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandPolicyCase {
    pub(crate) name: String,
    pub(crate) category: String,
    pub(crate) mode: String,
    pub(crate) allowed_argv_prefixes: Vec<Vec<String>>,
    pub(crate) forbidden_argv_prefixes: Vec<Vec<String>>,
    pub(crate) network_mode: String,
    pub(crate) read_only_allowlist: Vec<String>,
    pub(crate) command: String,
    pub(crate) lookup_command: String,
    pub(crate) args: Vec<String>,
    pub(crate) decision: String,
    pub(crate) denial_reason: Option<String>,
    pub(crate) matched_prefix: Option<Vec<String>>,
    pub(crate) denied_argv: Option<Vec<String>>,
    pub(crate) denied_command: Option<String>,
    pub(crate) denied_argument: Option<String>,
    pub(crate) denied_subcommand: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandSandboxCase {
    pub(crate) name: String,
    pub(crate) category: String,
    pub(crate) mode: String,
    pub(crate) workspace_write_sandbox_enforced: bool,
    pub(crate) decision: String,
    pub(crate) sandbox: Option<String>,
    pub(crate) denial_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCommandEnvCase {
    pub(crate) name: String,
    pub(crate) env_key: String,
    pub(crate) input_present: bool,
    pub(crate) input_name: String,
    pub(crate) input_value: String,
    pub(crate) output_name: String,
    pub(crate) expected_value_kind: Option<String>,
    pub(crate) expected_output_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanLiveOverlayCase {
    pub(crate) name: String,
    #[serde(rename = "liveOutputAvailable")]
    pub(crate) live_output_available: bool,
    #[serde(rename = "hasDurableOwner")]
    pub(crate) has_durable_owner: bool,
    #[serde(rename = "precedingToolCalls")]
    pub(crate) preceding_tool_calls: u64,
    #[serde(rename = "turnTerminal")]
    pub(crate) turn_terminal: bool,
    #[serde(rename = "turnLabel")]
    pub(crate) turn_label: String,
    #[serde(rename = "hasContent")]
    pub(crate) has_content: bool,
    #[serde(rename = "hasReasoning")]
    pub(crate) has_reasoning: bool,
    #[serde(rename = "expectOverlay")]
    pub(crate) expect_overlay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRequestProgressCase {
    pub(crate) name: String,
    #[serde(rename = "lifecycleState")]
    pub(crate) lifecycle_state: String,
    pub(crate) label: String,
    pub(crate) animated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPendingUserTurnCase {
    pub(crate) name: String,
    #[serde(rename = "hasDurableUserOwner")]
    pub(crate) has_durable_user_owner: bool,
    #[serde(rename = "retiredByAuthor")]
    pub(crate) retired_by_author: bool,
    #[serde(rename = "unrelatedUserTurns")]
    pub(crate) unrelated_user_turns: u64,
    #[serde(rename = "expectPendingTurn")]
    pub(crate) expect_pending_turn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringTraceCase {
    pub(crate) name: String,
    pub(crate) actions: Vec<LeanQueuedSteeringAction>,
    #[serde(rename = "requestId")]
    pub(crate) request_id: u64,
    #[serde(rename = "requestDocId")]
    pub(crate) request_doc_id: u64,
    #[serde(rename = "contentToken")]
    pub(crate) content_token: u64,
    pub(crate) entry: LeanQueuedSteeringEntry,
    #[serde(rename = "interruptAt")]
    pub(crate) interrupt_at: Option<u64>,
    #[serde(rename = "preparedCandidate")]
    pub(crate) prepared_candidate: Option<LeanQueuedSteeringCandidate>,
    pub(crate) capture: LeanQueuedSteeringCapture,
    #[serde(rename = "lifecycleState")]
    pub(crate) lifecycle_state: String,
    #[serde(rename = "acceptedInput")]
    pub(crate) accepted_input: bool,
    #[serde(rename = "queueActive")]
    pub(crate) queue_active: Option<u64>,
    #[serde(rename = "admissionVisible")]
    pub(crate) admission_visible: bool,
    #[serde(rename = "canonicalAuthoredCount")]
    pub(crate) canonical_authored_count: u64,
    #[serde(rename = "providerSendPermitted")]
    pub(crate) provider_send_permitted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanQueuedSteeringAction {
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
pub(crate) struct LeanQueuedSteeringEntry {
    #[serde(rename = "requestId")]
    pub(crate) request_id: u64,
    #[serde(rename = "createdAt")]
    pub(crate) created_at: u64,
    pub(crate) source: String,
    pub(crate) policy: String,
    #[serde(rename = "queueKey")]
    pub(crate) queue_key: Option<u64>,
    #[serde(rename = "queuedAfter")]
    pub(crate) queued_after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringCandidate {
    pub(crate) closing: super::canonical_output::LeanCanonicalSegment,
    pub(crate) message:
        super::canonical_output::LeanCanonicalMessage<super::canonical_output::LeanPayloadSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringCapture {
    #[serde(rename = "nodeDid")]
    pub(crate) node_did: u64,
    #[serde(rename = "sessionId")]
    pub(crate) session_id: u64,
    #[serde(rename = "requestDocId")]
    pub(crate) request_doc_id: u64,
    #[serde(rename = "turnIndex")]
    pub(crate) turn_index: u64,
    pub(crate) attempt: u64,
    #[serde(rename = "bodyToken")]
    pub(crate) body_token: u64,
    #[serde(rename = "priorBodyToken")]
    pub(crate) prior_body_token: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanQueuedSteeringGuardCase {
    pub(crate) name: String,
    #[serde(rename = "prefixAdmitted")]
    pub(crate) prefix_admitted: bool,
    pub(crate) admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityNode {
    pub(crate) did: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityAgent {
    pub(crate) label: String,
    pub(crate) did: String,
    pub(crate) creation_nonce: String,
    pub(crate) node: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityStructuralCase {
    pub(crate) name: String,
    pub(crate) nodes: Vec<LeanIdentityNode>,
    pub(crate) agents: Vec<LeanIdentityAgent>,
    pub(crate) well_formed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityPermissionGrant {
    pub(crate) actor: String,
    pub(crate) permission: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityPermissionCase {
    pub(crate) name: String,
    pub(crate) nodes: Vec<LeanIdentityNode>,
    pub(crate) agents: Vec<LeanIdentityAgent>,
    pub(crate) grants: Vec<LeanIdentityPermissionGrant>,
    pub(crate) permission: String,
    pub(crate) row_owner: String,
    pub(crate) actor_node: String,
    pub(crate) actor_did: String,
    pub(crate) peer_node: String,
    pub(crate) peer_did: String,
    pub(crate) expected_actor_did: Option<String>,
    pub(crate) expected_peer_did: Option<String>,
    pub(crate) expected_actor_allowed: bool,
    pub(crate) expected_peer_allowed: bool,
    pub(crate) same_node: bool,
    pub(crate) expected_decisions_equal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanIdentityContract {
    pub(crate) node_local_collections: Vec<String>,
    pub(crate) replicated_protocol_collections: Vec<String>,
    pub(crate) schema_actor: String,
    pub(crate) runtime_actor: String,
    pub(crate) name: String,
    pub(crate) statement: String,
    pub(crate) enforced: bool,
    pub(crate) tracked_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanQueueDeadlineConformanceCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) session_id: usize,
    pub(crate) legal: bool,
    pub(crate) pre_active_request_id: Option<usize>,
    pub(crate) post_active_request_id: Option<usize>,
    pub(crate) pre_pending_request_ids: Vec<usize>,
    pub(crate) post_pending_request_ids: Vec<usize>,
    pub(crate) claimed_request_id: Option<usize>,
    pub(crate) blocked_by_active: bool,
    pub(crate) superseded_request_ids: Vec<usize>,
    pub(crate) queue_key: Option<String>,
    pub(crate) post_coalesced_pending_count: usize,
    pub(crate) automated_drained_request_ids: Vec<usize>,
    pub(crate) preserved_user_pending_request_ids: Vec<usize>,
    pub(crate) preserved_foreign_requester_request_ids: Vec<usize>,
    pub(crate) preserved_foreign_owner_request_ids: Vec<usize>,
    pub(crate) post_terminal_request_ids: Vec<usize>,
    pub(crate) pre_request_deadline: Option<usize>,
    pub(crate) synthesized_claim_deadline: Option<usize>,
    pub(crate) post_deadline: Option<usize>,
    pub(crate) explicit_deadline_preserved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRecoverySweepCase {
    pub(crate) name: String,
    pub(crate) sweep_id: String,
    pub(crate) collection: String,
    pub(crate) rust_function: String,
    pub(crate) cadence: String,
    pub(crate) implementation_status: String,
    pub(crate) pre_state: String,
    pub(crate) terminal_state: String,
    pub(crate) measure_before: usize,
    pub(crate) measure_after: usize,
    pub(crate) deadline_expired: Option<bool>,
    pub(crate) parent_live: Option<bool>,
    pub(crate) parent_interrupted: Option<bool>,
    pub(crate) parent_terminal: Option<bool>,
    pub(crate) execution_registered: Option<bool>,
    pub(crate) process_outcome: Option<String>,
    pub(crate) owner_task_deleted: Option<bool>,
    pub(crate) owner_session_stopped: Option<bool>,
    pub(crate) recovery_cause: Option<String>,
    pub(crate) notification_reason: Option<String>,
    pub(crate) deadline_audit_ref: String,
}

/// `Recovery.RowKind`: which owner a running tool-call row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum LeanRestartRowKind {
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "sessionMessage")]
    SessionMessage,
    #[serde(rename = "pluginJob")]
    PluginJob,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRestartDispositionCase {
    pub(crate) name: String,
    pub(crate) rust_function: String,
    pub(crate) await_mode: String,
    pub(crate) session_message: bool,
    pub(crate) kind: LeanRestartRowKind,
    pub(crate) parent_observation: String,
    pub(crate) deadline_expired: bool,
    pub(crate) process_outcome: String,
    pub(crate) disposition: String,
    pub(crate) cause: Option<String>,
    pub(crate) terminal_state: Option<String>,
    pub(crate) notification_reason: Option<String>,
    pub(crate) queue_source: Option<String>,
    pub(crate) queue_key_prefix: Option<String>,
    #[allow(dead_code)]
    pub(crate) theorem: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanGoalLinkCase {
    pub(crate) collection: String,
    pub(crate) actor: String,
    pub(crate) mode: String,
    pub(crate) commit: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) owner: Option<String>,
    pub(crate) initial: LeanGoalLinkState,
    pub(crate) steps: Vec<LeanGoalLinkStep>,
    pub(crate) after_revocation: LeanGoalLinkState,
    pub(crate) repair_allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanGoalLinkState {
    pub(crate) parent: bool,
    pub(crate) worker: bool,
    pub(crate) linked: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanGoalLinkStep {
    pub(crate) parent_ok: bool,
    pub(crate) worker_ok: bool,
    pub(crate) mark_ok: bool,
    pub(crate) after: LeanGoalLinkState,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityParentPublicationCase {
    pub(crate) name: String,
    pub(crate) initial: LeanIdentityParentPublicationState,
    pub(crate) steps: Vec<LeanIdentityParentPublicationStep>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityParentPublicationState {
    pub(crate) committed: bool,
    pub(crate) parent_granted: bool,
    pub(crate) parent_linked: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityParentPublicationStep {
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) native_owner: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) claimed_creator: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) authenticated_creator: Option<String>,
    pub(crate) grant_ok: bool,
    pub(crate) mark_ok: bool,
    pub(crate) revoke_parent: bool,
    pub(crate) after: LeanIdentityParentPublicationState,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityHouseholdCase {
    pub(crate) name: String,
    pub(crate) policy_yaml: String,
    pub(crate) objects: Vec<LeanIdentityObject>,
    pub(crate) relations: Vec<LeanIdentityRelation>,
    pub(crate) enrolled: Vec<String>,
    pub(crate) policy_registrations: Vec<LeanIdentityPolicyRegistration>,
    #[serde(default)]
    pub(crate) task_checkpoint_cases: Vec<LeanTaskCheckpointCase>,
    pub(crate) self_config_actions: Vec<LeanIdentityAction>,
    #[serde(default)]
    pub(crate) invoke_session_actions: Vec<LeanIdentityAction>,
    pub(crate) self_config_lookup_cases: Vec<LeanSelfConfigLookupCase>,
    pub(crate) actions: Vec<LeanIdentityAction>,
    #[serde(default)]
    pub(crate) configuration_signed_create_collections:
        Vec<LeanConfigurationSignedCreateCollection>,
    #[serde(default)]
    pub(crate) configuration_signed_delete_collections: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) client_goal_transaction_actor_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) client_goal_graph_handoff_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_signed_write_retry_cases: Vec<LeanConfigurationSignedWriteRetryCase>,
    #[serde(default)]
    pub(crate) configuration_immutable_cases: Vec<LeanConfigurationImmutableCase>,
    #[serde(default)]
    pub(crate) held_client_signer_cases: Vec<LeanHeldClientSignerCase>,
    #[serde(default)]
    pub(crate) ordinary_client_mutation_cases: Vec<LeanOrdinaryClientMutationCase>,
    #[serde(default)]
    pub(crate) configuration_atomic_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_node_binding_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_provider_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "backend_credential_cases")]
    pub(crate) _backend_credential_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) runtime_default_account_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) runtime_default_choice_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) legacy_backend_lockdown_unavailable: bool,
    #[serde(default)]
    pub(crate) fresh_backend_admission_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "fresh_backend_origin_cases")]
    pub(crate) _fresh_backend_origin_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) fresh_backend_reopen_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "backend_credential_native_cases")]
    pub(crate) _backend_credential_native_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "backend_credential_publication_cases")]
    pub(crate) _backend_credential_publication_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_plugin_backend_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) backend_discovery_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) backend_probe_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) backend_removal_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) backend_genesis_distinct: bool,
    #[serde(default)]
    pub(crate) backend_observation_authority_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_inference_provider_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_backend_metadata_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_userset_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_inference_self_config_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_skill_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_compaction_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_tools_selection_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_task_target_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_tool_reference_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_plugin_profile_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_selection_manager_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_node_manager_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_node_manager_link_revocation_case: serde_json::Value,
    #[serde(default)]
    pub(crate) agent_node_manager_invoke_case: serde_json::Value,
    #[serde(default)]
    pub(crate) agent_node_manager_link_revocation_case: serde_json::Value,
    #[serde(default)]
    pub(crate) mcp_service_use_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_schedule_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_schedule_write_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_pack_installation_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_pack_removal_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_pack_cleanup_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_datastore_tool_use_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_datastore_write_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_ux_datastore_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_ux_create_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_affected_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) configuration_removal_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_configuration_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_cursor_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_cursor_rebind_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_cursor_move_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_stale_claim_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_generation_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_execution_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_rebound_checkpoint_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_rebound_visibility_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_module_reader_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) event_group_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) task_receipt_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_projection_cases")]
    pub(crate) _tool_grant_projection_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_group_cases")]
    pub(crate) _tool_grant_group_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_hierarchy_cases")]
    pub(crate) _tool_grant_hierarchy_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grant_rebinding_cases")]
    pub(crate) _tool_grant_rebinding_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "session_tool_access_cases")]
    pub(crate) _session_tool_access_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "tool_grantor_cases")]
    pub(crate) _tool_grantor_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) tool_operation_cases: Vec<serde_json::Value>,
    pub(crate) plugin_delete_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) managed_tool_identity_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) projection_binding_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_configuration_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_generation_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_module_claim_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_recovery_configuration_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_placement_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_child_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) pairing_overlay_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) unattended_agent_cases: Vec<serde_json::Value>,
    #[serde(default, rename = "unattended_receipt_cases")]
    pub(crate) _unattended_receipt_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) callback_checkpoint_claim_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) ordinary_receipt_visibility_cases: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanOrdinaryClientMutationCase {
    pub(crate) intent: bool,
    pub(crate) mutation: bool,
    pub(crate) allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanHeldClientSignerCase {
    pub(crate) actor: String,
    pub(crate) bearer: String,
    pub(crate) signer: String,
    pub(crate) has_private: bool,
    pub(crate) allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanConfigurationImmutableCase {
    pub(crate) observed: String,
    pub(crate) proposed: String,
    pub(crate) allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanConfigurationSignedWriteRetryCase {
    pub(crate) dispatched: bool,
    pub(crate) may_retry: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanConfigurationSignedCreateCollection {
    pub(crate) collection: String,
    pub(crate) supported: bool,
    pub(crate) update_supported: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTaskCheckpointCase {
    pub(crate) actor: String,
    pub(crate) cursor: u64,
    pub(crate) through: u64,
    pub(crate) receipt: bool,
    pub(crate) expected: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanSelfConfigLookupCase {
    pub(crate) name: String,
    pub(crate) present: bool,
    pub(crate) visible: bool,
    pub(crate) same_generation: bool,
    pub(crate) native_allows: bool,
    pub(crate) expected: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityPolicyRegistration {
    pub(crate) name: String,
    pub(crate) candidate_yaml: String,
    pub(crate) same_id: bool,
    pub(crate) accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityObject {
    pub(crate) resource: String,
    pub(crate) id: String,
    pub(crate) owner: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityRelation {
    pub(crate) resource: String,
    pub(crate) id: String,
    pub(crate) relation: String,
    pub(crate) subject: LeanIdentitySubject,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum LeanIdentitySubject {
    Actor {
        actor: String,
    },
    EntitySet {
        resource: String,
        id: String,
        target_relation: String,
    },
    Object {
        resource: String,
        id: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanIdentityAction {
    Grant {
        relation: LeanIdentityRelation,
    },
    Revoke {
        relation: LeanIdentityRelation,
    },
    GrantAs {
        actor: String,
        relation: LeanIdentityRelation,
        expected: bool,
    },
    RevokeAs {
        actor: String,
        relation: LeanIdentityRelation,
        expected: bool,
    },
    Check {
        actor: String,
        resource: String,
        id: String,
        permission: String,
        expected: bool,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityDerivationInput {
    pub(crate) node_key: String,
    pub(crate) creation_nonce: String,
    pub(crate) label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanIdentityDerivationCase {
    pub(crate) name: String,
    pub(crate) left: LeanIdentityDerivationInput,
    pub(crate) right: LeanIdentityDerivationInput,
    pub(crate) expected_same_did: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPackIdentityBinding {
    pub(crate) slot: String,
    pub(crate) creation_nonce: String,
    pub(crate) agent_did: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPackIdentityCase {
    pub(crate) name: String,
    pub(crate) recorded: Vec<LeanPackIdentityBinding>,
    pub(crate) requested: Vec<String>,
    pub(crate) provisioned: Vec<LeanPackIdentityBinding>,
    pub(crate) accepted: bool,
    pub(crate) selected: Vec<LeanPackIdentityBinding>,
    pub(crate) next_recorded: Vec<LeanPackIdentityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPackIdentityDependencyBindings {
    pub(crate) coordinate: String,
    pub(crate) bindings: Vec<LeanPackIdentityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPackIdentityReferenceCase {
    pub(crate) name: String,
    pub(crate) declared: Vec<String>,
    pub(crate) available: Vec<LeanPackIdentityDependencyBindings>,
    pub(crate) dependency: String,
    pub(crate) slot: String,
    pub(crate) expected: Option<LeanPackIdentityBinding>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPackEvalSubjectCase {
    pub(crate) name: String,
    pub(crate) selected: Option<LeanPackIdentityBinding>,
    pub(crate) slot: String,
    pub(crate) expected: Option<String>,
}
