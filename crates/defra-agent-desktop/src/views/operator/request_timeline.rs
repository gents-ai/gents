use defra_agent_protocol::row::AgentRequestRow;
use eframe::egui::Ui;

use crate::client::ClientStore;
use crate::state::ShellState;
use crate::views;

use super::editors::{editor_heading, read_only_field, read_only_multiline};
use super::shared::{abbreviate_identifier, compact_timestamp, summarize_request_content};
use super::EntitySummary;

pub(super) fn request_timeline_summaries(
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

pub(super) fn render_request_timeline_detail(ui: &mut Ui, state: &ShellState, store: &ClientStore) {
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

pub(super) fn render_request_detail(ui: &mut Ui, store: &ClientStore, request: &AgentRequestRow) {
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
