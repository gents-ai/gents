//! Baseline table and declarative step chain.
//!
//! The default registry freezes the cutover baseline: every gents-protocol
//! schema, feature-invariant, with zero post-baseline steps. Version ID pins
//! are filled by the chain-replay authoring test (Phase A ships `None` pins
//! and enforces the single-version DAG shape instead).

use crate::expectation::CollectionExpectation;

/// One collection registered at the migration baseline (lineage root).
#[derive(Debug, Clone, Copy)]
pub struct BaselineCollection {
    /// Collection name (must match the SDL type name).
    pub name: &'static str,
    /// GraphQL SDL for `add_schema`.
    pub sdl: &'static str,
    /// Pinned root VersionID. `None` during Phase A authoring.
    pub expected_version: Option<&'static str>,
    /// Full post-state expectation for the active baseline version.
    pub expected_state: CollectionExpectation,
}

/// Embedded wasm + args for a lens edge.
#[derive(Debug, Clone, Copy)]
pub struct LensSpec {
    /// Raw wasm module bytes (always `from_bytes` — never path).
    pub wasm: &'static [u8],
    /// Optional JSON args string for the module.
    pub args_json: Option<&'static str>,
}

/// One declarative migration step.
#[derive(Debug, Clone, Copy)]
pub enum MigrationStep {
    /// Register a collection that did not exist at the baseline.
    AddCollection {
        id: &'static str,
        sdl: &'static str,
        expected_version: Option<&'static str>,
        expected_state: CollectionExpectation,
    },
    /// Versioned change (field add/rename) with optional lens.
    PatchVersioned {
        id: &'static str,
        collection: &'static str,
        /// RFC 6902 patch; must include IsActive:false for the safe sequence.
        patch: &'static str,
        lens: Option<LensSpec>,
        expected_version: Option<&'static str>,
        expected_transform: Option<&'static str>,
        expected_state: CollectionExpectation,
    },
    /// In-place metadata change (indexes, embeddings) — no new version CID.
    PatchInPlace {
        id: &'static str,
        collection: &'static str,
        patch: &'static str,
        expected_state: CollectionExpectation,
    },
}

impl MigrationStep {
    /// Stable step id for errors and reports.
    pub fn id(&self) -> &'static str {
        match self {
            Self::AddCollection { id, .. }
            | Self::PatchVersioned { id, .. }
            | Self::PatchInPlace { id, .. } => id,
        }
    }

    /// Primary collection this step touches, when applicable.
    pub fn collection(&self) -> Option<&'static str> {
        match self {
            Self::AddCollection { .. } => None,
            Self::PatchVersioned { collection, .. } | Self::PatchInPlace { collection, .. } => {
                Some(*collection)
            }
        }
    }
}

/// Full migration registry: baseline + ordered step chain.
#[derive(Debug, Clone, Copy)]
pub struct Registry {
    pub baseline: &'static [BaselineCollection],
    pub steps: &'static [MigrationStep],
}

impl Registry {
    /// Names of every collection managed by this registry (baseline only for
    /// Phase A; AddCollection steps extend this at apply time).
    pub fn managed_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.baseline.iter().map(|b| b.name)
    }
}

// ---------------------------------------------------------------------------
// Default production registry (cutover baseline, zero steps)
// ---------------------------------------------------------------------------

macro_rules! baseline_entry {
    ($name:expr, $sdl:expr) => {
        BaselineCollection {
            name: $name,
            sdl: $sdl,
            expected_version: None,
            expected_state: CollectionExpectation::dag_only(),
        }
    };
}

/// Baseline SDL set: every schema in `gents_protocol::schemas::{RUNTIME_ALL, ALL}`,
/// feature-invariant (includes AgentMemory).
///
/// Order matches historical registration so relation resolution stays stable.
pub static DEFAULT_BASELINE: &[BaselineCollection] = &[
    // RUNTIME_ALL first (InferenceBackend)
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_BACKEND_NAME,
        gents_protocol::schemas::INFERENCE_BACKEND
    ),
    // ALL — same order as gents_protocol::schemas::ALL
    baseline_entry!(
        gents_protocol::schemas::AGENT_PRINCIPAL_NAME,
        gents_protocol::schemas::AGENT_PRINCIPAL
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_BEHAVIOR_NAME,
        gents_protocol::schemas::AGENT_BEHAVIOR
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_RUNTIME_NAME,
        gents_protocol::schemas::AGENT_RUNTIME
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_MEMORY_NAME,
        gents_protocol::schemas::AGENT_MEMORY
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SELECTION_NAME,
        gents_protocol::schemas::TOOL_SELECTION
    ),
    baseline_entry!(
        gents_protocol::schemas::SKILL_NAME,
        gents_protocol::schemas::SKILL
    ),
    baseline_entry!(
        gents_protocol::schemas::OAUTH_CREDENTIAL_NAME,
        gents_protocol::schemas::OAUTH_CREDENTIAL
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        gents_protocol::schemas::INFERENCE_PROFILE
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_CALL_NAME,
        gents_protocol::schemas::INFERENCE_CALL
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_CONVERSATION_NAME,
        gents_protocol::schemas::AGENT_CONVERSATION
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_REQUEST_NAME,
        gents_protocol::schemas::AGENT_REQUEST
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_RESPONSE_NAME,
        gents_protocol::schemas::AGENT_RESPONSE
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_RESULT_NAME,
        gents_protocol::schemas::AGENT_TOOL_RESULT
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_NAME,
        gents_protocol::schemas::AGENT_SESSION
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_NAME,
        gents_protocol::schemas::GOAL
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_MESSAGE_NAME,
        gents_protocol::schemas::AGENT_MESSAGE
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        gents_protocol::schemas::AGENT_TOOL_CALL
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_APPROVAL_NAME,
        gents_protocol::schemas::AGENT_TOOL_APPROVAL
    ),
    baseline_entry!(
        gents_protocol::schemas::COMPACTION_ENTRY_NAME,
        gents_protocol::schemas::COMPACTION_ENTRY
    ),
    baseline_entry!(
        gents_protocol::schemas::PROJECTION_ACP_BINDING_NAME,
        gents_protocol::schemas::PROJECTION_ACP_BINDING
    ),
    baseline_entry!(
        gents_protocol::schemas::TASK_NAME,
        gents_protocol::schemas::TASK
    ),
    baseline_entry!(
        gents_protocol::schemas::SCHEDULE_NAME,
        gents_protocol::schemas::SCHEDULE
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_TRIGGER_NAME,
        gents_protocol::schemas::EVENT_TRIGGER
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SERVICE_REGISTRY_NAME,
        gents_protocol::schemas::TOOL_SERVICE_REGISTRY
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE_NAME,
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::PEER_PAIRING_DESIRED
    ),
    baseline_entry!(
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_APPLIED_NAME,
        gents_protocol::schemas::PEER_PAIRING_APPLIED
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_REGISTRY_NAME,
        gents_protocol::schemas::PEER_REGISTRY
    ),
    baseline_entry!(
        gents_protocol::schemas::CONSUMED_INVITE_NONCE_NAME,
        gents_protocol::schemas::CONSUMED_INVITE_NONCE
    ),
    baseline_entry!(
        gents_protocol::schemas::RECIPROCAL_CONVERSATION_INTENT_NAME,
        gents_protocol::schemas::RECIPROCAL_CONVERSATION_INTENT
    ),
    baseline_entry!(
        gents_protocol::schemas::PAIRING_BEARER_CLAIM_NAME,
        gents_protocol::schemas::PAIRING_BEARER_CLAIM
    ),
    baseline_entry!(
        gents_protocol::schemas::BEARER_PAIRING_READY_NAME,
        gents_protocol::schemas::BEARER_PAIRING_READY
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_NETWORK_NAME,
        gents_protocol::schemas::AGENT_NETWORK
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_MEMBERSHIP_NAME,
        gents_protocol::schemas::NETWORK_MEMBERSHIP
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_ENDPOINT_NAME,
        gents_protocol::schemas::PEER_ENDPOINT
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_JOIN_REQUEST_NAME,
        gents_protocol::schemas::NETWORK_JOIN_REQUEST
    ),
];

/// Empty post-baseline chain at cutover.
pub static DEFAULT_STEPS: &[MigrationStep] = &[];

/// Production registry: full baseline, zero steps.
pub static DEFAULT_REGISTRY: Registry = Registry {
    baseline: DEFAULT_BASELINE,
    steps: DEFAULT_STEPS,
};
