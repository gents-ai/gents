use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use defra_agent_protocol::row::{
    AgentBehaviorRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow, ToolSelectionRow,
};
use eframe::egui::{self, RichText, TextEdit, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{
    BackendDraft, BehaviorDraft, InferenceProfileDraft, OperatorDraft, OperatorSection,
    ScheduledTaskDraft, ShellState, ToolSelectionDraft,
};
use crate::theme;
use crate::views;
use crate::views::chat::build_deployment_entries;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntitySummary {
    id: String,
    title: String,
    meta: String,
}

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);

    if state
        .operator
        .selected_peer_id
        .as_deref()
        .is_none_or(|peer_id| !deployments.iter().any(|entry| entry.peer_id == peer_id))
    {
        state.operator.selected_peer_id = deployments.first().map(|entry| entry.peer_id.clone());
    }

    if state
        .operator
        .selected_agent_did
        .as_deref()
        .is_none_or(|agent_did| {
            !deployments.iter().any(|entry| entry.agent_did == agent_did)
                && !store
                    .agent_principals
                    .iter()
                    .any(|row| row.agent_did == agent_did)
        })
    {
        state.operator.selected_agent_did = deployments
            .iter()
            .find(|entry| {
                Some(entry.peer_id.as_str()) == state.operator.selected_peer_id.as_deref()
            })
            .map(|entry| entry.agent_did.clone())
            .or_else(|| deployments.first().map(|entry| entry.agent_did.clone()))
            .or_else(|| {
                store
                    .agent_principals
                    .first()
                    .map(|row| row.agent_did.clone())
            });
    }

    let entries = entity_summaries(
        store,
        state.operator.selected_section,
        state.operator.selected_agent_did.as_deref(),
    );
    if state
        .operator
        .selected_entity_id
        .as_deref()
        .is_none_or(|entity_id| !entries.iter().any(|entry| entry.id == entity_id))
    {
        state.operator.selected_entity_id = entries.first().map(|entry| entry.id.clone());
    }

    let selected_entity_id = state.operator.selected_entity_id.clone();
    if !draft_matches_selection(
        &state.operator.draft,
        state.operator.draft_source_entity_id.as_deref(),
        state.operator.selected_section,
        selected_entity_id.as_deref(),
    ) {
        state.operator.draft = selected_entity_id.as_deref().and_then(|entity_id| {
            draft_for_selection(
                store,
                state.operator.selected_section,
                state.operator.selected_agent_did.as_deref(),
                entity_id,
            )
        });
        state.operator.draft_source_entity_id =
            state.operator.draft.as_ref().and(selected_entity_id);
        state.operator.last_apply_error = None;
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let palette = theme::palette();

    let Some(store) = store else {
        views::card(
            ui,
            "Operator Unavailable",
            "The desktop client must finish bootstrapping before operator documents can render.",
        );
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Deployments", Some("focus"));
        });
        ui.add_space(6.0);

        for deployment in &deployments {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    let meta = format!(
                        "{}  runtime {}",
                        deployment.agent_label,
                        if deployment.connected {
                            "online"
                        } else {
                            "lagging"
                        }
                    );
                    let response = views::side_row(
                        ui,
                        &deployment.label,
                        &meta,
                        state.operator.selected_peer_id.as_deref()
                            == Some(deployment.peer_id.as_str()),
                        if deployment.connected {
                            palette.accent
                        } else {
                            palette.warning
                        },
                        Some(if deployment.connected { "up" } else { "warn" }),
                    );
                    audit::record(
                        ui,
                        &audit::targets::operator_deployment(&deployment.peer_id),
                        &response,
                    );
                    if response.clicked() {
                        state.operator.selected_peer_id = Some(deployment.peer_id.clone());
                        state.operator.selected_agent_did = Some(deployment.agent_did.clone());
                        state.operator.selected_entity_id = None;
                        state.operator.entity_filter.clear();
                        state.operator.draft = None;
                        state.operator.draft_source_entity_id = None;
                    }

                    let response = views::tree_row(
                        ui,
                        &deployment.agent_label,
                        if deployment.connected { "live" } else { "lag" },
                        state.operator.selected_agent_did.as_deref()
                            == Some(deployment.agent_did.as_str()),
                    );
                    audit::record(
                        ui,
                        &audit::targets::operator_agent(&deployment.agent_did),
                        &response,
                    );
                    if response.clicked() {
                        state.operator.selected_peer_id = Some(deployment.peer_id.clone());
                        state.operator.selected_agent_did = Some(deployment.agent_did.clone());
                        state.operator.selected_entity_id = None;
                        state.operator.entity_filter.clear();
                        state.operator.draft = None;
                        state.operator.draft_source_entity_id = None;
                    }
                });
            });
            ui.add_space(10.0);
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Manage", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                for section in OperatorSection::MANAGE {
                    let (title, meta) =
                        section_meta(store, section, state.operator.selected_agent_did.as_deref());
                    let response = views::side_row(
                        ui,
                        title,
                        &meta,
                        state.operator.selected_section == section,
                        if state.operator.selected_section == section {
                            palette.accent
                        } else {
                            palette.text_3
                        },
                        None,
                    );
                    audit::record(ui, &audit::targets::operator_section(section), &response);
                    if response.clicked() {
                        state.operator.selected_section = section;
                        state.operator.selected_entity_id = None;
                        state.operator.entity_filter.clear();
                        state.operator.draft = None;
                        state.operator.draft_source_entity_id = None;
                        state.operator.last_apply_error = None;
                    }
                }
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Inspect", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                for section in OperatorSection::INSPECT {
                    let response = views::side_row(
                        ui,
                        section.label(),
                        "T10",
                        state.operator.selected_section == section,
                        palette.text_3,
                        None,
                    );
                    audit::record(ui, &audit::targets::operator_section(section), &response);
                    if response.clicked() {
                        state.operator.selected_section = section;
                        state.operator.selected_entity_id = None;
                        state.operator.entity_filter.clear();
                        state.operator.draft = None;
                        state.operator.draft_source_entity_id = None;
                    }
                }
            });
        });
    });
}

pub fn show_main(ui: &mut Ui, state: &mut ShellState, store: Option<&ClientStore>) {
    let palette = theme::palette();

    let Some(store) = store else {
        views::card(
            ui,
            "Operator Unavailable",
            "No local replica is available for operator views.",
        );
        return;
    };

    let section = state.operator.selected_section;
    let entries = entity_summaries(store, section, state.operator.selected_agent_did.as_deref());
    let breadcrumb = format!(
        "{} / {} / {}",
        state
            .operator
            .selected_peer_id
            .as_deref()
            .unwrap_or("local replica"),
        state
            .operator
            .selected_agent_did
            .as_deref()
            .unwrap_or("no agent"),
        section.label(),
    );

    ui.vertical(|ui| {
        views::toolbar(
            ui,
            "Operator Console",
            &breadcrumb,
            if state.operator.draft.is_some() {
                "editor: active"
            } else {
                "editor: idle"
            },
        );
        ui.add_space(12.0);

        match section {
            OperatorSection::Runtime => show_runtime_summary(ui, store, state),
            OperatorSection::Behaviors
            | OperatorSection::Backends
            | OperatorSection::ToolSelections
            | OperatorSection::InferenceProfiles
            | OperatorSection::ScheduledTasks
            | OperatorSection::RequestTimeline
            | OperatorSection::RecentFailures => {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("FILTER")
                            .monospace()
                            .size(10.5)
                            .color(palette.text_3),
                    );
                    audit::add_sized(
                        ui,
                        audit::targets::OPERATOR_ENTITY_FILTER,
                        [ui.available_width(), 28.0],
                        TextEdit::singleline(&mut state.operator.entity_filter)
                            .id_source(audit::targets::OPERATOR_ENTITY_FILTER)
                            .hint_text("name, id, backend, model"),
                    );
                });
                ui.add_space(10.0);

                if entries.is_empty() {
                    views::card(
                        ui,
                        "No Documents",
                        "No documents are currently replicated for this section.",
                    );
                } else {
                    let filtered_entries =
                        filter_entity_summaries(entries, state.operator.entity_filter.as_str());
                    if state
                        .operator
                        .selected_entity_id
                        .as_deref()
                        .is_some_and(|entity_id| {
                            !filtered_entries.iter().any(|entry| entry.id == entity_id)
                        })
                    {
                        state.operator.selected_entity_id =
                            filtered_entries.first().map(|entry| entry.id.clone());
                        let selected_entity_id = state.operator.selected_entity_id.clone();
                        state.operator.draft =
                            selected_entity_id.as_deref().and_then(|entity_id| {
                                draft_for_selection(
                                    store,
                                    section,
                                    state.operator.selected_agent_did.as_deref(),
                                    entity_id,
                                )
                            });
                        state.operator.draft_source_entity_id =
                            state.operator.draft.as_ref().and(selected_entity_id);
                        state.operator.last_apply_error = None;
                    }
                    if filtered_entries.is_empty() {
                    views::card(
                        ui,
                        "No Matches",
                        "The current filter does not match any replicated documents in this section.",
                    );
                    } else {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for entry in filtered_entries {
                                let selected = state.operator.selected_entity_id.as_deref()
                                    == Some(entry.id.as_str());
                                let response = views::side_row(
                                    ui,
                                    &entry.title,
                                    &entry.meta,
                                    selected,
                                    if selected {
                                        palette.accent
                                    } else {
                                        palette.text_3
                                    },
                                    None,
                                );
                                audit::record(
                                    ui,
                                    &audit::targets::operator_entity(&entry.id),
                                    &response,
                                );
                                if response.clicked() {
                                    state.operator.selected_entity_id = Some(entry.id.clone());
                                    state.operator.draft = draft_for_selection(
                                        store,
                                        section,
                                        state.operator.selected_agent_did.as_deref(),
                                        &entry.id,
                                    );
                                    state.operator.draft_source_entity_id =
                                        state.operator.draft.as_ref().map(|_| entry.id.clone());
                                    state.operator.last_apply_error = None;
                                }
                                ui.add_space(6.0);
                            }
                        });
                    }
                }
            }
        }
    });
}

pub fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    let palette = theme::palette();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        let (rail_title, rail_meta) = match state.operator.selected_section {
            OperatorSection::Runtime
            | OperatorSection::RequestTimeline
            | OperatorSection::RecentFailures => ("Inspector", Some("read only")),
            _ => ("Editor", Some("apply / discard")),
        };
        views::sidebar_heading(ui, rail_title, rail_meta);
        ui.add_space(10.0);

        let Some(store) = store else {
            views::card(
                ui,
                "Editor Offline",
                "The editor becomes available once the local replica is online.",
            );
            return;
        };

        match state.operator.selected_section {
            OperatorSection::Runtime => render_runtime_inspector(ui, store, state),
            OperatorSection::Behaviors => {
                if let Some(OperatorDraft::Behavior(draft)) = state.operator.draft.as_mut() {
                    render_behavior_editor(ui, draft);
                    render_editor_footer(ui, state, store, client, runtime);
                } else {
                    views::card(
                        ui,
                        "Behavior Editor",
                        "Select a behavior from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::Backends => {
                if let Some(OperatorDraft::Backend(draft)) = state.operator.draft.as_mut() {
                    render_backend_editor(ui, draft);
                    render_editor_footer(ui, state, store, client, runtime);
                } else {
                    views::card(
                        ui,
                        "Backend Editor",
                        "Select a backend from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::ToolSelections => {
                if let Some(OperatorDraft::ToolSelection(draft)) = state.operator.draft.as_mut() {
                    render_tool_selection_editor(ui, draft);
                    render_editor_footer(ui, state, store, client, runtime);
                } else {
                    views::card(
                        ui,
                        "Tool Selection Editor",
                        "Select a tool selection from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::InferenceProfiles => {
                if let Some(OperatorDraft::InferenceProfile(draft)) = state.operator.draft.as_mut()
                {
                    render_inference_profile_editor(ui, draft);
                    render_editor_footer(ui, state, store, client, runtime);
                } else {
                    views::card(
                        ui,
                        "Inference Profile Editor",
                        "Select a profile from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::ScheduledTasks => {
                if let Some(OperatorDraft::ScheduledTask(draft)) = state.operator.draft.as_mut() {
                    render_scheduled_task_editor(ui, draft);
                    render_editor_footer(ui, state, store, client, runtime);
                } else {
                    views::card(
                        ui,
                        "Scheduled Task Editor",
                        "Select a scheduled task from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::RequestTimeline => {
                render_request_timeline_detail(ui, state, store);
            }
            OperatorSection::RecentFailures => {
                render_recent_failure_detail(ui, state, store);
            }
        }

        if let Some(error) = state.operator.last_apply_error.as_deref() {
            ui.add_space(10.0);
            ui.label(
                RichText::new(error)
                    .monospace()
                    .size(10.5)
                    .color(palette.warning),
            );
        }
    });
}

fn show_runtime_summary(ui: &mut Ui, store: &ClientStore, state: &ShellState) {
    let palette = theme::palette();

    if let Some(agent_did) = state.operator.selected_agent_did.as_deref() {
        if let Some(runtime_row) = store.latest_runtime(agent_did) {
            ui.group(|ui| {
                ui.label(
                    RichText::new("runtime")
                        .family(theme::stencil_family())
                        .size(13.0)
                        .color(palette.text_1)
                        .strong(),
                );
                ui.add_space(6.0);
                for row in [
                    (
                        "process",
                        runtime_row.process_state.as_deref().unwrap_or("unknown"),
                    ),
                    (
                        "default behavior",
                        runtime_row
                            .default_behavior_id
                            .as_deref()
                            .unwrap_or("unbound"),
                    ),
                    (
                        "runnable",
                        &runtime_row
                            .runnable_behavior_count
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                    ),
                    (
                        "unavailable",
                        &runtime_row
                            .unavailable_behavior_count
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                    ),
                    (
                        "reconcile result",
                        runtime_row
                            .last_reconcile_result
                            .as_deref()
                            .unwrap_or("pending"),
                    ),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(row.0)
                                .monospace()
                                .size(11.0)
                                .color(palette.text_2),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(row.1)
                                    .monospace()
                                    .size(11.0)
                                    .color(palette.text_0),
                            );
                        });
                    });
                }
            });
        } else {
            views::card(
                ui,
                "Runtime Pending",
                "No AgentRuntime row is replicated yet for the selected agent.",
            );
        }
    } else {
        views::card(
            ui,
            "Select Agent",
            "Choose an agent from the deployment tree to inspect runtime state.",
        );
    }
}

fn render_runtime_inspector(ui: &mut Ui, store: &ClientStore, state: &ShellState) {
    if let Some(agent_did) = state.operator.selected_agent_did.as_deref() {
        if let Some(runtime_row) = store.latest_runtime(agent_did) {
            editor_heading(ui, "Runtime Inspector");
            read_only_field(ui, "Agent DID", agent_did);
            read_only_field(
                ui,
                "Process State",
                runtime_row.process_state.as_deref().unwrap_or("unknown"),
            );
            read_only_field(
                ui,
                "Reconcile Phase",
                runtime_row.reconcile_phase.as_deref().unwrap_or("unknown"),
            );
            read_only_field(
                ui,
                "Default Behavior",
                runtime_row
                    .default_behavior_id
                    .as_deref()
                    .unwrap_or("unbound"),
            );
            read_only_field(
                ui,
                "Runnable Behaviors",
                &runtime_row
                    .runnable_behavior_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            );
            read_only_field(
                ui,
                "Unavailable Behaviors",
                &runtime_row
                    .unavailable_behavior_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            );
            read_only_field(
                ui,
                "Last Result",
                runtime_row
                    .last_reconcile_result
                    .as_deref()
                    .unwrap_or("pending"),
            );
            read_only_multiline(
                ui,
                "Last Error",
                runtime_row.last_reconcile_error.as_deref().unwrap_or(""),
                4,
            );
            read_only_field(
                ui,
                "Completed At",
                runtime_row
                    .last_reconcile_completed_at
                    .as_deref()
                    .unwrap_or("unset"),
            );
            read_only_field(
                ui,
                "Observed Behaviors",
                &store
                    .behaviors
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == Some(agent_did))
                    .count()
                    .to_string(),
            );
            read_only_field(
                ui,
                "Scheduled Tasks",
                &store
                    .scheduled_tasks
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == Some(agent_did))
                    .count()
                    .to_string(),
            );
        } else {
            views::card(
                ui,
                "Runtime Pending",
                "The selected agent has no replicated AgentRuntime row yet.",
            );
        }
    } else {
        views::card(
            ui,
            "Select Agent",
            "Choose an agent from the deployment tree to inspect runtime state.",
        );
    }
}

fn request_timeline_summaries(
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

fn recent_failure_summaries(
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

    for task in store
        .scheduled_tasks
        .iter()
        .filter(|row| row.agent_did.as_deref() == selected_agent_did)
    {
        let Some(error) = normalize_optional_owned(task.last_error.as_deref().unwrap_or("")) else {
            continue;
        };

        rows.push((
            task.last_run_at.clone().or_else(|| task.updated_at.clone()),
            EntitySummary {
                id: format!("task:{}", task.task_id),
                title: task.name.clone().unwrap_or_else(|| task.task_id.clone()),
                meta: format!(
                    "scheduled task  {}  {}",
                    task.last_status.as_deref().unwrap_or("error"),
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

fn render_request_timeline_detail(ui: &mut Ui, state: &ShellState, store: &ClientStore) {
    editor_heading(ui, "Request Detail");
    let Some(request_id) = state.operator.selected_entity_id.as_deref() else {
        views::card(
            ui,
            "Request Detail",
            "Select a request from the timeline list to inspect it.",
        );
        return;
    };

    let Some(request) = store
        .requests
        .iter()
        .find(|row| row.request_id == request_id)
    else {
        views::card(
            ui,
            "Request Missing",
            "The selected request is no longer present in the local replica.",
        );
        return;
    };

    render_request_detail(ui, store, request);
}

fn render_recent_failure_detail(ui: &mut Ui, state: &ShellState, store: &ClientStore) {
    editor_heading(ui, "Failure Detail");
    let Some(selected_id) = state.operator.selected_entity_id.as_deref() else {
        views::card(
            ui,
            "Failure Detail",
            "Select a failure from the list to inspect its context.",
        );
        return;
    };

    if let Some(request_id) = selected_id.strip_prefix("request:") {
        if let Some(request) = store
            .requests
            .iter()
            .find(|row| row.request_id == request_id)
        {
            render_request_detail(ui, store, request);
            return;
        }
    }

    if let Some(task_id) = selected_id.strip_prefix("task:") {
        if let Some(task) = store
            .scheduled_tasks
            .iter()
            .find(|row| row.task_id == task_id)
        {
            render_scheduled_task_failure_detail(ui, task);
            return;
        }
    }

    views::card(
        ui,
        "Failure Missing",
        "The selected failure record is no longer present in the local replica.",
    );
}

fn render_request_detail(
    ui: &mut Ui,
    store: &ClientStore,
    request: &defra_agent_protocol::row::AgentRequestRow,
) {
    let latest_response = store.latest_response_for_request(&request.request_id);

    read_only_field(ui, "Request ID", &request.request_id);
    read_only_field(
        ui,
        "Session ID",
        request.session_id.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Behavior ID",
        request.behavior_id.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Lifecycle State",
        request.lifecycle_state.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Execution Origin",
        request.execution_origin.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Retry Count",
        &format!(
            "{}/{}",
            request.retry_count.unwrap_or_default(),
            request.max_retries.unwrap_or_default()
        ),
    );
    read_only_field(
        ui,
        "Created At",
        request.created_at.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Claimed At",
        request.claimed_at.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Deadline",
        request.deadline.as_deref().unwrap_or("unset"),
    );
    read_only_multiline(
        ui,
        "Failure Reason",
        request.failure_reason.as_deref().unwrap_or(""),
        3,
    );
    read_only_multiline(
        ui,
        "Request Content",
        request.content.as_deref().unwrap_or(""),
        6,
    );

    if let Some(response) = latest_response {
        read_only_field(
            ui,
            "Response Status",
            response.status.as_deref().unwrap_or("unset"),
        );
        read_only_multiline(
            ui,
            "Response Error",
            response.error_message.as_deref().unwrap_or(""),
            3,
        );
        read_only_multiline(
            ui,
            "Response Content",
            response.content.as_deref().unwrap_or(""),
            6,
        );
    }
}

fn render_scheduled_task_failure_detail(ui: &mut Ui, task: &ScheduledTaskRow) {
    read_only_field(ui, "Task ID", &task.task_id);
    read_only_field(ui, "Name", task.name.as_deref().unwrap_or("unset"));
    read_only_field(
        ui,
        "Behavior ID",
        task.behavior_id.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Last Status",
        task.last_status.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Last Run At",
        task.last_run_at.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Next Run At",
        task.next_run_at.as_deref().unwrap_or("unset"),
    );
    read_only_multiline(
        ui,
        "Last Error",
        task.last_error.as_deref().unwrap_or(""),
        4,
    );
    read_only_multiline(ui, "Prompt", task.prompt.as_deref().unwrap_or(""), 6);
}

fn render_editor_footer(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) {
    let palette = theme::palette();

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        let can_run_now = client.is_some()
            && matches!(
                state.operator.draft,
                Some(OperatorDraft::ScheduledTask(ref draft)) if draft.enabled
            );

        if audit::button(ui, audit::targets::OPERATOR_DISCARD, "Discard").clicked() {
            let selected_entity_id = state.operator.selected_entity_id.clone();
            state.operator.draft = selected_entity_id.as_deref().and_then(|entity_id| {
                draft_for_selection(
                    store,
                    state.operator.selected_section,
                    state.operator.selected_agent_did.as_deref(),
                    entity_id,
                )
            });
            state.operator.draft_source_entity_id =
                state.operator.draft.as_ref().and(selected_entity_id);
            state.operator.last_apply_error = None;
        }

        let can_apply = client.is_some() && state.operator.draft.is_some();
        if audit::add_enabled(
            ui,
            audit::targets::OPERATOR_APPLY,
            can_apply,
            egui::Button::new("Apply"),
        )
        .clicked()
        {
            match apply_draft(state, client, runtime) {
                Ok(()) => {
                    state.operator.last_apply_error = None;
                    state.operator.draft = None;
                    state.operator.draft_source_entity_id = None;
                }
                Err(error) => {
                    state.operator.last_apply_error = Some(error.to_string());
                }
            }
        }

        if matches!(
            state.operator.selected_section,
            OperatorSection::ScheduledTasks
        ) && audit::add_enabled(
            ui,
            audit::targets::OPERATOR_RUN_NOW,
            can_run_now,
            egui::Button::new("Run Now"),
        )
        .clicked()
        {
            match run_now_draft(state, client, runtime) {
                Ok(()) => {
                    state.operator.last_apply_error = None;
                    state.operator.draft = None;
                    state.operator.draft_source_entity_id = None;
                }
                Err(error) => {
                    state.operator.last_apply_error = Some(error.to_string());
                }
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("1:1 document editor")
                    .monospace()
                    .size(10.5)
                    .color(palette.text_3),
            );
        });
    });
}

fn apply_draft(state: &ShellState, client: Option<&ClientCore>, runtime: &Runtime) -> Result<()> {
    let client = client.context("client core is offline")?;
    let draft = state
        .operator
        .draft
        .as_ref()
        .context("no operator draft is selected")?;

    match draft {
        OperatorDraft::Behavior(draft) => {
            runtime.block_on(client.save_behavior(&behavior_row(draft)?))
        }
        OperatorDraft::Backend(draft) => {
            runtime.block_on(client.save_backend(&backend_row(draft)?))
        }
        OperatorDraft::ToolSelection(draft) => {
            runtime.block_on(client.save_tool_selection(&tool_selection_row(draft)?))
        }
        OperatorDraft::InferenceProfile(draft) => {
            runtime.block_on(client.save_inference_profile(&inference_profile_row(draft)?))
        }
        OperatorDraft::ScheduledTask(draft) => {
            runtime.block_on(client.save_scheduled_task(&scheduled_task_row(draft)?))
        }
    }
}

fn run_now_draft(state: &ShellState, client: Option<&ClientCore>, runtime: &Runtime) -> Result<()> {
    let client = client.context("client core is offline")?;
    let draft = state
        .operator
        .draft
        .as_ref()
        .context("no operator draft is selected")?;

    match draft {
        OperatorDraft::ScheduledTask(draft) => {
            runtime.block_on(client.run_scheduled_task_now(&scheduled_task_row(draft)?))
        }
        _ => Err(anyhow!("run now is only available for scheduled tasks")),
    }
}

fn entity_summaries(
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
                        "{}  model {}  backend {}",
                        if row.enabled == Some(false) {
                            "disabled"
                        } else {
                            "enabled"
                        },
                        row.model_name.as_deref().unwrap_or("unbound"),
                        row.backend_id.as_deref().unwrap_or("unbound"),
                    ),
                })
                .collect()
        }
        OperatorSection::Backends => {
            let backend_ids = backend_ids_for_agent(store, selected_agent_did);
            let mut rows = store
                .inference_backends
                .iter()
                .filter(|row| backend_ids.contains(&row.backend_id.as_str()))
                .collect::<Vec<_>>();
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
            let profile_ids = inference_profile_ids_for_agent(store, selected_agent_did);
            let mut rows = store
                .inference_profiles
                .iter()
                .filter(|row| profile_ids.contains(&row.profile_id.as_str()))
                .collect::<Vec<_>>();
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

fn backend_ids_for_agent<'a>(
    store: &'a ClientStore,
    selected_agent_did: Option<&str>,
) -> Vec<&'a str> {
    store
        .behaviors
        .iter()
        .filter(|row| row.agent_did.as_deref() == selected_agent_did)
        .filter_map(|row| row.backend_id.as_deref())
        .collect()
}

fn inference_profile_ids_for_agent<'a>(
    store: &'a ClientStore,
    selected_agent_did: Option<&str>,
) -> Vec<&'a str> {
    store
        .behaviors
        .iter()
        .filter(|row| row.agent_did.as_deref() == selected_agent_did)
        .filter_map(|row| row.inference_profile_id.as_deref())
        .collect()
}

fn filter_entity_summaries(entries: Vec<EntitySummary>, filter: &str) -> Vec<EntitySummary> {
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

fn section_meta(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
) -> (&'static str, String) {
    match section {
        OperatorSection::Runtime => (
            "Runtime",
            selected_agent_did
                .and_then(|agent_did| store.latest_runtime(agent_did))
                .and_then(|runtime| runtime.process_state.clone())
                .unwrap_or_else(|| "current behavior, health, loop state".to_string()),
        ),
        OperatorSection::Behaviors => (
            "Behaviors",
            format!(
                "{} profiles",
                store
                    .behaviors
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::Backends => (
            "Backends",
            format!(
                "{} inference backends",
                backend_ids_for_agent(store, selected_agent_did).len()
            ),
        ),
        OperatorSection::ToolSelections => (
            "Tool selections",
            format!(
                "{} presets",
                store
                    .tool_selections
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::InferenceProfiles => (
            "Inference profiles",
            format!(
                "{} profiles",
                inference_profile_ids_for_agent(store, selected_agent_did).len()
            ),
        ),
        OperatorSection::ScheduledTasks => (
            "Scheduled Tasks",
            format!(
                "{} tasks",
                store
                    .scheduled_tasks
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::RequestTimeline => (
            "Request Timeline",
            format!(
                "{} requests",
                store
                    .requests
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == selected_agent_did)
                    .count()
            ),
        ),
        OperatorSection::RecentFailures => (
            "Recent Failures",
            format!(
                "{} failures",
                recent_failure_summaries(store, selected_agent_did).len()
            ),
        ),
    }
}

fn draft_matches_selection(
    draft: &Option<OperatorDraft>,
    draft_source_entity_id: Option<&str>,
    section: OperatorSection,
    selected_entity_id: Option<&str>,
) -> bool {
    match (draft, draft_source_entity_id, selected_entity_id) {
        (Some(draft), Some(source_entity_id), Some(entity_id)) => {
            draft.section() == section && source_entity_id == entity_id
        }
        (None, _, None) => true,
        _ => false,
    }
}

fn draft_for_selection(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
    entity_id: &str,
) -> Option<OperatorDraft> {
    match section {
        OperatorSection::Behaviors => store
            .behaviors
            .iter()
            .find(|row| {
                row.behavior_id == entity_id && row.agent_did.as_deref() == selected_agent_did
            })
            .map(|row| {
                OperatorDraft::Behavior(BehaviorDraft {
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
        OperatorSection::Backends => {
            let backend_ids = backend_ids_for_agent(store, selected_agent_did);
            store
                .inference_backends
                .iter()
                .find(|row| {
                    row.backend_id == entity_id && backend_ids.contains(&row.backend_id.as_str())
                })
                .map(|row| {
                    OperatorDraft::Backend(BackendDraft {
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
                })
        }
        OperatorSection::ToolSelections => store
            .tool_selections
            .iter()
            .find(|row| {
                row.selection_id == entity_id && row.agent_did.as_deref() == selected_agent_did
            })
            .map(|row| {
                OperatorDraft::ToolSelection(ToolSelectionDraft {
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
        OperatorSection::InferenceProfiles => {
            let profile_ids = inference_profile_ids_for_agent(store, selected_agent_did);
            store
                .inference_profiles
                .iter()
                .find(|row| {
                    row.profile_id == entity_id && profile_ids.contains(&row.profile_id.as_str())
                })
                .map(|row| {
                    OperatorDraft::InferenceProfile(InferenceProfileDraft {
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
                })
        }
        OperatorSection::ScheduledTasks => store
            .scheduled_tasks
            .iter()
            .find(|row| row.task_id == entity_id && row.agent_did.as_deref() == selected_agent_did)
            .map(|row| {
                OperatorDraft::ScheduledTask(ScheduledTaskDraft {
                    task_id: row.task_id.clone(),
                    agent_did: row.agent_did.clone().unwrap_or_default(),
                    behavior_id: row.behavior_id.clone().unwrap_or_default(),
                    name: row.name.clone().unwrap_or_default(),
                    prompt: row.prompt.clone().unwrap_or_default(),
                    interval_secs: row
                        .interval_secs
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    enabled: row.enabled.unwrap_or(true),
                    next_run_at: row.next_run_at.clone().unwrap_or_default(),
                    last_run_at: row.last_run_at.clone().unwrap_or_default(),
                    last_status: row.last_status.clone().unwrap_or_default(),
                    last_error: row.last_error.clone().unwrap_or_default(),
                    run_count: row
                        .run_count
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    created_at: row.created_at.clone().unwrap_or_default(),
                    updated_at: row.updated_at.clone().unwrap_or_default(),
                })
            }),
        _ => None,
    }
}

fn render_behavior_editor(ui: &mut Ui, draft: &mut BehaviorDraft) {
    editor_heading(ui, "Behavior");
    text_field(ui, "Behavior ID", &mut draft.behavior_id);
    text_field(ui, "Agent DID", &mut draft.agent_did);
    text_field(ui, "Display Name", &mut draft.display_name);
    multiline_field(ui, "System Prompt", &mut draft.system_prompt, 8);
    text_field(ui, "Backend ID", &mut draft.backend_id);
    text_field(ui, "Model Name", &mut draft.model_name);
    text_field(ui, "Tool Selection ID", &mut draft.tool_selection_id);
    text_field(ui, "Inference Profile ID", &mut draft.inference_profile_id);
    text_field(ui, "Compaction Strategy", &mut draft.compaction_strategy);
    text_field(ui, "Compaction Threshold", &mut draft.compaction_threshold);
    toggle_field(ui, "Enabled", &mut draft.enabled);
}

fn render_backend_editor(ui: &mut Ui, draft: &mut BackendDraft) {
    editor_heading(ui, "Backend");
    text_field(ui, "Backend ID", &mut draft.backend_id);
    text_field(ui, "Name", &mut draft.name);
    text_field(ui, "Provider Kind", &mut draft.provider_kind);
    text_field(ui, "Endpoint", &mut draft.endpoint);
    text_field(ui, "API Key", &mut draft.api_key);
    text_field(ui, "API Key Env Var", &mut draft.api_key_env_var);
    text_field(ui, "Max Concurrent", &mut draft.max_concurrent);
    text_field(ui, "Max Queue Depth", &mut draft.max_queue_depth);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    multiline_field(ui, "Models", &mut draft.models, 4);
    text_field(ui, "Probe Status", &mut draft.probe_status);
}

fn render_tool_selection_editor(ui: &mut Ui, draft: &mut ToolSelectionDraft) {
    editor_heading(ui, "Tool Selection");
    text_field(ui, "Selection ID", &mut draft.selection_id);
    text_field(ui, "Agent DID", &mut draft.agent_did);
    text_field(ui, "Display Name", &mut draft.display_name);
    toggle_field(ui, "Enable File Tools", &mut draft.enable_file_tools);
    text_field(ui, "File Tools Mode", &mut draft.file_tools_mode);
    toggle_field(ui, "Enable Bash", &mut draft.enable_bash);
    text_field(ui, "Bash Mode", &mut draft.bash_mode);
    multiline_field(ui, "CLI Tool Names", &mut draft.cli_tool_names, 4);
    toggle_field(ui, "Enable Meta Tools", &mut draft.enable_meta_tools);
    multiline_field(ui, "Delegate To", &mut draft.delegate_to, 4);
}

fn render_inference_profile_editor(ui: &mut Ui, draft: &mut InferenceProfileDraft) {
    editor_heading(ui, "Inference Profile");
    text_field(ui, "Profile ID", &mut draft.profile_id);
    text_field(ui, "Display Name", &mut draft.display_name);
    text_field(ui, "Context Window", &mut draft.context_window);
    text_field(ui, "Max Output Tokens", &mut draft.max_output_tokens);
    text_field(ui, "Max Turns", &mut draft.max_turns);
    text_field(ui, "Temperature", &mut draft.temperature);
    text_field(ui, "Stream Batch Ms", &mut draft.stream_batch_ms);
    text_field(
        ui,
        "Deadline Duration Secs",
        &mut draft.deadline_duration_secs,
    );
}

fn render_scheduled_task_editor(ui: &mut Ui, draft: &mut ScheduledTaskDraft) {
    editor_heading(ui, "Scheduled Task");
    text_field(ui, "Task ID", &mut draft.task_id);
    text_field(ui, "Agent DID", &mut draft.agent_did);
    text_field(ui, "Behavior ID", &mut draft.behavior_id);
    text_field(ui, "Name", &mut draft.name);
    multiline_field(ui, "Prompt", &mut draft.prompt, 8);
    text_field(ui, "Interval Secs", &mut draft.interval_secs);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    text_field(ui, "Next Run At", &mut draft.next_run_at);
    read_only_field(ui, "Last Run At", draft.last_run_at.as_str());
    read_only_field(ui, "Last Status", draft.last_status.as_str());
    read_only_multiline(ui, "Last Error", draft.last_error.as_str(), 4);
    read_only_field(ui, "Run Count", draft.run_count.as_str());
    read_only_field(ui, "Created At", draft.created_at.as_str());
    read_only_field(ui, "Updated At", draft.updated_at.as_str());
}

fn editor_heading(ui: &mut Ui, title: &str) {
    let palette = theme::palette();
    ui.label(
        RichText::new(title)
            .family(theme::stencil_family())
            .size(13.0)
            .color(palette.text_1)
            .strong(),
    );
    ui.add_space(8.0);
}

fn text_field(ui: &mut Ui, label: &str, value: &mut String) {
    let palette = theme::palette();
    let target = audit::targets::operator_field(label);
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    audit::add(
        ui,
        &target,
        TextEdit::singleline(value)
            .id_source(&target)
            .desired_width(ui.available_width()),
    );
    ui.add_space(6.0);
}

fn multiline_field(ui: &mut Ui, label: &str, value: &mut String, rows: usize) {
    let palette = theme::palette();
    let target = audit::targets::operator_field(label);
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    audit::add_sized(
        ui,
        &target,
        [ui.available_width(), rows as f32 * 18.0 + 12.0],
        TextEdit::multiline(value)
            .id_source(&target)
            .desired_rows(rows),
    );
    ui.add_space(6.0);
}

fn toggle_field(ui: &mut Ui, label: &str, value: &mut bool) {
    let target = audit::targets::operator_toggle(label);
    audit::add(ui, &target, egui::Checkbox::new(value, label));
    ui.add_space(6.0);
}

fn read_only_field(ui: &mut Ui, label: &str, value: &str) {
    let palette = theme::palette();
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    ui.label(
        RichText::new(if value.trim().is_empty() {
            "unset"
        } else {
            value
        })
        .monospace()
        .size(10.5)
        .color(palette.text_1),
    );
    ui.add_space(6.0);
}

fn read_only_multiline(ui: &mut Ui, label: &str, value: &str, rows: usize) {
    let mut value = if value.trim().is_empty() {
        "unset".to_string()
    } else {
        value.to_string()
    };
    let palette = theme::palette();
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    ui.add_sized(
        [ui.available_width(), rows as f32 * 18.0 + 12.0],
        TextEdit::multiline(&mut value)
            .desired_rows(rows)
            .interactive(false),
    );
    ui.add_space(6.0);
}

fn behavior_row(draft: &BehaviorDraft) -> Result<AgentBehaviorRow> {
    Ok(AgentBehaviorRow {
        behavior_id: normalize_required("behavior_id", &draft.behavior_id)?.to_string(),
        agent_did: Some(normalize_required("agent_did", &draft.agent_did)?.to_string()),
        display_name: normalize_optional_owned(&draft.display_name),
        system_prompt: normalize_optional_owned(&draft.system_prompt),
        backend_id: normalize_optional_owned(&draft.backend_id),
        model_name: normalize_optional_owned(&draft.model_name),
        tool_selection_id: normalize_optional_owned(&draft.tool_selection_id),
        inference_profile_id: normalize_optional_owned(&draft.inference_profile_id),
        compaction_strategy: normalize_optional_owned(&draft.compaction_strategy),
        compaction_threshold: parse_optional_f64(
            "compaction_threshold",
            &draft.compaction_threshold,
        )?,
        enabled: Some(draft.enabled),
        created_at: normalize_optional_owned(&draft.created_at),
    })
}

fn backend_row(draft: &BackendDraft) -> Result<InferenceBackendRow> {
    Ok(InferenceBackendRow {
        backend_id: normalize_required("backend_id", &draft.backend_id)?.to_string(),
        name: normalize_optional_owned(&draft.name),
        provider_kind: normalize_optional_owned(&draft.provider_kind),
        endpoint: normalize_optional_owned(&draft.endpoint),
        api_key: normalize_optional_owned(&draft.api_key),
        api_key_env_var: normalize_optional_owned(&draft.api_key_env_var),
        max_concurrent: parse_optional_i64("max_concurrent", &draft.max_concurrent)?,
        max_queue_depth: parse_optional_i64("max_queue_depth", &draft.max_queue_depth)?,
        enabled: Some(draft.enabled),
        models: split_csv(&draft.models),
        last_probe: None,
        probe_status: normalize_optional_owned(&draft.probe_status),
    })
}

fn tool_selection_row(draft: &ToolSelectionDraft) -> Result<ToolSelectionRow> {
    Ok(ToolSelectionRow {
        selection_id: normalize_required("selection_id", &draft.selection_id)?.to_string(),
        agent_did: Some(normalize_required("agent_did", &draft.agent_did)?.to_string()),
        display_name: normalize_optional_owned(&draft.display_name),
        enable_file_tools: Some(draft.enable_file_tools),
        file_tools_mode: normalize_optional_owned(&draft.file_tools_mode),
        enable_bash: Some(draft.enable_bash),
        bash_mode: normalize_optional_owned(&draft.bash_mode),
        cli_tool_names: split_csv(&draft.cli_tool_names),
        enable_meta_tools: Some(draft.enable_meta_tools),
        delegate_to: split_csv(&draft.delegate_to),
    })
}

fn inference_profile_row(draft: &InferenceProfileDraft) -> Result<InferenceProfileRow> {
    Ok(InferenceProfileRow {
        profile_id: normalize_required("profile_id", &draft.profile_id)?.to_string(),
        display_name: normalize_optional_owned(&draft.display_name),
        context_window: parse_optional_i64("context_window", &draft.context_window)?,
        max_output_tokens: parse_optional_i64("max_output_tokens", &draft.max_output_tokens)?,
        max_turns: parse_optional_i64("max_turns", &draft.max_turns)?,
        temperature: parse_optional_f64("temperature", &draft.temperature)?,
        stream_batch_ms: parse_optional_i64("stream_batch_ms", &draft.stream_batch_ms)?,
        deadline_duration_secs: parse_optional_i64(
            "deadline_duration_secs",
            &draft.deadline_duration_secs,
        )?,
    })
}

fn scheduled_task_row(draft: &ScheduledTaskDraft) -> Result<ScheduledTaskRow> {
    Ok(ScheduledTaskRow {
        task_id: normalize_required("task_id", &draft.task_id)?.to_string(),
        agent_did: Some(normalize_required("agent_did", &draft.agent_did)?.to_string()),
        behavior_id: Some(normalize_required("behavior_id", &draft.behavior_id)?.to_string()),
        name: Some(normalize_required("name", &draft.name)?.to_string()),
        prompt: Some(normalize_required("prompt", &draft.prompt)?.to_string()),
        interval_secs: Some(parse_required_positive_i64(
            "interval_secs",
            &draft.interval_secs,
        )?),
        enabled: Some(draft.enabled),
        next_run_at: parse_optional_rfc3339("next_run_at", &draft.next_run_at)?,
        last_run_at: parse_optional_rfc3339("last_run_at", &draft.last_run_at)?,
        last_status: normalize_optional_owned(&draft.last_status),
        last_error: normalize_optional_owned(&draft.last_error),
        run_count: parse_optional_i64("run_count", &draft.run_count)?,
        created_at: parse_optional_rfc3339("created_at", &draft.created_at)?,
        updated_at: parse_optional_rfc3339("updated_at", &draft.updated_at)?,
    })
}

fn normalize_optional_owned(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

fn parse_optional_i64(field: &str, value: &str) -> Result<Option<i64>> {
    match normalize_optional_owned(value) {
        Some(value) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|error| anyhow!("{field} must be an integer: {error}")),
        None => Ok(None),
    }
}

fn parse_required_positive_i64(field: &str, value: &str) -> Result<i64> {
    let value = normalize_required(field, value)?;
    let parsed = value
        .parse::<i64>()
        .map_err(|error| anyhow!("{field} must be an integer: {error}"))?;
    if parsed <= 0 {
        return Err(anyhow!("{field} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_optional_f64(field: &str, value: &str) -> Result<Option<f64>> {
    match normalize_optional_owned(value) {
        Some(value) => value
            .parse::<f64>()
            .map(Some)
            .map_err(|error| anyhow!("{field} must be a number: {error}")),
        None => Ok(None),
    }
}

fn parse_optional_rfc3339(field: &str, value: &str) -> Result<Option<String>> {
    match normalize_optional_owned(value) {
        Some(value) => DateTime::parse_from_rfc3339(&value)
            .map(|parsed| Some(parsed.with_timezone(&Utc).to_rfc3339()))
            .map_err(|error| anyhow!("{field} must be RFC3339: {error}")),
        None => Ok(None),
    }
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn bool_word(value: Option<bool>) -> &'static str {
    if value == Some(true) {
        "on"
    } else {
        "off"
    }
}

fn scheduled_task_is_due(row: &ScheduledTaskRow) -> bool {
    if row.enabled == Some(false) {
        return false;
    }

    match row.next_run_at.as_deref().and_then(parse_task_timestamp) {
        Some(next_run_at) => Utc::now() >= next_run_at,
        None => true,
    }
}

fn scheduled_task_next_run_label(row: &ScheduledTaskRow) -> String {
    match row.next_run_at.as_deref().and_then(parse_task_timestamp) {
        Some(next_run_at) if Utc::now() >= next_run_at => "now".to_string(),
        Some(next_run_at) => next_run_at.format("%Y-%m-%d %H:%MZ").to_string(),
        None => "now".to_string(),
    }
}

fn parse_task_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

fn summarize_request_content(content: &str, fallback_id: &str) -> String {
    let normalized = truncate_line(content, 72);
    if normalized.is_empty() {
        abbreviate_identifier(fallback_id)
    } else {
        normalized
    }
}

fn truncate_line(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let mut truncated = normalized.chars().take(max_chars).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

fn abbreviate_identifier(value: &str) -> String {
    if value.len() <= 10 {
        value.to_string()
    } else {
        format!("{}..{}", &value[..6], &value[value.len() - 2..])
    }
}

fn compact_timestamp(value: &str) -> String {
    parse_task_timestamp(value)
        .map(|timestamp| timestamp.format("%Y-%m-%d %H:%MZ").to_string())
        .unwrap_or_else(|| "time unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_csv_trims_commas_and_lines() {
        assert_eq!(
            split_csv("alpha, beta\n gamma "),
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn parse_optional_numbers_accepts_empty() {
        assert_eq!(parse_optional_i64("x", "").unwrap(), None);
        assert_eq!(parse_optional_f64("x", "").unwrap(), None);
    }

    #[test]
    fn parse_optional_rfc3339_accepts_empty_and_normalizes() {
        assert_eq!(parse_optional_rfc3339("x", "").unwrap(), None);
        assert_eq!(
            parse_optional_rfc3339("x", "2026-04-14T01:02:03+00:00").unwrap(),
            Some("2026-04-14T01:02:03+00:00".to_string())
        );
    }

    #[test]
    fn scheduled_task_due_defaults_true_without_next_run() {
        let row = ScheduledTaskRow {
            task_id: "task-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            name: Some("Task".to_string()),
            prompt: Some("Prompt".to_string()),
            interval_secs: Some(60),
            enabled: Some(true),
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            last_error: None,
            run_count: Some(0),
            created_at: None,
            updated_at: None,
        };

        assert!(scheduled_task_is_due(&row));
        assert_eq!(scheduled_task_next_run_label(&row), "now");
    }
}
