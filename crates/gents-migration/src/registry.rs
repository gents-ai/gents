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
        "bafyreiamcmhv7qxizirye3dntmr57e5he5uttsk74nm65abbxqy5vj2dxm"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_PRINCIPAL_NAME,
        gents_protocol::schemas::AGENT_PRINCIPAL,
        "bafyreiayiyvgnp74u2xu42qmqeo3eygnib65uppm3yigzu2c5qrzzcnglu"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_BEHAVIOR_NAME,
        gents_protocol::schemas::AGENT_BEHAVIOR,
        "bafyreifal4nxjp5tj5eoagpiwr3emkt7gmbigajsxukvhu5k73tva73pam"
    ),
    baseline_entry!(
        gents_protocol::schemas::COMPACTION_CONFIG_NAME,
        gents_protocol::schemas::COMPACTION_CONFIG,
        "bafyreih3w3aeusza2pu5uwgr3fqkcdwicq3xvfxa7ujcpkkwbub6w5q244"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_CONTEXT_NAME,
        gents_protocol::schemas::AGENT_CONTEXT,
        "bafyreieq6mlc6yvruovlup5ctquafgmdbzj4c5a7nl4hcxjmdrtzsjykp4"
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
        gents_protocol::schemas::TOOLS_NAME,
        gents_protocol::schemas::TOOLS,
        "bafyreianpmeiccjdnuvgby5mfnstrhe7o54whywumqqmbaguarnj2ja6bq"
    ),
    baseline_entry!(
        gents_protocol::schemas::SKILL_NAME,
        gents_protocol::schemas::SKILL,
        "bafyreiadshuzujbs6t25khjmk5mjiahia7t6crcyhzdp6m3kwxkgaerzk4"
    ),
    baseline_entry!(
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE_NAME,
        gents_protocol::schemas::DATASTORE_TOOL_SURFACE,
        "bafyreiesqgo7tpeimnhlsbqfzhd26smonu4tns6j4nekvr3432mbrsaqkm"
    ),
    baseline_entry!(
        gents_protocol::schemas::CHAIN_KEY_BINDING_NAME,
        gents_protocol::schemas::CHAIN_KEY_BINDING,
        "bafyreiclna7b44tt4kixlqdhawps56mgmhvwcenttjwhermmj7vqfk622y"
    ),
    baseline_entry!(
        gents_protocol::schemas::ETH_TOOL_NAME,
        gents_protocol::schemas::ETH_TOOL,
        "bafyreia2ril3odny6qfpdaldtvj3ohgbiku6uykewclgvutpuonfdr53nq"
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
        "bafyreibvt64thhbf3htz23wgwvia2uripitwit7yoymnp4uhf7wtmy4kyy"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_PLACEMENT_NAME,
        gents_protocol::schemas::WORKSPACE_PLACEMENT,
        "bafyreiggamel6etlokfsgjyqzecvxnuvppqic2o4sixinote7zecdhzsqy"
    ),
    baseline_entry!(
        gents_protocol::schemas::REPOSITORY_PLACEMENT_NAME,
        gents_protocol::schemas::REPOSITORY_PLACEMENT,
        "bafyreidwvwna3akbkvdvvc5zebiitjofstgl3b2gss2abvwfsrswx742ma"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_BINDING_NAME,
        gents_protocol::schemas::WORKSPACE_BINDING,
        "bafyreiarlccqm6hwjp2n6zmdxgxfsnnyzgbsckzwqvnwglmc3ta6lxdjly"
    ),
    baseline_entry!(
        gents_protocol::schemas::WORKSPACE_RECEIPT_NAME,
        gents_protocol::schemas::WORKSPACE_RECEIPT,
        "bafyreicagx2x2cdnt7t67blfcsfyyxaoy4ufjv4zgydxs7d4ftfjo6jscy"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_NAME,
        gents_protocol::schemas::CALLBACK,
        "bafyreie3hnkgggvycgsni2q3i44i5vsuazgvbwyd2di2r6fzl4hrnh6cty"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_SOURCE_NAME,
        gents_protocol::schemas::EVENT_SOURCE,
        "bafyreifcnrj6gkwqg22j2ekxfwkjszzj347wbalqxahvbizoybm7rkrpnu"
    ),
    baseline_entry!(
        gents_protocol::schemas::TRIGGER_NAME,
        gents_protocol::schemas::TRIGGER,
        "bafyreicfvlbhf6qaqirppdspitoe6fm35kjrnxk7tutaqozx7hdp6jxv64"
    ),
    baseline_entry!(
        gents_protocol::schemas::SUBAGENT_TARGET_NAME,
        gents_protocol::schemas::SUBAGENT_TARGET,
        "bafyreiffuzq6xce3vas2sfjoh5u4ccpapyvm7b2fu3i323pbs6mez3iycu"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_MODULE_NAME,
        gents_protocol::schemas::CALLBACK_MODULE,
        "bafyreiea4l7jypgzxnolwg4q4jdjufsf5ntdxrpjllhp7p7pvktgbdyzge"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_BINDING_NAME,
        gents_protocol::schemas::CALLBACK_BINDING,
        "bafyreickc74uuoxp2szyj2z4eg45epth36zp4g35zmvxf6swne4srdo6ay"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_INVOCATION_NAME,
        gents_protocol::schemas::CALLBACK_INVOCATION,
        "bafyreiacgrllu5orp4gl42owirfknwh2qu2gxpjifxbhptszkxhx5pdooa"
    ),
    baseline_entry!(
        gents_protocol::schemas::CALLBACK_RESULT_NAME,
        gents_protocol::schemas::CALLBACK_RESULT,
        "bafyreiaxuay4dmn424aqjbvi6nqehzuscxcaf7smgyzn6dobawazdblfca"
    ),
    baseline_entry!(
        gents_protocol::schemas::OAUTH_CREDENTIAL_NAME,
        gents_protocol::schemas::OAUTH_CREDENTIAL,
        "bafyreiab3wqm3em2cepvj22l733ziz4azytl3gc7zozcm5e2s7nuehkx6u"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        gents_protocol::schemas::INFERENCE_PROFILE,
        "bafyreibd54aukeo6tjk6x46fz5p4d7jmijsgblkzotqtrekm77s4wvpjtm"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_RETRY_POLICY_NAME,
        gents_protocol::schemas::INFERENCE_RETRY_POLICY,
        "bafyreiaxi52fh44qighc3utd5j2glpj5kswavb3w7nqurcfe2ymzbkumcm"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_EXECUTION_NAME,
        gents_protocol::schemas::INFERENCE_EXECUTION,
        "bafyreihjwsdfpjfihlqy7sjaxmiwxn3j2n4osbbgfty6bu5iesvaqpufg4"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_SAMPLING_NAME,
        gents_protocol::schemas::INFERENCE_SAMPLING,
        "bafyreiahd7xljxsayg3kkipq5x566bb5sq23byhhixnjbzaxal7nvspq64"
    ),
    baseline_entry!(
        gents_protocol::schemas::INFERENCE_CALL_NAME,
        gents_protocol::schemas::INFERENCE_CALL,
        "bafyreib6rfxuo7nk2gugwotw52ozguu2vvquyobfqsur5n7jsqurbvhfni"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_REQUEST_NAME,
        gents_protocol::schemas::AGENT_REQUEST,
        "bafyreieyeycrjfo5xsumx6ddnwqwtlvt4ufjjb4ayole7dgnrsp3wplhqq"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_RESPONSE_NAME,
        gents_protocol::schemas::AGENT_RESPONSE,
        "bafyreigr4eflydkzsigq7m2dzpdd7yy3ny5zwdwicefyzntjrsfiptua2u"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_TOOL_RESULT_NAME,
        gents_protocol::schemas::AGENT_TOOL_RESULT,
        "bafyreievrced2cec6gsu4bg4htj2i4dq2sofvnyysmekokbue5rrbmi65e"
    ),
    baseline_entry!(
        gents_protocol::schemas::AGENT_SESSION_NAME,
        gents_protocol::schemas::AGENT_SESSION,
        "bafyreiellcxq57kc7pua4iglqrovadmt4bifjg2blyc2rxxm2snrvdfp3y"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_NAME,
        gents_protocol::schemas::GOAL,
        "bafyreibftdbl5ykoxainbguuhspcchp5aevzrsbbcpnwdqyb6ngsqknyie"
    ),
    baseline_entry!(
        gents_protocol::schemas::GOAL_CREATION_CLAIM_NAME,
        gents_protocol::schemas::GOAL_CREATION_CLAIM,
        "bafyreicgpz3pvsz3k7ijl2znkewhjbhhqpd5g3ykmwp7wg3bpd3nvrr67q"
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
        "bafyreigb4fvfiyixw73psc5xqsxlzuhyoy7dxkowesrrpke6ktla74d5ji"
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
        "bafyreidfmn5thhao2ii6t4yjmwcexnl3io2zaepowkltbtyam2aj5xi7ee"
    ),
    baseline_entry!(
        gents_protocol::schemas::TASK_NAME,
        gents_protocol::schemas::TASK,
        "bafyreidl57hmgc7us47tmtdzo3r4ehpfcjsnhfhf5cumsckgia73uxqmxy"
    ),
    baseline_entry!(
        gents_protocol::schemas::SCHEDULE_NAME,
        gents_protocol::schemas::SCHEDULE,
        "bafyreieaj5ysgwgshwca3jvwcq4pl3s75hhzh2qgwpzcfzpdmyeufv4ji4"
    ),
    baseline_entry!(
        gents_protocol::schemas::EVENT_GROUP_STATE_NAME,
        gents_protocol::schemas::EVENT_GROUP_STATE,
        "bafyreial3j6oa3j64wuaxz37vdjztevmlokbetqqvmepxkirewzg7kefyy"
    ),
    baseline_entry!(
        gents_protocol::schemas::GRAPH_DEFINITION_NAME,
        gents_protocol::schemas::GRAPH_DEFINITION,
        "bafyreihsvqtsxkkawrw4n7e72bqz4skowzh7xvy3wikd24de5tenwq73pu"
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
        "bafyreiah7fpyxr64i7p46v7ckvgfxihpcwwipcepe7ylz6ni4strjk7hxq"
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
        "bafyreianm22jl7ecpu6kl55wnvadusazbky7hz3dsrgl2jzhzknlj4ivba"
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
        "bafyreicmpatd7phppn77232g3pmarwsf6h55vw3rqxv3ymqzyalqub7sum"
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
    gents_protocol::schemas::AGENT_RESPONSE_NAME,
    gents_protocol::schemas::AGENT_MESSAGE_NAME,
    gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
    gents_protocol::schemas::AGENT_TOOL_RESULT_NAME,
    gents_protocol::schemas::AGENT_SESSION_NAME,
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
