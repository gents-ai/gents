use crate::*;
use crate::canonical_output::LeanCanonicalMessage;
use crate::required_nullable;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn lean_auxiliary_output_cases() -> &'static [LeanAuxiliaryOutputCase] {
    &lean_contract_snapshot().auxiliary_output_cases
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

pub fn lean_client_session_turn_cases() -> &'static [LeanSessionTurnCase] {
    &lean_contract_snapshot().client_session_turn_cases
}

pub fn lean_compaction_canonical_projection_cases() -> &'static [LeanCanonicalCompactionCase] {
    &lean_contract_snapshot().compaction_canonical_projection_cases
}

pub fn lean_completion_retry_cases() -> &'static [LeanCompletionRetryCase] {
    &lean_contract_snapshot().completion_retry_cases
}

pub fn lean_composed_invariant_witnesses() -> &'static [LeanComposedInvariantWitness] {
    &lean_contract_snapshot().composed_invariant_witnesses
}

pub fn lean_feature_matrix() -> &'static LeanFeatureMatrix {
    &lean_contract_snapshot().feature_matrix
}

pub fn lean_feature_surface_requirements() -> &'static [LeanFeatureSurfaceRequirement] {
    &lean_contract_snapshot().feature_surface_requirements
}

pub fn lean_goal_create_cases() -> &'static [LeanGoalCreateCase] {
    &lean_contract_snapshot().goal_create_cases
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

pub fn lean_managed_exec_liveness_cases() -> &'static [LeanManagedExecLivenessCase] {
    &lean_contract_snapshot().managed_exec_liveness_cases
}

pub fn lean_managed_exec_tool_boundary_cases() -> &'static [LeanManagedExecToolBoundaryCase] {
    &lean_contract_snapshot().managed_exec_tool_boundary_cases
}

pub fn lean_native_filesystem_boundary_cases() -> &'static [LeanNativeFilesystemBoundaryCase] {
    &lean_contract_snapshot().native_filesystem_boundary_cases
}

pub fn lean_persistence_failure_policy_cases() -> &'static [LeanPersistenceFailurePolicyCase] {
    &lean_contract_snapshot().persistence_failure_policy_cases
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

pub fn lean_process_stop_cases() -> &'static [LeanProcessStopCase] {
    &lean_contract_snapshot().process_stop_cases
}

pub fn lean_process_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().process_transition_cases
}

pub fn lean_queue_deadline_cases() -> &'static [LeanQueueDeadlineConformanceCase] {
    &lean_contract_snapshot().queue_deadline_conformance_cases
}

pub fn lean_r6_backgrounding_cases() -> &'static [LeanR6BackgroundingCase] {
    &lean_contract_snapshot().r6_backgrounding_cases
}

pub fn lean_reasoning_audit_cases() -> &'static [LeanReasoningAuditCase] {
    &lean_contract_snapshot().reasoning_audit_cases
}

pub fn lean_reasoning_signature_cases() -> &'static [LeanReasoningSignatureCase] {
    &lean_contract_snapshot().reasoning_signature_cases
}

pub fn lean_recovery_sweep_cases() -> &'static [LeanRecoverySweepCase] {
    &lean_contract_snapshot().recovery_sweep_cases
}

pub fn lean_request_progress_cases() -> &'static [LeanRequestProgressCase] {
    &lean_contract_snapshot().request_progress_cases
}

pub fn lean_request_transition_cases() -> &'static [LeanLifecycleTransitionCase] {
    &lean_contract_snapshot().request_transition_cases
}

pub fn lean_runtime_reconcile_cases() -> &'static [LeanRuntimeReconcileCase] {
    &lean_contract_snapshot().runtime_reconcile_cases
}

pub fn lean_session_input_edit_cases() -> &'static [LeanSessionInputEditCase] {
    &lean_contract_snapshot().session_input_edit_cases
}

pub fn lean_storage_observation_runtime_cases() -> &'static [LeanStorageObservationRuntimeCase] {
    &lean_contract_snapshot().storage_observation_runtime_cases
}

pub fn lean_task_goal_publication_cases() -> &'static [LeanTaskGoalPublicationCase] {
    &lean_contract_snapshot().task_goal_publication_cases
}

pub fn lean_transcript_cases() -> &'static [LeanTranscriptCase] {
    &lean_contract_snapshot().transcript_conformance_cases
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanClaimedDid {
    Key(String),
    Derived { node_key: String, creation_nonce: String },
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanContractSnapshot {
    pub tool_admission_handoff_cases: Vec<LeanToolAdmissionHandoffCase>,
    pub delegated_publication_cases: Vec<serde_json::Value>,
    pub generated_by: String,
    pub vocabularies: Vec<LeanVocabularyContract>,
    pub goal_claimed_publication_cases: Vec<LeanGoalClaimedPublicationCase>,
    pub goal_operator_resume_cases: Vec<LeanGoalOperatorResumeCase>,
    pub goal_operator_resume_on_cases: Vec<LeanGoalOperatorResumeOnCase>,
    pub trigger_delivery_callback_visibility_cases: serde_json::Value,
    pub trigger_delivery_latest_admission_cases: serde_json::Value,
    pub workspace_path_capability_cases: Vec<LeanWorkspacePathCapabilityCase>,
    pub file_inventory_direct_reader_cases: Vec<serde_json::Value>,
    pub file_inventory_fork_cases: Vec<serde_json::Value>,
    pub file_inventory_reader_cases: Vec<serde_json::Value>,
    pub file_inventory_source_projection_cases: Vec<serde_json::Value>,
    pub file_review_subscription_cases: Vec<LeanFileReviewSubscriptionCase>,
    pub count_carrier_cases: serde_json::Value,
    pub schema_argument_repair_cases: Vec<LeanSchemaArgumentRepairCase>,
    pub plugin_job_admission_cases: Vec<LeanPluginJobAdmissionCase>,
    pub plugin_job_adoption_cases: Vec<LeanPluginJobAdoptionCase>,
    pub plugin_job_control_cases: Vec<LeanPluginJobControlCase>,
    pub identity_publication_cases: Vec<LeanIdentityPublicationCase>,
    pub identity_grant_recipient_cases: Vec<LeanGrantRecipientCase>,
    pub client_publication_protected_collection: String,
    pub watcher_scoped_scan_cases: Vec<serde_json::Value>,
    pub identity_parent_publication_cases: Vec<LeanIdentityParentPublicationCase>,
    pub identity_readiness_preflight_cases: Vec<serde_json::Value>,
    pub identity_pairing_partition_cases: Vec<LeanIdentityPairingPartitionCase>,
    pub identity_pairing_reader_cases: Vec<LeanIdentityPairingReaderCase>,
    pub identity_replica_grant_cases: Vec<LeanIdentityReplicaGrantCase>,
    pub identity_enrollment_policy_cases: Vec<LeanIdentityEnrollmentPolicyCase>,
    pub identity_readiness_recovery_cases: Vec<serde_json::Value>,
    pub identity_readiness_observation_cases: Vec<serde_json::Value>,
    pub identity_readiness_settled_cases: Vec<LeanIdentityReadinessSettledCase>,
    pub goal_link_cases: Vec<LeanGoalLinkCase>,
    pub cross_actor_authorization_cases: Vec<LeanCrossActorAuthorizationCase>,
    pub client_data_publication_cases: LeanClientDataPublicationCases,
    pub client_request_closure_cases: Vec<serde_json::Value>,
    pub request_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub process_transition_cases: Vec<LeanLifecycleTransitionCase>,
    pub callback_transition_cases: Vec<LeanCallbackTransitionCase>,
    pub request_input_cases: Vec<serde_json::Value>,
    pub goal_create_cases: Vec<LeanGoalCreateCase>,
    pub goal_client_delivery_cases: Vec<serde_json::Value>,
    pub goal_client_control_cases: Vec<LeanGoalClientControlCase>,
    pub goal_client_start_cases: Vec<serde_json::Value>,
    pub goal_client_receipt_cases: Vec<LeanGoalClientReceiptCase>,
    pub plugin_approval_cases: Vec<LeanPluginApprovalCase>,
    pub task_goal_publication_cases: Vec<LeanTaskGoalPublicationCase>,
    pub causal_hop_default_max_request_hop: Option<u32>,
    pub frontend_client_shell_case_count: usize,
    pub frontend_client_shell_cases: Vec<LeanClientShellCase>,
    pub client_session_turn_cases: Vec<LeanSessionTurnCase>,
    pub runtime_reconcile_cases: Vec<LeanRuntimeReconcileCase>,
    pub persistence_failure_policy_cases: Vec<LeanPersistenceFailurePolicyCase>,
    pub storage_observation_runtime_cases: Vec<LeanStorageObservationRuntimeCase>,
    pub native_filesystem_boundary_cases: Vec<LeanNativeFilesystemBoundaryCase>,
    pub managed_exec_tool_boundary_cases: Vec<LeanManagedExecToolBoundaryCase>,
    pub managed_exec_liveness_cases: Vec<LeanManagedExecLivenessCase>,
    pub process_stop_cases: Vec<LeanProcessStopCase>,
    pub completion_retry_cases: Vec<LeanCompletionRetryCase>,
    pub deviations: Vec<LeanDeviation>,
    pub request_progress_cases: Vec<LeanRequestProgressCase>,
    pub queue_deadline_conformance_cases: Vec<LeanQueueDeadlineConformanceCase>,
    pub recovery_sweep_cases: Vec<LeanRecoverySweepCase>,
    pub r6_backgrounding_cases: Vec<LeanR6BackgroundingCase>,
    pub composed_invariant_witnesses: Vec<LeanComposedInvariantWitness>,
    pub transcript_conformance_cases: Vec<LeanTranscriptCase>,
    pub canonical_output_projection_cases: Vec<LeanCanonicalOutputProjectionCase>,
    pub reasoning_audit_cases: Vec<LeanReasoningAuditCase>,
    pub reasoning_signature_cases: Vec<LeanReasoningSignatureCase>,
    pub auxiliary_output_cases: Vec<LeanAuxiliaryOutputCase>,
    pub canonical_execution_gate_cases: Vec<LeanCanonicalExecutionCase>,
    pub canonical_dispatch_observation_cases: Vec<LeanDispatchObservationCase>,
    pub session_input_edit_cases: Vec<LeanSessionInputEditCase>,
    pub session_input_edit_client_observation_cases: Vec<
        LeanSessionInputEditClientObservationCase,
    >,
    pub canonical_payload_presentation_cases: Vec<LeanPayloadPresentationCase>,
    pub compaction_canonical_projection_cases: Vec<LeanCanonicalCompactionCase>,
    pub native_completion_cases: Vec<LeanNativeCompletionCase>,
    pub native_claim_lifetime_cases: Vec<LeanNativeClaimLifetimeCase>,
    pub families: BTreeMap<String, LeanFamily>,
    pub family_vocabularies: BTreeMap<String, serde_json::Value>,
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
    pub pack_operator_wire_cases: Vec<LeanPackOperatorWireCase>,
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFileReviewSubscriptionCase {
    pub collection: String,
    pub replicated: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanIdentityAgent {
    pub label: String,
    pub did: String,
    pub creation_nonce: String,
    pub node: String,
    pub enabled: bool,
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
pub struct LeanIdentityEnrollmentPolicyCase {
    pub actor: String,
    pub allowed: bool,
    pub document: String,
    pub name: String,
    pub permission: String,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum LeanIdentitySubject {
    Actor { actor: String },
    EntitySet { resource: String, id: String, target_relation: String },
    Object { resource: String, id: String },
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
    pub caller_abandonment_observed: bool,
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPackOperatorWireCase {
    pub allowed: bool,
    pub home: bool,
    pub operator: bool,
    pub ready: bool,
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

/// What a kill observes about the request one child caused
/// (`Recovery.KillObservation`'s contract spelling).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanPluginJobObservation {
    pub retry_key: String,
    pub observation: String,
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
pub struct LeanPluginJobReceiptDecisions {
    pub terminal_decision: String,
    pub late_decision: String,
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
pub struct LeanPluginJobWireBinding {
    pub wire: String,
    pub target: LeanPluginJobTarget,
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
pub struct LeanPluginJobWithin {
    pub base: String,
    pub cwd: String,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanTerminalSelection {
    Message { id: u64 },
    NoMessage,
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

#[derive(Debug, Deserialize)]
pub struct LeanVocabularyContract {
    pub domain: String,
    pub values: Vec<String>,
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
