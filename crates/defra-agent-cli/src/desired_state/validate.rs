use std::collections::BTreeSet;

use defra_agent::{parse_template_for_validation, VariableRef};

use super::DesiredStateManifest;

pub(crate) fn validate_manifest(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    let principal_agent_did = manifest.agent_principal.agent_did.trim();
    if principal_agent_did.is_empty() {
        errors.push("agent-principal.json must contain a non-empty agent_did".to_string());
    }

    let mut behavior_ids = BTreeSet::new();
    let mut backend_ids = BTreeSet::new();
    let mut tool_selection_ids = BTreeSet::new();
    let mut profile_ids = BTreeSet::new();
    let mut service_ids = BTreeSet::new();

    for backend in &manifest.inference_backends {
        let backend_id = backend.backend_id.trim();
        if backend_id.is_empty() {
            errors.push(
                "inference-backends.json contains a backend with an empty backend_id".to_string(),
            );
        } else if !backend_ids.insert(backend_id.to_string()) {
            errors.push(format!(
                "duplicate backend_id in inference-backends.json: {backend_id}"
            ));
        }

        if backend.endpoint.trim().is_empty() {
            errors.push(format!(
                "backend {} in inference-backends.json must contain a non-empty endpoint",
                backend.backend_id
            ));
        }

        if backend
            .api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| value.is_empty())
        {
            errors.push(format!(
                "backend {} in inference-backends.json contains an empty api_key",
                backend.backend_id
            ));
        }

        if backend
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
            && backend
                .api_key_env_var
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
        {
            errors.push(format!(
                "backend {} in inference-backends.json must not set both api_key and api_key_env_var",
                backend.backend_id
            ));
        }
    }

    for selection in &manifest.tool_selections {
        let selection_id = selection.selection_id.trim();
        if selection_id.is_empty() {
            errors.push(
                "tool-selections.json contains a tool selection with an empty selection_id"
                    .to_string(),
            );
        } else if !tool_selection_ids.insert(selection_id.to_string()) {
            errors.push(format!(
                "duplicate selection_id in tool-selections.json: {selection_id}"
            ));
        }

        if !principal_agent_did.is_empty() && selection.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "tool selection {} belongs to {} not {}",
                selection.selection_id, selection.agent_did, manifest.agent_principal.agent_did
            ));
        }
    }

    for profile in &manifest.inference_profiles {
        let profile_id = profile.profile_id.trim();
        if profile_id.is_empty() {
            errors.push(
                "inference-profiles.json contains a profile with an empty profile_id".to_string(),
            );
        } else if !profile_ids.insert(profile_id.to_string()) {
            errors.push(format!(
                "duplicate profile_id in inference-profiles.json: {profile_id}"
            ));
        }
    }

    for service in &manifest.tool_service_registries {
        let service_id = service.service_id.trim();
        if service_id.is_empty() {
            errors.push(
                "tool-services manifest contains a service with an empty service_id".to_string(),
            );
        } else if !service_ids.insert(service_id.to_string()) {
            errors.push(format!(
                "duplicate service_id in tool-services manifest: {service_id}"
            ));
        }

        if service.mcp_port.unwrap_or_default() <= 0 {
            errors.push(format!(
                "service {} in tool-services manifest must contain a positive mcp_port",
                service.service_id
            ));
        }

        if non_empty(&service.hostname).is_none()
            && non_empty(&service.tailscale_ip).is_none()
            && non_empty(&service.lan_ip).is_none()
        {
            errors.push(format!(
                "service {} in tool-services manifest must contain at least one of hostname, tailscale_ip, or lan_ip",
                service.service_id
            ));
        }
    }

    for behavior in &manifest.agent_behaviors {
        let behavior_id = behavior.behavior_id.trim();
        if behavior_id.is_empty() {
            errors.push(
                "agent-behaviors.json contains a behavior with an empty behavior_id".to_string(),
            );
        } else if !behavior_ids.insert(behavior_id.to_string()) {
            errors.push(format!(
                "duplicate behavior_id in agent-behaviors.json: {behavior_id}"
            ));
        }

        if !principal_agent_did.is_empty() && behavior.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "behavior {} belongs to {} not {}",
                behavior.behavior_id, behavior.agent_did, manifest.agent_principal.agent_did
            ));
        }

        if let Some(backend_id) = non_empty(&behavior.backend_id) {
            if !backend_ids.contains(backend_id) {
                errors.push(format!(
                    "behavior {} references missing backend_id {}",
                    behavior.behavior_id, backend_id
                ));
            }
        }

        if let Some(selection_id) = non_empty(&behavior.tool_selection_id) {
            if !tool_selection_ids.contains(selection_id) {
                errors.push(format!(
                    "behavior {} references missing tool_selection_id {}",
                    behavior.behavior_id, selection_id
                ));
            }
        }

        if let Some(profile_id) = non_empty(&behavior.inference_profile_id) {
            if !profile_ids.contains(profile_id) {
                errors.push(format!(
                    "behavior {} references missing inference_profile_id {}",
                    behavior.behavior_id, profile_id
                ));
            }
        }
    }

    match non_empty(&manifest.agent_principal.default_behavior_id) {
        Some(default_behavior_id) => {
            if !behavior_ids.contains(default_behavior_id) {
                errors.push(format!(
                    "agent-principal.json default_behavior_id {} is not present in agent-behaviors.json",
                    default_behavior_id
                ));
            }
        }
        None => errors
            .push("agent-principal.json must contain a non-empty default_behavior_id".to_string()),
    }

    let mut task_ids = BTreeSet::new();
    for task in &manifest.tasks {
        let task_id = task.task_id.trim();
        if task_id.is_empty() {
            errors.push("tasks manifest contains a task with an empty task_id".to_string());
        } else if !task_ids.insert(task_id.to_string()) {
            errors.push(format!("duplicate task_id in tasks manifest: {task_id}"));
        }

        if task.name.trim().is_empty() {
            errors.push(format!(
                "task {} in tasks manifest must contain a non-empty name",
                task.task_id
            ));
        }

        let behavior_id = task.behavior_id.trim();
        if behavior_id.is_empty() {
            errors.push(format!(
                "task {} in tasks manifest must contain a non-empty behavior_id",
                task.task_id
            ));
        } else if !behavior_ids.contains(behavior_id) {
            errors.push(format!(
                "task {} references missing behavior_id {}",
                task.task_id, behavior_id
            ));
        }
    }

    let mut schedule_ids = BTreeSet::new();
    for schedule in &manifest.schedules {
        let schedule_id = schedule.schedule_id.trim();
        if schedule_id.is_empty() {
            errors.push(
                "schedules manifest contains a schedule with an empty schedule_id".to_string(),
            );
        } else if !schedule_ids.insert(schedule_id.to_string()) {
            errors.push(format!(
                "duplicate schedule_id in schedules manifest: {schedule_id}"
            ));
        }

        let task_id = schedule.task_id.trim();
        if task_id.is_empty() {
            errors.push(format!(
                "schedule {} in schedules manifest must contain a non-empty task_id",
                schedule.schedule_id
            ));
        } else if !task_ids.contains(task_id) {
            errors.push(format!(
                "schedule {} references missing task_id {}",
                schedule.schedule_id, task_id
            ));
        }

        if schedule.interval_secs < 1 {
            errors.push(format!(
                "schedule {} in schedules manifest must contain an interval_secs >= 1",
                schedule.schedule_id
            ));
        }

        match schedule.concurrency.trim() {
            "parallel" | "serial" | "latest_only" => {}
            other => errors.push(format!(
                "schedule {} in schedules manifest has unknown concurrency {}",
                schedule.schedule_id, other
            )),
        }

        // Schedule scope only supplies `event.*` — reject any `doc.*` or
        // `args.*` references in the linked task's prompt template so the
        // trigger cannot fail at render time with a missing scope.
        if !task_id.is_empty() {
            if let Some(task) = manifest.tasks.iter().find(|task| task.task_id == task_id) {
                match parse_template_for_validation(&task.prompt_template) {
                    Ok(refs) => {
                        let mut reported: BTreeSet<&str> = BTreeSet::new();
                        for var in &refs {
                            if let Some(root) = var.root() {
                                if (root == "doc" || root == "args") && reported.insert(root) {
                                    errors.push(format!(
                                        "schedule {} prompt template references forbidden scope: {}; schedule scope only permits event.*",
                                        schedule.schedule_id,
                                        format_variable_ref(var),
                                    ));
                                }
                            }
                        }
                    }
                    Err(err) => errors.push(format!(
                        "schedule {} prompt template failed to parse: {}",
                        schedule.schedule_id, err
                    )),
                }
            }
        }
    }

    let mut event_trigger_ids = BTreeSet::new();
    for trig in &manifest.event_triggers {
        let trigger_id = trig.trigger_id.trim();
        if trigger_id.is_empty() {
            errors.push(
                "event-triggers manifest contains a trigger with an empty trigger_id".to_string(),
            );
            continue;
        }
        if !event_trigger_ids.insert(trigger_id.to_string()) {
            errors.push(format!(
                "duplicate trigger_id in event-triggers manifest: {trigger_id}"
            ));
        }

        let task_id = trig.task_id.trim();
        if task_id.is_empty() {
            errors.push(format!(
                "event_trigger {} in event-triggers manifest must contain a non-empty task_id",
                trig.trigger_id
            ));
        }

        if trig.source_collection.trim().is_empty() {
            errors.push(format!(
                "event_trigger {} in event-triggers manifest must contain a non-empty source_collection",
                trig.trigger_id
            ));
        }

        // v1 only supports "created"
        if trig.event_kind != "created" {
            errors.push(format!(
                "event_trigger {} uses unsupported event_kind {:?} (v1 supports only \"created\")",
                trig.trigger_id, trig.event_kind
            ));
        }

        match trig.concurrency.trim() {
            "parallel" | "serial" | "latest_only" => {}
            other => errors.push(format!(
                "event_trigger {} in event-triggers manifest has unknown concurrency {}; expected parallel|serial|latest_only",
                trig.trigger_id, other
            )),
        }

        // Cross-ref: task_id must exist in manifest.tasks
        if !task_id.is_empty() && !manifest.tasks.iter().any(|t| t.task_id == task_id) {
            errors.push(format!(
                "event_trigger {} references unknown task_id {}",
                trig.trigger_id, trig.task_id
            ));
        }

        // Template scope validation: doc.* IS allowed for event triggers; args.* is NOT.
        if !task_id.is_empty() {
            if let Some(task) = manifest.tasks.iter().find(|t| t.task_id == task_id) {
                match parse_template_for_validation(&task.prompt_template) {
                    Ok(refs) => {
                        let mut reported: BTreeSet<&str> = BTreeSet::new();
                        for vref in &refs {
                            if let Some(root) = vref.root() {
                                if root == "args" && reported.insert("args") {
                                    errors.push(format!(
                                        "event_trigger {} prompt template references forbidden scope: args; event scope only permits event.* and doc.*",
                                        trig.trigger_id
                                    ));
                                }
                            }
                        }
                    }
                    Err(err) => errors.push(format!(
                        "event_trigger {} prompt template failed to parse: {}",
                        trig.trigger_id, err
                    )),
                }
            }
        }
    }
}

fn format_variable_ref(var: &VariableRef) -> String {
    if var.path.is_empty() {
        String::new()
    } else {
        var.path.join(".")
    }
}

pub(crate) fn non_empty(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn normalize_tool_service_string(value: Option<String>) -> String {
    value.unwrap_or_default().trim().to_string()
}

pub(crate) fn normalize_tool_service_mcp_path(value: Option<String>) -> String {
    use super::DEFAULT_TOOL_SERVICE_MCP_PATH;
    let trimmed = value.as_deref().unwrap_or_default().trim();
    if trimmed.is_empty() {
        DEFAULT_TOOL_SERVICE_MCP_PATH.to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

pub(super) fn optional_string_from_value(
    field: &str,
    value: Option<&serde_json::Value>,
) -> anyhow::Result<Option<String>> {
    use anyhow::anyhow;
    use serde_json::Value;
    match value {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(anyhow!(
            "ToolServiceRegistry field {field} must be a string or null, got {value}"
        )),
    }
}

pub(super) fn optional_i64_from_value(
    field: &str,
    value: Option<&serde_json::Value>,
) -> anyhow::Result<Option<i64>> {
    use anyhow::anyhow;
    use serde_json::Value;
    match value {
        Some(Value::Number(value)) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow!("ToolServiceRegistry field {field} must be an integer")),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(anyhow!(
            "ToolServiceRegistry field {field} must be an integer or null, got {value}"
        )),
    }
}
