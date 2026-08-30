use super::super::DesiredStateManifest;
use super::{agent, automation, projection, tooling};

pub(crate) fn validate_manifest(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    let principal_agent_did = agent::validate_principal(manifest, errors);
    tooling::validate_surfaces(manifest, principal_agent_did, errors);
    tooling::validate_eth_tools(manifest, principal_agent_did, errors);
    let (backend_ids, backend_models) = agent::validate_backends(manifest, errors);
    let tool_selection_ids =
        tooling::validate_tool_selections(manifest, principal_agent_did, errors);
    let profile_ids = agent::validate_profiles(manifest, errors);
    tooling::validate_tool_service_registries(manifest, errors);
    let skill_ids = agent::validate_skills(manifest, principal_agent_did, errors);
    let behavior_ids = agent::validate_behaviors(
        manifest,
        principal_agent_did,
        &backend_ids,
        &backend_models,
        &tool_selection_ids,
        &profile_ids,
        &skill_ids,
        errors,
    );
    projection::validate_projection_bindings(manifest, principal_agent_did, &behavior_ids, errors);
    agent::validate_default_behavior(manifest, &behavior_ids, errors);
    let task_ids = automation::validate_tasks(manifest, &behavior_ids, errors);
    automation::validate_schedules(manifest, &task_ids, errors);
    automation::validate_event_triggers(manifest, errors);
    automation::validate_callback_bindings(manifest, errors);
    automation::validate_repository_placements(manifest, errors);
}
