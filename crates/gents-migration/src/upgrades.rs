use defra_node::EmbeddedNode;

use crate::error::{Error, Result};
use crate::expectation::CollectionExpectation;
use crate::registry::{BaselineCollection, MigrationStep};

const PROFILE_ROOT: &str = "bafyreiat2uhx24kbgbm54lz7nt56vtjjsfttkiwop2ntii7qn2xtceehxi";
const PROFILE_TARGET: &str = "bafyreieksrjdkg546ymilx5qrpbtciuyr3cwqyq25u52o64lobpoek3qqi";
const TOOL_ROOT: &str = "bafyreiexmbjxfqyg6yhjst6r746spzge27f6mhh2moyrkawbmvtvba4mbi";
const TOOL_TARGET: &str = "bafyreibo27yezuxj3re6rbppwuqu2gsy27u4vnzlozowglguvg4a7xyeva";

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
    ] {
        let Some(current) = node
            .get_collection(name)
            .map_err(|error| Error::Node(error.into()))?
        else {
            continue;
        };
        if current.version_id != root && current.version_id != target {
            continue;
        }
        if let Some(entry) = baseline.iter_mut().find(|entry| entry.name == name) {
            entry.expected_version = Some(root);
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
