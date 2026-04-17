use chrono::{DateTime, Local, Utc};
use defra_agent_protocol::row::AgentConversationRow;

use crate::client::{ClientPeerStatus, ClientStore};

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
    pub behavior_id: Option<String>,
    pub label: String,
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

    let mut entries = vec![BehaviorEntry {
        behavior_id: None,
        label: default_behavior_id
            .map(|behavior_id| format!("Default ({behavior_id})"))
            .unwrap_or_else(|| "Inherited default".to_string()),
    }];

    for row in rows {
        if Some(row.behavior_id.as_str()) == default_behavior_id {
            continue;
        }

        entries.push(BehaviorEntry {
            behavior_id: Some(row.behavior_id.clone()),
            label: behavior_label(row.display_name.as_deref(), Some(row.behavior_id.as_str())),
        });
    }

    entries
}

pub(super) fn display_behavior_label(
    store: &ClientStore,
    agent_did: &str,
    behavior_id: Option<&str>,
) -> String {
    match behavior_id {
        Some(behavior_id) => store
            .behavior_row(agent_did, behavior_id)
            .map(|row| behavior_label(row.display_name.as_deref(), Some(behavior_id)))
            .unwrap_or_else(|| behavior_id.to_string()),
        None => store
            .default_behavior_id_for_agent(agent_did)
            .map(|behavior_id| format!("Default ({behavior_id})"))
            .unwrap_or_else(|| "Inherited default".to_string()),
    }
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

fn behavior_label(display_name: Option<&str>, behavior_id: Option<&str>) -> String {
    match (
        display_name
            .map(str::trim)
            .filter(|display_name| !display_name.is_empty()),
        behavior_id
            .map(str::trim)
            .filter(|behavior_id| !behavior_id.is_empty()),
    ) {
        (Some(display_name), Some(behavior_id)) if display_name != behavior_id => {
            format!("{display_name} ({behavior_id})")
        }
        (Some(display_name), _) => display_name.to_string(),
        (_, Some(behavior_id)) => behavior_id.to_string(),
        _ => "Inherited default".to_string(),
    }
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
