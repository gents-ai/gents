use crate::*;
use crate::canonical_output::LeanCanonicalMessage;
use crate::required_nullable;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn lean_canonical_dispatch_observation_cases() -> &'static [LeanDispatchObservationCase] {
    &lean_contract_snapshot().canonical_dispatch_observation_cases
}

pub fn lean_canonical_execution_gate_cases() -> &'static [LeanCanonicalExecutionCase] {
    &lean_contract_snapshot().canonical_execution_gate_cases
}

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

pub fn lean_process_stop_cases() -> &'static [LeanProcessStopCase] {
    &lean_contract_snapshot().process_stop_cases
}

pub fn lean_r6_backgrounding_cases() -> &'static [LeanR6BackgroundingCase] {
    &lean_contract_snapshot().r6_backgrounding_cases
}

pub fn lean_recovery_sweep_cases() -> &'static [LeanRecoverySweepCase] {
    &lean_contract_snapshot().recovery_sweep_cases
}

pub fn lean_session_input_edit_cases() -> &'static [LeanSessionInputEditCase] {
    &lean_contract_snapshot().session_input_edit_cases
}

pub fn lean_transcript_cases() -> &'static [LeanTranscriptCase] {
    &lean_contract_snapshot().transcript_conformance_cases
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalClosure {
    Closed { outcome: LeanOutcome, segments: u64, stream_bytes: Vec<u64> },
    Retracted,
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
    pub identity_grant_recipient_cases: Vec<LeanGrantRecipientCase>,
    pub request_input_cases: Vec<serde_json::Value>,
    pub managed_exec_tool_boundary_cases: Vec<LeanManagedExecToolBoundaryCase>,
    pub process_stop_cases: Vec<LeanProcessStopCase>,
    pub completion_retry_cases: Vec<LeanCompletionRetryCase>,
    pub recovery_sweep_cases: Vec<LeanRecoverySweepCase>,
    pub r6_backgrounding_cases: Vec<LeanR6BackgroundingCase>,
    pub transcript_conformance_cases: Vec<LeanTranscriptCase>,
    pub canonical_execution_gate_cases: Vec<LeanCanonicalExecutionCase>,
    pub canonical_dispatch_observation_cases: Vec<LeanDispatchObservationCase>,
    pub session_input_edit_cases: Vec<LeanSessionInputEditCase>,
    pub families: BTreeMap<String, LeanFamily>,
    pub family_vocabularies: BTreeMap<String, serde_json::Value>,
    pub coverage_ledger: Vec<LeanCoverageEntry>,
    pub feature_surface_requirements: Vec<LeanFeatureSurfaceRequirement>,
    pub feature_matrix: LeanFeatureMatrix,
    pub identity_household_cases: Vec<LeanIdentityHouseholdCase>,
    pub recovery_sweep_ids: Vec<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub causal_hop_default_max_request_hop: Option<serde_json::Value>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGrantRecipientCase {
    pub recipient: String,
    pub direct: Vec<String>,
    pub projected: Vec<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanMediaKind {
    Image,
    Audio,
    Video,
    Document,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanQueueManagementState {
    pub active: Option<u64>,
    pub pending: Vec<LeanFoldQueueEntry>,
    pub folding: Vec<LeanFoldQueueEntry>,
    pub terminal: Vec<u64>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionLease {
    pub status: LeanRequestExecutionLeaseStatus,
    pub generation: Option<u64>,
    pub duration: Option<u64>,
    pub explicit_deadline: Option<u64>,
    pub outcome: Option<LeanRequestExecutionOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionLeaseStatus {
    Vacant,
    Active,
    Recoverable,
    Terminal,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum LeanRequestPurpose {
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "title-audit")]
    TitleAudit,
    #[serde(rename = "access-review")]
    AccessReview,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanTerminalSelection {
    Message { id: u64 },
    NoMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolIdentity {
    pub id: String,
    pub call_id: Option<String>,
    pub name: String,
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
