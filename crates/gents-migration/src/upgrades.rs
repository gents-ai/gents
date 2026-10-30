use defra_node::EmbeddedNode;

use crate::error::{Error, Result};
use crate::expectation::CollectionExpectation;
use crate::registry::{BaselineCollection, MigrationStep};

const NODE_MEMORY_ROOT: &str = "bafyreifcm3bi3qofvhfws7nps45xomrsxb2zslvjkkhb7uwh34esxi5okq";
const NODE_MEMORY_TEXT_TARGET: &str = "bafyreihb3faohzgyj4zedddsedlnivyjhonk6tkhdovvrzyiaris3rkmtu";
const NODE_MEMORY_TEXT_PATCH: &str = r#"[{"op":"add","path":"/NodeMemory/Fields/-","value":{"Name":"value_text","Kind":"String","Typ":6}}]"#;

const INFERENCE_CALL_ROOT: &str = "bafyreibbbdmb7au32traw5ufp6rqytb3jv63bmjflykfhize2u6bwvqiwe";
const INFERENCE_CALL_TARGET: &str = "bafyreie6maihybat7mu2hbygehkkcsb7x6h5bcdcvg53ygjhfcoikfdfau";
const INFERENCE_CALL_USAGE_PATCH: &str = r#"[
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"session_id","Kind":"String","Immutable":true}},
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"model_name","Kind":"String","Immutable":true}},
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"profile_id","Kind":"String","Immutable":true}},
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"cache_creation_input_tokens","Kind":"Int"}},
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"reasoning_output_tokens","Kind":"Int"}},
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"provider_total_tokens","Kind":"Int"}},
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"usage_completeness","Kind":"String"}}
]"#;

const PROFILE_ROOT: &str = "bafyreiat2uhx24kbgbm54lz7nt56vtjjsfttkiwop2ntii7qn2xtceehxi";
const PROFILE_TARGET: &str = "bafyreieksrjdkg546ymilx5qrpbtciuyr3cwqyq25u52o64lobpoek3qqi";
const TOOL_ROOT: &str = "bafyreiexmbjxfqyg6yhjst6r746spzge27f6mhh2moyrkawbmvtvba4mbi";
const TOOL_TARGET: &str = "bafyreibo27yezuxj3re6rbppwuqu2gsy27u4vnzlozowglguvg4a7xyeva";

const TOOL_BACKGROUND_PATCH: &str = r#"[{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"background_requested_at","Kind":"DateTime"}}]"#;
const TOOL_FRESH_ROOT: &str = "bafyreihfkiuhbdzdlmfqf6wm4q5xgrad4wl2zi4fvcj25co3kdjrktjv5a";
const TOOL_FRESH_BACKGROUND: &str = "bafyreifddp6p4viubjrzhgbrgyjnnyzuq2u5qzpcavw2rkkmrbgfltboza";
const TOOL_LEGACY_BACKGROUND: &str = "bafyreigxgxq4itlpku2ajqwpb7axfy2pqckgpab3l5zmrljm4lcht7fhoe";
const SESSION_ROOT: &str = "bafyreietunsrqsijc3k6x3gf6y2nwkim74uf6j4lskv7p3pjrl44q4tthe";
const SESSION_TARGET: &str = "bafyreifbqcjrp64kux3uawewu3rn4xrfi7v2soi2mcuwzq4bk7vuqsq5au";

const SESSION_CURRENT_ROOT: &str = "bafyreiajedcyxilotygvagpzwbruh3nofyd5mr3kao7phf6fc73x3sdc7u";
const SESSION_ARCHIVE_CURRENT: &str = "bafyreif5fb2oygthj7amoqgnt7rkgctqyku5bwk4vdmtssqoftnsdgzvwa";
const SESSION_ARCHIVE_AFTER_STOP: &str =
    "bafyreih2muyccdxla5ox6f32aoriserpdko45a5fxqsb6zgrkkg2yeenrm";
const SESSION_ARCHIVE_PATCH: &str = r#"[{"op":"add","path":"/AgentSession/Fields/-","value":{"Name":"archived_at","Kind":"String"}},{"op":"add","path":"/AgentSession/Fields/-","value":{"Name":"archive_revision","Kind":"Int"}}]"#;

const SESSION_PRE_STARTER: &str = "bafyreigjtrpi4dok7u5lehp3n4ekttzafnok4eqmykcr46rxzvhfbd2m3q";
const SESSION_STARTER_CURRENT: &str = "bafyreido3epv2t4dknlbw5a63tzj6xgxwoqlcnazubdhg4n3khl3bjql24";
const SESSION_STARTER_AFTER_ARCHIVE: &str =
    "bafyreie6lkf4ascdijkqrrpzmeoasakjmoloyxtedouksnv2difdk5jh7y";
const SESSION_STARTER_AFTER_STOP: &str =
    "bafyreiauwwgyvzce24ptednterbddvmruuqwnq7zbd66pedrlxgqblxj7a";
const SESSION_STARTER_PATCH: &str = r#"[{"op":"add","path":"/AgentSession/Fields/-","value":{"Name":"started_by_node_did","Kind":"String","Immutable":true}},{"op":"add","path":"/AgentSession/Fields/-","value":{"Name":"started_by_session_id","Kind":"String","Immutable":true}}]"#;

fn session_before_starter(version: &str) -> &str {
    match version {
        SESSION_STARTER_CURRENT => SESSION_PRE_STARTER,
        SESSION_STARTER_AFTER_ARCHIVE => SESSION_ARCHIVE_CURRENT,
        SESSION_STARTER_AFTER_STOP => SESSION_ARCHIVE_AFTER_STOP,
        _ => version,
    }
}

/// Desired-state apply stamps `updated_at` on every create so a document
/// recreated after deletion gets a fresh content-addressed docID. DefraDB
/// refuses fields a collection does not declare, so these collections append it.
macro_rules! updated_at_upgrade {
    ($name:literal, $id:literal, $root:literal, $target:literal) => {
        (
            $name,
            $root,
            $target,
            $id,
            concat!(
                r#"[{"op":"add","path":"/"#,
                $name,
                r#"/Fields/-","value":{"Name":"updated_at","Kind":"DateTime"}}]"#
            ),
            &["updated_at"][..],
        )
    };
}

/// DefraDB field positions are persistent identifiers. Existing RC1 lineages
/// append fields through versioned patches; fresh stores retain the current
/// canonical roots. Only these exact source and destination versions qualify.
pub(crate) fn include_existing(
    node: &EmbeddedNode,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    include_existing_by_version(
        &|name| {
            node.get_collection(name)
                .map(|current| current.map(|current| current.version_id))
                .map_err(|error| Error::Node(error.into()))
        },
        baseline,
        steps,
    )
}

pub(crate) fn include_existing_by_version(
    get_version: &impl Fn(&str) -> Result<Option<String>>,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    for (name, root, target, id, patch, fields) in [
        (
            "InferenceBackend",
            "bafyreihvtdq5hp7qrehkfhgkdtqql6v6mq4podhdnfrpbrcy4x3xasddwy",
            "bafyreiabl7unecvetnazzwctm4iihrsb7x3jeo6l3m45u6bbjodj7zxfxi",
            "inference-backend-catalog-freshness",
            r#"[{"op":"add","path":"/InferenceBackend/Fields/-","value":{"Name":"catalog_freshness","Kind":"JSON"}}]"#,
            &["catalog_freshness"][..],
        ),
        (
            "PeerPairingApplied",
            "bafyreifunn7vevp6b6rzg232gjfypp2lqviafe5now5ldlwo3na5nfinq4",
            "bafyreifiujskeayi7adradcst2jit3srboodcd6ejy2m6gppwy2mv6k7ri",
            "peer-pairing-applied-bind-enrollment-generation",
            r#"[{"op":"add","path":"/PeerPairingApplied/Fields/-","value":{"Name":"enrollment_request_digest","Kind":"String"}},{"op":"add","path":"/PeerPairingApplied/Fields/-","value":{"Name":"enrollment_authorization_sequence","Kind":"Int"}},{"op":"add","path":"/PeerPairingApplied/Fields/-","value":{"Name":"enrollment_authorization_expires_at","Kind":"DateTime"}}]"#,
            &[
                "enrollment_request_digest",
                "enrollment_authorization_sequence",
                "enrollment_authorization_expires_at",
            ][..],
        ),
        (
            "InferenceCall",
            INFERENCE_CALL_ROOT,
            INFERENCE_CALL_TARGET,
            "inference-call-add-provider-usage",
            INFERENCE_CALL_USAGE_PATCH,
            &[
                "session_id",
                "model_name",
                "profile_id",
                "cache_creation_input_tokens",
                "reasoning_output_tokens",
                "provider_total_tokens",
                "usage_completeness",
            ][..],
        ),
        (
            "InferenceProfile",
            PROFILE_ROOT,
            PROFILE_TARGET,
            "inference-profile-add-service-tier",
            r#"[{"op":"add","path":"/InferenceProfile/Fields/-","value":{"Name":"service_tier","Kind":"String"}}]"#,
            &["service_tier"][..],
        ),
        (
            "AgentToolCall",
            TOOL_ROOT,
            TOOL_TARGET,
            "agent-tool-call-add-job-state",
            r#"[{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"job_pin","Kind":"JSON"}},{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"job_progress","Kind":"JSON"}},{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"job_step_state","Kind":"JSON"}}]"#,
            &["job_pin", "job_progress", "job_step_state"][..],
        ),
        (
            "AgentToolCall",
            TOOL_FRESH_ROOT,
            TOOL_FRESH_BACKGROUND,
            "agent-tool-call-add-background-request",
            TOOL_BACKGROUND_PATCH,
            &["background_requested_at"][..],
        ),
        (
            "AgentToolCall",
            TOOL_TARGET,
            TOOL_LEGACY_BACKGROUND,
            "agent-tool-call-add-background-request",
            TOOL_BACKGROUND_PATCH,
            &["background_requested_at"][..],
        ),
        (
            "AgentSession",
            SESSION_ROOT,
            SESSION_TARGET,
            "agent-session-add-stopped-at",
            r#"[{"op":"add","path":"/AgentSession/Fields/-","value":{"Name":"stopped_at","Kind":"String"}}]"#,
            &["stopped_at"][..],
        ),
        updated_at_upgrade!(
            "AgentTarget",
            "agent-target-add-updated-at",
            "bafyreieqevckhv5c6eli4ceysd3p2wlfhdzjeve2r2mn2v37arv3vn6hti",
            "bafyreierhwy2pombqxf2ufiqih275phvgdxj2w3h6e7wnuwme2tsllgoje"
        ),
        updated_at_upgrade!(
            "Tools",
            "tools-add-updated-at",
            "bafyreih62xjqp376qygv2wgbfpvlu4juroh4oqfufem37koomf6bzgny5q",
            "bafyreibrawpzq3wv3gm6ew2gd26k75rfi6xm64fp4x55njpfdics6xx26e"
        ),
        updated_at_upgrade!(
            "ChainKeyBinding",
            "chain-key-binding-add-updated-at",
            "bafyreigkgc7vx3bacpnoop5ouryihgdckohdnkjzitapw3xqgakmimxd6a",
            "bafyreias5umbmxkuo6iguihj6tvovbuzgjscwj7wjcmz23pirgk7jo4uom"
        ),
        updated_at_upgrade!(
            "EthTool",
            "eth-tool-add-updated-at",
            "bafyreieeavm747hdfwfrlm524uilhmnokwz3wodn22x4daidndbyg3pz6a",
            "bafyreie24fvfqig33silif6nxrf4jfua2yokujyqbf4ydvstrifvhweaca"
        ),
        updated_at_upgrade!(
            "Callback",
            "callback-add-updated-at",
            "bafyreigwyv5fj5jfy2evxtq5ca4xz6bqd7eq2q5mcw5moiize4gpgusu2q",
            "bafyreibijc44aul7l2m7kmvlojizbze7236sjixhi57osoqkevsreda5p4"
        ),
        updated_at_upgrade!(
            "CallbackModule",
            "callback-module-add-updated-at",
            "bafyreib7haitwuad7wkxrermsm4nyrtvjc5asva3ubms34kspq3psfmjde",
            "bafyreiceftkewzvdjuylk3djznmbw656qbopzikxneqq3v7krigoqsvqtu"
        ),
        updated_at_upgrade!(
            "CallbackBinding",
            "callback-binding-add-updated-at",
            "bafyreiepwxqpz5fakzqk34hw3w7psrgvpqi2n4q7remzsrxidzskfdvk3y",
            "bafyreigvksodxohqggry2u7dr65xak5kvlyp65rvfm62pgxvp4vlnfgody"
        ),
    ] {
        let Some(current) = get_version(name)? else {
            continue;
        };
        let current_version = if name == "AgentSession" {
            session_before_starter(&current)
        } else {
            &current
        };
        let after_job_patch =
            name == "AgentToolCall" && root == TOOL_TARGET && current_version == TOOL_ROOT;
        let after_background_patch = name == "AgentToolCall"
            && root == TOOL_ROOT
            && current_version == TOOL_LEGACY_BACKGROUND;
        let after_session_archive =
            name == "AgentSession" && current_version == SESSION_ARCHIVE_AFTER_STOP;
        if current_version != root
            && current_version != target
            && !after_job_patch
            && !after_background_patch
            && !after_session_archive
        {
            continue;
        }
        if let Some(entry) = baseline.iter_mut().find(|entry| entry.name == name) {
            if !(name == "AgentToolCall" && root == TOOL_TARGET) {
                entry.expected_version = Some(root);
            }
            steps.push(MigrationStep::PatchVersioned {
                id,
                collection: name,
                patch,
                lens: None,
                expected_version: Some(target),
                expected_transform: None,
                expected_state: CollectionExpectation::fields(fields),
            });
        }
    }
    if let Some(current) = get_version("NodeMemory")? {
        if [NODE_MEMORY_ROOT, NODE_MEMORY_TEXT_TARGET].contains(&current.as_str()) {
            if let Some(entry) = baseline.iter_mut().find(|entry| entry.name == "NodeMemory") {
                entry.expected_version = Some(NODE_MEMORY_ROOT);
                steps.push(MigrationStep::PatchVersioned {
                    id: "node-memory-add-text-value",
                    collection: "NodeMemory",
                    patch: NODE_MEMORY_TEXT_PATCH,
                    lens: None,
                    expected_version: Some(NODE_MEMORY_TEXT_TARGET),
                    expected_transform: None,
                    expected_state: CollectionExpectation::fields(&["value_text"]),
                });
            }
        }
    }
    include_session_archive(get_version, baseline, steps)?;
    include_session_starter(get_version, baseline, steps)?;
    Ok(())
}

fn include_session_archive(
    get_version: &impl Fn(&str) -> Result<Option<String>>,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    let Some(current) = get_version("AgentSession")? else {
        return Ok(());
    };
    let target = match session_before_starter(&current) {
        SESSION_ROOT | SESSION_TARGET | SESSION_ARCHIVE_AFTER_STOP => SESSION_ARCHIVE_AFTER_STOP,
        SESSION_CURRENT_ROOT | SESSION_ARCHIVE_CURRENT => {
            if let Some(entry) = baseline
                .iter_mut()
                .find(|entry| entry.name == "AgentSession")
            {
                entry.expected_version = Some(SESSION_CURRENT_ROOT);
            }
            SESSION_ARCHIVE_CURRENT
        }
        _ => return Ok(()),
    };
    steps.push(MigrationStep::PatchVersioned {
        id: "agent-session-add-archive-state",
        collection: "AgentSession",
        patch: SESSION_ARCHIVE_PATCH,
        lens: None,
        expected_version: Some(target),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&["archived_at", "archive_revision"]),
    });
    Ok(())
}

fn include_session_starter(
    get_version: &impl Fn(&str) -> Result<Option<String>>,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    let Some(current) = get_version("AgentSession")? else {
        return Ok(());
    };
    let preceding = steps.iter().rev().find_map(|step| match step {
        MigrationStep::PatchVersioned {
            collection: "AgentSession",
            expected_version,
            ..
        } => *expected_version,
        _ => None,
    });
    let source = preceding.unwrap_or_else(|| session_before_starter(&current));
    let (source, target) = match source {
        SESSION_PRE_STARTER => (SESSION_PRE_STARTER, SESSION_STARTER_CURRENT),
        SESSION_ARCHIVE_CURRENT => (SESSION_ARCHIVE_CURRENT, SESSION_STARTER_AFTER_ARCHIVE),
        SESSION_ARCHIVE_AFTER_STOP => (SESSION_ARCHIVE_AFTER_STOP, SESSION_STARTER_AFTER_STOP),
        _ => return Ok(()),
    };
    if preceding.is_none() {
        if let Some(entry) = baseline
            .iter_mut()
            .find(|entry| entry.name == "AgentSession")
        {
            entry.expected_version = Some(source);
        }
    }
    steps.push(MigrationStep::PatchVersioned {
        id: "agent-session-add-starter-reference",
        collection: "AgentSession",
        patch: SESSION_STARTER_PATCH,
        lens: None,
        expected_version: Some(target),
        expected_transform: None,
        expected_state: CollectionExpectation::fields(&[
            "started_by_node_did",
            "started_by_session_id",
        ]),
    });
    Ok(())
}
