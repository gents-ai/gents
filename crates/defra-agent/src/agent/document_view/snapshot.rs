use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

use crate::admission::backend_admission_configs_from_backends;
use crate::config::BehaviorConfig;
use crate::document_config::{default_behavior_id_for_agent, AgentBehavior};
use crate::runtime_snapshot::{
    ConcurrencyMode, ResolvedEventTrigger, ResolvedRuntimeSnapshot, ResolvedSchedule, ResolvedTask,
};
use crate::tool_surface::ToolSelection;

use super::DocumentRuntimeView;

use crate::agent::{
    behavior_config_from_documents, tool_selection_from_document, DocumentResolveContext,
};

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

    let mut behaviors = Vec::<Arc<BehaviorConfig>>::new();
    let mut tool_surfaces = HashMap::new();
    let mut unavailable_behaviors = HashMap::new();

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
            let inference_profile = behavior
                .inference_profile_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|profile_id| {
                    view.inference_profiles
                        .get(profile_id)
                        .map(|record| &record.value)
                        .ok_or_else(|| {
                            anyhow!(
                                "behavior {} references missing inference profile {}",
                                behavior.behavior_id,
                                profile_id
                            )
                        })
                })
                .transpose()?;
            let tool_selection = match behavior
                .tool_selection_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                Some(selection_id) => match view.tool_selections.get(selection_id) {
                    Some(record) => tool_selection_from_document(&record.value)?,
                    None => anyhow::bail!(
                        "behavior {} references missing tool selection {}",
                        behavior.behavior_id,
                        selection_id
                    ),
                },
                None => ToolSelection::default(),
            };

            let behavior_config = behavior_config_from_documents(
                context.identity.clone(),
                behavior,
                backend,
                inference_profile,
                tool_selection,
                &context.tool_ceiling,
            )?;
            let behavior = Arc::new(behavior_config);
            let tool_surface = Arc::new(behavior.tools.resolve(node).await?);
            Ok::<_, anyhow::Error>((behavior, tool_surface))
        }
        .await;

        match resolved {
            Ok((behavior_config, tool_surface)) => {
                tool_surfaces.insert(behavior_config.name.clone(), tool_surface);
                behaviors.push(behavior_config);
            }
            Err(error) => {
                unavailable_behaviors.insert(behavior.behavior_id.clone(), error.to_string());
            }
        }
    }

    let backend_admission_configs = backend_admission_configs_from_backends(
        view.backends.values().map(|record| &record.value),
    )?;

    let (active_schedules, unavailable_schedules) = resolve_schedules(view, &unavailable_behaviors);
    let (active_event_triggers, unavailable_event_triggers) =
        resolve_event_triggers(view, &unavailable_behaviors);

    Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_behavior_id,
        behaviors,
        tool_surfaces,
        backend_admission_configs,
        unavailable_behaviors,
    )
    .with_schedules(active_schedules, unavailable_schedules)
    .with_event_triggers(active_event_triggers, unavailable_event_triggers))
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

        let concurrency = match ConcurrencyMode::parse(schedule.concurrency.as_deref().unwrap_or(""))
        {
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

        let concurrency =
            match ConcurrencyMode::parse(trigger.concurrency.as_deref().unwrap_or("")) {
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
    behavior: &AgentBehavior,
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
    behavior: &AgentBehavior,
) -> bool {
    let mut details = Vec::new();
    collect_unresolved_behavior_references(view, behavior, &mut details);
    details.is_empty()
}

pub(super) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
