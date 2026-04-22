use crate::client::ClientStore;
use crate::state::{
    BackendDraft, BehaviorDraft, EventTriggerDraft, InferenceProfileDraft, ManageDraft,
    ManageDraftOrigin, ManageSection, ScheduleDraft, TaskDraft, ToolSelectionDraft,
};

use super::{
    abbreviate_identifier, bool_word, compact_timestamp, normalize_optional_owned, schedule_is_due,
    schedule_next_run_label, summarize_request_content, truncate_line,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitySummary {
    pub id: String,
    pub title: String,
    pub meta: String,
}

pub fn draft_for_selection(
    store: &ClientStore,
    section: ManageSection,
    selected_agent_did: Option<&str>,
    entity_id: &str,
) -> Option<ManageDraft> {
    match section {
        ManageSection::Behaviors => store
            .behaviors
            .iter()
            .find(|row| {
                row.behavior_id == entity_id && row.agent_did.as_deref() == selected_agent_did
            })
            .map(|row| {
                ManageDraft::Behavior(BehaviorDraft {
                    behavior_id: row.behavior_id.clone(),
                    agent_did: row.agent_did.clone().unwrap_or_default(),
                    display_name: row.display_name.clone().unwrap_or_default(),
                    system_prompt: row.system_prompt.clone().unwrap_or_default(),
                    backend_id: row.backend_id.clone().unwrap_or_default(),
                    model_name: row.model_name.clone().unwrap_or_default(),
                    tool_selection_id: row.tool_selection_id.clone().unwrap_or_default(),
                    inference_profile_id: row.inference_profile_id.clone().unwrap_or_default(),
                    compaction_strategy: row.compaction_strategy.clone().unwrap_or_default(),
                    compaction_threshold: row
                        .compaction_threshold
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    enabled: row.enabled.unwrap_or(true),
                    created_at: row.created_at.clone().unwrap_or_default(),
                })
            }),
        ManageSection::Backends => store
            .inference_backends
            .iter()
            .find(|row| row.backend_id == entity_id)
            .map(|row| {
                ManageDraft::Backend(BackendDraft {
                    backend_id: row.backend_id.clone(),
                    name: row.name.clone().unwrap_or_default(),
                    provider_kind: row.provider_kind.clone().unwrap_or_default(),
                    endpoint: row.endpoint.clone().unwrap_or_default(),
                    api_key: row.api_key.clone().unwrap_or_default(),
                    api_key_env_var: row.api_key_env_var.clone().unwrap_or_default(),
                    max_concurrent: row
                        .max_concurrent
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    max_queue_depth: row
                        .max_queue_depth
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    enabled: row.enabled.unwrap_or(true),
                    models: row.models.join(", "),
                    probe_status: row.probe_status.clone().unwrap_or_default(),
                })
            }),
        ManageSection::ToolSelections => store
            .tool_selections
            .iter()
            .find(|row| {
                row.selection_id == entity_id && row.agent_did.as_deref() == selected_agent_did
            })
            .map(|row| {
                ManageDraft::ToolSelection(ToolSelectionDraft {
                    selection_id: row.selection_id.clone(),
                    agent_did: row.agent_did.clone().unwrap_or_default(),
                    display_name: row.display_name.clone().unwrap_or_default(),
                    enable_file_tools: row.enable_file_tools.unwrap_or(false),
                    file_tools_mode: row.file_tools_mode.clone().unwrap_or_default(),
                    enable_bash: row.enable_bash.unwrap_or(false),
                    bash_mode: row.bash_mode.clone().unwrap_or_default(),
                    cli_tool_names: row.cli_tool_names.join(", "),
                    enable_meta_tools: row.enable_meta_tools.unwrap_or(false),
                    delegate_to: row.delegate_to.join(", "),
                })
            }),
        ManageSection::InferenceProfiles => store
            .inference_profiles
            .iter()
            .find(|row| row.profile_id == entity_id)
            .map(|row| {
                ManageDraft::InferenceProfile(InferenceProfileDraft {
                    profile_id: row.profile_id.clone(),
                    display_name: row.display_name.clone().unwrap_or_default(),
                    context_window: row
                        .context_window
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    max_output_tokens: row
                        .max_output_tokens
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    max_turns: row
                        .max_turns
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    temperature: row
                        .temperature
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    stream_batch_ms: row
                        .stream_batch_ms
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    deadline_duration_secs: row
                        .deadline_duration_secs
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                })
            }),
        ManageSection::Tasks => {
            store
                .tasks
                .iter()
                .find(|row| row.task_id == entity_id)
                .map(|row| {
                    ManageDraft::Task(TaskDraft {
                        task_id: row.task_id.clone(),
                        name: row.name.clone().unwrap_or_default(),
                        description: row.description.clone().unwrap_or_default(),
                        behavior_id: row.behavior_id.clone().unwrap_or_default(),
                        prompt_template: row.prompt_template.clone().unwrap_or_default(),
                        enabled: row.enabled.unwrap_or(true),
                        output_schema_ref: row.output_schema_ref.clone().unwrap_or_default(),
                        created_at: row.created_at.clone().unwrap_or_default(),
                        updated_at: row.updated_at.clone().unwrap_or_default(),
                    })
                })
        }
        ManageSection::Schedules => store
            .schedules
            .iter()
            .find(|row| row.schedule_id == entity_id)
            .map(|row| {
                ManageDraft::Schedule(ScheduleDraft {
                    schedule_id: row.schedule_id.clone(),
                    task_id: row.task_id.clone().unwrap_or_default(),
                    interval_secs: row
                        .interval_secs
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    enabled: row.enabled.unwrap_or(true),
                    concurrency: row.concurrency.clone().unwrap_or_default(),
                    next_run_at: row.next_run_at.clone().unwrap_or_default(),
                    last_attempt_at: row.last_attempt_at.clone().unwrap_or_default(),
                    last_status: row.last_status.clone().unwrap_or_default(),
                    last_error: row.last_error.clone().unwrap_or_default(),
                    fire_count: row
                        .fire_count
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    created_at: row.created_at.clone().unwrap_or_default(),
                    updated_at: row.updated_at.clone().unwrap_or_default(),
                })
            }),
        ManageSection::EventTriggers => store
            .event_triggers
            .iter()
            .find(|row| row.trigger_id == entity_id)
            .map(|row| {
                ManageDraft::EventTrigger(EventTriggerDraft {
                    trigger_id: row.trigger_id.clone(),
                    task_id: row.task_id.clone().unwrap_or_default(),
                    source_collection: row.source_collection.clone().unwrap_or_default(),
                    event_kind: row
                        .event_kind
                        .clone()
                        .unwrap_or_else(|| "created".to_string()),
                    filter: row.filter.clone().unwrap_or_default(),
                    enabled: row.enabled.unwrap_or(true),
                    concurrency: row.concurrency.clone().unwrap_or_default(),
                    created_at: row.created_at.clone().unwrap_or_default(),
                    updated_at: row.updated_at.clone().unwrap_or_default(),
                    last_attempt_at: row.last_attempt_at.clone().unwrap_or_default(),
                    last_fired_source_doc_id: row
                        .last_fired_source_doc_id
                        .clone()
                        .unwrap_or_default(),
                    last_status: row.last_status.clone().unwrap_or_default(),
                    last_error: row.last_error.clone().unwrap_or_default(),
                    fire_count: row
                        .fire_count
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                })
            }),
        _ => {
            let _ = selected_agent_did;
            None
        }
    }
}

pub fn new_draft_for_section(
    section: ManageSection,
    selected_agent_did: Option<&str>,
) -> Option<ManageDraft> {
    match section {
        ManageSection::Behaviors => Some(ManageDraft::Behavior(BehaviorDraft {
            behavior_id: String::new(),
            agent_did: selected_agent_did.unwrap_or_default().to_string(),
            display_name: String::new(),
            system_prompt: String::new(),
            backend_id: String::new(),
            model_name: String::new(),
            tool_selection_id: String::new(),
            inference_profile_id: String::new(),
            compaction_strategy: "StripThenSummarize".to_string(),
            compaction_threshold: String::new(),
            enabled: true,
            created_at: String::new(),
        })),
        ManageSection::Backends => Some(ManageDraft::Backend(BackendDraft {
            backend_id: String::new(),
            name: String::new(),
            provider_kind: String::new(),
            endpoint: String::new(),
            api_key: String::new(),
            api_key_env_var: String::new(),
            max_concurrent: String::new(),
            max_queue_depth: String::new(),
            enabled: true,
            models: String::new(),
            probe_status: String::new(),
        })),
        ManageSection::ToolSelections => Some(ManageDraft::ToolSelection(ToolSelectionDraft {
            selection_id: String::new(),
            agent_did: selected_agent_did.unwrap_or_default().to_string(),
            display_name: String::new(),
            enable_file_tools: false,
            file_tools_mode: String::new(),
            enable_bash: false,
            bash_mode: String::new(),
            cli_tool_names: String::new(),
            enable_meta_tools: false,
            delegate_to: String::new(),
        })),
        ManageSection::InferenceProfiles => {
            Some(ManageDraft::InferenceProfile(InferenceProfileDraft {
                profile_id: String::new(),
                display_name: String::new(),
                context_window: String::new(),
                max_output_tokens: String::new(),
                max_turns: String::new(),
                temperature: String::new(),
                stream_batch_ms: String::new(),
                deadline_duration_secs: String::new(),
            }))
        }
        ManageSection::Tasks => Some(ManageDraft::Task(TaskDraft {
            task_id: String::new(),
            name: String::new(),
            description: String::new(),
            behavior_id: String::new(),
            prompt_template: String::new(),
            enabled: true,
            output_schema_ref: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        })),
        ManageSection::Schedules => Some(ManageDraft::Schedule(ScheduleDraft {
            schedule_id: String::new(),
            task_id: String::new(),
            interval_secs: String::new(),
            enabled: true,
            concurrency: String::new(),
            next_run_at: String::new(),
            last_attempt_at: String::new(),
            last_status: String::new(),
            last_error: String::new(),
            fire_count: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        })),
        ManageSection::EventTriggers => Some(ManageDraft::EventTrigger(EventTriggerDraft {
            trigger_id: String::new(),
            task_id: String::new(),
            source_collection: String::new(),
            // PR 2 only supports "created" events; other event kinds
            // will land in later PRs as the event-source gains more
            // probe surfaces.
            event_kind: "created".to_string(),
            filter: String::new(),
            enabled: true,
            concurrency: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            last_attempt_at: String::new(),
            last_fired_source_doc_id: String::new(),
            last_status: String::new(),
            last_error: String::new(),
            fire_count: String::new(),
        })),
        _ => None,
    }
}

pub fn draft_matches_selection(
    draft: &Option<ManageDraft>,
    draft_origin: Option<&ManageDraftOrigin>,
    section: ManageSection,
    selected_entity_id: Option<&str>,
) -> bool {
    match (draft, draft_origin, selected_entity_id) {
        (
            Some(draft),
            Some(ManageDraftOrigin::ExistingEntity(source_entity_id)),
            Some(entity_id),
        ) => draft.section() == section && source_entity_id == entity_id,
        (Some(draft), Some(ManageDraftOrigin::NewDocument), None) => draft.section() == section,
        (None, _, None) => true,
        _ => false,
    }
}

pub fn entity_summaries(
    store: &ClientStore,
    section: ManageSection,
    selected_agent_did: Option<&str>,
) -> Vec<EntitySummary> {
    match section {
        ManageSection::Behaviors => {
            let mut rows: Vec<_> = store
                .behaviors
                .iter()
                .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                .collect();
            rows.sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));
            rows.into_iter()
                .map(|row| {
                    let behavior_tasks = store.tasks_for_behavior(
                        selected_agent_did.unwrap_or_default(),
                        &row.behavior_id,
                    );
                    let task_ids: Vec<&str> = behavior_tasks
                        .iter()
                        .map(|task| task.task_id.as_str())
                        .collect();
                    let schedules = store.schedules_for_tasks(&task_ids);
                    EntitySummary {
                        id: row.behavior_id.clone(),
                        title: row
                            .display_name
                            .clone()
                            .unwrap_or_else(|| row.behavior_id.clone()),
                        meta: format!(
                            "{}  model {}  backend {}  tasks {}  schedules {}  convos {}",
                            if row.enabled == Some(false) {
                                "disabled"
                            } else {
                                "enabled"
                            },
                            row.model_name.as_deref().unwrap_or("unbound"),
                            row.backend_id.as_deref().unwrap_or("unbound"),
                            behavior_tasks.len(),
                            schedules.len(),
                            store
                                .conversations_for_behavior(
                                    selected_agent_did.unwrap_or_default(),
                                    &row.behavior_id,
                                )
                                .len(),
                        ),
                    }
                })
                .collect()
        }
        ManageSection::Backends => {
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
        ManageSection::ToolSelections => {
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
        ManageSection::InferenceProfiles => {
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
        ManageSection::Tasks => {
            // Tasks are globally addressed by `task_id`. When a deployment
            // is selected we narrow to tasks whose behavior is owned by
            // that agent; this preserves the per-deployment feel of the
            // manage workspace.
            let mut rows: Vec<_> = store
                .tasks
                .iter()
                .filter(
                    |row| match (selected_agent_did, row.behavior_id.as_deref()) {
                        (Some(agent_did), Some(behavior_id)) => {
                            store.behavior_row(agent_did, behavior_id).is_some()
                        }
                        (Some(_), None) => false,
                        (None, _) => true,
                    },
                )
                .collect();
            rows.sort_by(|left, right| left.task_id.cmp(&right.task_id));
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.task_id.clone(),
                    title: row.name.clone().unwrap_or_else(|| row.task_id.clone()),
                    meta: format!(
                        "{}  behavior {}  updated {}",
                        if row.enabled == Some(false) {
                            "disabled"
                        } else {
                            "enabled"
                        },
                        row.behavior_id.as_deref().unwrap_or("unbound"),
                        compact_timestamp(
                            row.updated_at
                                .as_deref()
                                .or(row.created_at.as_deref())
                                .unwrap_or(""),
                        ),
                    ),
                })
                .collect()
        }
        ManageSection::Schedules => {
            // Schedules are globally addressed by `schedule_id`. When a
            // deployment is selected we filter to schedules whose task is
            // bound to a behavior owned by that agent (matching the Tasks
            // section's filter above).
            let task_ids_for_agent: std::collections::HashSet<String> = match selected_agent_did {
                Some(agent_did) => store
                    .tasks
                    .iter()
                    .filter(|task| {
                        task.behavior_id.as_deref().is_some_and(|behavior_id| {
                            store.behavior_row(agent_did, behavior_id).is_some()
                        })
                    })
                    .map(|task| task.task_id.clone())
                    .collect(),
                None => store
                    .tasks
                    .iter()
                    .map(|task| task.task_id.clone())
                    .collect(),
            };

            let mut rows: Vec<_> = store
                .schedules
                .iter()
                .filter(|row| match row.task_id.as_deref() {
                    Some(task_id) => task_ids_for_agent.contains(task_id),
                    None => selected_agent_did.is_none(),
                })
                .collect();
            rows.sort_by(|left, right| {
                left.next_run_at
                    .cmp(&right.next_run_at)
                    .then_with(|| left.schedule_id.cmp(&right.schedule_id))
            });
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.schedule_id.clone(),
                    title: row
                        .task_id
                        .clone()
                        .unwrap_or_else(|| row.schedule_id.clone()),
                    meta: format!(
                        "{}  every {}s  next {}  fires {}",
                        if row.enabled == Some(false) {
                            "disabled"
                        } else if schedule_is_due(row) {
                            "due"
                        } else {
                            "armed"
                        },
                        row.interval_secs
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "na".to_string()),
                        schedule_next_run_label(row),
                        row.fire_count
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                    ),
                })
                .collect()
        }
        ManageSection::EventTriggers => {
            // EventTriggers are globally addressed by `trigger_id`. Like
            // Schedules, when a deployment is selected we narrow to
            // triggers whose task is bound to a behavior owned by that
            // agent.
            let task_ids_for_agent: std::collections::HashSet<String> = match selected_agent_did {
                Some(agent_did) => store
                    .tasks
                    .iter()
                    .filter(|task| {
                        task.behavior_id
                            .as_deref()
                            .is_some_and(|behavior_id| {
                                store.behavior_row(agent_did, behavior_id).is_some()
                            })
                    })
                    .map(|task| task.task_id.clone())
                    .collect(),
                None => store.tasks.iter().map(|task| task.task_id.clone()).collect(),
            };

            let mut rows: Vec<_> = store
                .event_triggers
                .iter()
                .filter(|row| match row.task_id.as_deref() {
                    Some(task_id) => task_ids_for_agent.contains(task_id),
                    None => selected_agent_did.is_none(),
                })
                .collect();
            rows.sort_by(|left, right| left.trigger_id.cmp(&right.trigger_id));
            rows.into_iter()
                .map(|row| EntitySummary {
                    id: row.trigger_id.clone(),
                    title: row
                        .task_id
                        .clone()
                        .unwrap_or_else(|| row.trigger_id.clone()),
                    meta: format!(
                        "{}  {}  source {}  fires {}",
                        if row.enabled == Some(false) {
                            "disabled"
                        } else {
                            "enabled"
                        },
                        row.event_kind.as_deref().unwrap_or("created"),
                        row.source_collection.as_deref().unwrap_or("unbound"),
                        row.fire_count
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                    ),
                })
                .collect()
        }
        ManageSection::RequestTimeline => request_timeline_summaries(store, selected_agent_did),
        ManageSection::RecentFailures => recent_failure_summaries(store, selected_agent_did),
    }
}

pub fn request_timeline_summaries(
    store: &ClientStore,
    selected_agent_did: Option<&str>,
) -> Vec<EntitySummary> {
    let mut rows: Vec<_> = store
        .requests
        .iter()
        .filter(|row| row.agent_did.as_deref() == selected_agent_did)
        .collect();
    rows.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.request_id.cmp(&left.request_id))
    });

    rows.into_iter()
        .map(|row| {
            let latest_response = store.latest_response_for_request(&row.request_id);
            let response_state = latest_response
                .and_then(|response| response.status.as_deref())
                .unwrap_or("waiting");
            EntitySummary {
                id: row.request_id.clone(),
                title: summarize_request_content(
                    row.content.as_deref().unwrap_or_default(),
                    &row.request_id,
                ),
                meta: format!(
                    "{}  rsp {}  session {}  {}",
                    row.lifecycle_state.as_deref().unwrap_or("pending"),
                    response_state,
                    abbreviate_identifier(row.session_id.as_deref().unwrap_or("none")),
                    compact_timestamp(
                        row.claimed_at
                            .as_deref()
                            .or(row.created_at.as_deref())
                            .unwrap_or(""),
                    ),
                ),
            }
        })
        .collect()
}

pub fn recent_failure_summaries(
    store: &ClientStore,
    selected_agent_did: Option<&str>,
) -> Vec<EntitySummary> {
    let mut rows: Vec<(Option<String>, EntitySummary)> = Vec::new();

    for request in store
        .requests
        .iter()
        .filter(|row| row.agent_did.as_deref() == selected_agent_did)
    {
        let failure = normalize_optional_owned(request.failure_reason.as_deref().unwrap_or(""))
            .or_else(|| {
                store
                    .latest_response_for_request(&request.request_id)
                    .and_then(|response| {
                        normalize_optional_owned(response.error_message.as_deref().unwrap_or(""))
                    })
            });
        let Some(failure) = failure else {
            continue;
        };

        rows.push((
            request
                .claimed_at
                .clone()
                .or_else(|| request.created_at.clone()),
            EntitySummary {
                id: format!("request:{}", request.request_id),
                title: summarize_request_content(
                    request.content.as_deref().unwrap_or_default(),
                    &request.request_id,
                ),
                meta: format!(
                    "request  {}  {}",
                    request.lifecycle_state.as_deref().unwrap_or("failed"),
                    truncate_line(&failure, 64),
                ),
            },
        ));
    }

    // Scope schedule failures to schedules whose task is bound to a
    // behavior owned by the selected agent (mirroring Schedules section
    // filtering above).
    let task_ids_for_agent: std::collections::HashSet<String> = match selected_agent_did {
        Some(agent_did) => store
            .tasks
            .iter()
            .filter(|task| {
                task.behavior_id
                    .as_deref()
                    .is_some_and(|behavior_id| store.behavior_row(agent_did, behavior_id).is_some())
            })
            .map(|task| task.task_id.clone())
            .collect(),
        None => store
            .tasks
            .iter()
            .map(|task| task.task_id.clone())
            .collect(),
    };

    for schedule in store
        .schedules
        .iter()
        .filter(|row| match row.task_id.as_deref() {
            Some(task_id) => task_ids_for_agent.contains(task_id),
            None => selected_agent_did.is_none(),
        })
    {
        let Some(error) = normalize_optional_owned(schedule.last_error.as_deref().unwrap_or(""))
        else {
            continue;
        };

        rows.push((
            schedule
                .last_attempt_at
                .clone()
                .or_else(|| schedule.updated_at.clone()),
            EntitySummary {
                id: format!("schedule:{}", schedule.schedule_id),
                title: schedule
                    .task_id
                    .clone()
                    .unwrap_or_else(|| schedule.schedule_id.clone()),
                meta: format!(
                    "schedule  {}  {}",
                    schedule.last_status.as_deref().unwrap_or("error"),
                    truncate_line(&error, 64),
                ),
            },
        ));
    }

    rows.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.id.cmp(&left.1.id))
    });
    rows.into_iter().map(|(_, summary)| summary).collect()
}
