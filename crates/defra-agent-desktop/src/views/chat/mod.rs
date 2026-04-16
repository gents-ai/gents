mod composer;
mod header;
mod sidebar;
mod transcript;

use chrono::{DateTime, Local, Utc};
use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::AgentConversationRow;
use eframe::egui::{self, RichText, Ui};
use egui_commonmark::CommonMarkCache;
use tokio::runtime::Runtime;

use crate::audit;
use crate::chat::controller;
use crate::chat::projection::project_chat;
use crate::client::{ClientCore, ClientPeerStatus, ClientStore};
use crate::state::ShellState;
use crate::theme;
use crate::views;

pub use transcript::markdown_theme_names;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentEntry {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub agent_label: String,
    pub addr: String,
    pub connected: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationEntry {
    pub session_id: String,
    pub title: String,
    pub meta: String,
    pub timestamp_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationBucket {
    pub label: &'static str,
    pub entries: Vec<ConversationEntry>,
}

pub fn prepare_state(
    state: &mut ShellState,
    _client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        return;
    };

    if let Some(agent_did) = state.chat.selected_agent_did.clone() {
        state.status.active_agent = display_name_for_agent(store, &agent_did);
        state.status.runtime_state = store
            .latest_runtime(&agent_did)
            .and_then(|runtime| runtime.process_state.as_deref())
            .unwrap_or("observing")
            .to_string();
    } else {
        state.status.active_agent = "no agent selected".to_string();
        state.status.runtime_state = "idle".to_string();
        state.chat.selected_session_id = None;
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    let palette = theme::palette();

    let Some(store) = store else {
        views::card(
            ui,
            "Chat Unavailable",
            "The desktop client must finish bootstrapping before replicated chat data can render.",
        );
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);
    let selected_agent = state.chat.selected_agent_did.clone();
    let selected_session = state.chat.selected_session_id.clone();
    let conversations = selected_agent
        .as_deref()
        .map(|agent_did| {
            build_conversation_buckets(&store.conversation_rows(agent_did), Utc::now())
        })
        .unwrap_or_default();

    sidebar::show(
        ui,
        palette,
        state,
        client,
        store,
        runtime,
        &deployments,
        &conversations,
        selected_agent.as_deref(),
        selected_session.as_deref(),
    );
}

pub fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
    markdown_cache: &mut CommonMarkCache,
) {
    let Some(store) = store else {
        views::card(
            ui,
            "Chat Unavailable",
            "The local replica is offline. Bootstrap must succeed before the chat activity can render.",
        );
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let projection = project_chat(&state.chat, store, &peer_statuses, client.is_some());
    let selected_agent_did = projection.selected_agent_did.clone();
    let selected_session_id = projection.selected_session_id.clone();
    let turn_state = projection.turn_state;
    let send_status = projection.send_status;
    let show_first_conversation_nudge = projection.show_first_conversation_nudge;

    egui::Panel::bottom("chat_composer_panel")
        .resizable(false)
        .exact_size(208.0)
        .show_inside(ui, |ui| {
            composer::show(
                ui,
                state,
                client,
                store,
                runtime,
                selected_agent_did.as_deref(),
                turn_state,
                send_status,
            );
        });
    ui.vertical(|ui| {
        header::show(
            ui,
            state,
            header::HeaderProps {
                store,
                client,
                runtime,
                selected_agent_did: selected_agent_did.as_deref(),
                selected_session_id: selected_session_id.as_deref(),
                turn_state,
            },
        );
        ui.add_space(12.0);
        if show_first_conversation_nudge {
            render_first_conversation_nudge(
                ui,
                state,
                client,
                runtime,
                selected_agent_did.as_deref(),
            );
        } else {
            transcript::show(
                ui,
                state,
                store,
                selected_session_id.as_deref(),
                turn_state,
                markdown_cache,
            );
        }
    });
}

pub fn build_deployment_entries(
    peer_statuses: &[ClientPeerStatus],
    store: &ClientStore,
) -> Vec<DeploymentEntry> {
    let mut entries: Vec<_> = peer_statuses
        .iter()
        .map(|status| DeploymentEntry {
            peer_id: status.peer_id.clone(),
            label: status.label.clone(),
            agent_did: status.agent_did.clone(),
            agent_label: display_name_for_agent(store, &status.agent_did),
            addr: abbreviate_address(&status.addr),
            connected: status.dial_succeeded,
            warning: status.last_error.clone(),
        })
        .collect();

    entries.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.peer_id.cmp(&right.peer_id))
    });
    entries
}

pub fn build_conversation_buckets(
    conversations: &[&AgentConversationRow],
    now: DateTime<Utc>,
) -> Vec<ConversationBucket> {
    let local_now = now.with_timezone(&Local);
    let today = local_now.date_naive();
    let yesterday = today - chrono::Duration::days(1);

    let mut today_entries = Vec::new();
    let mut yesterday_entries = Vec::new();
    let mut earlier_entries = Vec::new();

    for conversation in conversations {
        let timestamp = conversation
            .updated_at
            .as_deref()
            .or(conversation.created_at.as_deref())
            .and_then(parse_timestamp);
        let entry = ConversationEntry {
            session_id: conversation.session_id.clone(),
            title: conversation
                .title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or("New Conversation")
                .to_string(),
            meta: format!("session {}", abbreviate_id(&conversation.session_id)),
            timestamp_label: timestamp
                .map(|timestamp| relative_timestamp_label(local_now, timestamp))
                .unwrap_or_else(|| "unknown".to_string()),
        };

        match timestamp.map(|timestamp| timestamp.date_naive()) {
            Some(date) if date == today => today_entries.push(entry),
            Some(date) if date == yesterday => yesterday_entries.push(entry),
            _ => earlier_entries.push(entry),
        }
    }

    let mut buckets = Vec::new();
    if !today_entries.is_empty() {
        buckets.push(ConversationBucket {
            label: "TODAY",
            entries: today_entries,
        });
    }
    if !yesterday_entries.is_empty() {
        buckets.push(ConversationBucket {
            label: "YESTERDAY",
            entries: yesterday_entries,
        });
    }
    if !earlier_entries.is_empty() {
        buckets.push(ConversationBucket {
            label: "EARLIER",
            entries: earlier_entries,
        });
    }
    buckets
}

pub fn send_disabled(
    client_available: bool,
    selected_agent_did: Option<&str>,
    composer_text: &str,
    turn_state: Option<ClientTurnState>,
) -> bool {
    !client_available
        || selected_agent_did.is_none()
        || composer_text.trim().is_empty()
        || turn_state.is_some_and(|turn| !turn.is_terminal())
}

pub fn turn_state_label(turn_state: Option<ClientTurnState>) -> &'static str {
    match turn_state {
        Some(ClientTurnState::WaitingForClaim) => "waiting for claim",
        Some(ClientTurnState::Streaming) => "streaming",
        Some(ClientTurnState::Completed) => "completed",
        Some(ClientTurnState::Failed) => "failed",
        Some(ClientTurnState::Superseded) => "superseded",
        None => "idle",
    }
}

fn render_first_conversation_nudge(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
    selected_agent_did: Option<&str>,
) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Create Conversation")
                .family(theme::stencil_family())
                .size(16.0)
                .color(palette.text_0)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Create a conversation explicitly when this agent has no observed sessions yet. This avoids hiding snapshot lag behind automatic local state repair.",
            )
            .size(13.0)
            .color(palette.text_1)
            .line_height(Some(18.0)),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let can_create = client.is_some() && selected_agent_did.is_some();
            if audit::add_enabled(
                ui,
                audit::targets::CHAT_CREATE_CONVERSATION,
                can_create,
                egui::Button::new("Create Conversation"),
            )
            .clicked()
            {
                let _ = selected_agent_did;
                match controller::create_conversation(&mut state.chat, client, runtime) {
                    Ok(()) => {}
                    Err(error) => {
                        state.chat.last_submission_error = Some(error.to_string());
                    }
                }
            }

            if let Some(agent_did) = selected_agent_did {
                ui.label(
                    RichText::new(format!("target {agent_did}"))
                        .monospace()
                        .size(11.0)
                        .color(palette.text_2),
                );
            }
        });
    });
}

fn display_name_for_agent(store: &ClientStore, agent_did: &str) -> String {
    store
        .agent_principals
        .iter()
        .find(|row| row.agent_did == agent_did)
        .and_then(|row| row.display_name.as_deref())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            agent_did
                .rsplit(':')
                .next()
                .filter(|segment| !segment.trim().is_empty())
                .unwrap_or(agent_did)
                .to_string()
        })
}

fn abbreviate_id(value: &str) -> String {
    if value.len() <= 8 {
        return value.to_string();
    }

    format!("{}..{}", &value[..4], &value[value.len() - 2..])
}

fn abbreviate_address(value: &str) -> String {
    if value.len() <= 18 {
        return value.to_string();
    }

    format!("{}..{}", &value[..10], &value[value.len() - 4..])
}

fn parse_timestamp(value: &str) -> Option<chrono::DateTime<Local>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Local))
}

fn relative_timestamp_label(
    now: chrono::DateTime<Local>,
    timestamp: chrono::DateTime<Local>,
) -> String {
    let delta = now.signed_duration_since(timestamp);
    if delta.num_minutes() < 1 {
        "now".to_string()
    } else if delta.num_hours() < 1 {
        format!("{}m ago", delta.num_minutes())
    } else if delta.num_days() < 1 {
        format!("{}h ago", delta.num_hours())
    } else if delta.num_days() == 1 {
        "yesterday".to_string()
    } else {
        timestamp.format("%Y-%m-%d").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::client::{ClientCoreOptions, DesktopPaths};

    #[test]
    fn create_first_conversation_selects_new_session() -> anyhow::Result<()> {
        let runtime = Runtime::new()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let principal_resp = runtime.block_on(core.node().execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:amy"
                    display_name: "Amy"
                    default_behavior_id: "amy-default"
                    enabled: true
                }) { agent_did }
            }"#,
        ));
        assert!(!principal_resp.has_errors());

        let mut state = ShellState::default();
        state.chat.selected_agent_did = Some("did:defra:amy".to_string());
        controller::create_conversation(&mut state.chat, Some(&core), &runtime)?;

        assert!(state.chat.selected_session_id.is_some());
        assert_eq!(core.store().snapshot().conversations.len(), 1);
        runtime.block_on(core.shutdown())?;
        Ok(())
    }
}
