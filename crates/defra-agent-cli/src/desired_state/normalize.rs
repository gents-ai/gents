use serde_json::{Map, Value};

use super::DesiredStateManifest;

pub(crate) fn normalize_manifest(manifest: &mut DesiredStateManifest) {
    manifest
        .agent_behaviors
        .sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));
    manifest
        .tool_selections
        .sort_by(|left, right| left.selection_id.cmp(&right.selection_id));
    manifest
        .inference_backends
        .sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
    manifest
        .inference_profiles
        .sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
    manifest
        .tool_service_registries
        .sort_by(|left, right| left.service_id.cmp(&right.service_id));
    manifest
        .scheduled_tasks
        .sort_by(|left, right| left.task_id.cmp(&right.task_id));

    for selection in &mut manifest.tool_selections {
        selection.cli_tool_names.sort();
        selection.cli_tool_names.dedup();
        selection.delegate_to.sort();
        selection.delegate_to.dedup();
    }
    for backend in &mut manifest.inference_backends {
        backend.models.sort();
        backend.models.dedup();
    }
}

pub(crate) fn default_max_queue_depth() -> i64 {
    100
}

pub(crate) fn strip_deprecated_inference_backend_fields(object: &mut Map<String, Value>) {
    for field in [
        "supports_tool_calls",
        "supports_streaming",
        "supports_structured_outputs",
        "supports_json_schema",
        "context_window",
        "max_output_tokens",
    ] {
        object.remove(field);
    }
}
