use chrono::{DateTime, Local, Utc};
use defra_agent_protocol::row::AgentConversationRow;

use crate::client::{ClientPeerStatus, ClientStore};
use crate::state::ShellState;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BehaviorEntry {
    pub represented_behavior_id: Option<String>,
    pub override_behavior_id: Option<String>,
    pub label: String,
    pub meta: String,
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
            title: conversation_title(conversation),
            meta: conversation
                .behavior_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|behavior_id| format!("behavior {behavior_id}"))
                .unwrap_or_else(|| "default behavior".to_string()),
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

pub(super) fn behavior_selection_entries(
    store: &ClientStore,
    agent_did: &str,
) -> Vec<BehaviorEntry> {
    let default_behavior_id = store.default_behavior_id_for_agent(agent_did);
    let mut rows = store.behavior_rows(agent_did);
    rows.sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));

    let default_entry = if let Some(default_behavior_id) = default_behavior_id {
        if let Some(default_row) = rows
            .iter()
            .find(|row| row.behavior_id == default_behavior_id)
            .copied()
        {
            BehaviorEntry {
                represented_behavior_id: Some(default_behavior_id.to_string()),
                override_behavior_id: None,
                label: simple_behavior_label(
                    default_row.display_name.as_deref(),
                    Some(default_behavior_id),
                ),
                meta: behavior_meta(store, default_row.tool_selection_id.as_deref(), true),
            }
        } else {
            BehaviorEntry {
                represented_behavior_id: Some(default_behavior_id.to_string()),
                override_behavior_id: None,
                label: default_behavior_id.to_string(),
                meta: "default".to_string(),
            }
        }
    } else {
        BehaviorEntry {
            represented_behavior_id: None,
            override_behavior_id: None,
            label: "Inherited default".to_string(),
            meta: "default".to_string(),
        }
    };

    let mut entries = vec![default_entry];

    for row in rows {
        if Some(row.behavior_id.as_str()) == default_behavior_id {
            continue;
        }

        entries.push(BehaviorEntry {
            represented_behavior_id: Some(row.behavior_id.clone()),
            override_behavior_id: Some(row.behavior_id.clone()),
            label: simple_behavior_label(
                row.display_name.as_deref(),
                Some(row.behavior_id.as_str()),
            ),
            meta: behavior_meta(store, row.tool_selection_id.as_deref(), false),
        });
    }

    entries
}

pub(super) fn effective_behavior_id(
    state: &ShellState,
    store: &ClientStore,
    selected_agent_did: Option<&str>,
) -> Option<String> {
    state
        .chat
        .shell
        .selected_session_id
        .as_deref()
        .and_then(|session_id| store.session_behavior_id(session_id, selected_agent_did))
        .or_else(|| state.chat.editor.selected_behavior_override.clone())
        .or_else(|| {
            selected_agent_did.and_then(|agent_did| {
                store
                    .default_behavior_id_for_agent(agent_did)
                    .map(ToOwned::to_owned)
            })
        })
}

pub(super) fn display_name_for_agent(store: &ClientStore, agent_did: &str) -> String {
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

fn abbreviate_address(value: &str) -> String {
    if value.len() <= 18 {
        return value.to_string();
    }

    format!("{}..{}", &value[..10], &value[value.len() - 4..])
}

fn conversation_title(conversation: &AgentConversationRow) -> String {
    conversation
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| {
            conversation
                .preview_text
                .as_deref()
                .map(str::trim)
                .filter(|preview| !preview.is_empty())
        })
        .unwrap_or("New Conversation")
        .to_string()
}

pub(super) fn simple_behavior_label(
    display_name: Option<&str>,
    behavior_id: Option<&str>,
) -> String {
    match (
        display_name
            .map(str::trim)
            .filter(|display_name| !display_name.is_empty()),
        behavior_id
            .map(str::trim)
            .filter(|behavior_id| !behavior_id.is_empty()),
    ) {
        (Some(display_name), _) => display_name.to_string(),
        (_, Some(behavior_id)) => behavior_id.to_string(),
        _ => "Inherited default".to_string(),
    }
}

fn behavior_meta(store: &ClientStore, tool_selection_id: Option<&str>, is_default: bool) -> String {
    let mut parts = vec![if is_default { "default" } else { "behavior" }.to_string()];
    let hints = tool_hints(store, tool_selection_id);
    if !hints.is_empty() {
        parts.push(hints.join(" "));
    }
    parts.join("  ")
}

fn tool_hints(store: &ClientStore, tool_selection_id: Option<&str>) -> Vec<&'static str> {
    let Some(selection_id) = tool_selection_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };
    let Some(selection) = store
        .tool_selections
        .iter()
        .find(|row| row.selection_id == selection_id)
    else {
        return Vec::new();
    };

    let mut hints = Vec::new();
    if selection.enable_file_tools.unwrap_or(false) {
        hints.push("F");
    }
    if selection.enable_bash.unwrap_or(false) || !selection.cli_tool_names.is_empty() {
        hints.push("B");
    }
    if selection.enable_meta_tools.unwrap_or(false) {
        hints.push("M");
    }
    hints
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
    use super::behavior_selection_entries;
    use crate::client::{ClientStore, ClientStoreRows};
    use defra_agent_protocol::row::{AgentBehaviorRow, AgentPrincipalRow};

    #[test]
    fn behavior_selection_entries_keep_inherited_default_without_explicit_default_id() {
        let store = ClientStore::from_rows(ClientStoreRows {
            agent_principals: vec![AgentPrincipalRow {
                agent_did: "did:defra:amy".to_string(),
                display_name: Some("Amy".to_string()),
                default_behavior_id: None,
                enabled: Some(true),
                created_at: None,
                created_by: None,
            }],
            behaviors: vec![AgentBehaviorRow {
                behavior_id: "amy-alt".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                display_name: Some("Amy Alt".to_string()),
                system_prompt: None,
                backend_id: None,
                model_name: None,
                tool_selection_id: None,
                inference_profile_id: None,
                compaction_strategy: None,
                compaction_threshold: None,
                enabled: Some(true),
                created_at: None,
            }],
            ..ClientStoreRows::default()
        });

        let entries = behavior_selection_entries(&store, "did:defra:amy");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].represented_behavior_id, None);
        assert_eq!(entries[0].override_behavior_id, None);
        assert_eq!(entries[0].label, "Inherited default");
        assert_eq!(entries[0].meta, "default");
    }
}
