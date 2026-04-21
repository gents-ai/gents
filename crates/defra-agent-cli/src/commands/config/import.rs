use anyhow::Result;
use serde_json::json;

use crate::cli::*;
use crate::print_json;
use crate::{
    apply_import_collection, read_config_import_bundle, resolve_config_access,
    validate_config_import_bundle,
};

pub(super) async fn config_import(args: ConfigImportArgs) -> Result<()> {
    let bundle = read_config_import_bundle(args.path.as_deref())?;
    validate_config_import_bundle(&bundle)?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    let imported_backends = apply_import_collection(
        &access,
        "InferenceBackend",
        "backend_id",
        &bundle.inference_backends,
        args.override_existing,
    )
    .await?;
    let imported_profiles = apply_import_collection(
        &access,
        "InferenceProfile",
        "profile_id",
        &bundle.inference_profiles,
        args.override_existing,
    )
    .await?;
    let imported_tool_service_registries = apply_import_collection(
        &access,
        "ToolServiceRegistry",
        "service_id",
        &bundle.tool_service_registries,
        args.override_existing,
    )
    .await?;
    let imported_tool_selections = apply_import_collection(
        &access,
        "ToolSelection",
        "selection_id",
        &bundle.tool_selections,
        args.override_existing,
    )
    .await?;
    let imported_behaviors = apply_import_collection(
        &access,
        "AgentBehavior",
        "behavior_id",
        &bundle.agent_behaviors,
        args.override_existing,
    )
    .await?;
    let imported_tasks = apply_import_collection(
        &access,
        "Task",
        "task_id",
        &bundle.tasks,
        args.override_existing,
    )
    .await?;
    let imported_schedules = apply_import_collection(
        &access,
        "Schedule",
        "schedule_id",
        &bundle.schedules,
        args.override_existing,
    )
    .await?;
    let imported_principal = apply_import_collection(
        &access,
        "AgentPrincipal",
        "agent_did",
        &bundle
            .agent_principal
            .clone()
            .into_iter()
            .collect::<Vec<_>>(),
        args.override_existing,
    )
    .await?;

    let output = json!({
        "status": "imported",
        "format": bundle.format,
        "agent_did": bundle.agent_did,
        "access_mode": access.mode(),
        "override": args.override_existing,
        "counts": {
            "agent_principal": imported_principal,
            "agent_behaviors": imported_behaviors,
            "tool_selections": imported_tool_selections,
            "inference_backends": imported_backends,
            "inference_profiles": imported_profiles,
            "tool_service_registries": imported_tool_service_registries,
            "tasks": imported_tasks,
            "schedules": imported_schedules,
        },
    });
    print_json(&output)?;
    Ok(())
}
