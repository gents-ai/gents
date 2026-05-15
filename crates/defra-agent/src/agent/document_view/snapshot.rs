use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::admission::backend_admission_configs_from_backends;
use crate::config::AgentBehavior;
use crate::document_config::{
    default_behavior_id_for_agent, AgentBehavior as AgentBehaviorDocument,
};
use crate::runtime_snapshot::{
    ConcurrencyMode, ResolvedEventTrigger, ResolvedRuntimeSnapshot, ResolvedSchedule, ResolvedTask,
};
use crate::tool_surface::ToolSelection;

use super::{validate_subagent_targets_resolve, DocumentRuntimeView};

use crate::agent::{
    behavior_config_from_documents, subagent_tool_config_from_document,
    tool_selection_from_document, DocumentResolveContext,
};
use crate::tool_surface::SubagentToolConfig;

pub(crate) async fn resolve_document_runtime_snapshot_from_view(
    node: &EmbeddedNode,
    context: &DocumentResolveContext,
    view: &DocumentRuntimeView,
) -> Result<ResolvedRuntimeSnapshot> {
    if !view.principal.value.enabled {
        anyhow::bail!(
            "agent principal {} is disabled",
            view.principal.value.agent_did
        );
    }

    let default_behavior_id = view
        .principal
        .value
        .default_behavior_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_behavior_id_for_agent(context.identity.did()));

    let mut behaviors = Vec::<Arc<AgentBehavior>>::new();
    let mut unavailable_behaviors = HashMap::new();

    // TODO(Task 7): construct a single Arc<AgentPrincipal> above the loop and
    // clone into each behavior_config_from_documents call so all behaviors
    // share the snapshot's principal Arc per the single-principal invariant.
    for behavior_record in view.behaviors.values() {
        let behavior = &behavior_record.value;
        if !behavior.enabled {
            unavailable_behaviors.insert(
                behavior.behavior_id.clone(),
                format!("behavior {} is disabled", behavior.behavior_id),
            );
            continue;
        }

        let backend = match behavior
            .backend_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Some(backend_id) => view
                .backends
                .get(backend_id)
                .map(|record| &record.value)
                .ok_or_else(|| {
                    anyhow!(
                        "behavior {} references missing backend {}",
                        behavior.behavior_id,
                        backend_id
                    )
                }),
            None => Err(anyhow!(
                "behavior {} has no backend binding",
                behavior.behavior_id
            )),
        };

        let resolved = async {
            let backend = backend?;
            if !backend.is_available() {
                anyhow::bail!(
                    "behavior {} backend {} is unavailable (enabled={} probe_status={})",
                    behavior.behavior_id,
                    backend.backend_id,
                    backend.enabled,
                    backend.probe_status
                );
            }
            let profile_id = behavior
                .inference_profile_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "behavior {} has no inference profile binding",
                        behavior.behavior_id
                    )
                })?;
            let inference_profile = view
                .inference_profiles
                .get(profile_id)
                .map(|record| &record.value)
                .ok_or_else(|| {
                    anyhow!(
                        "behavior {} references missing inference profile {}",
                        behavior.behavior_id,
                        profile_id
                    )
                })?;
            let (tool_selection, subagent_tools) = match behavior
                .tool_selection_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                Some(selection_id) => match view.tool_selections.get(selection_id) {
                    Some(record) => {
                        record.value.validate()?;
                        validate_subagent_targets_resolve(&record.value, view)?;
                        (
                            tool_selection_from_document(&record.value)?,
                            subagent_tool_config_from_document(&record.value),
                        )
                    }
                    None => anyhow::bail!(
                        "behavior {} references missing tool selection {}",
                        behavior.behavior_id,
                        selection_id
                    ),
                },
                None => (ToolSelection::default(), SubagentToolConfig::default()),
            };

            let behavior_config = behavior_config_from_documents(
                context.identity.clone(),
                behavior,
                backend,
                inference_profile,
                tool_selection,
                subagent_tools,
                &context.tool_ceiling,
            )?;
            Ok::<_, anyhow::Error>(Arc::new(behavior_config))
        }
        .await;

        match resolved {
            Ok(behavior_config) => {
                behaviors.push(behavior_config);
            }
            Err(error) => {
                unavailable_behaviors.insert(behavior.behavior_id.clone(), error.to_string());
            }
        }
    }

    let candidate_behavior_ids = behaviors
        .iter()
        .map(|behavior| behavior.name.clone())
        .collect::<HashSet<_>>();
    let mut behavior_surfaces = Vec::with_capacity(behaviors.len());
    for behavior in behaviors {
        match behavior
            .tools
            .resolve_with_available_subagent_targets(node, &candidate_behavior_ids)
            .await
        {
            Ok(tool_surface) => behavior_surfaces.push((behavior, tool_surface)),
            Err(error) => {
                unavailable_behaviors.insert(behavior.name.clone(), error.to_string());
            }
        }
    }

    let active_behavior_ids = behavior_surfaces
        .iter()
        .map(|(behavior, _)| behavior.name.clone())
        .collect::<HashSet<_>>();
    let mut behaviors = Vec::with_capacity(behavior_surfaces.len());
    let mut tool_surfaces = HashMap::with_capacity(behavior_surfaces.len());
    for (behavior, mut tool_surface) in behavior_surfaces {
        tool_surface.retain_subagent_targets(&active_behavior_ids);
        tool_surfaces.insert(behavior.name.clone(), Arc::new(tool_surface));
        behaviors.push(behavior);
    }

    let backend_admission_configs = backend_admission_configs_from_backends(
        view.backends.values().map(|record| &record.value),
    )?;

    let (active_schedules, unavailable_schedules) = resolve_schedules(view, &unavailable_behaviors);
    let (active_event_triggers, unavailable_event_triggers) =
        resolve_event_triggers(view, &unavailable_behaviors);
    let active_tasks = resolve_tasks(view, &unavailable_behaviors);
    let paired_peer_dids = load_paired_peer_dids(node).await?;

    Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_behavior_id,
        behaviors,
        tool_surfaces,
        backend_admission_configs,
        unavailable_behaviors,
    )
    .with_local_did(context.identity.did().to_string())
    .with_paired_peer_dids(paired_peer_dids)
    .with_schedules(active_schedules, unavailable_schedules)
    .with_event_triggers(active_event_triggers, unavailable_event_triggers)
    .with_tasks(active_tasks))
}

#[derive(Debug, Deserialize)]
struct PeerPairingDesiredDidRow {
    peer_id: String,
    agent_did: Option<String>,
}

async fn load_paired_peer_dids(node: &EmbeddedNode) -> Result<HashSet<String>> {
    let query = r#"{
        PeerPairingDesired {
            peer_id
            agent_did
        }
    }"#;
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query PeerPairingDesired for paired peer DIDs failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<PeerPairingDesiredDidRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("PeerPairingDesired"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.agent_did
                .as_deref()
                .map(str::trim)
                .filter(|did| !did.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    let peer_id = row.peer_id.trim();
                    peer_id.starts_with("did:").then(|| peer_id.to_string())
                })
        })
        .collect())
}

/// Classify every `Task` in `view` into `active_tasks` if it's enabled and
/// bound to an available behavior. Unlike `resolve_schedules` /
/// `resolve_event_triggers`, we don't keep an `unavailable_tasks` set here —
/// tasks without a trigger aren't exposed to any runtime consumer except
/// `ManualTriggerHandle::run_task_now`, which reports unavailability at the
/// call site via a "not in the active snapshot" error.
fn resolve_tasks(
    view: &DocumentRuntimeView,
    unavailable_behaviors: &HashMap<String, String>,
) -> HashMap<String, ResolvedTask> {
    let mut active_tasks = HashMap::new();

    for (task_id, task_record) in &view.tasks {
        let task = &task_record.value;

        if !task.enabled {
            continue;
        }

        let behavior_id = match task.behavior_id.as_deref().and_then(non_empty) {
            Some(id) => id,
            None => continue,
        };

        let behavior_record = match view.behaviors.get(behavior_id) {
            Some(record) => record,
            None => continue,
        };
        if !behavior_record.value.enabled {
            continue;
        }
        if unavailable_behaviors.contains_key(behavior_id) {
            continue;
        }

        let resolved_task = ResolvedTask {
            task_id: task.task_id.clone(),
            name: task.name.clone(),
            behavior_id: behavior_id.to_string(),
            prompt_template: task.prompt_template.clone().unwrap_or_default(),
            output_schema_ref: task.output_schema_ref.clone(),
        };
        active_tasks.insert(task_id.clone(), resolved_task);
    }

    active_tasks
}

/// Classify every `Schedule` in `view` into either `active_schedules`
/// (resolvable with its task and behavior) or `unavailable_schedules`
/// (anything that fails one of the resolution gates). Mirrors the
/// behavior-resolution pattern above: we never fail the whole snapshot for a
/// single unresolvable schedule; we mark it unavailable instead.
fn resolve_schedules(
    view: &DocumentRuntimeView,
    unavailable_behaviors: &HashMap<String, String>,
) -> (HashMap<String, ResolvedSchedule>, HashSet<String>) {
    let mut active_schedules = HashMap::new();
    let mut unavailable_schedules = HashSet::new();

    for schedule_record in view.schedules.values() {
        let schedule = &schedule_record.value;
        let schedule_id = schedule.schedule_id.clone();

        let concurrency =
            match ConcurrencyMode::parse(schedule.concurrency.as_deref().unwrap_or("")) {
                Some(mode) => mode,
                None => {
                    unavailable_schedules.insert(schedule_id);
                    continue;
                }
            };

        if !schedule.enabled {
            unavailable_schedules.insert(schedule_id);
            continue;
        }

        let task_id = schedule.task_id.as_deref().unwrap_or("");
        let task_record = match view.tasks.get(task_id) {
            Some(record) => record,
            None => {
                unavailable_schedules.insert(schedule_id);
                continue;
            }
        };
        let task = &task_record.value;

        if !task.enabled {
            unavailable_schedules.insert(schedule_id);
            continue;
        }

        let behavior_id = task.behavior_id.as_deref().unwrap_or("");
        let behavior_record = match view.behaviors.get(behavior_id) {
            Some(record) => record,
            None => {
                unavailable_schedules.insert(schedule_id);
                continue;
            }
        };
        if !behavior_record.value.enabled {
            unavailable_schedules.insert(schedule_id);
            continue;
        }
        if unavailable_behaviors.contains_key(behavior_id) {
            unavailable_schedules.insert(schedule_id);
            continue;
        }

        let resolved_task = ResolvedTask {
            task_id: task.task_id.clone(),
            name: task.name.clone(),
            behavior_id: task.behavior_id.clone().unwrap_or_default(),
            prompt_template: task.prompt_template.clone().unwrap_or_default(),
            output_schema_ref: task.output_schema_ref.clone(),
        };
        let resolved_schedule = ResolvedSchedule {
            schedule_id: schedule.schedule_id.clone(),
            task_id: schedule.task_id.clone().unwrap_or_default(),
            task: resolved_task,
            interval_secs: schedule.interval_secs.unwrap_or(0),
            enabled: schedule.enabled,
            concurrency,
        };
        active_schedules.insert(resolved_schedule.schedule_id.clone(), resolved_schedule);
    }

    (active_schedules, unavailable_schedules)
}

/// Classify every `EventTrigger` in `view` into either `active_event_triggers`
/// (resolvable with its task and behavior) or `unavailable_event_triggers`
/// (anything that fails one of the resolution gates). Mirrors
/// `resolve_schedules`: we never fail the whole snapshot for a single
/// unresolvable trigger; we mark it unavailable instead.
fn resolve_event_triggers(
    view: &DocumentRuntimeView,
    unavailable_behaviors: &HashMap<String, String>,
) -> (HashMap<String, ResolvedEventTrigger>, HashSet<String>) {
    let mut active_event_triggers = HashMap::new();
    let mut unavailable_event_triggers = HashSet::new();

    for trigger_record in view.event_triggers.values() {
        let trigger = &trigger_record.value;
        let trigger_id = trigger.trigger_id.clone();

        let concurrency = match ConcurrencyMode::parse(trigger.concurrency.as_deref().unwrap_or(""))
        {
            Some(mode) => mode,
            None => {
                unavailable_event_triggers.insert(trigger_id);
                continue;
            }
        };

        if trigger.enabled != Some(true) {
            unavailable_event_triggers.insert(trigger_id);
            continue;
        }

        let task_id = trigger.task_id.as_deref().unwrap_or("");
        let task_record = match view.tasks.get(task_id) {
            Some(record) => record,
            None => {
                unavailable_event_triggers.insert(trigger_id);
                continue;
            }
        };
        let task = &task_record.value;

        if !task.enabled {
            unavailable_event_triggers.insert(trigger_id);
            continue;
        }

        let behavior_id = task.behavior_id.as_deref().unwrap_or("");
        let behavior_record = match view.behaviors.get(behavior_id) {
            Some(record) => record,
            None => {
                unavailable_event_triggers.insert(trigger_id);
                continue;
            }
        };
        if !behavior_record.value.enabled {
            unavailable_event_triggers.insert(trigger_id);
            continue;
        }
        if unavailable_behaviors.contains_key(behavior_id) {
            unavailable_event_triggers.insert(trigger_id);
            continue;
        }

        let source_collection = trigger.source_collection.clone().unwrap_or_default();
        let event_kind = trigger.event_kind.clone().unwrap_or_default();
        if source_collection.is_empty() || event_kind.is_empty() {
            unavailable_event_triggers.insert(trigger_id);
            continue;
        }

        let resolved_task = ResolvedTask {
            task_id: task.task_id.clone(),
            name: task.name.clone(),
            behavior_id: task.behavior_id.clone().unwrap_or_default(),
            prompt_template: task.prompt_template.clone().unwrap_or_default(),
            output_schema_ref: task.output_schema_ref.clone(),
        };
        let resolved_trigger = ResolvedEventTrigger {
            trigger_id: trigger.trigger_id.clone(),
            task_id: trigger.task_id.clone().unwrap_or_default(),
            task: resolved_task,
            source_collection,
            event_kind,
            filter: trigger.filter.clone(),
            enabled: true,
            concurrency,
        };
        active_event_triggers.insert(resolved_trigger.trigger_id.clone(), resolved_trigger);
    }

    (active_event_triggers, unavailable_event_triggers)
}

pub(super) fn collect_unresolved_behavior_references(
    view: &DocumentRuntimeView,
    behavior: &AgentBehaviorDocument,
    details: &mut Vec<String>,
) {
    if let Some(selection_id) = behavior.tool_selection_id.as_deref().and_then(non_empty) {
        if !view.tool_selections.contains_key(selection_id) {
            details.push(format!(
                "behavior {} references missing tool selection {}",
                behavior.behavior_id, selection_id
            ));
        }
    }

    if let Some(profile_id) = behavior.inference_profile_id.as_deref().and_then(non_empty) {
        if !view.inference_profiles.contains_key(profile_id) {
            details.push(format!(
                "behavior {} references missing inference profile {}",
                behavior.behavior_id, profile_id
            ));
        }
    }

    if let Some(backend_id) = behavior.backend_id.as_deref().and_then(non_empty) {
        if !view.backends.contains_key(backend_id) {
            details.push(format!(
                "behavior {} references missing backend {}",
                behavior.behavior_id, backend_id
            ));
        }
    }
}

pub(super) fn behavior_references_ready(
    view: &DocumentRuntimeView,
    behavior: &AgentBehaviorDocument,
) -> bool {
    let mut details = Vec::new();
    collect_unresolved_behavior_references(view, behavior, &mut details);
    details.is_empty()
}

pub(super) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
