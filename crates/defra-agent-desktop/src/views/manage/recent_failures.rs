use defra_agent_protocol::row::ScheduleRow;
use eframe::egui::Ui;

use crate::client::ClientStore;
use crate::state::ShellState;
use crate::views;

use super::editors::{editor_heading, read_only_field, read_only_multiline};
use super::request_timeline::render_request_detail;
pub(super) fn render_recent_failure_detail(ui: &mut Ui, state: &ShellState, store: &ClientStore) {
    editor_heading(ui, "Failure Detail");
    let Some(selected_id) = state.manage.selected_entity_id.as_deref() else {
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

    // `schedule:` failures are surfaced by the Recent Failures list
    // once a `Schedule` records a non-empty `last_error`. `task:`
    // failures are no longer emitted because the legacy
    // `ScheduledTask` collection is gone; `Task` documents do not
    // carry run bookkeeping (see Task 53 for the schedule view).
    if let Some(schedule_id) = selected_id.strip_prefix("schedule:") {
        if let Some(schedule) = store
            .schedules
            .iter()
            .find(|row| row.schedule_id == schedule_id)
        {
            render_schedule_failure_detail(ui, store, schedule);
            return;
        }
    }

    views::card(
        ui,
        "Failure Missing",
        "The selected failure record is no longer present in the local replica.",
    );
}

fn render_schedule_failure_detail(ui: &mut Ui, store: &ClientStore, schedule: &ScheduleRow) {
    read_only_field(ui, "Schedule ID", &schedule.schedule_id);
    read_only_field(
        ui,
        "Task ID",
        schedule.task_id.as_deref().unwrap_or("unset"),
    );

    let task = schedule
        .task_id
        .as_deref()
        .and_then(|task_id| store.tasks.iter().find(|row| row.task_id == task_id));
    read_only_field(
        ui,
        "Task Name",
        task.and_then(|task| task.name.as_deref())
            .unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Behavior ID",
        task.and_then(|task| task.behavior_id.as_deref())
            .unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Last Status",
        schedule.last_status.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Last Attempt At",
        schedule.last_attempt_at.as_deref().unwrap_or("unset"),
    );
    read_only_field(
        ui,
        "Next Run At",
        schedule.next_run_at.as_deref().unwrap_or("unset"),
    );
    read_only_multiline(
        ui,
        "Last Error",
        schedule.last_error.as_deref().unwrap_or(""),
        4,
    );
    read_only_multiline(
        ui,
        "Prompt Template",
        task.and_then(|task| task.prompt_template.as_deref())
            .unwrap_or(""),
        6,
    );
}
