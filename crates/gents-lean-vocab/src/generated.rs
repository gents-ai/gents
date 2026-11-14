use crate::*;
use crate::required_nullable;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn lean_completion_retry_cases() -> &'static [LeanCompletionRetryCase] {
    &lean_contract_snapshot().completion_retry_cases
}

pub fn lean_feature_matrix() -> &'static LeanFeatureMatrix {
    &lean_contract_snapshot().feature_matrix
}

pub fn lean_feature_surface_requirements() -> &'static [LeanFeatureSurfaceRequirement] {
    &lean_contract_snapshot().feature_surface_requirements
}

pub fn lean_identity_household_cases() -> &'static [LeanIdentityHouseholdCase] {
    &lean_contract_snapshot().identity_household_cases
}

pub fn lean_managed_exec_tool_boundary_cases() -> &'static [LeanManagedExecToolBoundaryCase] {
    &lean_contract_snapshot().managed_exec_tool_boundary_cases
}

pub fn lean_recovery_sweep_cases() -> &'static [LeanRecoverySweepCase] {
    &lean_contract_snapshot().recovery_sweep_cases
}

pub fn lean_transcript_cases() -> &'static [LeanTranscriptCase] {
    &lean_contract_snapshot().transcript_conformance_cases
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanClaimedDid {
    Key(String),
    Derived { node_key: String, creation_nonce: String },
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanContractSnapshot {
    pub delegated_publication_cases: Vec<serde_json::Value>,
    pub generated_by: String,
    pub goal_client_start_cases: Vec<serde_json::Value>,
    #[serde(deserialize_with = "required_nullable")]
    pub causal_hop_default_max_request_hop: Option<u32>,
    pub managed_exec_tool_boundary_cases: Vec<LeanManagedExecToolBoundaryCase>,
    pub completion_retry_cases: Vec<LeanCompletionRetryCase>,
    pub recovery_sweep_cases: Vec<LeanRecoverySweepCase>,
    pub transcript_conformance_cases: Vec<LeanTranscriptCase>,
    pub families: BTreeMap<String, LeanFamily>,
    pub family_vocabularies: BTreeMap<String, serde_json::Value>,
    pub coverage_ledger: Vec<LeanCoverageEntry>,
    pub feature_surface_requirements: Vec<LeanFeatureSurfaceRequirement>,
    pub feature_matrix: LeanFeatureMatrix,
    pub identity_household_cases: Vec<LeanIdentityHouseholdCase>,
    pub recovery_sweep_ids: Vec<String>,
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
    pub event_group_state_cases: Option<
        Vec<LeanIdentityHouseholdCaseEventGroupStateCase>,
    >,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanIdentityHouseholdCaseEventGroupStateCase {
    pub actor: String,
    pub expected: bool,
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
pub struct LeanIdentityPolicyRegistration {
    pub name: String,
    pub candidate_yaml: String,
    pub same_id: bool,
    pub accepted: bool,
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
    Actor { actor: String },
    EntitySet { resource: String, id: String, target_relation: String },
    Object { resource: String, id: String },
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanOrdinaryClientMutationCase {
    pub intent: bool,
    pub mutation: bool,
    pub allowed: bool,
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
