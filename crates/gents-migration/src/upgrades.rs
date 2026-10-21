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
        if current.version_id != root
            && current.version_id != target
            && !after_job_patch
            && !after_background_patch
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
    Ok(())
}
