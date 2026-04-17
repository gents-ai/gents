use eframe::egui::{RichText, Ui};

use crate::client::ClientStore;
use crate::theme;
use crate::views;

use super::editors::editor_heading;
use super::shared::{
    abbreviate_identifier, compact_timestamp, scheduled_task_next_run_label,
    summarize_request_content,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SummaryLine {
    title: String,
    meta: String,
}

pub(super) fn render_behavior_context(
    ui: &mut Ui,
    store: &ClientStore,
    agent_did: &str,
    behavior_id: &str,
) {
    let behavior_id = behavior_id.trim();
    if behavior_id.is_empty() {
        return;
    }

    let tasks = scheduled_task_lines(store, agent_did, behavior_id);
    let conversations = conversation_lines(store, agent_did, behavior_id);
    let requests = request_lines(store, agent_did, behavior_id);

    ui.add_space(14.0);
    editor_heading(ui, "Behavior Activity");
    render_counts(ui, tasks.len(), conversations.len(), requests.len());

    render_summary_block(
        ui,
        "Scheduled Tasks",
        &tasks,
        "No scheduled tasks are bound to this behavior.",
    );
    render_summary_block(
        ui,
        "Conversations",
        &conversations,
        "No conversations have been observed for this behavior yet.",
    );
    render_summary_block(
        ui,
        "Recent Requests",
        &requests,
        "No requests have been observed for this behavior yet.",
    );
}

fn render_counts(ui: &mut Ui, task_count: usize, conversation_count: usize, request_count: usize) {
    let palette = theme::palette();
    ui.horizontal_wrapped(|ui| {
        for line in [
            format!("{task_count} tasks"),
            format!("{conversation_count} conversations"),
            format!("{request_count} requests"),
        ] {
            ui.label(
                RichText::new(line)
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
            );
        }
    });
    ui.add_space(8.0);
}

fn render_summary_block(ui: &mut Ui, title: &str, lines: &[SummaryLine], empty_message: &str) {
    views::section_kicker(ui, title);
    ui.add_space(4.0);
    if lines.is_empty() {
        views::card(ui, title, empty_message);
        ui.add_space(8.0);
        return;
    }

    ui.group(|ui| {
        for line in lines {
            ui.label(
                RichText::new(&line.title)
                    .size(12.0)
                    .color(theme::palette().text_1),
            );
            ui.label(
                RichText::new(&line.meta)
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().text_2),
            );
            ui.add_space(6.0);
        }
    });
    ui.add_space(8.0);
}

fn scheduled_task_lines(
    store: &ClientStore,
    agent_did: &str,
    behavior_id: &str,
) -> Vec<SummaryLine> {
    let mut rows = store.scheduled_tasks_for_behavior(agent_did, behavior_id);
    rows.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.task_id.cmp(&left.task_id))
    });

    rows.into_iter()
        .take(6)
        .map(|row| SummaryLine {
            title: row.name.clone().unwrap_or_else(|| row.task_id.clone()),
            meta: format!(
                "{}  next {}  runs {}",
                row.last_status.as_deref().unwrap_or("idle"),
                scheduled_task_next_run_label(row),
                row.run_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            ),
        })
        .collect()
}

fn conversation_lines(store: &ClientStore, agent_did: &str, behavior_id: &str) -> Vec<SummaryLine> {
    store
        .conversations_for_behavior(agent_did, behavior_id)
        .into_iter()
        .take(6)
        .map(|row| SummaryLine {
            title: row
                .title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or("New Conversation")
                .to_string(),
            meta: format!(
                "{}  session {}  {}",
                row.status.as_deref().unwrap_or("active"),
                abbreviate_identifier(&row.session_id),
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

fn request_lines(store: &ClientStore, agent_did: &str, behavior_id: &str) -> Vec<SummaryLine> {
    let mut rows = store.requests_for_behavior(agent_did, behavior_id);
    rows.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.request_id.cmp(&left.request_id))
    });

    rows.into_iter()
        .take(6)
        .map(|row| SummaryLine {
            title: summarize_request_content(
                row.content.as_deref().unwrap_or_default(),
                &row.request_id,
            ),
            meta: format!(
                "{}  session {}  {}",
                row.lifecycle_state.as_deref().unwrap_or("pending"),
                abbreviate_identifier(row.session_id.as_deref().unwrap_or("none")),
                compact_timestamp(row.created_at.as_deref().unwrap_or("")),
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientStoreRows;
    use defra_agent_protocol::row::{AgentConversationRow, AgentRequestRow, ScheduledTaskRow};

    #[test]
    fn behavior_context_filters_rows_by_behavior() {
        let store = ClientStore::from_rows(ClientStoreRows {
            conversations: vec![
                AgentConversationRow {
                    session_id: "session-match".to_string(),
                    agent_name: None,
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    title: Some("Matched".to_string()),
                    preview_text: None,
                    status: Some("completed".to_string()),
                    created_at: Some("2026-04-17T00:00:00Z".to_string()),
                    updated_at: Some("2026-04-17T00:01:00Z".to_string()),
                    latest_request_id: Some("req-match".to_string()),
                },
                AgentConversationRow {
                    session_id: "session-other".to_string(),
                    agent_name: None,
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-alt".to_string()),
                    title: Some("Other".to_string()),
                    preview_text: None,
                    status: Some("completed".to_string()),
                    created_at: Some("2026-04-17T00:00:00Z".to_string()),
                    updated_at: Some("2026-04-17T00:01:00Z".to_string()),
                    latest_request_id: Some("req-other".to_string()),
                },
            ],
            requests: vec![
                AgentRequestRow {
                    request_id: "req-match".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    session_id: Some("session-match".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("hello from default".to_string()),
                    status: None,
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: None,
                    failure_reason: None,
                    created_at: Some("2026-04-17T00:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: None,
                    max_retries: None,
                },
                AgentRequestRow {
                    request_id: "req-other".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-alt".to_string()),
                    session_id: Some("session-other".to_string()),
                    retry_parent_request: None,
                    retry_root_request: None,
                    superseded_by_request: None,
                    content: Some("hello from alt".to_string()),
                    status: None,
                    lifecycle_state: Some("completed".to_string()),
                    backend_id: None,
                    execution_origin: None,
                    failure_reason: None,
                    created_at: Some("2026-04-17T00:00:00Z".to_string()),
                    claimed_at: None,
                    deadline: None,
                    retry_count: None,
                    max_retries: None,
                },
            ],
            scheduled_tasks: vec![
                ScheduledTaskRow {
                    task_id: "task-match".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-default".to_string()),
                    name: Some("Matched Task".to_string()),
                    prompt: Some("run".to_string()),
                    interval_secs: Some(60),
                    enabled: Some(true),
                    next_run_at: Some("2026-04-17T00:30:00Z".to_string()),
                    last_run_at: None,
                    last_status: Some("armed".to_string()),
                    last_error: None,
                    run_count: Some(2),
                    created_at: None,
                    updated_at: Some("2026-04-17T00:01:00Z".to_string()),
                },
                ScheduledTaskRow {
                    task_id: "task-other".to_string(),
                    agent_did: Some("did:defra:amy".to_string()),
                    behavior_id: Some("amy-alt".to_string()),
                    name: Some("Other Task".to_string()),
                    prompt: Some("run".to_string()),
                    interval_secs: Some(60),
                    enabled: Some(true),
                    next_run_at: Some("2026-04-17T00:30:00Z".to_string()),
                    last_run_at: None,
                    last_status: Some("armed".to_string()),
                    last_error: None,
                    run_count: Some(2),
                    created_at: None,
                    updated_at: Some("2026-04-17T00:01:00Z".to_string()),
                },
            ],
            ..ClientStoreRows::default()
        });

        let conversations = conversation_lines(&store, "did:defra:amy", "amy-default");
        let requests = request_lines(&store, "did:defra:amy", "amy-default");
        let tasks = scheduled_task_lines(&store, "did:defra:amy", "amy-default");

        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].title, "Matched");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].title.contains("hello from default"));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Matched Task");
    }
}
