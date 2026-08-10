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
    ($name:expr, $sdl:expr, $version:literal) => {
        BaselineCollection {
            name: $name,
            sdl: $sdl,
            expected_version: Some($version),
            expected_state: CollectionExpectation::dag_only(),
        }
    };
}

// Frozen at the migration cutover. New fields belong in DEFAULT_STEPS so
// existing stores retain a known lineage instead of silently changing roots.
const INFERENCE_PROFILE_BASELINE_SDL: &str = r#"
type InferenceProfile {
    profile_id: String @index(unique: true)
    display_name: String
    context_window: Int
    max_output_tokens: Int
    max_turns: Int
    temperature: Float
    top_p: Float
    top_k: Int
    min_p: Float
    frequency_penalty: Float
    presence_penalty: Float
    repetition_penalty: Float
    stream_batch_ms: Int
    stream_liveness_timeout_secs: Int
    deadline_duration_secs: Int
    retry_max_transport: Int
    retry_backoff_ms: [Int]
    retry_max_resample: Int
    retry_allow_repair: Boolean
    retry_interactive_max: Int
    updated_at: DateTime @index(direction: DESC)
}
"#;

// Frozen at the 1087 provenance cutover (live AgentRequest from that branch,
// before seed / max_total_tokens). New request fields advance through
// DEFAULT_STEPS so existing stores keep a known lineage.
const AGENT_REQUEST_BASELINE_SDL: &str = r#"
type AgentRequest @branchable {
    request_id: String @index
    agent_did: String @index @immutable
    requester_did: String @index @immutable
    behavior_id: String @index
    session_id: String @index
    retry_parent_request: String
    retry_parent_request_doc_id: String @index @immutable
    retry_root_request: String
    retry_key: String @index(unique: true) @immutable
    superseded_by_request: String
    superseded_by_request_doc_id: String @index
    content: String
    temperature: Float
    top_p: Float
    top_k: Int
    max_tokens: Int
    metadata: String
    status: String @index
    lifecycle_state: String @index
    backend_id: String @index
    execution_origin: String
    caused_by_trigger_id: String @index
    caused_by_trigger_kind: String @index
    caused_by_source_doc_id: String @index @immutable
    failure_reason: String
    terminalized_at: String @index
    terminal_redrive_attempts: Int @index
    created_at: String @index
    claimed_at: String
    deadline: String
    retry_count: Int
    max_retries: Int
    interrupt_requested_at: String
    valid_until: String
    subagent_depth: Int
    caused_by_parent_request_id: String @index
    caused_by_parent_request_doc_id: String @index @immutable
    caused_by_parent_tool_call_id: String @index
    caused_by_parent_tool_call_doc_id: String @index @immutable
}
"#;

const INFERENCE_PROFILE_ADD_REASONING_EFFORT_PATCH: &str = r#"[
  {"op":"add","path":"/InferenceProfile/Fields/-","value":{"Name":"reasoning_effort","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const INFERENCE_PROFILE_ADD_SEED_PATCH: &str = r#"[
  {"op":"add","path":"/InferenceProfile/Fields/-","value":{"Name":"seed","Kind":"Int"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const AGENT_REQUEST_ADD_SEED_PATCH: &str = r#"[
  {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"seed","Kind":"Int"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const AGENT_REQUEST_ADD_MAX_TOTAL_TOKENS_PATCH: &str = r#"[
  {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"max_total_tokens","Kind":"Int"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

/// Frozen baseline SDL set, ordered like
/// `gents_protocol::schemas::{RUNTIME_ALL, ALL}` and feature-invariant (includes
/// AgentMemory). Collections with post-cutover changes use frozen local SDL
/// constants here and advance through [`DEFAULT_STEPS`].
///
/// A *brand-new* collection is added here, not as a
/// [`MigrationStep::AddCollection`]. The two are mutually exclusive: the
/// baseline is asserted set-equal and order-equal to the protocol catalog, no
/// pin-authoring workflow exists for steps, and `Registry::managed_names`
/// excludes AddCollection collections from eager materialization. Adding a new
/// collection changes no existing lineage — `register_baseline` simply
/// registers it on stores that lack it.
pub static DEFAULT_BASELINE: &[BaselineCollection<'static>] = &[
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_BACKEND_NAME,
        gents_protocol::schemas::INFERENCE_BACKEND,
        "bafyreifljyf2sr7czygvf6y6cy2rlsg2c2brmzegx5wpedpqnf6hn745ju"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_PRINCIPAL_NAME,
        gents_protocol::schemas::AGENT_PRINCIPAL,
        "bafyreiar2j7qsshchz3dsm4olsgql2z2gfjsvfwgu2kr5cnmer64yto63i"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_BEHAVIOR_NAME,
        gents_protocol::schemas::AGENT_BEHAVIOR,
        "bafyreie27gfobswc4wntubqfg4ki3laofglss3mam53uqrru6shtjlutwu"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_RUNTIME_NAME,
        gents_protocol::schemas::AGENT_RUNTIME,
        "bafyreig2kkghd2y4fwjjzomtsp753nby752xon7rx4jt3efl4xflhm6wgy"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_DIRECTORY_ENTRY_NAME,
        gents_protocol::schemas::AGENT_DIRECTORY_ENTRY,
        "bafyreibeqn5k6xtjkespahskl7irv7eulokw4yywolddm2yzdydtyoi4nu"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_MEMORY_NAME,
        gents_protocol::schemas::AGENT_MEMORY,
        "bafyreidqrnco3ylgzeucb6vu2dhhkviklq23nwpn4npqblkm64bntdbbli"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SELECTION_NAME,
        gents_protocol::schemas::TOOL_SELECTION,
        "bafyreie4seb5qunpvokrmvdumozlefwovchlc3arpwr7afldydpfyeozfy"
    ),
    baseline_entry!(
        gents_protocol::schemas::SKILL_NAME,
        gents_protocol::schemas::SKILL,
        "bafyreib6grod5kwezldwy74gt5425ewoymiyyjvmygtfzhq25zwngwsrly"
    ),
    baseline_entry!(
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE_NAME,
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE,
        "bafyreib5unizcyuuwsfcabepagjvuac23xpnqrt3wl7fkhfp5lwlfas6oi"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_ROOT_NAME,
        gents_protocol::schemas::WORKSPACE_ROOT,
        "bafyreibw7kuk4xise6epukrca2inza3j44bgsxfbkse3tkbk6enqzsr6ui"
    ),
    baseline_entry!(
        gents_protocol::schemas::OAUTH_CREDENTIAL_NAME,
        gents_protocol::schemas::OAUTH_CREDENTIAL,
        "bafyreiab3wqm3em2cepvj22l733ziz4azytl3gc7zozcm5e2s7nuehkx6u"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        INFERENCE_PROFILE_BASELINE_SDL,
        "bafyreibhnljm6hqgbiyct7fq53vpfagmn2q2pe2apykujttk6tghwtqb5e"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_CALL_NAME,
        gents_protocol::schemas::INFERENCE_CALL,
        "bafyreidz4yn2zxshvpjekf42uotxd3wnrurzldnt2t4ldlnomi2gibtipm"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_CONVERSATION_NAME,
        gents_protocol::schemas::AGENT_CONVERSATION,
        "bafyreide7lgaj6zensfdbrhhafhpj3yxedj3luuhmnttt23qoma7isnnoa"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_REQUEST_NAME,
        AGENT_REQUEST_BASELINE_SDL,
        // Same root pin as 1087's live AgentRequest (baseline is that SDL).
        "bafyreigk6dk2fv33a2cguirrafuxaghlcs5ucwoqpxokfckcn5km5wlf3a"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_RESPONSE_NAME,
        gents_protocol::schemas::AGENT_RESPONSE,
        "bafyreigr4eflydkzsigq7m2dzpdd7yy3ny5zwdwicefyzntjrsfiptua2u"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_RESULT_NAME,
        gents_protocol::schemas::AGENT_TOOL_RESULT,
        "bafyreihsejgpwhha27y2sdaigigxdqv6tvqr6h7latzmlpns2qusqzck34"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_NAME,
        gents_protocol::schemas::AGENT_SESSION,
        "bafyreih3e34ribdzce6ajpiuwjehx6tu3loeldugxj6y3ce35yv7tdzwi4"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_NAME,
        gents_protocol::schemas::GOAL,
        "bafyreig5hlyzlujmegnnlww6tjt6krquzuq2ltgh2pjqwwzxzjbognuguu"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_MESSAGE_NAME,
        gents_protocol::schemas::AGENT_MESSAGE,
        "bafyreig7x5jbsj5mlpd2k2whc2v6d4tbwnwik6l3nvu67oiwb2vc4x2wru"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        gents_protocol::schemas::AGENT_TOOL_CALL,
        "bafyreicok6ibr6xcnu4c25wec4pp4ed2h6d4onb7lmsm5m337ijua6e4fa"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_APPROVAL_NAME,
        gents_protocol::schemas::AGENT_TOOL_APPROVAL,
        "bafyreibtmy2oaf5j5gxlugjofklqdrw6et2vuf3h2wnybhl3cyb6kx3fd4"
    ),
    baseline_entry!(
        gents_protocol::schemas::COMPACTION_ENTRY_NAME,
        gents_protocol::schemas::COMPACTION_ENTRY,
        "bafyreihhmopyxndufk2fktp6ctbietqgv4jas76hdlrmvvdzabxyo7affi"
    ),
    baseline_entry!(
        gents_protocol::schemas::RENDERED_REQUEST_NAME,
        gents_protocol::schemas::RENDERED_REQUEST,
        "bafyreicderii4drvuggodfzo24q5ergcponrix4u6zv6qfo75uvescmwh4"
    ),
    baseline_entry!(
        gents_protocol::schemas::PROJECTION_ACP_BINDING_NAME,
        gents_protocol::schemas::PROJECTION_ACP_BINDING,
        "bafyreiauzohlxkx3x7wndadh7yl3pfbknle6crgjl7mpcqt37onus6em4i"
    ),
    baseline_entry!(
        gents_protocol::schemas::TASK_NAME,
        gents_protocol::schemas::TASK,
        "bafyreih2yansmfmsye5xktsx2rbf7tri4zvtifselok46pdmm4qmde7blu"
    ),
    baseline_entry!(
        gents_protocol::schemas::SCHEDULE_NAME,
        gents_protocol::schemas::SCHEDULE,
        "bafyreid2l4a57zydsgrxret3qkewued42bxjb4pcaqalqnjkhinlz4gsn4"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_TRIGGER_NAME,
        gents_protocol::schemas::EVENT_TRIGGER,
        "bafyreih4b54rbekqry2nwymuppgil7v2ldmxirim5upv7nbjkvxjhihnwi"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SERVICE_REGISTRY_NAME,
        gents_protocol::schemas::TOOL_SERVICE_REGISTRY,
        "bafyreidyt2lufdrv2dhjsm2kusylwekdqktefp7jeyyvfik76zchfp5plq"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE_NAME,
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE,
        "bafyreif3vui3absvxqcthnguigulgso7w7ktcfo3orptrgqlhmp6ae2ani"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::PEER_PAIRING_DESIRED,
        "bafyreicoms7ndji7z76sellaqumottresgd3uojmvrts5hxciho4lvu5xy"
    ),
    baseline_entry!(
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED,
        "bafyreia63drc777juius2tcsukzfnw425hjyz4xchz6f6ykeoed2gqmjd4"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_APPLIED_NAME,
        gents_protocol::schemas::PEER_PAIRING_APPLIED,
        "bafyreifunn7vevp6b6rzg232gjfypp2lqviafe5now5ldlwo3na5nfinq4"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_REGISTRY_NAME,
        gents_protocol::schemas::PEER_REGISTRY,
        "bafyreieyihzl6ibujs4jzxji64gmpff7jcyqqjpdfxz6my6dcswafjwhla"
    ),
    baseline_entry!(
        gents_protocol::schemas::CONSUMED_INVITE_NONCE_NAME,
        gents_protocol::schemas::CONSUMED_INVITE_NONCE,
        "bafyreigbedn4ebkz4cap5x6d53uif2ddimsyivpxbbijkjhqedbpuponfa"
    ),
    baseline_entry!(
        gents_protocol::schemas::RECIPROCAL_CONVERSATION_INTENT_NAME,
        gents_protocol::schemas::RECIPROCAL_CONVERSATION_INTENT,
        "bafyreid6huhh2y4sslnmuw55mk7mnzundgjpjbqhihhfmhs3yrp2aaq6bm"
    ),
    baseline_entry!(
        gents_protocol::schemas::PAIRING_BEARER_CLAIM_NAME,
        gents_protocol::schemas::PAIRING_BEARER_CLAIM,
        "bafyreidlvcvil4o22lsy2byeh2yjugpj5mnnvkotxj56pg6mjswg2bzjjq"
    ),
    baseline_entry!(
        gents_protocol::schemas::BEARER_PAIRING_READY_NAME,
        gents_protocol::schemas::BEARER_PAIRING_READY,
        "bafyreihuy5majnvf6mqy3ow54xjlzllgwqcvivuz2kvqtjrhdmb427ynry"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_NETWORK_NAME,
        gents_protocol::schemas::AGENT_NETWORK,
        "bafyreifafg2su5zfp2zzrmtnp2we5iu2owkweuevvu4hq25qposuyiuyfm"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_MEMBERSHIP_NAME,
        gents_protocol::schemas::NETWORK_MEMBERSHIP,
        "bafyreiav6v7v2nab45gyqkcj2jrp7mfmtojuc7ypupzw4qnpsd5sidqni4"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_ENDPOINT_NAME,
        gents_protocol::schemas::PEER_ENDPOINT,
        "bafyreidubdiopvxh3zm447ttse6fbs7jzyagiyt7ipw4toib2z3svr4neq"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_JOIN_REQUEST_NAME,
        gents_protocol::schemas::NETWORK_JOIN_REQUEST,
        "bafyreib5ufwrdzy77qvfziodcdgevd44pqoix4jcrriir3arpyyjwhdjym"
    ),
    baseline_entry!(
        gents_protocol::schemas::PERSONA_CONFIG_REQUEST_NAME,
        gents_protocol::schemas::PERSONA_CONFIG_REQUEST,
        "bafyreihvhau2vf2wxh6jfbyfbwdndyfsrfamfvpceghflx4m7vdaangb5q"
    ),
];

/// Ordered post-baseline schema evolution chain.
pub static DEFAULT_STEPS: &[MigrationStep<'static>] = &[
    MigrationStep::PatchVersioned {
        id: "inference-profile-add-reasoning-effort",
        collection: gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        patch: INFERENCE_PROFILE_ADD_REASONING_EFFORT_PATCH,
        lens: None,
        // Authored by applying the inactive patch to the frozen baseline.
        expected_version: Some("bafyreigiimbcequesxdifamoiiqio2loqn7uco7kt4slp2ws3no4prl25e"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["reasoning_effort"]),
    },
    MigrationStep::PatchVersioned {
        id: "inference-profile-add-seed",
        collection: gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        patch: INFERENCE_PROFILE_ADD_SEED_PATCH,
        lens: None,
        expected_version: Some("bafyreid4qn3axuic3fced2jp2vpsvjwrn4gisexrp3ri2zkiou3eeinyme"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["reasoning_effort", "seed"]),
    },
    MigrationStep::PatchVersioned {
        id: "agent-request-add-seed",
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        patch: AGENT_REQUEST_ADD_SEED_PATCH,
        lens: None,
        // Authored against the 1087 AgentRequest baseline (physical doc_id edges).
        expected_version: Some("bafyreiad77dcqzv325ooveypjj32l7qrkpltizvqtsphhdgehoz5igwwye"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["seed"]),
    },
    MigrationStep::PatchVersioned {
        id: "agent-request-add-max-total-tokens",
        collection: gents_protocol::schemas::AGENT_REQUEST_NAME,
        patch: AGENT_REQUEST_ADD_MAX_TOTAL_TOKENS_PATCH,
        lens: None,
        expected_version: Some("bafyreic32bvie4zcjcaeqycfxcwxls6qgv5o6xivkd7tksz4rlb4xvnfqe"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["seed", "max_total_tokens"]),
    },
];

/// Production registry: frozen baseline plus the ordered migration chain.
pub static DEFAULT_REGISTRY: Registry<'static> = Registry {
    baseline: DEFAULT_BASELINE,
    steps: DEFAULT_STEPS,
};

/// Embedded fixture lens wasm (built by `build.rs`).
pub fn fixture_lens_wasm() -> &'static [u8] {
    include_bytes!(env!("GENTS_LENS_FIXTURE_ADD_LABEL_WASM_PATH"))
}
