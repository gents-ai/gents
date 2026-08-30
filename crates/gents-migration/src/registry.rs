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

const INFERENCE_PROFILE_ADD_REASONING_EFFORT_PATCH: &str = r#"[
  {"op":"add","path":"/InferenceProfile/Fields/-","value":{"Name":"reasoning_effort","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const INFERENCE_PROFILE_ADD_SEED_PATCH: &str = r#"[
  {"op":"add","path":"/InferenceProfile/Fields/-","value":{"Name":"seed","Kind":"Int"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

// Frozen at the migration cutover. Live `gents_protocol::schemas::TOOL_SELECTION`
// may grow fields; those belong in DEFAULT_STEPS so existing stores keep a
// known lineage instead of silently changing roots.
const TOOL_SELECTION_BASELINE_SDL: &str = include_str!("baseline/tool_selection.graphql");
const INFERENCE_CALL_BASELINE_SDL: &str = include_str!("baseline/inference_call.graphql");

const INFERENCE_CALL_ADD_CONTEXT_ACCOUNTING_PATCH: &str = r#"[
  {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"context_accounting_json","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const TOOL_SELECTION_ADD_LSP_FIELDS_PATCH: &str = r#"[
  {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"enable_lsp","Kind":"Boolean"}},
  {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"lsp_config","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const TOOL_SELECTION_ADD_REQUIRED_MCP_SERVICES_PATCH: &str = r#"[
  {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"required_mcp_service_ids","Kind":21}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const TOOL_SELECTION_ADD_ETH_TOOL_IDS_PATCH: &str = r#"[
  {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"eth_tool_ids","Kind":"[String]"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const CALLBACK_RESULT_BASELINE_SDL: &str = include_str!("baseline/callback_result.graphql");

const CALLBACK_RESULT_ADD_BINDING_ID_PATCH: &str = r#"[
  {"op":"add","path":"/CallbackResult/Fields/-","value":{"Name":"binding_id","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const EVENT_TRIGGER_BASELINE_SDL: &str = include_str!("baseline/event_trigger.graphql");
const WORKSPACE_RECEIPT_BASELINE_SDL: &str = include_str!("baseline/workspace_receipt.graphql");

const EVENT_TRIGGER_ADD_WORKSPACE_AUTHORITY_PATCH: &str = r#"[
  {"op":"add","path":"/EventTrigger/Fields/-","value":{"Name":"workspace_authority","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const CALLBACK_RESULT_ADD_WORK_UNIT_ID_PATCH: &str = r#"[
  {"op":"add","path":"/CallbackResult/Fields/-","value":{"Name":"work_unit_id","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

const WORKSPACE_RECEIPT_ADD_LINEAGE_FIELDS_PATCH: &str = r#"[
  {"op":"add","path":"/WorkspaceReceipt/Fields/-","value":{"Name":"work_unit_id","Kind":"String"}},
  {"op":"add","path":"/WorkspaceReceipt/Fields/-","value":{"Name":"caused_by_correlation","Kind":"String"}},
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
        "bafyreidb7aoppwicwdsujra6iqgejtxeohiyyx4ylif6bsyllvt2sukrpe"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_BEHAVIOR_READINESS_NAME,
        gents_protocol::schemas::AGENT_BEHAVIOR_READINESS,
        "bafyreiacvnnbi2vgx5py54oaqmbc3c4bep5nj26urw3zazxkowkncbmbym"
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
        TOOL_SELECTION_BASELINE_SDL,
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
        gents_protocol::schemas::CHAIN_KEY_BINDING_NAME,
        gents_protocol::schemas::CHAIN_KEY_BINDING,
        "bafyreieafeztdrlxxvivunowfjltmdm5ks6tjjdfuxkcpjgdidowydzure"
    ),
    baseline_entry!(
        gents_protocol::schemas::ETH_TOOL_NAME,
        gents_protocol::schemas::ETH_TOOL,
        "bafyreifog5znpi3loloky4xeprsoqksy2ylr6e4cpcs2loulcmxxxqa4dy"
    ),
    baseline_entry!(
        gents_protocol::schemas::ETH_SUBMISSION_NAME,
        gents_protocol::schemas::ETH_SUBMISSION,
        "bafyreifm54mjrhhy6wnebzgbwoimpy27ifcfrfcdq3ig4edulvgr3cm4jy"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_ROOT_NAME,
        gents_protocol::schemas::WORKSPACE_ROOT,
        "bafyreibw7kuk4xise6epukrca2inza3j44bgsxfbkse3tkbk6enqzsr6ui"
    ),
    baseline_entry!(
        gents_protocol::schemas::ISOLATED_WORKSPACE_NAME,
        gents_protocol::schemas::ISOLATED_WORKSPACE,
        "bafyreiet4b2ljharppkzevalc4krhgzby3sron4fx5ttzng7h26dus3yka"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_PLACEMENT_NAME,
        gents_protocol::schemas::WORKSPACE_PLACEMENT,
        "bafyreifmkdq2x2qunuznfeufgezni45qpdx7kavkx6khecl7gp3hlh5ot4"
    ),
    baseline_entry!(
        gents_protocol::schemas::REPOSITORY_PLACEMENT_NAME,
        gents_protocol::schemas::REPOSITORY_PLACEMENT,
        "bafyreiff6pdc63xqisj3nvzlzopttqdvtsyewl3c6btahe6tdjtotkdr6i"
    ),
    baseline_entry!(
        gents_protocol::schemas::HOST_DEPLOYMENT_NAME,
        gents_protocol::schemas::HOST_DEPLOYMENT,
        "bafyreig6g7dw6jzqlo3lilx3nd6cxtru7tleda7qa324pabl2zynqhyvwu"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_BINDING_NAME,
        gents_protocol::schemas::WORKSPACE_BINDING,
        "bafyreiaigh5f54titovkve66jkyxp3g4skygvn3tvwmgszg25h74gbjfia"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_RECEIPT_NAME,
        WORKSPACE_RECEIPT_BASELINE_SDL,
        "bafyreibhkbakhtousobptnsedlksgpm2x4fwicmdbmutx2ndpvrai5vame"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_MODULE_NAME,
        gents_protocol::schemas::CALLBACK_MODULE,
        "bafyreigyzru4dkbhixf55rbtqmqaq2fytbhubos6gjilm6z3ndoi4qzjdm"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_BINDING_NAME,
        gents_protocol::schemas::CALLBACK_BINDING,
        "bafyreiaj3lkp7ai4nx2x5qnoh23g3x3r4wuypqyxaikd6ehijq2m4d3j4y"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_INVOCATION_NAME,
        gents_protocol::schemas::CALLBACK_INVOCATION,
        "bafyreid4yx7bnhrod4h2qejd5ledox34pviuqqkqzpnzucgqids4om6gt4"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_RESULT_NAME,
        CALLBACK_RESULT_BASELINE_SDL,
        "bafyreib7bwk6btbxbumfe6pabj4avfh6alhtgck4jxrifo5odi5qx5l2ru"
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
        INFERENCE_CALL_BASELINE_SDL,
        "bafyreidz4yn2zxshvpjekf42uotxd3wnrurzldnt2t4ldlnomi2gibtipm"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_CONVERSATION_NAME,
        gents_protocol::schemas::AGENT_CONVERSATION,
        "bafyreide7lgaj6zensfdbrhhafhpj3yxedj3luuhmnttt23qoma7isnnoa"
    ),
    // Client-authored plane (#1123): kept chain-free so a fresh client store
    // fresh-applying the live SDL mints the same version identity as the
    // server. Do not add DEFAULT_STEPS entries for this collection — neither
    // PatchVersioned nor PatchInPlace (see CLIENT_AUTHORED_COLLECTIONS) —
    // any future change must land directly in the live SDL and this pin
    // must move with it.
    baseline_entry!(
        gents_protocol::schemas::AGENT_REQUEST_NAME,
        gents_protocol::schemas::AGENT_REQUEST,
        "bafyreigedclw5numik2xdoggcmxzz3vypmigy7wnvre7v2bi6hy2jvdweu"
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
        gents_protocol::schemas::MAILBOX_ITEM_NAME,
        gents_protocol::schemas::MAILBOX_ITEM,
        "bafyreidaq3d7gfg2vkux3w5fz3kk4akgwnq356niwalgggqitnr4xlj4ma"
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
        "bafyreiagy34ktocj6ththl2w4r7ikb73mxv4xdnsl4dp2glkmwke46sgeq"
    ),
    baseline_entry!(
        gents_protocol::schemas::RENDERED_REQUEST_NAME,
        gents_protocol::schemas::RENDERED_REQUEST,
        "bafyreicderii4drvuggodfzo24q5ergcponrix4u6zv6qfo75uvescmwh4"
    ),
    baseline_entry!(
        gents_protocol::schemas::PROVIDER_CONTEXT_REDUCTION_NAME,
        gents_protocol::schemas::PROVIDER_CONTEXT_REDUCTION,
        "bafyreicrv4m7jfwnfeydicb4lm4q4avcvy6uvbezxdmxlft6mcgck5twwq"
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
        EVENT_TRIGGER_BASELINE_SDL,
        "bafyreidtnxrndbqf7bkw7nzydp45hxgutjewh5w3ug2naxx3f4oudsnymq"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_TRIGGER_GROUP_STATE_NAME,
        gents_protocol::schemas::EVENT_TRIGGER_GROUP_STATE,
        "bafyreierekkunke7pqlclttwsv3iugbxqq37fz4zstkfxxuju2d7htxhly"
    ),
    baseline_entry!(
        gents_protocol::schemas::GRAPH_DEFINITION_NAME,
        gents_protocol::schemas::GRAPH_DEFINITION,
        "bafyreih2u3gjfvkom4zmckqkbdnwy7ulc7ntwn7ndgkcclv4wnespmna2m"
    ),
    baseline_entry!(
        gents_protocol::schemas::GRAPH_REVISION_NAME,
        gents_protocol::schemas::GRAPH_REVISION,
        "bafyreidnpse724dsdau3zqcqihx5vesnldll3ky2u3p6w4hjkd34r6bhgq"
    ),
    baseline_entry!(
        gents_protocol::schemas::GRAPH_RUN_NAME,
        gents_protocol::schemas::GRAPH_RUN,
        "bafyreigxtaqm3t34jy5yislq4m3ihnwxqzti35dhfwbxhhrqqp7rpn7qki"
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
        "bafyreiglnk2kzvb6eoczcyppcr6c5alihf262z24yjjaxaz7gkn6v5lgmu"
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
        gents_protocol::schemas::AGENT_NETWORK_NAME,
        gents_protocol::schemas::AGENT_NETWORK,
        "bafyreifafg2su5zfp2zzrmtnp2we5iu2owkweuevvu4hq25qposuyiuyfm"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_ENDPOINT_NAME,
        gents_protocol::schemas::PEER_ENDPOINT,
        "bafyreidubdiopvxh3zm447ttse6fbs7jzyagiyt7ipw4toib2z3svr4neq"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ADMIN_PIN_NAME,
        gents_protocol::schemas::NETWORK_ADMIN_PIN,
        "bafyreihqlke25nkquhgf3jiokj26dz2gc4do62o3odhw3iwcfbsofy5iki"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
        gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST,
        "bafyreih7u36s37itjkab6z33vayo6ig2sybwq2sn2bdvw3cnbjtjxks6aa"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
        gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION,
        "bafyreiekqlaylsyoty5d7wehjitd4xt3z2oxduj6wnkova4yfevxqd7kwu"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
        gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION,
        "bafyreiezjnue5b7alot7eynlqss3l4nm7ppbttpthkzqcc3n7crela4grm"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
        gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT,
        "bafyreictae7jrd6dsljzwmhemsp47byzajy4psiiz6ubujbhdzyqluat4y"
    ),
    baseline_entry!(
        gents_protocol::schemas::ENROLLMENT_OPERATOR_NONCE_NAME,
        gents_protocol::schemas::ENROLLMENT_OPERATOR_NONCE,
        "bafyreihqgfh2e6gc73zftjp6d2qlyba7hxsc3ab6mhgfb7jn4yydvdihaq"
    ),
    baseline_entry!(
        gents_protocol::schemas::PERSONA_CONFIG_REQUEST_NAME,
        gents_protocol::schemas::PERSONA_CONFIG_REQUEST,
        "bafyreidoth5phfvohyp2mzpuocyf2nqxjzu367ytomvpr57lopqjysmgta"
    ),
    baseline_entry!(
        gents_protocol::schemas::SESSION_HYDRATION_REQUEST_NAME,
        gents_protocol::schemas::SESSION_HYDRATION_REQUEST,
        "bafyreifuhe35hisrqxfuejsqf2y66rhgl26enmtwgjjmy4tapwcwmrerku"
    ),
];

/// Ordered post-baseline schema evolution chain.
pub static DEFAULT_STEPS: &[MigrationStep<'static>] = &[
    MigrationStep::PatchVersioned {
        id: "inference-call-add-context-accounting",
        collection: gents_protocol::schemas::INFERENCE_CALL_NAME,
        patch: INFERENCE_CALL_ADD_CONTEXT_ACCOUNTING_PATCH,
        lens: None,
        expected_version: Some("bafyreigecktl6sgfz5ykqc62dkakr3l7h5lmlm3a24z7ragutlfo6ffzqa"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["context_accounting_json"]),
    },
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
        id: "tool-selection-add-lsp-fields",
        collection: gents_protocol::schemas::TOOL_SELECTION_NAME,
        patch: TOOL_SELECTION_ADD_LSP_FIELDS_PATCH,
        lens: None,
        // Authored by applying the inactive patch to the frozen ToolSelection baseline.
        expected_version: Some("bafyreibzvuogmrsg7z5mz2mlnmb2f5avdas54a35fpoghu2bbwyt4fiame"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["enable_lsp", "lsp_config"]),
    },
    MigrationStep::PatchVersioned {
        id: "tool-selection-add-required-mcp-services",
        collection: gents_protocol::schemas::TOOL_SELECTION_NAME,
        patch: TOOL_SELECTION_ADD_REQUIRED_MCP_SERVICES_PATCH,
        lens: None,
        // Pin is authored by applying this inactive patch after the LSP step.
        expected_version: Some("bafyreihwtdnzzstrwzbdr2gfiebsqtdvujhwtilcjf2xkk47tm4dr3pcpq"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&[
            "enable_lsp",
            "lsp_config",
            "required_mcp_service_ids",
        ]),
    },
    MigrationStep::PatchVersioned {
        id: "tool-selection-add-eth-tool-ids",
        collection: gents_protocol::schemas::TOOL_SELECTION_NAME,
        patch: TOOL_SELECTION_ADD_ETH_TOOL_IDS_PATCH,
        lens: None,
        expected_version: Some("bafyreiam46672yl2mse4lbuqukdb56wg5dfkaxm4deozti7zud4qecperm"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&[
            "enable_lsp",
            "lsp_config",
            "required_mcp_service_ids",
            "eth_tool_ids",
        ]),
    },
    MigrationStep::PatchVersioned {
        id: "callback-result-add-binding-id",
        collection: gents_protocol::schemas::CALLBACK_RESULT_NAME,
        patch: CALLBACK_RESULT_ADD_BINDING_ID_PATCH,
        lens: None,
        expected_version: Some("bafyreica3zpcebkzqvbkeweck5frjr3stgv6rabjkdkjzdfujrg3uqenni"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["binding_id"]),
    },
    MigrationStep::PatchVersioned {
        id: "event-trigger-add-workspace-authority",
        collection: gents_protocol::schemas::EVENT_TRIGGER_NAME,
        patch: EVENT_TRIGGER_ADD_WORKSPACE_AUTHORITY_PATCH,
        lens: None,
        expected_version: Some("bafyreig4ta2rafasuureoay2lzsogkgjio52se4n5mmzhzkjvpghtftztu"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["workspace_authority"]),
    },
    MigrationStep::PatchVersioned {
        id: "callback-result-add-work-unit-id",
        collection: gents_protocol::schemas::CALLBACK_RESULT_NAME,
        patch: CALLBACK_RESULT_ADD_WORK_UNIT_ID_PATCH,
        lens: None,
        expected_version: Some("bafyreifz4nmwt64jumql4olqxgt7d72xakoymx54wli5zankmlg5l3xqvq"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["work_unit_id"]),
    },
    MigrationStep::PatchVersioned {
        id: "workspace-receipt-add-lineage-fields",
        collection: gents_protocol::schemas::WORKSPACE_RECEIPT_NAME,
        patch: WORKSPACE_RECEIPT_ADD_LINEAGE_FIELDS_PATCH,
        lens: None,
        expected_version: Some("bafyreihrwpefsqgyqikoou4vvd2e5s73lv4y36j2uwjd35uqpge6rn7cjm"),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["work_unit_id", "caused_by_correlation"]),
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

// ---------------------------------------------------------------------------
// Client-authored (conversation-plane) collections (#1123 / #1125)
// ---------------------------------------------------------------------------

/// Collections a paired client fresh-applies its bundled SDL into and then
/// authors documents into directly: the conversation-plane transcript
/// (`AgentRequest`, `AgentResponse`, `AgentMessage`, `AgentToolCall`,
/// `AgentToolResult`, `AgentSession`, `AgentConversation`,
/// `CompactionEntry`), the signed `PeerEndpoint` heartbeat,
/// `PersonaConfigRequest`, `SessionHydrationRequest`, and the fleet-discovery
/// `AgentDirectoryEntry`. It also includes the authenticated-enrollment
/// request/decision/revision/route-receipt exchange: those documents use
/// exact owner-scoped push rather than the machine template, but still cross
/// stores and therefore need the same genesis-version identity fence.
///
/// A client mints its store from the collection's *current* SDL with no
/// server-side history: a single `add_schema` call produces a genesis
/// version whose DAG-CBOR block has empty `heads`. A server-side
/// [`MigrationStep::PatchVersioned`] step instead chains a new version onto
/// its predecessor's CID as `heads`. Because a version's CID is the hash of
/// that DAG-CBOR block, a chain-tip CID (non-empty heads) can never equal a
/// fresh client's genesis CID (empty heads) — even when the two collections
/// end up with byte-identical fields. This is a structural property of
/// DefraDB's version DAG, not something schema authoring discipline alone
/// can avoid.
///
/// Until #1123's option 1 or 2 lands (a mechanism that lets `ensure_migrations`
/// accept more than one known root/tip per collection), every collection in
/// this list MUST evolve by **re-pinning its baseline** to the new
/// fresh-apply CID — never through `DEFAULT_STEPS`, of either kind:
/// [`MigrationStep::PatchVersioned`] chains the version DAG so the CIDs can
/// never match again, and [`MigrationStep::PatchInPlace`] keeps the CID
/// while diverging the server's indexes/policies from what a bare fresh
/// apply mints — a silent divergence no CID comparison can detect. PR #1125
/// is the worked example: `AgentRequest` had drifted onto a chain (a
/// `PatchVersioned` step appended fields after the baseline), which broke
/// fresh mobile stores against a v0.11 server; the fix folded the fields
/// into the baseline SDL and re-pinned the root to the fresh-apply CID.
///
/// `tests/fresh_apply_parity.rs` enforces CID parity for every collection
/// listed here; the step guard in
/// `default_baseline_matches_ordered_protocol_catalog`
/// (`tests/baseline_ensure.rs`) statically rejects a `DEFAULT_STEPS` entry
/// of any kind targeting any of them. The
/// `client_authored_collections_fence` test in the `gents` crate keeps this
/// list synced with the client push surface that gents actually configures.
pub const CLIENT_AUTHORED_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::AGENT_REQUEST_NAME,
    gents_protocol::schemas::AGENT_RESPONSE_NAME,
    gents_protocol::schemas::AGENT_MESSAGE_NAME,
    gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
    gents_protocol::schemas::AGENT_TOOL_RESULT_NAME,
    gents_protocol::schemas::AGENT_SESSION_NAME,
    gents_protocol::schemas::AGENT_CONVERSATION_NAME,
    gents_protocol::schemas::COMPACTION_ENTRY_NAME,
    gents_protocol::schemas::PEER_ENDPOINT_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
    gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
    gents_protocol::schemas::PERSONA_CONFIG_REQUEST_NAME,
    gents_protocol::schemas::SESSION_HYDRATION_REQUEST_NAME,
    gents_protocol::schemas::AGENT_DIRECTORY_ENTRY_NAME,
    gents_protocol::schemas::MAILBOX_ITEM_NAME,
];
