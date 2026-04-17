use defra_agent_protocol::row::ScheduledTaskRow;
use eframe::egui::Ui;

use crate::client::ClientStore;
use crate::state::ShellState;
use crate::views;

use super::editors::{editor_heading, read_only_field, read_only_multiline};
use super::request_timeline::render_request_detail;
use super::shared::{normalize_optional_owned, truncate_line};
use super::EntitySummary;

pub(super) fn recent_failure_summaries(
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
                title: super::shared::summarize_request_content(
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

pub(super) fn render_recent_failure_detail(ui: &mut Ui, state: &ShellState, store: &ClientStore) {
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
