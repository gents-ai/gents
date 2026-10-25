use defra_node::EmbeddedNode;

use crate::error::{Error, Result};
use crate::expectation::CollectionExpectation;
use crate::registry::{BaselineCollection, MigrationStep};

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

/// DefraDB field positions are persistent identifiers. Existing RC1 lineages
/// append fields through versioned patches; fresh stores retain the current
/// canonical roots. Only these exact source and destination versions qualify.
pub(crate) fn include_existing(
    node: &EmbeddedNode,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    for (name, root, target, id, patch, fields) in [
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
    ] {
        let Some(current) = node
            .get_collection(name)
            .map_err(|error| Error::Node(error.into()))?
        else {
            continue;
        };
        let after_job_patch =
            name == "AgentToolCall" && root == TOOL_TARGET && current.version_id == TOOL_ROOT;
        let after_background_patch = name == "AgentToolCall"
            && root == TOOL_ROOT
            && current.version_id == TOOL_LEGACY_BACKGROUND;
        let after_session_archive =
            name == "AgentSession" && current.version_id == SESSION_ARCHIVE_AFTER_STOP;
        if current.version_id != root
            && current.version_id != target
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
    include_session_archive(node, baseline, steps)?;
    Ok(())
}

fn include_session_archive(
    node: &EmbeddedNode,
    baseline: &mut [BaselineCollection<'static>],
    steps: &mut Vec<MigrationStep<'static>>,
) -> Result<()> {
    let Some(current) = node
        .get_collection("AgentSession")
        .map_err(|error| Error::Node(error.into()))?
    else {
        return Ok(());
    };
    let target = match current.version_id.as_str() {
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
