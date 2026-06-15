use std::collections::{BTreeSet, HashSet};

use anyhow::Result;
use defra_agent::template::{
    catalog::{default_catalog, Site},
    reads::validate_system_template,
};
use defra_agent::{
    is_reserved_builtin_tool_name, parse_template_for_validation,
    schedule_cron::validate_cron_schedule, CommandExecutionMode, CommandNetworkMode,
    SubagentTarget, VariableRef, WriteToolDecl,
};

use super::DesiredStateManifest;

use crate::config_writes::ConfigAccess;

const PROJECTION_ACP_BINDING_PROJECTION_IDS: &[&str] = &[
    "openai_codex_run_trace",
    "langgraph_state_history",
    "multi_agent_task",
];

const PROJECTION_ACP_RUNTIME_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentResponse",
    "AgentSession",
    "AgentConversation",
];

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
    let mut projection_binding_ids = BTreeSet::new();

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

        if let Some(mode) = selection
            .subagent_default_await_mode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match mode {
                "foreground" => {}
                "background" if selection.subagent_background_enabled => {}
                "background" => errors.push(format!(
                    "tool selection {} sets subagent_default_await_mode=background but subagent_background_enabled is false",
                    selection.selection_id
                )),
                other => errors.push(format!(
                    "tool selection {} has invalid subagent_default_await_mode {other:?}; expected foreground or background",
                    selection.selection_id
                )),
            }
        }

        if let Some(mode) = selection.command_execution_policy.as_deref() {
            if let Err(error) = CommandExecutionMode::parse(mode) {
                errors.push(format!(
                    "tool selection {} has invalid command_execution_policy: {error}",
                    selection.selection_id
                ));
            }
        }

        for (index, tool_name) in selection.backgroundable_tool_names.iter().enumerate() {
            if tool_name.trim().is_empty() {
                errors.push(format!(
                    "tool selection {} has empty backgroundable_tool_names[{index}]",
                    selection.selection_id
                ));
            }
        }
        for (index, target) in selection.subagent_targets.iter().enumerate() {
            if target.trim().is_empty() {
                errors.push(format!(
                    "tool selection {} has empty subagent_targets[{index}]",
                    selection.selection_id
                ));
            }
        }
        if let Some(mode) = selection.command_network_mode.as_deref() {
            if let Err(error) = CommandNetworkMode::parse(mode) {
                errors.push(format!(
                    "tool selection {} has invalid command_network_mode: {error}",
                    selection.selection_id
                ));
            }
        }
        validate_argv_prefixes(
            &selection.selection_id,
            "command_allowed_argv_prefixes",
            &selection.command_allowed_argv_prefixes,
            errors,
        );
        validate_argv_prefixes(
            &selection.selection_id,
            "command_forbidden_argv_prefixes",
            &selection.command_forbidden_argv_prefixes,
            errors,
        );
        validate_non_empty_values(
            &selection.selection_id,
            "allowed_mcp_service_ids",
            &selection.allowed_mcp_service_ids,
            errors,
        );
        validate_subagent_targets(
            &selection.selection_id,
            selection.agent_did.trim(),
            selection.subagent_allow_cross_deployment,
            &selection.subagent_targets,
            errors,
        );
        validate_write_tools(
            &selection.selection_id,
            &selection.write_tools,
            &selection.cli_tool_names,
            errors,
        );
        if selection.subagent_spawn_enabled {
            if selection.subagent_targets.is_empty() {
                errors.push(format!(
                    "tool selection {} sets subagent_spawn_enabled but has no subagent_targets; the tools would be inert",
                    selection.selection_id
                ));
            }
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
        if profile
            .stream_liveness_timeout_secs
            .is_some_and(|value| value <= 0)
        {
            errors.push(format!(
                "InferenceProfile {profile_id} stream_liveness_timeout_secs must be positive"
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

    let mut skill_ids = BTreeSet::new();
    for skill in &manifest.skills {
        let skill_id = skill.skill_id.trim();
        if skill_id.is_empty() {
            errors.push("skills manifest contains a skill with an empty skill_id".to_string());
        } else if !skill_ids.insert(skill_id.to_string()) {
            errors.push(format!("duplicate skill_id in skills manifest: {skill_id}"));
        }

        if !principal_agent_did.is_empty() && skill.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "skill {} belongs to {} not {}",
                skill.skill_id, skill.agent_did, manifest.agent_principal.agent_did
            ));
        }

        if !matches!(skill.scope.trim(), "principal" | "behavior") {
            errors.push(format!(
                "skill {} has invalid scope {:?}; expected \"principal\" or \"behavior\"",
                skill.skill_id, skill.scope
            ));
        }

        if skill.name.trim().is_empty() {
            errors.push(format!(
                "skill {} in skills manifest must contain a non-empty name",
                skill.skill_id
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

        if let Some(system_prompt) = behavior.system_prompt.as_deref() {
            validate_behavior_system_template(&behavior.behavior_id, system_prompt, errors);
        }

        // skill_refs / skill_excludes must resolve to skills in this manifest.
        // Because every skill is validated above to belong to this principal,
        // this also enforces D6 (no live cross-principal skill references —
        // share by importing a copy instead).
        for skill_ref in &behavior.skill_refs {
            let skill_ref = skill_ref.trim();
            if !skill_ref.is_empty() && !skill_ids.contains(skill_ref) {
                errors.push(format!(
                    "behavior {} references missing skill_ref {} (import the skill first)",
                    behavior.behavior_id, skill_ref
                ));
            }
        }
        for skill_exclude in &behavior.skill_excludes {
            let skill_exclude = skill_exclude.trim();
            if !skill_exclude.is_empty() && !skill_ids.contains(skill_exclude) {
                errors.push(format!(
                    "behavior {} references missing skill_exclude {}",
                    behavior.behavior_id, skill_exclude
                ));
            }
        }
    }

    for binding in &manifest.projection_acp_bindings {
        let binding_id = binding.binding_id.trim();
        if binding_id.is_empty() {
            errors.push(
                "projection-acp-bindings manifest contains a binding with an empty binding_id"
                    .to_string(),
            );
        } else if !projection_binding_ids.insert(binding_id.to_string()) {
            errors.push(format!(
                "duplicate binding_id in projection-acp-bindings manifest: {binding_id}"
            ));
        }

        if non_empty(&binding.agent_did).is_none() {
            errors.push(format!(
                "projection ACP binding {} must contain a non-empty agent_did",
                binding.binding_id
            ));
        } else if !principal_agent_did.is_empty()
            && non_empty(&binding.agent_did) != Some(principal_agent_did)
        {
            errors.push(format!(
                "projection ACP binding {} belongs to {} not {}",
                binding.binding_id,
                binding.agent_did.as_deref().unwrap_or_default(),
                manifest.agent_principal.agent_did
            ));
        }

        if binding.policy_id.trim().is_empty() {
            errors.push(format!(
                "projection ACP binding {} must contain a non-empty policy_id",
                binding.binding_id
            ));
        }

        if let Some(behavior_id) = non_empty(&binding.behavior_id) {
            if !behavior_ids.contains(behavior_id) {
                errors.push(format!(
                    "projection ACP binding {} references missing behavior_id {}",
                    binding.binding_id, behavior_id
                ));
            }
        }

        validate_projection_id(binding, errors);
        validate_projection_policy_lifecycle(binding, errors);
        validate_projection_resource_map_json(binding, errors);
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

        validate_task_template_catalog_scope(task, errors);
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

        validate_schedule_cadence(schedule, errors);

        match schedule.concurrency.trim() {
            "parallel" | "serial" | "latest_only" => {}
            other => errors.push(format!(
                "schedule {} in schedules manifest has unknown concurrency {}",
                schedule.schedule_id, other
            )),
        }

        // Schedule scope supplies `event.*` plus the task catalog scope
        // (`node.*`, `ctx.now`) — reject any `doc.*` or `args.*` references
        // in the linked task's prompt template so the trigger cannot fail at
        // render time with a missing scope.
        if !task_id.is_empty() {
            if let Some(task) = manifest.tasks.iter().find(|task| task.task_id == task_id) {
                match parse_template_for_validation(&task.prompt_template) {
                    Ok(refs) => {
                        let mut reported: BTreeSet<&str> = BTreeSet::new();
                        for var in &refs {
                            if let Some(root) = var.root() {
                                if (root == "doc" || root == "args") && reported.insert(root) {
                                    errors.push(format!(
                                        "schedule {} prompt template references forbidden scope: {}; schedule scope only permits event.*, node.*, and ctx.now",
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
                                        "event_trigger {} prompt template references forbidden scope: args; event scope only permits event.*, doc.*, node.*, and ctx.now",
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

fn validate_behavior_system_template(
    behavior_id: &str,
    system_prompt: &str,
    errors: &mut Vec<String>,
) {
    if !contains_template_marker(system_prompt) {
        return;
    }
    let catalog = default_catalog();
    if let Err(error) = validate_system_template(system_prompt, &catalog) {
        errors.push(format!(
            "behavior {behavior_id} system_prompt template is invalid: {error}"
        ));
    }
}

fn validate_task_template_catalog_scope(task: &super::DesiredTask, errors: &mut Vec<String>) {
    let refs = match parse_template_for_validation(&task.prompt_template) {
        Ok(refs) => refs,
        Err(error) => {
            errors.push(format!(
                "task {} prompt template failed to parse: {}",
                task.task_id, error
            ));
            return;
        }
    };
    let catalog = default_catalog();
    let mut reported: BTreeSet<String> = BTreeSet::new();
    for var in refs {
        let Some(root) = var.root() else {
            continue;
        };
        if root != "node" && root != "ctx" {
            continue;
        }
        let full_ref = format_variable_ref(&var);
        if catalog.is_available_at(&full_ref, Site::Task) {
            continue;
        }
        if reported.insert(full_ref.clone()) {
            errors.push(format!(
                "task {} prompt_template references unavailable template variable {}; task scope permits node.node_did, node.behavior_id, and ctx.now",
                task.task_id, full_ref
            ));
        }
    }
}

fn contains_template_marker(value: &str) -> bool {
    value.contains("{{") || value.contains("{%") || value.contains("{#")
}

fn validate_projection_resource_map_json(
    binding: &super::DesiredProjectionAcpBinding,
    errors: &mut Vec<String>,
) {
    let Some(raw) = non_empty(&binding.resource_map_json) else {
        return;
    };
    let parsed = serde_json::from_str::<std::collections::BTreeMap<String, String>>(raw);
    let resource_map = match parsed {
        Ok(resource_map) => resource_map,
        Err(error) => {
            errors.push(format!(
                "projection ACP binding {} has invalid resource_map_json: {}",
                binding.binding_id, error
            ));
            return;
        }
    };
    for (collection, resource_name) in resource_map {
        let collection = collection.trim();
        let resource_name = resource_name.trim();
        if collection.is_empty() || resource_name.is_empty() {
            errors.push(format!(
                "projection ACP binding {} resource_map_json must map non-empty collection names to non-empty ACP resource names",
                binding.binding_id
            ));
            break;
        }
        if !PROJECTION_ACP_RUNTIME_COLLECTIONS.contains(&collection) {
            errors.push(format!(
                "projection ACP binding {} resource_map_json contains unknown runtime collection {}; expected one of {}",
                binding.binding_id,
                collection,
                PROJECTION_ACP_RUNTIME_COLLECTIONS.join(", ")
            ));
        }
    }
}

fn validate_projection_id(binding: &super::DesiredProjectionAcpBinding, errors: &mut Vec<String>) {
    let Some(projection_id) = non_empty(&binding.projection_id) else {
        return;
    };
    if !PROJECTION_ACP_BINDING_PROJECTION_IDS.contains(&projection_id) {
        errors.push(format!(
            "projection ACP binding {} has invalid projection_id {}; expected one of {}",
            binding.binding_id,
            projection_id,
            PROJECTION_ACP_BINDING_PROJECTION_IDS.join(", ")
        ));
    }
}

fn validate_projection_policy_lifecycle(
    binding: &super::DesiredProjectionAcpBinding,
    errors: &mut Vec<String>,
) {
    let policy_id = binding.policy_id.trim();
    let staged_policy_id = non_empty(&binding.staged_policy_id);
    let previous_policy_id = non_empty(&binding.previous_policy_id);
    if let Some(staged_policy_id) = staged_policy_id {
        if staged_policy_id == policy_id {
            errors.push(format!(
                "projection ACP binding {} staged_policy_id must differ from active policy_id",
                binding.binding_id
            ));
        }
        if previous_policy_id == Some(staged_policy_id) {
            errors.push(format!(
                "projection ACP binding {} staged_policy_id must differ from previous_policy_id",
                binding.binding_id
            ));
        }
    }
    if previous_policy_id == Some(policy_id) {
        errors.push(format!(
            "projection ACP binding {} previous_policy_id must differ from active policy_id",
            binding.binding_id
        ));
    }

    match non_empty(&binding.publication_status) {
        None => {
            if staged_policy_id.is_some() {
                errors.push(format!(
                    "projection ACP binding {} staged_policy_id requires publication_status staged or rotating",
                    binding.binding_id
                ));
            }
        }
        Some("draft") => {
            if binding.enabled {
                errors.push(format!(
                    "projection ACP binding {} publication_status draft must not be enabled",
                    binding.binding_id
                ));
            }
            if staged_policy_id.is_some() {
                errors.push(format!(
                    "projection ACP binding {} publication_status draft must not keep staged_policy_id",
                    binding.binding_id
                ));
            }
        }
        Some("staged") => {
            if binding.enabled {
                errors.push(format!(
                    "projection ACP binding {} publication_status staged must not be enabled",
                    binding.binding_id
                ));
            }
            if staged_policy_id.is_none() {
                errors.push(format!(
                    "projection ACP binding {} publication_status staged requires staged_policy_id",
                    binding.binding_id
                ));
            }
        }
        Some("published") => {
            if staged_policy_id.is_some() {
                errors.push(format!(
                    "projection ACP binding {} publication_status published must not keep staged_policy_id; promote it to policy_id",
                    binding.binding_id
                ));
            }
        }
        Some("rotating") => {
            if staged_policy_id.is_none() {
                errors.push(format!(
                    "projection ACP binding {} publication_status rotating requires staged_policy_id",
                    binding.binding_id
                ));
            }
        }
        Some("retired") => {
            if binding.enabled {
                errors.push(format!(
                    "projection ACP binding {} publication_status retired must not be enabled",
                    binding.binding_id
                ));
            }
        }
        Some(status) => errors.push(format!(
            "projection ACP binding {} has invalid publication_status {}; expected draft, staged, published, rotating, or retired",
            binding.binding_id, status
        )),
    }
}

fn validate_schedule_cadence(schedule: &super::DesiredSchedule, errors: &mut Vec<String>) {
    let interval_secs = schedule.interval_secs;
    let cron = schedule
        .cron
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (interval_secs, cron) {
        (Some(interval_secs), None) if interval_secs >= 1 => {}
        (Some(_), Some(_)) => errors.push(format!(
            "schedule {} in schedules manifest must contain exactly one of interval_secs or cron",
            schedule.schedule_id
        )),
        (Some(_), None) => errors.push(format!(
            "schedule {} in schedules manifest must contain an interval_secs >= 1",
            schedule.schedule_id
        )),
        (None, Some(expression)) => {
            let timezone = schedule
                .timezone
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let Some(timezone) = timezone else {
                errors.push(format!(
                    "schedule {} in schedules manifest must contain a timezone when cron is set",
                    schedule.schedule_id
                ));
                return;
            };
            if let Err(error) =
                validate_cron_schedule(expression, timezone, schedule.missed_run_policy.as_deref())
            {
                errors.push(format!(
                    "schedule {} in schedules manifest has invalid cron schedule: {}",
                    schedule.schedule_id, error
                ));
            }
        }
        (None, None) => errors.push(format!(
            "schedule {} in schedules manifest must contain exactly one of interval_secs or cron",
            schedule.schedule_id
        )),
    }
}

/// Live-DB validation that complements the pure `validate_manifest`.
///
/// Unlike `validate_manifest`, this probes the live database schema and
/// filter syntax for every `EventTrigger`. It is only invoked from code
/// paths that already hold a live `ConfigAccess` (i.e. `config apply`).
///
/// Two checks per trigger:
///
/// 1. **Filter syntax probe.** Run `{collection}(filter: <trigger.filter>,
///    limit: 1) { _docID }` — DefraDB surfaces parse errors as GraphQL
///    errors, which `ConfigAccess::execute` turns into an `Err`. We catch
///    it and report the underlying message. An empty / absent filter is a
///    no-op (engine substitutes an always-match filter).
///
/// 2. **Template `doc.*` field resolution.** Parse the referenced Task's
///    `prompt_template`, extract every `doc.<field>` root, and introspect
///    the source collection's GraphQL type. Reject when any top-level
///    `doc.X` field does not exist on the source. Deep-path (`doc.a.b`)
///    resolution is explicitly out of scope for v1 — top-level existence
///    is the guarantee we offer.
pub(crate) async fn validate_manifest_against_live(
    manifest: &DesiredStateManifest,
    access: &ConfigAccess,
) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    for trig in &manifest.event_triggers {
        // Skip triggers that failed basic structural validation; the pure
        // validator already reported those and live probes on empty
        // source_collection / trigger_id would only add noise.
        let source_collection = trig.source_collection.trim();
        let trigger_id = trig.trigger_id.trim();
        if source_collection.is_empty() || trigger_id.is_empty() {
            continue;
        }

        // 1. Filter syntax probe.
        if let Some(filter) = trig.filter.as_deref().map(str::trim) {
            if !filter.is_empty() {
                let probe = format!(
                    r#"query {{ {collection}(filter: {filter}, limit: 1) {{ _docID }} }}"#,
                    collection = source_collection,
                    filter = filter,
                );
                match access.execute(&probe).await {
                    Ok(_) => {}
                    Err(err) => {
                        errors.push(format!(
                            "event_trigger {} filter syntax error: {}",
                            trigger_id, err
                        ));
                    }
                }
            }
        }

        // 2. Template doc.* path resolution.
        //
        // Locate the referenced Task. If it is missing the pure validator
        // already reported the broken cross-ref; skip the live probe here.
        let task_id = trig.task_id.trim();
        if task_id.is_empty() {
            continue;
        }
        let Some(task) = manifest.tasks.iter().find(|t| t.task_id.trim() == task_id) else {
            continue;
        };
        let refs = match parse_template_for_validation(&task.prompt_template) {
            Ok(refs) => refs,
            Err(_) => {
                // parse_template_for_validation failure is already
                // reported by the pure validator; don't duplicate.
                continue;
            }
        };
        let doc_paths: Vec<Vec<String>> = refs
            .into_iter()
            .filter(|v| v.root() == Some("doc"))
            .map(|v| v.path.clone())
            .collect();
        if doc_paths.is_empty() {
            continue;
        }

        // Introspect the source collection.
        let introspect = format!(
            r#"query {{ __type(name: "{name}") {{ fields {{ name }} }} }}"#,
            name = source_collection,
        );
        let response = match access.execute(&introspect).await {
            Ok(response) => response,
            Err(err) => {
                errors.push(format!(
                    "event_trigger {} introspection of source_collection {} failed: {}",
                    trigger_id, source_collection, err
                ));
                continue;
            }
        };
        // `__type(name: "Missing")` returns `{ "data": { "__type": null } }`
        // — not a GraphQL error. Detect it explicitly so we can produce a
        // friendly message instead of silently passing.
        let type_node = response.get("data").and_then(|d| d.get("__type"));
        let fields = type_node
            .filter(|v| !v.is_null())
            .and_then(|t| t.get("fields"))
            .and_then(serde_json::Value::as_array);
        let Some(fields) = fields else {
            errors.push(format!(
                "event_trigger {} references unknown source_collection {}",
                trigger_id, source_collection
            ));
            continue;
        };
        let top_level: HashSet<&str> = fields
            .iter()
            .filter_map(|f| f.get("name").and_then(|n| n.as_str()))
            .collect();
        let mut reported: BTreeSet<String> = BTreeSet::new();
        for path in &doc_paths {
            // path is ["doc", field1, field2, ...]. Skip the "doc" root;
            // a bare `{{ doc }}` (path == ["doc"]) has no sub-field to
            // verify — nothing to do.
            let Some(first) = path.get(1).map(String::as_str) else {
                continue;
            };
            if top_level.contains(first) {
                continue;
            }
            if !reported.insert(first.to_string()) {
                continue;
            }
            errors.push(format!(
                "event_trigger {} template references doc.{} but {} has no such field",
                trigger_id, first, source_collection
            ));
        }
    }

    // NOTE: subagent_targets are NOT resolved against local AgentBehavior docs.
    // A delegation target is a named (agent_did, behavior_id) pair; a target may
    // legitimately live on a remote deployment that never replicates its
    // AgentBehavior locally. Structural validation (JSON shape, non-empty
    // fields, unique names) happens in `validate_manifest`; cross-node
    // resolution is handled out-of-band via P2P at runtime.

    Ok(errors)
}

fn format_variable_ref(var: &VariableRef) -> String {
    if var.path.is_empty() {
        String::new()
    } else {
        var.path.join(".")
    }
}

fn validate_argv_prefixes(
    selection_id: &str,
    field: &str,
    prefixes: &[String],
    errors: &mut Vec<String>,
) {
    for prefix in prefixes {
        let trimmed = prefix.trim();
        if trimmed.is_empty() {
            errors.push(format!(
                "tool selection {selection_id} has an empty {field} entry"
            ));
            continue;
        }

        // Bare prefixes are split with `split_ascii_whitespace` at runtime;
        // after the non-empty trim check above, that cannot create empty
        // tokens. JSON prefixes can contain empty strings, so validate them.
        if trimmed.starts_with('[') {
            match serde_json::from_str::<Vec<String>>(trimmed) {
                Ok(tokens)
                    if !tokens.is_empty() && tokens.iter().all(|token| !token.trim().is_empty()) => {}
                Ok(_) => errors.push(format!(
                    "tool selection {selection_id} {field} JSON entry must contain non-empty argv tokens"
                )),
                Err(error) => errors.push(format!(
                    "tool selection {selection_id} {field} JSON entry is invalid: {error}"
                )),
            }
        }
    }
}

/// Validate `subagent_targets` entries. Each entry must be a JSON
/// [`SubagentTarget`] with non-empty `name`/`agent_did`/`behavior_id`, and the
/// model-facing `name` must be unique within the selection. Remote targets are
/// NOT resolved against local AgentBehavior docs (they legitimately do not
/// resolve locally and reach the owning node via P2P).
fn validate_subagent_targets(
    selection_id: &str,
    selection_agent_did: &str,
    allow_cross_deployment: bool,
    entries: &[String],
    errors: &mut Vec<String>,
) {
    let mut seen_names: HashSet<String> = HashSet::new();
    for entry in entries {
        let target = match SubagentTarget::parse(entry) {
            Ok(target) => target,
            Err(error) => {
                errors.push(format!(
                    "tool selection {selection_id} subagent_targets entry {entry:?} is not valid SubagentTarget JSON: {error}"
                ));
                continue;
            }
        };
        if !target.is_structurally_valid() {
            errors.push(format!(
                "tool selection {selection_id} subagent_targets entry {entry:?} must have non-empty name, agent_did, and behavior_id"
            ));
            continue;
        }
        if !seen_names.insert(target.name.trim().to_string()) {
            errors.push(format!(
                "tool selection {selection_id} has a duplicate subagent target name {:?}",
                target.name
            ));
        }
        // Cross-deployment (remote-DID) delegation is deferred behind an opt-in
        // flag. When the flag is false (default), reject any target whose DID
        // differs from the selection's own agent_did.
        if !allow_cross_deployment
            && !selection_agent_did.is_empty()
            && target.agent_did.trim() != selection_agent_did
        {
            errors.push(format!(
                "cross-deployment subagent delegation is deferred; remote target {} requires subagent_allow_cross_deployment=true (trusted-fleet only).",
                target.name
            ));
        }
    }
}

/// Validate `write_tools` entries pre-apply, mirroring the runtime checks in
/// `ToolSelectionDocument::validate()` so a malformed decl is rejected by
/// `config validate` instead of failing only when the runtime ingests it.
///
/// Entries are stored as JSON-encoded [`WriteToolDecl`] strings (the same
/// `[String]` storage form as `subagent_targets`). Each entry must:
///   * parse as a [`WriteToolDecl`],
///   * have a non-empty `tool_name` and `collection`,
///   * have a non-empty `name` on every field,
///   * and have a `tool_name` that is unique within the selection.
fn validate_write_tools(
    selection_id: &str,
    entries: &[String],
    cli_tool_names: &[String],
    errors: &mut Vec<String>,
) {
    // Sibling cli_tool_names are advertised as individually-named tools in the
    // same selection; a write tool reusing one is the same dispatch collision
    // as reusing a built-in name. Mirrors the runtime check in
    // `ToolSelectionDocument::validate()`.
    let cli_tool_names: HashSet<&str> = cli_tool_names.iter().map(|name| name.trim()).collect();
    let mut seen_tool_names: HashSet<String> = HashSet::new();
    for entry in entries {
        let decl: WriteToolDecl = match serde_json::from_str(entry) {
            Ok(decl) => decl,
            Err(error) => {
                errors.push(format!(
                    "tool selection {selection_id} write_tools entry {entry:?} is not valid WriteToolDecl JSON: {error}"
                ));
                continue;
            }
        };
        if decl.tool_name.trim().is_empty() {
            errors.push(format!(
                "tool selection {selection_id} write_tools entry {entry:?} must have a non-empty tool_name"
            ));
        }
        if decl.collection.trim().is_empty() {
            errors.push(format!(
                "tool selection {selection_id} write_tools tool {:?} must have a non-empty collection",
                decl.tool_name
            ));
        }
        // A declared write tool may not reuse a built-in tool name: doing so
        // silently shadows the built-in at registration. Mirrors the runtime
        // check in `ToolSelectionDocument::validate()`.
        if is_reserved_builtin_tool_name(&decl.tool_name) {
            errors.push(format!(
                "tool selection {selection_id} write_tools tool_name {:?} collides with a \
                 built-in tool; declared write tools must use a name not already provided by the \
                 native, meta, subagent, or built-in (defra_query, context_budget, sessions, \
                 memory) tool surface",
                decl.tool_name.trim()
            ));
        }
        if cli_tool_names.contains(decl.tool_name.trim()) {
            errors.push(format!(
                "tool selection {selection_id} write_tools tool_name {:?} collides with a \
                 cli_tool_names entry in the same tool selection; each tool must have a unique name",
                decl.tool_name.trim()
            ));
        }
        let mut seen_field_names: HashSet<String> = HashSet::new();
        for field in &decl.fields {
            if field.name.trim().is_empty() {
                errors.push(format!(
                    "tool selection {selection_id} write_tools tool {:?} has a field with an empty name",
                    decl.tool_name
                ));
            } else if !seen_field_names.insert(field.name.trim().to_string()) {
                errors.push(format!(
                    "tool selection {selection_id} write_tools tool {:?} has a duplicate field name {:?}",
                    decl.tool_name,
                    field.name.trim()
                ));
            }
        }
        if !decl.tool_name.trim().is_empty()
            && !seen_tool_names.insert(decl.tool_name.trim().to_string())
        {
            errors.push(format!(
                "tool selection {selection_id} has a duplicate write_tools tool_name {:?}",
                decl.tool_name.trim()
            ));
        }
    }
}

fn validate_non_empty_values(
    selection_id: &str,
    field: &str,
    values: &[String],
    errors: &mut Vec<String>,
) {
    for value in values {
        if value.trim().is_empty() {
            errors.push(format!(
                "tool selection {selection_id} has an empty {field} entry"
            ));
        }
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

#[cfg(test)]
mod tests {
    use super::super::{
        DesiredAgentBehavior, DesiredAgentPrincipal, DesiredStateManifest, DesiredTask,
    };
    use super::*;

    fn manifest(system_prompt: Option<&str>, task_prompt: Option<&str>) -> DesiredStateManifest {
        DesiredStateManifest {
            agent_principal: DesiredAgentPrincipal {
                agent_did: "did:key:test-template-validation".to_string(),
                display_name: None,
                default_behavior_id: Some("default".to_string()),
                enabled: true,
            },
            agent_behaviors: vec![DesiredAgentBehavior {
                behavior_id: "default".to_string(),
                agent_did: "did:key:test-template-validation".to_string(),
                display_name: None,
                description: None,
                summary: None,
                system_prompt: system_prompt.map(str::to_string),
                request_context_template: None,
                backend_id: None,
                model_name: None,
                tool_selection_id: None,
                inference_profile_id: None,
                compaction_strategy: None,
                compaction_threshold: None,
                enabled: true,
                skill_refs: Vec::new(),
                skill_excludes: Vec::new(),
            }],
            skills: Vec::new(),
            tool_selections: Vec::new(),
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            projection_acp_bindings: Vec::new(),
            tasks: task_prompt
                .map(|prompt| {
                    vec![DesiredTask {
                        task_id: "task".to_string(),
                        name: "Task".to_string(),
                        description: None,
                        behavior_id: "default".to_string(),
                        prompt_template: prompt.to_string(),
                        enabled: true,
                        output_schema_ref: None,
                    }]
                })
                .unwrap_or_default(),
            schedules: Vec::new(),
            event_triggers: Vec::new(),
        }
    }

    fn validate_errors(manifest: DesiredStateManifest) -> Vec<String> {
        let mut errors = Vec::new();
        validate_manifest(&manifest, &mut errors);
        errors
    }

    #[test]
    fn system_prompt_rejects_per_request_ref() {
        let errors = validate_errors(manifest(Some("now {{ ctx.now }}"), None));

        assert!(
            errors
                .iter()
                .any(|error| error.contains("per-request variable `ctx.now`")),
            "expected ctx.now rejection, got {errors:?}"
        );
    }

    #[test]
    fn system_prompt_accepts_literal_raw_and_node_refs() {
        for prompt in [
            "literal text with no MiniJinja markers",
            "{% raw %}{{ ctx.now }}{% endraw %}",
            "node {{ node.node_did }} / {{ node.behavior_id }}",
        ] {
            let errors = validate_errors(manifest(Some(prompt), None));
            assert!(errors.is_empty(), "prompt {prompt:?} failed: {errors:?}");
        }
    }

    #[test]
    fn task_prompt_accepts_task_catalog_refs() {
        let errors = validate_errors(manifest(
            None,
            Some("run {{ node.node_did }} {{ node.behavior_id }} {{ ctx.now }}"),
        ));

        assert!(errors.is_empty(), "task refs should pass: {errors:?}");
    }

    #[test]
    fn task_prompt_rejects_request_context_only_ref() {
        let errors = validate_errors(manifest(None, Some("state {{ ctx.collection_summary }}")));

        assert!(
            errors.iter().any(|error| error.contains(
                "unavailable template variable ctx.collection_summary"
            )),
            "expected ctx.collection_summary rejection, got {errors:?}"
        );
    }
}

#[cfg(test)]
mod live_tests {
    use anyhow::Result;
    use defra_agent::defra_node::{EmbeddedNode, StorageBackend};
    use defra_agent::ensure_runtime_schemas;

    use super::*;
    use crate::config_writes::ConfigAccess;

    /// Build a minimal `DesiredStateManifest` whose only content is a single
    /// tool selection with the given `subagent_targets` list and
    /// `subagent_spawn_enabled` flag.  All other collections are empty (the
    /// live validator only iterates `event_triggers` and `tool_selections`).
    fn manifest_with_subagent_targets(targets: Vec<SubagentTarget>) -> DesiredStateManifest {
        use super::super::{DesiredAgentPrincipal, DesiredStateManifest, DesiredToolSelection};
        let targets: Vec<String> = targets.iter().map(SubagentTarget::to_entry).collect();
        DesiredStateManifest {
            agent_principal: DesiredAgentPrincipal {
                agent_did: "did:key:test-live-validate".to_string(),
                display_name: None,
                default_behavior_id: None,
                enabled: true,
            },
            agent_behaviors: Vec::new(),
            skills: Vec::new(),
            tool_selections: vec![DesiredToolSelection {
                selection_id: "live-test-sel".to_string(),
                agent_did: "did:key:test-live-validate".to_string(),
                display_name: None,
                enable_file_tools: false,
                file_tools_mode: "ReadOnly".to_string(),
                file_tool_root: None,
                enable_bash: false,
                bash_mode: "ReadOnly".to_string(),
                command_execution_policy: None,
                command_allowed_argv_prefixes: Vec::new(),
                command_forbidden_argv_prefixes: Vec::new(),
                command_network_mode: None,
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                allowed_mcp_service_ids: Vec::new(),
                delegate_to: Vec::new(),
                backgroundable_tool_names: Vec::new(),
                enable_memory: false,
                enable_session_history_tool: false,
                enable_defra_query: true,
                defra_query_collections: Vec::new(),
                subagent_targets: targets,
                subagent_spawn_enabled: true,
                subagent_steering_enabled: false,
                subagent_background_enabled: false,
                subagent_default_await_mode: None,
                subagent_allow_cross_deployment: false,
                cross_deployment_spawn_timeout_seconds: None,
                write_tools: Vec::new(),
            }],
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            projection_acp_bindings: Vec::new(),
            tasks: Vec::new(),
            schedules: Vec::new(),
            event_triggers: Vec::new(),
        }
    }

    /// Live-validator does NOT resolve subagent targets against local
    /// AgentBehavior docs: a target whose behavior lives on a remote deployment
    /// (and never replicates locally) must not produce a live-validation error.
    /// This is the post-#377 contract that removed the cross-node resolution
    /// seam.
    #[tokio::test]
    async fn live_validate_does_not_resolve_remote_subagent_target() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;

        // No local AgentBehavior is seeded; the target names a remote agent_did.
        let access = ConfigAccess::Local(node);

        let manifest = manifest_with_subagent_targets(vec![SubagentTarget {
            name: "remote-researcher".to_string(),
            agent_did: "did:key:zRemotePeer".to_string(),
            behavior_id: "does-not-exist-locally".to_string(),
            description: None,
        }]);
        let errors = validate_manifest_against_live(&manifest, &access).await?;

        assert!(
            !errors
                .iter()
                .any(|msg| msg.contains("does-not-exist-locally") || msg.contains("live-test-sel")),
            "remote subagent target must not trigger live resolution errors, got {errors:?}"
        );
        Ok(())
    }

    /// Live-validator does NOT report an error for a local subagent target
    /// either: target resolution is no longer a live-validation concern.
    #[tokio::test]
    async fn live_validate_passes_for_known_subagent_target() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;

        let access = ConfigAccess::Local(node);

        let manifest = manifest_with_subagent_targets(vec![SubagentTarget {
            name: "researcher".to_string(),
            agent_did: "did:key:test-live-validate".to_string(),
            behavior_id: "amy-research".to_string(),
            description: None,
        }]);
        let errors = validate_manifest_against_live(&manifest, &access).await?;

        assert!(
            !errors
                .iter()
                .any(|msg| msg.contains("amy-research") || msg.contains("live-test-sel")),
            "expected no subagent errors for known target, got {errors:?}"
        );
        Ok(())
    }

    /// Apply a tool selection with all subagent fields set, then:
    ///   (a) read the ToolSelection back and assert all persisted, and
    ///   (b) recompute the apply diff and assert it shows UNCHANGED (idempotent).
    ///
    /// Before the fix this test fails because:
    ///  - `DesiredToolSelection` was missing the three new fields → manifest
    ///    deserialization with `deny_unknown_fields` would reject them.
    ///  - `EXPORT_TOOL_SELECTION_FIELDS` omitted subagent fields → live read
    ///    always saw them as `None` → diff never converged.
    #[tokio::test]
    async fn all_subagent_fields_persist_and_apply_is_idempotent() -> Result<()> {
        use std::path::PathBuf;

        use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
        use crate::config_import::{apply_desired_state_changes, diff_has_pending_apply};
        use crate::desired_state::{diff_manifests, export_bundle_from_manifest};

        let tempdir = tempfile::tempdir()?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;

        let access = ConfigAccess::Local(node);

        // Seed a minimal AgentPrincipal so build_desired_state_live_bundle can
        // find the agent on the second (post-apply) read.
        {
            use defra_agent::graphql::escape_graphql_string;
            let did = escape_graphql_string("did:key:test-subagent-idempotency");
            access
                .execute(&format!(
                    r#"mutation {{ create_AgentPrincipal(input: {{ agent_did: "{did}", enabled: true }}) {{ _docID }} }}"#
                ))
                .await?;
        }

        // Build the desired manifest with all subagent fields set.
        let desired_manifest = {
            use super::super::{DesiredAgentPrincipal, DesiredStateManifest, DesiredToolSelection};
            DesiredStateManifest {
                agent_principal: DesiredAgentPrincipal {
                    agent_did: "did:key:test-subagent-idempotency".to_string(),
                    display_name: None,
                    default_behavior_id: None,
                    enabled: true,
                },
                agent_behaviors: Vec::new(),
                skills: Vec::new(),
                tool_selections: vec![DesiredToolSelection {
                    selection_id: "subagent-idempotency-sel".to_string(),
                    agent_did: "did:key:test-subagent-idempotency".to_string(),
                    display_name: None,
                    enable_file_tools: false,
                    file_tools_mode: "ReadOnly".to_string(),
                    file_tool_root: None,
                    enable_bash: false,
                    bash_mode: "ReadOnly".to_string(),
                    command_execution_policy: None,
                    command_allowed_argv_prefixes: Vec::new(),
                    command_forbidden_argv_prefixes: Vec::new(),
                    command_network_mode: None,
                    cli_tool_names: Vec::new(),
                    enable_meta_tools: false,
                    allowed_mcp_service_ids: Vec::new(),
                    delegate_to: Vec::new(),
                    backgroundable_tool_names: Vec::new(),
                    enable_memory: false,
                    enable_session_history_tool: false,
                    enable_defra_query: true,
                    defra_query_collections: Vec::new(),
                    subagent_targets: vec![SubagentTarget {
                        name: "researcher".to_string(),
                        agent_did: "did:key:test-subagent-idempotency".to_string(),
                        behavior_id: "amy-research".to_string(),
                        description: None,
                    }
                    .to_entry()],
                    subagent_spawn_enabled: true,
                    subagent_steering_enabled: true,
                    subagent_background_enabled: true,
                    subagent_default_await_mode: Some("background".to_string()),
                    subagent_allow_cross_deployment: true,
                    cross_deployment_spawn_timeout_seconds: Some(90),
                    write_tools: Vec::new(),
                }],
                inference_backends: Vec::new(),
                inference_profiles: Vec::new(),
                tool_service_registries: Vec::new(),
                projection_acp_bindings: Vec::new(),
                tasks: Vec::new(),
                schedules: Vec::new(),
                event_triggers: Vec::new(),
            }
        };

        let root = PathBuf::from(".");
        let desired_bundle = export_bundle_from_manifest(&desired_manifest, "local")?;

        // ── First apply ──────────────────────────────────────────────────────
        let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
        let (live_principal, live_manifest) =
            live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
        let planned = diff_manifests(
            &root,
            "local",
            &desired_manifest,
            live_principal.as_ref(),
            &live_manifest,
            false,
        );

        let txn = access.begin_apply_txn().await?;
        apply_desired_state_changes(&txn, &desired_bundle, &planned).await?;
        txn.commit().await?;

        // ── (a) Read back and assert all subagent fields persisted ────────────
        let remaining_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
        let (remaining_principal, remaining_manifest) =
            live_manifest_from_bundle(&desired_manifest, &remaining_bundle)?;

        let live_sel = remaining_manifest
            .tool_selections
            .iter()
            .find(|s| s.selection_id == "subagent-idempotency-sel")
            .expect("ToolSelection should exist after apply");

        assert_eq!(
            live_sel.subagent_targets,
            vec![SubagentTarget {
                name: "researcher".to_string(),
                agent_did: "did:key:test-subagent-idempotency".to_string(),
                behavior_id: "amy-research".to_string(),
                description: None,
            }
            .to_entry()],
            "subagent_targets must persist through apply"
        );
        assert_eq!(
            live_sel.subagent_spawn_enabled, true,
            "subagent_spawn_enabled must persist through apply"
        );
        assert_eq!(
            live_sel.subagent_steering_enabled, true,
            "subagent_steering_enabled must persist through apply"
        );
        assert_eq!(
            live_sel.subagent_background_enabled, true,
            "subagent_background_enabled must persist through apply"
        );
        assert_eq!(
            live_sel.subagent_default_await_mode.as_deref(),
            Some("background"),
            "subagent_default_await_mode must persist through apply"
        );
        assert_eq!(
            live_sel.subagent_allow_cross_deployment, true,
            "subagent_allow_cross_deployment must persist through apply"
        );
        assert_eq!(
            live_sel.cross_deployment_spawn_timeout_seconds,
            Some(90),
            "cross_deployment_spawn_timeout_seconds must persist through apply"
        );

        // ── (b) Re-diff: tool selection must show as UNCHANGED ────────────────
        let second_diff = diff_manifests(
            &root,
            "local",
            &desired_manifest,
            remaining_principal.as_ref(),
            &remaining_manifest,
            false,
        );

        assert!(
            !diff_has_pending_apply(&second_diff.counts),
            "second diff must have no pending apply (idempotent); got: {:?}",
            second_diff.counts
        );
        assert!(
            second_diff
                .collections
                .tool_selections
                .unchanged
                .contains(&"subagent-idempotency-sel".to_string()),
            "tool selection must be in the 'unchanged' set after re-apply; got: {:?}",
            second_diff.collections.tool_selections
        );

        Ok(())
    }

    /// Apply a manifest with an `AgentBehavior` that has `description` and
    /// `summary` set, then:
    ///   (a) read the `AgentBehavior` back and assert both fields persisted, and
    ///   (b) recompute the apply diff and assert the behavior is UNCHANGED
    ///       (idempotent).
    ///
    /// Before the fix this test fails because the fields were absent from:
    ///  - `DesiredAgentBehavior` → manifest deserialization would lose them.
    ///  - `EXPORT_AGENT_BEHAVIOR_FIELDS` → live read always saw `None` → diff
    ///    never converged.
    ///  - the `convert.rs` whitelist → fields stripped during export→manifest.
    #[tokio::test]
    async fn behavior_description_and_summary_persist_and_apply_is_idempotent() -> Result<()> {
        use std::path::PathBuf;

        use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
        use crate::config_import::{apply_desired_state_changes, diff_has_pending_apply};
        use crate::desired_state::{diff_manifests, export_bundle_from_manifest};

        let tempdir = tempfile::tempdir()?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;

        let access = ConfigAccess::Local(node);

        // Seed a minimal AgentPrincipal so build_desired_state_live_bundle can
        // find the agent on the second (post-apply) read.
        {
            use defra_agent::graphql::escape_graphql_string;
            let did = escape_graphql_string("did:key:test-behavior-desc-idempotency");
            access
                .execute(&format!(
                    r#"mutation {{ create_AgentPrincipal(input: {{ agent_did: "{did}", enabled: true }}) {{ _docID }} }}"#
                ))
                .await?;
        }

        // Build the desired manifest with description and summary set.
        let desired_manifest = {
            use super::super::{DesiredAgentBehavior, DesiredAgentPrincipal, DesiredStateManifest};
            DesiredStateManifest {
                agent_principal: DesiredAgentPrincipal {
                    agent_did: "did:key:test-behavior-desc-idempotency".to_string(),
                    display_name: None,
                    default_behavior_id: None,
                    enabled: true,
                },
                agent_behaviors: vec![DesiredAgentBehavior {
                    behavior_id: "desc-idempotency-behavior".to_string(),
                    agent_did: "did:key:test-behavior-desc-idempotency".to_string(),
                    display_name: Some("Research Assistant".to_string()),
                    description: Some(
                        "A general-purpose assistant for research and writing tasks.".to_string(),
                    ),
                    summary: Some("Research assistant".to_string()),
                    system_prompt: None,
                    request_context_template: None,
                    backend_id: None,
                    model_name: None,
                    tool_selection_id: None,
                    inference_profile_id: None,
                    compaction_strategy: None,
                    compaction_threshold: None,
                    enabled: true,
                    skill_refs: Vec::new(),
                    skill_excludes: Vec::new(),
                }],
                skills: Vec::new(),
                tool_selections: Vec::new(),
                inference_backends: Vec::new(),
                inference_profiles: Vec::new(),
                tool_service_registries: Vec::new(),
                projection_acp_bindings: Vec::new(),
                tasks: Vec::new(),
                schedules: Vec::new(),
                event_triggers: Vec::new(),
            }
        };

        let root = PathBuf::from(".");
        let desired_bundle = export_bundle_from_manifest(&desired_manifest, "local")?;

        // ── First apply ──────────────────────────────────────────────────────
        let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
        let (live_principal, live_manifest) =
            live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
        let planned = diff_manifests(
            &root,
            "local",
            &desired_manifest,
            live_principal.as_ref(),
            &live_manifest,
            false,
        );

        let txn = access.begin_apply_txn().await?;
        apply_desired_state_changes(&txn, &desired_bundle, &planned).await?;
        txn.commit().await?;

        // ── (a) Read back and assert description + summary persisted ─────────
        let remaining_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
        let (remaining_principal, remaining_manifest) =
            live_manifest_from_bundle(&desired_manifest, &remaining_bundle)?;

        let live_behavior = remaining_manifest
            .agent_behaviors
            .iter()
            .find(|b| b.behavior_id == "desc-idempotency-behavior")
            .expect("AgentBehavior should exist after apply");

        assert_eq!(
            live_behavior.description,
            Some("A general-purpose assistant for research and writing tasks.".to_string()),
            "description must persist through apply"
        );
        assert_eq!(
            live_behavior.summary,
            Some("Research assistant".to_string()),
            "summary must persist through apply"
        );

        // ── (b) Re-diff: behavior must show as UNCHANGED ─────────────────────
        let second_diff = diff_manifests(
            &root,
            "local",
            &desired_manifest,
            remaining_principal.as_ref(),
            &remaining_manifest,
            false,
        );

        assert!(
            !diff_has_pending_apply(&second_diff.counts),
            "second diff must have no pending apply (idempotent); got: {:?}",
            second_diff.counts
        );
        assert!(
            second_diff
                .collections
                .agent_behaviors
                .unchanged
                .contains(&"desc-idempotency-behavior".to_string()),
            "behavior must be in the 'unchanged' set after re-apply; got: {:?}",
            second_diff.collections.agent_behaviors
        );

        Ok(())
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
