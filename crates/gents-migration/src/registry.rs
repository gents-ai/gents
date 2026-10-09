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
    /// Pinned root VersionID. Production entries must specify a pin; dynamic
    /// registries may omit it while authoring a new migration.
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
// Default production registry (canonical baseline, zero steps)
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

/// Fresh canonical schema baseline. Root pins are checked against DefraDB;
/// historical migrations are intentionally absent from this refactor baseline.
/// Re-author pins after changing the protocol catalog with the pin-authoring test.
pub static DEFAULT_BASELINE: &[BaselineCollection<'static>] = &[
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_BACKEND_NAME,
        gents_protocol::schemas::INFERENCE_BACKEND,
        "bafyreihvtdq5hp7qrehkfhgkdtqql6v6mq4podhdnfrpbrcy4x3xasddwy"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_NAME,
        gents_protocol::schemas::NODE,
        "bafyreiebppz3w6ri6lg6ft2jy3il7gk6bexfy3wjqizbcct4pzx3puppby"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_NAME,
        gents_protocol::schemas::AGENT,
        "bafyreiawuyih6xqeqoriami7aj7alpvuqzo22qh7k5vbfm3kyrelyooi6a"
    ),
    baseline_entry!(
        gents_protocol::schemas::COMPACTION_CONFIG_NAME,
        gents_protocol::schemas::COMPACTION_CONFIG,
        "bafyreiggbqysnj3to7zcw3534eah7ulx5fyfyqfq3le75cli6nf527r2ce"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_CONTEXT_NAME,
        gents_protocol::schemas::AGENT_CONTEXT,
        "bafyreigvm34xfkhx34jvxyskcreltg2rbmb7n6evuok44gxqrijoaw7lti"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TARGET_NAME,
        gents_protocol::schemas::AGENT_TARGET,
        "bafyreieqevckhv5c6eli4ceysd3p2wlfhdzjeve2r2mn2v37arv3vn6hti"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_RUNTIME_NAME,
        gents_protocol::schemas::NODE_RUNTIME,
        "bafyreif7je44mc6mdxtpp6wpakbdhuzsjibgtifxlo6pzgfhf2sqlcdrgq"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_READINESS_NAME,
        gents_protocol::schemas::NODE_READINESS,
        "bafyreihnrcbuikvai4cit2aq7fpbjavoixq3qj3bwbllslq4np545fxmmu"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME,
        gents_protocol::schemas::NODE_DIRECTORY_ENTRY,
        "bafyreia2wluxobv4to5niaxnhelmvemi6ga3t5lk3m3alefupwk7c6abve"
    ),
    baseline_entry!(
        gents_protocol::schemas::NODE_MEMORY_NAME,
        gents_protocol::schemas::NODE_MEMORY,
        "bafyreifcm3bi3qofvhfws7nps45xomrsxb2zslvjkkhb7uwh34esxi5okq"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOLS_NAME,
        gents_protocol::schemas::TOOLS,
        "bafyreih62xjqp376qygv2wgbfpvlu4juroh4oqfufem37koomf6bzgny5q"
    ),
    baseline_entry!(
        gents_protocol::schemas::SKILL_NAME,
        gents_protocol::schemas::SKILL,
        "bafyreidhsmcxsmklefypyp7uaig5tgefwvpt4dv6nurgy7rbxlj3egpwla"
    ),
    baseline_entry!(
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE_NAME,
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE,
        "bafyreibhujbx2v6qqrp3l3iqg2j5ph6ggckf5r263ykmsks7dnodjygope"
    ),
    baseline_entry!(
        gents_protocol::schemas::CHAIN_KEY_BINDING_NAME,
        gents_protocol::schemas::CHAIN_KEY_BINDING,
        "bafyreigkgc7vx3bacpnoop5ouryihgdckohdnkjzitapw3xqgakmimxd6a"
    ),
    baseline_entry!(
        gents_protocol::schemas::ETH_TOOL_NAME,
        gents_protocol::schemas::ETH_TOOL,
        "bafyreieeavm747hdfwfrlm524uilhmnokwz3wodn22x4daidndbyg3pz6a"
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
        "bafyreigqhbaoq4ttjzt6cwi7p43tktv4urviml22zmmpzzvndfr7sh5afy"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_PLACEMENT_NAME,
        gents_protocol::schemas::WORKSPACE_PLACEMENT,
        "bafyreidhu4e5ddjund4nzwlw4hxo3k5vwi6eidwtnsyxf345wef7ffbv2i"
    ),
    baseline_entry!(
        gents_protocol::schemas::REPOSITORY_PLACEMENT_NAME,
        gents_protocol::schemas::REPOSITORY_PLACEMENT,
        "bafyreieks33oeixd4h4sdqfmli3fyuksjo3bkw7povukuak6xvavl7sbxq"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_BINDING_NAME,
        gents_protocol::schemas::WORKSPACE_BINDING,
        "bafyreigofvbludczwhf5dm5kua4e5jhkzrtf3fgvktk6p5k75utwhdts2y"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_RECEIPT_NAME,
        gents_protocol::schemas::WORKSPACE_RECEIPT,
        "bafyreicagx2x2cdnt7t67blfcsfyyxaoy4ufjv4zgydxs7d4ftfjo6jscy"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_NAME,
        gents_protocol::schemas::CALLBACK,
        "bafyreigwyv5fj5jfy2evxtq5ca4xz6bqd7eq2q5mcw5moiize4gpgusu2q"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_SOURCE_NAME,
        gents_protocol::schemas::EVENT_SOURCE,
        "bafyreidbvp5dtcm2frjhmb7ymgkl76egaxa4qcgour4j3xoys7ppc5umai"
    ),
    baseline_entry!(
        gents_protocol::schemas::TRIGGER_NAME,
        gents_protocol::schemas::TRIGGER,
        "bafyreiez56udcss6hqfibh4j5fxfhbac7wp63y7pfz77vwvqz2fqrt5f74"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_SOURCE_CURSOR_NAME,
        gents_protocol::schemas::EVENT_SOURCE_CURSOR,
        "bafyreichgs26urdud6sk4qualskmzpcodc5o2mlljnswcxmleihz2vo2qq"
    ),
    baseline_entry!(
        gents_protocol::schemas::TRIGGER_FIRE_NAME,
        gents_protocol::schemas::TRIGGER_FIRE,
        "bafyreibgl6fpygvxtcgwmdk2wgufhmujonq4p7e2muvy23f6nadeyolzle"
    ),
    baseline_entry!(
        gents_protocol::schemas::FIRE_OUTCOME_NAME,
        gents_protocol::schemas::FIRE_OUTCOME,
        "bafyreiebsekkoog62amjydsetyiky2rgiritfyphcqn7mvgskjaylw2h5m"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_MODULE_NAME,
        gents_protocol::schemas::CALLBACK_MODULE,
        "bafyreiaujetzfiauz2wfduqsmmynybedpwm4udowmkcfo66u7stdr552iq"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_BINDING_NAME,
        gents_protocol::schemas::CALLBACK_BINDING,
        "bafyreiepwxqpz5fakzqk34hw3w7psrgvpqi2n4q7remzsrxidzskfdvk3y"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_INVOCATION_NAME,
        gents_protocol::schemas::CALLBACK_INVOCATION,
        "bafyreickkoqhejwyeosqantqkiaf4zrokdcmovya3ps63vxpznzqzcquja"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_RESULT_NAME,
        gents_protocol::schemas::CALLBACK_RESULT,
        "bafyreid4tt2p2fv6zgizqsaitlysxouqszy6pipzy6rtzq3ra253ijgri4"
    ),
    baseline_entry!(
        gents_protocol::schemas::OAUTH_CREDENTIAL_NAME,
        gents_protocol::schemas::OAUTH_CREDENTIAL,
        "bafyreia6tt5aickbpdovdivyhzq7xfidy4s5mpa7sqlksq6etpw5w6xjdm"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        gents_protocol::schemas::INFERENCE_PROFILE,
        "bafyreiat2uhx24kbgbm54lz7nt56vtjjsfttkiwop2ntii7qn2xtceehxi"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_RETRY_POLICY_NAME,
        gents_protocol::schemas::INFERENCE_RETRY_POLICY,
        "bafyreiasnka6siox245v35twdscltsap7wigaej2ytjgf6tleoonxnsvlm"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_EXECUTION_NAME,
        gents_protocol::schemas::INFERENCE_EXECUTION,
        "bafyreig4qstxcthbe4r56v3saqtvidikn5ymrjunyt2ofvmc5g7duqhsea"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_SAMPLING_NAME,
        gents_protocol::schemas::INFERENCE_SAMPLING,
        "bafyreicf7mrn3mfyu7smgy6vx4huo6iu6cj7j5cuuz2czmwj2gctfnjppq"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_CALL_NAME,
        gents_protocol::schemas::INFERENCE_CALL,
        "bafyreibbbdmb7au32traw5ufp6rqytb3jv63bmjflykfhize2u6bwvqiwe"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_REQUEST_NAME,
        gents_protocol::schemas::AGENT_REQUEST,
        "bafyreiajyoffdf3n47qyyrhjict76bmdli2neksug7k5mj5z4m6xrt2w5a"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
        gents_protocol::schemas::AGENT_OUTPUT_SEGMENT,
        "bafyreih6n2g2ofyqw7i23ur3jikelitjlgwaet57hozuxpge3hoibiffzi"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_NAME,
        gents_protocol::schemas::AGENT_SESSION,
        "bafyreietunsrqsijc3k6x3gf6y2nwkim74uf6j4lskv7p3pjrl44q4tthe"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_NAME,
        gents_protocol::schemas::GOAL,
        "bafyreids226gvgylkkfgv6ft6qbug4xz3qsrrmhqr6py6uidphk7kkrqtq"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_CREATION_CLAIM_NAME,
        gents_protocol::schemas::GOAL_CREATION_CLAIM,
        "bafyreifr7fntfbkueuclpi5zxk6zbmrkmzekacogqwx3zsvs3kqo577i3m"
    ),
    baseline_entry!(
        gents_protocol::schemas::MAILBOX_ITEM_NAME,
        gents_protocol::schemas::MAILBOX_ITEM,
        "bafyreifsmnmr74fhl2otyeapkuddraoq5z5nrqg4xdr7jb37ey7is77esu"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_MESSAGE_NAME,
        gents_protocol::schemas::AGENT_MESSAGE,
        "bafyreibpbyfmfgfookleozyqvuwse6qtfnjf7ipxjq6ryypovs226ziw6a"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
        gents_protocol::schemas::AGENT_TOOL_CALL,
        "bafyreicuz2wf6aqbpfqtji6f6advclq5aeuiaaxerczwbaiel2w3aj56oy"
    ),
    baseline_entry!(
        gents_protocol::schemas::COMPACTION_ENTRY_NAME,
        gents_protocol::schemas::COMPACTION_ENTRY,
        "bafyreidfp36wuikilut4cnvs7qwmqbyyd5yyyaz5g7umk2lfqwzqojgn4q"
    ),
    baseline_entry!(
        gents_protocol::schemas::RENDERED_REQUEST_NAME,
        gents_protocol::schemas::RENDERED_REQUEST,
        "bafyreifnqpcrodl272dxd6cefewcug4gtkt3nvmlx3nhyqmiefdfayhodi"
    ),
    baseline_entry!(
        gents_protocol::schemas::RENDERED_REQUEST_BLOCK_NAME,
        gents_protocol::schemas::RENDERED_REQUEST_BLOCK,
        "bafyreicl23h6anxhpfmdmomvgpegd42apgmvddtjtcqlteabtcog3d7h5i"
    ),
    baseline_entry!(
        gents_protocol::schemas::PROVIDER_CONTEXT_REDUCTION_NAME,
        gents_protocol::schemas::PROVIDER_CONTEXT_REDUCTION,
        "bafyreifxbsldqfvxupmg54bimohgnurlnkw4iwghm7jpqarunmiw4dgvdi"
    ),
    baseline_entry!(
        gents_protocol::schemas::PROJECTION_ACP_BINDING_NAME,
        gents_protocol::schemas::PROJECTION_ACP_BINDING,
        "bafyreihnwe664snzwwcnxyy4x6r3d3y4xy27ch5dpxnxu6hfqdjhxds7de"
    ),
    baseline_entry!(
        gents_protocol::schemas::TASK_NAME,
        gents_protocol::schemas::TASK,
        "bafyreibxt5wt6v4skfdsmwlcnao3y6nktlsyprtn7xhngrk77qvuz2gxii"
    ),
    baseline_entry!(
        gents_protocol::schemas::SCHEDULE_NAME,
        gents_protocol::schemas::SCHEDULE,
        "bafyreibi3hlloxszn4lgipphx55nksivy5x3orhfhshft6qzr64ghedf5a"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_GROUP_STATE_NAME,
        gents_protocol::schemas::EVENT_GROUP_STATE,
        "bafyreids6vkxatjttmedkypqlibwbyyebyapq5p5vrn7dlfyh5iqbzrq6y"
    ),
    baseline_entry!(
        gents_protocol::schemas::GRAPH_DEFINITION_NAME,
        gents_protocol::schemas::GRAPH_DEFINITION,
        "bafyreigxzdxynnf2s5wlpsbyjboo7a6wie5cdmfbpuix5q6o4zsq5jvpdy"
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
        "bafyreigckps33q33zvnrodzqlz72ri4osyodxleniwy6txwkfhnq7tgg3q"
    ),
    baseline_entry!(
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE_NAME,
        gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE,
        "bafyreiatdha4wto462443nnyh4ic35t7xsgc5pvo6jrbrppx3pxsvpgehi"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::PEER_PAIRING_DESIRED,
        "bafyreiagpv74772zsb3n7y6dxqbv3tit5hactyrtxk73occf6sxxagw7he"
    ),
    baseline_entry!(
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED_NAME,
        gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED,
        "bafyreiaitj36nr5nmsevxytznad44gg3bmunlbodwfpq6xueleofu67dpm"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_PAIRING_APPLIED_NAME,
        gents_protocol::schemas::PEER_PAIRING_APPLIED,
        "bafyreifunn7vevp6b6rzg232gjfypp2lqviafe5now5ldlwo3na5nfinq4"
    ),
    baseline_entry!(
        gents_protocol::schemas::PEER_REGISTRY_NAME,
        gents_protocol::schemas::PEER_REGISTRY,
        "bafyreiai3zeedy2yf2zt5e3izybimqlk73fqx5exxxe5k6sbvffhquguf4"
    ),
    baseline_entry!(
        gents_protocol::schemas::NETWORK_NAME,
        gents_protocol::schemas::NETWORK,
        "bafyreighwrxb6enx4wppybg6qycvrikw3ph3zelexnzcnyujgpykpkl6pi"
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
        gents_protocol::schemas::SESSION_HYDRATION_REQUEST_NAME,
        gents_protocol::schemas::SESSION_HYDRATION_REQUEST,
        "bafyreiebw7lxwzdcmltfz6khjz6e32utfpfwpxydnqwcnnsbtj4qqhxcmq"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_DEFINITION_NAME,
        gents_protocol::schemas::EVAL_DEFINITION,
        "bafyreibeokxszqypuejna3pckyp36vujfpfg2yahl3y3u3scpngdbigjtm"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_RUN_NAME,
        gents_protocol::schemas::EVAL_RUN,
        "bafyreihebflrtxgdxkjwt7wr3ep2vaej7arrsqzbqcl5z7zeahqoekdsqi"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_TRIAL_NAME,
        gents_protocol::schemas::EVAL_TRIAL,
        "bafyreic5i7efn5ygbpnaarkkhji6sjuojihzamf75tcypp4fptkkn6enum"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVAL_VERDICT_NAME,
        gents_protocol::schemas::EVAL_VERDICT,
        "bafyreic3hxrxirhln4b2mnsjmeuz73oycdjw6dnfpzfz3qogzq6anbbzbe"
    ),
    baseline_entry!(
        gents_protocol::schemas::OPTIMIZATION_JOB_NAME,
        gents_protocol::schemas::OPTIMIZATION_JOB,
        "bafyreiamgjslitr3wvsrdcgqp75onccmpetjbi4nq4jwxgexbxyfaqwp7m"
    ),
    baseline_entry!(
        gents_protocol::schemas::PACK_INSTALLATION_NAME,
        gents_protocol::schemas::PACK_INSTALLATION,
        "bafyreid74trtjvnquhtehlqi2on37cdd7q77jvpwxadbudn6t3k2bx7ety"
    ),
    baseline_entry!(
        gents_protocol::schemas::PROVIDER_ACCOUNT_USAGE_NAME,
        gents_protocol::schemas::PROVIDER_ACCOUNT_USAGE,
        "bafyreihlwf4r2path7v4yufdnxmqrpe5e2f2g4m2vh5ghn2s6oh3xonc2e"
    ),
];

/// Future schema evolution starts here, after the canonical baseline lands.
pub static DEFAULT_STEPS: &[MigrationStep<'static>] = &[];

/// Production registry: canonical pinned baseline plus future migrations.
pub static DEFAULT_REGISTRY: Registry<'static> = Registry {
    baseline: DEFAULT_BASELINE,
    steps: DEFAULT_STEPS,
};

/// Embedded fixture lens wasm (built by `build.rs`).
pub fn fixture_lens_wasm() -> &'static [u8] {
    include_bytes!(env!("GENTS_LENS_FIXTURE_ADD_LABEL_WASM_PATH"))
}

// ---------------------------------------------------------------------------
// Client-authored collections
// ---------------------------------------------------------------------------

/// Collections authored by paired clients must share the server's fresh-apply
/// schema root. A versioned migration has predecessor heads and therefore a
/// different CID from fresh application, even with identical final fields.
/// In-place steps can also diverge metadata without changing that CID.
/// `fresh_apply_parity` and the baseline step guard enforce both constraints.
pub const CLIENT_AUTHORED_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::AGENT_REQUEST_NAME,
    gents_protocol::schemas::AGENT_MESSAGE_NAME,
    gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
    gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
    gents_protocol::schemas::AGENT_SESSION_NAME,
    gents_protocol::schemas::COMPACTION_ENTRY_NAME,
    gents_protocol::schemas::PEER_ENDPOINT_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
    gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
    gents_protocol::schemas::SESSION_HYDRATION_REQUEST_NAME,
    gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME,
    gents_protocol::schemas::MAILBOX_ITEM_NAME,
];
