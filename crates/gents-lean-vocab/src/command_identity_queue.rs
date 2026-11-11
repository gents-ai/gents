use super::*;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanPermissionChange {
    Revoke,
    Grant,
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

#[derive(Debug, Deserialize)]
pub struct LeanCommandSandboxCase {
    pub name: String,
    pub category: String,
    pub mode: String,
    pub workspace_write_sandbox_enforced: bool,
    pub decision: String,
    pub sandbox: Option<String>,
    pub denial_reason: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanRequestProgressCase {
    pub name: String,
    #[serde(rename = "lifecycleState")]
    pub lifecycle_state: String,
    pub label: String,
    pub animated: bool,
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
pub struct LeanQueuedSteeringCandidate {
    pub closing: super::canonical_output::LeanCanonicalSegment,
    pub message:
        super::canonical_output::LeanCanonicalMessage<super::canonical_output::LeanPayloadSpec>,
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
pub struct LeanQueuedSteeringGuardCase {
    pub name: String,
    #[serde(rename = "prefixAdmitted")]
    pub prefix_admitted: bool,
    pub admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityNode {
    pub did: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityAgent {
    pub label: String,
    pub did: String,
    pub creation_nonce: String,
    pub node: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityStructuralCase {
    pub name: String,
    pub nodes: Vec<LeanIdentityNode>,
    pub agents: Vec<LeanIdentityAgent>,
    pub well_formed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityPermissionGrant {
    pub actor: String,
    pub permission: String,
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
    pub configuration_signed_create_collections: Vec<LeanConfigurationSignedCreateCollection>,
    #[serde(default)]
    pub configuration_signed_delete_collections: Vec<serde_json::Value>,
    #[serde(default)]
    pub client_goal_transaction_actor_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub client_goal_graph_handoff_cases: Vec<serde_json::Value>,
    #[serde(default)]
    pub configuration_signed_write_retry_cases: Vec<LeanConfigurationSignedWriteRetryCase>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanManagedToolIdentityCase {
    pub claimed: Option<LeanClaimedDid>,
    pub node_key: String,
    pub creation_nonce: Option<String>,
    pub allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanClaimedDid {
    Key(String),
    Derived {
        node_key: String,
        creation_nonce: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanOrdinaryClientMutationCase {
    pub intent: bool,
    pub mutation: bool,
    pub allowed: bool,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanConfigurationImmutableCase {
    pub observed: String,
    pub proposed: String,
    pub allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanConfigurationSignedWriteRetryCase {
    pub dispatched: bool,
    pub confirmed_abort: bool,
    pub replay_authorized: bool,
    pub may_retry: bool,
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
pub struct LeanTaskCheckpointCase {
    pub actor: String,
    pub cursor: u64,
    pub through: u64,
    pub receipt: bool,
    pub expected: u64,
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
#[serde(deny_unknown_fields)]
pub struct LeanIdentityPolicyRegistration {
    pub name: String,
    pub candidate_yaml: String,
    pub same_id: bool,
    pub accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityObject {
    pub resource: String,
    pub id: String,
    pub owner: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityRelation {
    pub resource: String,
    pub id: String,
    pub relation: String,
    pub subject: LeanIdentitySubject,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum LeanIdentitySubject {
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
pub enum LeanIdentityAction {
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
pub struct LeanIdentityDerivationInput {
    pub node_key: String,
    pub creation_nonce: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityDerivationCase {
    pub name: String,
    pub left: LeanIdentityDerivationInput,
    pub right: LeanIdentityDerivationInput,
    pub expected_same_did: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackIdentityBinding {
    pub slot: String,
    pub creation_nonce: String,
    pub agent_did: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackIdentityCase {
    pub name: String,
    pub recorded: Vec<LeanPackIdentityBinding>,
    pub requested: Vec<String>,
    pub provisioned: Vec<LeanPackIdentityBinding>,
    pub accepted: bool,
    pub selected: Vec<LeanPackIdentityBinding>,
    pub next_recorded: Vec<LeanPackIdentityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPackIdentityDependencyBindings {
    pub coordinate: String,
    pub bindings: Vec<LeanPackIdentityBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPackIdentityReferenceCase {
    pub name: String,
    pub declared: Vec<String>,
    pub available: Vec<LeanPackIdentityDependencyBindings>,
    pub dependency: String,
    pub slot: String,
    pub expected: Option<LeanPackIdentityBinding>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackEvalSubjectCase {
    pub name: String,
    pub selected: Option<LeanPackIdentityBinding>,
    pub slot: String,
    pub expected: Option<String>,
}
