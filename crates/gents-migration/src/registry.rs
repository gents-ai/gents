//! Baseline table and declarative step chain.
//!
//! Types are lifetime-parameterized so tests can inject discovered pins
//! (`DynamicRegistry`) while production keeps `'static` constants.

use crate::expectation::CollectionExpectation;

/// One collection registered at the migration baseline (lineage root).
#[derive(Debug, Clone, Copy)]
pub struct BaselineCollection<'a> {
    /// Collection name (must match the SDL type name).
    pub name: &'a str,
    /// GraphQL SDL for `add_schema`.
    pub sdl: &'a str,
    /// Pinned root VersionID. `None` until chain-replay freezes pins.
    pub expected_version: Option<&'a str>,
    /// Full post-state expectation for the active baseline version.
    pub expected_state: CollectionExpectation,
}

/// Embedded wasm + args for a lens edge.
#[derive(Debug, Clone, Copy)]
pub struct LensSpec<'a> {
    /// Raw wasm module bytes (always `from_bytes` — never path).
    pub wasm: &'a [u8],
    /// Optional JSON args string for the module.
    pub args_json: Option<&'a str>,
}

/// One declarative migration step.
#[derive(Debug, Clone, Copy)]
pub enum MigrationStep<'a> {
    /// Register a collection that did not exist at the baseline.
    AddCollection {
        id: &'a str,
        sdl: &'a str,
        expected_version: Option<&'a str>,
        expected_state: CollectionExpectation,
    },
    /// Versioned change (field add/rename) with optional lens.
    PatchVersioned {
        id: &'a str,
        collection: &'a str,
        /// RFC 6902 patch; must include IsActive:false for the safe sequence.
        patch: &'a str,
        lens: Option<LensSpec<'a>>,
        expected_version: Option<&'a str>,
        expected_transform: Option<&'a str>,
        expected_state: CollectionExpectation,
    },
    /// In-place metadata change (indexes, embeddings) — no new version CID.
    PatchInPlace {
        id: &'a str,
        collection: &'a str,
        patch: &'a str,
        expected_state: CollectionExpectation,
    },
}

impl<'a> MigrationStep<'a> {
    /// Stable step id for errors and reports.
    pub fn id(&self) -> &'a str {
        match self {
            Self::AddCollection { id, .. }
            | Self::PatchVersioned { id, .. }
            | Self::PatchInPlace { id, .. } => id,
        }
    }

    /// Primary collection this step touches, when applicable.
    pub fn collection(&self) -> Option<&'a str> {
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
pub struct Registry<'a> {
    pub baseline: &'a [BaselineCollection<'a>],
    pub steps: &'a [MigrationStep<'a>],
}

impl<'a> Registry<'a> {
    /// Names of every collection managed by this registry (baseline only;
    /// AddCollection steps extend the managed set at apply time).
    pub fn managed_names(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.baseline.iter().map(|b| b.name)
    }
}

// ---------------------------------------------------------------------------
// Owned / dynamic registry (tests + pin authoring)
// ---------------------------------------------------------------------------

/// Owned baseline entry for dynamic registries.
#[derive(Debug, Clone)]
pub struct BaselineCollectionOwned {
    pub name: String,
    pub sdl: String,
    pub expected_version: Option<String>,
    pub expected_state: CollectionExpectation,
}

/// Owned lens spec (wasm held by the owner).
#[derive(Debug, Clone)]
pub struct LensSpecOwned {
    pub wasm: Vec<u8>,
    pub args_json: Option<String>,
}

/// Owned step for dynamic registries.
#[derive(Debug, Clone)]
pub enum MigrationStepOwned {
    AddCollection {
        id: String,
        sdl: String,
        expected_version: Option<String>,
        expected_state: CollectionExpectation,
    },
    PatchVersioned {
        id: String,
        collection: String,
        patch: String,
        lens: Option<LensSpecOwned>,
        expected_version: Option<String>,
        expected_transform: Option<String>,
        expected_state: CollectionExpectation,
    },
    PatchInPlace {
        id: String,
        collection: String,
        patch: String,
        expected_state: CollectionExpectation,
    },
}

/// Heap-owned registry used by conformance tests that discover pins at runtime.
#[derive(Debug, Clone, Default)]
pub struct DynamicRegistry {
    pub baseline: Vec<BaselineCollectionOwned>,
    pub steps: Vec<MigrationStepOwned>,
}

impl DynamicRegistry {
    /// Borrow as a [`Registry`] for the engine. The returned views are valid
    /// for the lifetime of `self`.
    pub fn as_registry(&self) -> (Vec<BaselineCollection<'_>>, Vec<MigrationStep<'_>>) {
        let baseline = self
            .baseline
            .iter()
            .map(|b| BaselineCollection {
                name: b.name.as_str(),
                sdl: b.sdl.as_str(),
                expected_version: b.expected_version.as_deref(),
                expected_state: b.expected_state,
            })
            .collect();
        let steps = self
            .steps
            .iter()
            .map(|s| match s {
                MigrationStepOwned::AddCollection {
                    id,
                    sdl,
                    expected_version,
                    expected_state,
                } => MigrationStep::AddCollection {
                    id: id.as_str(),
                    sdl: sdl.as_str(),
                    expected_version: expected_version.as_deref(),
                    expected_state: *expected_state,
                },
                MigrationStepOwned::PatchVersioned {
                    id,
                    collection,
                    patch,
                    lens,
                    expected_version,
                    expected_transform,
                    expected_state,
                } => MigrationStep::PatchVersioned {
                    id: id.as_str(),
                    collection: collection.as_str(),
                    patch: patch.as_str(),
                    lens: lens.as_ref().map(|l| LensSpec {
                        wasm: l.wasm.as_slice(),
                        args_json: l.args_json.as_deref(),
                    }),
                    expected_version: expected_version.as_deref(),
                    expected_transform: expected_transform.as_deref(),
                    expected_state: *expected_state,
                },
                MigrationStepOwned::PatchInPlace {
                    id,
                    collection,
                    patch,
                    expected_state,
                } => MigrationStep::PatchInPlace {
                    id: id.as_str(),
                    collection: collection.as_str(),
                    patch: patch.as_str(),
                    expected_state: *expected_state,
                },
            })
            .collect();
        (baseline, steps)
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
pub static DEFAULT_BASELINE: &[BaselineCollection<'static>] = &[
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_BACKEND_NAME,
        gents_protocol::schemas::INFERENCE_BACKEND
    ),
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
        gents_protocol::schemas::AGENT_DIRECTORY_ENTRY_NAME,
        gents_protocol::schemas::AGENT_DIRECTORY_ENTRY
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

/// Empty post-baseline chain at cutover. Real steps land when schema changes.
pub static DEFAULT_STEPS: &[MigrationStep<'static>] = &[];

/// Production registry: full baseline, zero steps.
pub static DEFAULT_REGISTRY: Registry<'static> = Registry {
    baseline: DEFAULT_BASELINE,
    steps: DEFAULT_STEPS,
};

/// Embedded fixture lens wasm (built by `build.rs`).
pub fn fixture_lens_wasm() -> &'static [u8] {
    include_bytes!(env!("GENTS_LENS_FIXTURE_ADD_LABEL_WASM_PATH"))
}
