use crate::client::ClientStore;
use crate::state::OperatorSection;

use super::super::recent_failures::recent_failure_summaries;
use super::super::request_timeline::request_timeline_summaries;
use super::super::shared::{bool_word, scheduled_task_is_due, scheduled_task_next_run_label};
use super::super::EntitySummary;

pub(super) fn entity_summaries(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
) -> Vec<EntitySummary> {
    match section {
        OperatorSection::Behaviors => {
            let mut rows: Vec<_> = store
                .behaviors
                .iter()
                .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                .collect();
            rows.sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.behavior_id.clone(),
                    title: row
                        .display_name
                        .clone()
                        .unwrap_or_else(|| row.behavior_id.clone()),
                    meta: format!(
                        "{}  model {}  backend {}  tasks {}  convos {}",
                        if row.enabled == Some(false) {
                            "disabled"
                        } else {
                            "enabled"
                        },
                        row.model_name.as_deref().unwrap_or("unbound"),
                        row.backend_id.as_deref().unwrap_or("unbound"),
                        store
                            .scheduled_tasks_for_behavior(
                                selected_agent_did.unwrap_or_default(),
                                &row.behavior_id,
                            )
                            .len(),
                        store
                            .conversations_for_behavior(
                                selected_agent_did.unwrap_or_default(),
                                &row.behavior_id,
                            )
                            .len(),
                    ),
                })
                .collect()
        }
        OperatorSection::Backends => {
            let mut rows = store.inference_backends.iter().collect::<Vec<_>>();
            rows.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.backend_id.clone(),
                    title: row.name.clone().unwrap_or_else(|| row.backend_id.clone()),
                    meta: format!(
                        "{}  probe {}  models {}",
                        row.provider_kind.as_deref().unwrap_or("provider"),
                        row.probe_status.as_deref().unwrap_or("unknown"),
                        row.models.len(),
                    ),
                })
                .collect()
        }
        OperatorSection::ToolSelections => {
            let mut rows: Vec<_> = store
                .tool_selections
                .iter()
                .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                .collect();
            rows.sort_by(|left, right| left.selection_id.cmp(&right.selection_id));
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.selection_id.clone(),
                    title: row
                        .display_name
                        .clone()
                        .unwrap_or_else(|| row.selection_id.clone()),
                    meta: format!(
                        "file:{} bash:{} meta:{}",
                        bool_word(row.enable_file_tools),
                        bool_word(row.enable_bash),
                        bool_word(row.enable_meta_tools),
                    ),
                })
                .collect()
        }
        OperatorSection::InferenceProfiles => {
            let mut rows = store.inference_profiles.iter().collect::<Vec<_>>();
            rows.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.profile_id.clone(),
                    title: row
                        .display_name
                        .clone()
                        .unwrap_or_else(|| row.profile_id.clone()),
                    meta: format!(
                        "ctx {}  out {}  temp {}",
                        row.context_window
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "na".to_string()),
                        row.max_output_tokens
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "na".to_string()),
                        row.temperature
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "na".to_string()),
                    ),
                })
                .collect()
        }
        OperatorSection::ScheduledTasks => {
            let mut rows: Vec<_> = store
                .scheduled_tasks
                .iter()
                .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                .collect();
            rows.sort_by(|left, right| {
                left.next_run_at
                    .cmp(&right.next_run_at)
                    .then_with(|| left.task_id.cmp(&right.task_id))
            });
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.task_id.clone(),
                    title: row.name.clone().unwrap_or_else(|| row.task_id.clone()),
                    meta: format!(
                        "{}  every {}s  next {}  runs {}",
                        if row.enabled == Some(false) {
                            "disabled"
                        } else if scheduled_task_is_due(row) {
                            "due"
                        } else {
                            "armed"
                        },
                        row.interval_secs
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "na".to_string()),
                        scheduled_task_next_run_label(row),
                        row.run_count
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                    ),
                })
                .collect()
        }
        OperatorSection::RequestTimeline => request_timeline_summaries(store, selected_agent_did),
        OperatorSection::RecentFailures => recent_failure_summaries(store, selected_agent_did),
        _ => Vec::new(),
    }
}

pub(super) fn filter_entity_summaries(
    entries: Vec<EntitySummary>,
    filter: &str,
) -> Vec<EntitySummary> {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return entries;
    }

    entries
        .into_iter()
        .filter(|entry| {
            [entry.id.as_str(), entry.title.as_str(), entry.meta.as_str()]
                .into_iter()
                .any(|field| field.to_lowercase().contains(&filter))
        })
        .collect()
}
