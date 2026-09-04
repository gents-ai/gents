use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::lifecycle::queue::parse_queue_hints;

const WAKE_SCAN_LIMIT: usize = 1024;
const NOTIFICATION_SCAN_LIMIT: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundCompletionDiagnostics {
    pub scanned_wakes: usize,
    pub scanned_notifications: usize,
    pub scan_truncated: bool,
    pub pending_notifications: usize,
    pub acknowledged_notifications: usize,
    pub stranded_notifications: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_pending_age_seconds: Option<i64>,
    pub epochs: Vec<BackgroundCompletionEpochDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundCompletionEpochDiagnostic {
    pub root_request_id: String,
    pub active_request_id: String,
    pub session_id: String,
    pub coalescing_key: String,
    pub state: String,
    pub attempt_count: i64,
    pub retry_count: i64,
    pub max_retries: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_through_sequence: Option<i64>,
    pub notification_count: usize,
    pub acknowledged_notification_count: usize,
    pub attempted_notification_keys: Vec<String>,
    pub acknowledged_notification_keys: Vec<String>,
    pub pending_notification_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_age_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminalized_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct NotificationRow {
    message_key: String,
    request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    latest_request_id: String,
    updated_at: String,
    title: String,
    preview_text: String,
}

impl ConversationRow {
    fn rank(&self) -> (String, usize, String) {
        let richness = [
            self.title.trim(),
            self.preview_text.trim(),
            self.latest_request_id.trim(),
        ]
        .iter()
        .filter(|field| !field.is_empty())
        .count();
        (self.updated_at.clone(), richness, self.doc_id.clone())
    }
}

/// Load the durable completion-delivery state shown by operator status
/// surfaces. Each epoch is one canonical coalescing wake plus its bounded
/// retry descendants; notifications are considered acknowledged only after
/// one attempt in that epoch completes successfully.
pub async fn load_background_completion_diagnostics(
    access: &ConfigAccess,
    agent_did: &str,
) -> Result<BackgroundCompletionDiagnostics> {
    let agent_did = escape_graphql_string(agent_did);
    let response = access
        .execute(&format!(
            r#"{{
                wakes: AgentRequest(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}], limit: {WAKE_SCAN_LIMIT}) {{
                    request_id session_id retry_root_request metadata lifecycle_state
                    failure_reason created_at claimed_at terminalized_at
                    retry_count max_retries background_completion_input_through_sequence
                    background_completion_notification_keys_json
                }}
                notifications: AgentMessage(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    message_key: {{ _like: "background-completion-notification:%" }}
                }}, order: {{ timestamp: DESC }}, limit: {NOTIFICATION_SCAN_LIMIT}) {{
                    message_key request_id
                }}
                conversations: AgentConversation(filter: {{
                    agent_did: {{ _eq: "{agent_did}" }}
                }}) {{
                    _docID session_id latest_request_id updated_at title preview_text
                }}
            }}"#
        ))
        .await?;
    let data = response
        .get("data")
        .context("background completion diagnostics has no data")?;
    let wakes: Vec<AgentRequestRow> = serde_json::from_value(
        data.get("wakes")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .context("decoding background completion wakes")?;
    for wake in &wakes {
        if wake.session_id.is_none() {
            anyhow::bail!(
                "background completion wake {} has no session_id",
                wake.request_id
            );
        }
    }
    let notifications: Vec<NotificationRow> = serde_json::from_value(
        data.get("notifications")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .context("decoding background completion notifications")?;
    let conversations: Vec<ConversationRow> = serde_json::from_value(
        data.get("conversations")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .context("decoding background completion conversations")?;
    Ok(summarize(wakes, notifications, conversations, Utc::now()))
}

fn summarize(
    wakes: Vec<AgentRequestRow>,
    notifications: Vec<NotificationRow>,
    conversations: Vec<ConversationRow>,
    now: DateTime<Utc>,
) -> BackgroundCompletionDiagnostics {
    let scanned_wakes = wakes.len();
    let scanned_notifications = notifications.len();
    let wakes = wakes
        .into_iter()
        .filter(|wake| crate::lifecycle::is_background_completion_request(wake.metadata.as_deref()))
        .collect::<Vec<_>>();
    let roots_by_request = wakes
        .iter()
        .map(|wake| (wake.request_id.clone(), retry_root(wake).to_string()))
        .collect::<BTreeMap<_, _>>();
    let mut notifications_by_root = BTreeMap::<String, BTreeSet<String>>::new();
    let mut notifications_by_request = BTreeMap::<String, BTreeSet<String>>::new();
    for notification in notifications {
        if !crate::background_completion::is_background_completion_notification_message_key(
            &notification.message_key,
        ) {
            continue;
        }
        let Some(root) = notification
            .request_id
            .as_deref()
            .and_then(|request_id| roots_by_request.get(request_id))
        else {
            continue;
        };
        notifications_by_root
            .entry(root.clone())
            .or_default()
            .insert(notification.message_key.clone());
        notifications_by_request
            .entry(notification.request_id.unwrap_or_default())
            .or_default()
            .insert(notification.message_key);
    }

    let mut acknowledged_by_identity = BTreeMap::<(String, String), BTreeSet<String>>::new();
    let mut attempted_by_root = BTreeMap::<String, BTreeSet<String>>::new();
    for wake in &wakes {
        let identity = wake_identity(wake);
        let root = retry_root(wake).to_string();
        let mut attempted = snapshot_keys(wake);
        if attempted.is_empty() {
            attempted.extend(
                notifications_by_request
                    .get(&wake.request_id)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
        attempted_by_root
            .entry(root)
            .or_default()
            .extend(attempted.iter().cloned());
        if request_completed(wake) {
            acknowledged_by_identity
                .entry(identity)
                .or_default()
                .extend(attempted);
        }
    }

    let mut wakes_by_root = BTreeMap::<String, Vec<AgentRequestRow>>::new();
    for wake in wakes {
        wakes_by_root
            .entry(retry_root(&wake).to_string())
            .or_default()
            .push(wake);
    }
    let mut conversations_by_session = BTreeMap::<String, Vec<ConversationRow>>::new();
    for conversation in conversations {
        conversations_by_session
            .entry(conversation.session_id.clone())
            .or_default()
            .push(conversation);
    }
    let latest_request_by_session = conversations_by_session
        .into_iter()
        .filter_map(|(session_id, mut rows)| {
            rows.sort_by(|left, right| right.rank().cmp(&left.rank()));
            rows.first()
                .map(|row| (session_id, row.latest_request_id.clone()))
        })
        .collect::<BTreeMap<_, _>>();

    let mut diagnostics = BackgroundCompletionDiagnostics {
        scanned_wakes,
        scanned_notifications,
        scan_truncated: scanned_wakes >= WAKE_SCAN_LIMIT
            || scanned_notifications >= NOTIFICATION_SCAN_LIMIT,
        ..Default::default()
    };
    for (root_request_id, mut chain) in wakes_by_root {
        chain.sort_by(|left, right| wake_rank(left).cmp(&wake_rank(right)));
        let latest = chain.last().expect("wake chain is non-empty");
        let notification_keys = notifications_by_root
            .get(&root_request_id)
            .cloned()
            .unwrap_or_default();
        let notification_count = notification_keys.len();
        let acknowledged_keys = acknowledged_by_identity
            .get(&wake_identity(latest))
            .cloned()
            .unwrap_or_default();
        let acknowledged_notification_keys = notification_keys
            .intersection(&acknowledged_keys)
            .cloned()
            .collect::<Vec<_>>();
        let pending_notification_keys = notification_keys
            .difference(&acknowledged_keys)
            .cloned()
            .collect::<Vec<_>>();
        let acknowledged_notification_count = acknowledged_notification_keys.len();
        let pending_notification_count = notification_count - acknowledged_notification_count;
        let completed_in_chain = chain.iter().any(request_completed);
        let acknowledged =
            notification_count > 0 && acknowledged_notification_count == notification_count;
        let pending_age_seconds = (!acknowledged)
            .then(|| parse_time(chain.first().and_then(|wake| wake.created_at.as_deref())))
            .flatten()
            .map(|created_at| now.signed_duration_since(created_at).num_seconds().max(0));
        let retry_count = latest.retry_count.unwrap_or_default().max(0);
        let max_retries = latest.max_retries.unwrap_or_default().max(0);
        let latest_failed = request_failed(latest);
        let retry_is_latest = latest_request_by_session
            .get(latest.session_id.as_deref().expect("validated session_id"))
            .is_some_and(|request_id| request_id == &latest.request_id);
        let next_retry = (latest_failed && retry_count < max_retries && retry_is_latest)
            .then(|| {
                crate::background_wake_next_retry_at(latest.terminalized_at.as_deref(), retry_count)
            })
            .flatten();
        let state = if acknowledged && completed_in_chain {
            "acknowledged"
        } else if acknowledged {
            "acknowledged_by_successor"
        } else if request_active(latest) {
            "active"
        } else if request_pending(latest) && retry_count > 0 {
            "retry_scheduled"
        } else if request_pending(latest) {
            "pending"
        } else if latest_failed && retry_count >= max_retries {
            "exhausted"
        } else if latest_failed && !retry_is_latest {
            "retry_ineligible_not_latest"
        } else if latest_failed {
            "retry_backoff"
        } else {
            "terminal_unacknowledged"
        };
        let stranded = pending_notification_count > 0
            && matches!(
                state,
                "exhausted" | "retry_ineligible_not_latest" | "terminal_unacknowledged"
            );

        diagnostics.pending_notifications += pending_notification_count;
        diagnostics.acknowledged_notifications += acknowledged_notification_count;
        diagnostics.stranded_notifications += usize::from(stranded) * pending_notification_count;
        diagnostics.oldest_pending_age_seconds =
            match (diagnostics.oldest_pending_age_seconds, pending_age_seconds) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (None, Some(age)) => Some(age),
                (current, None) => current,
            };
        diagnostics
            .epochs
            .push(BackgroundCompletionEpochDiagnostic {
                root_request_id: root_request_id.clone(),
                active_request_id: latest.request_id.clone(),
                session_id: latest.session_id.clone().expect("validated session_id"),
                coalescing_key: parse_queue_hints(latest.metadata.as_deref())
                    .and_then(|hints| hints.key)
                    .unwrap_or_default(),
                state: state.to_string(),
                attempt_count: retry_count + 1,
                retry_count,
                max_retries,
                input_through_sequence: latest.background_completion_input_through_sequence,
                notification_count,
                acknowledged_notification_count,
                attempted_notification_keys: attempted_by_root
                    .remove(&root_request_id)
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                acknowledged_notification_keys,
                pending_notification_keys,
                pending_age_seconds,
                last_failure: chain
                    .iter()
                    .rev()
                    .find(|wake| request_failed(wake))
                    .and_then(|wake| non_empty(wake.failure_reason.clone())),
                next_retry_at: next_retry.map(|timestamp| timestamp.to_rfc3339()),
                created_at: latest.created_at.clone(),
                claimed_at: latest.claimed_at.clone(),
                terminalized_at: latest.terminalized_at.clone(),
            });
    }
    diagnostics.epochs.sort_by(|left, right| {
        right
            .pending_age_seconds
            .cmp(&left.pending_age_seconds)
            .then_with(|| left.root_request_id.cmp(&right.root_request_id))
    });
    diagnostics
}

fn retry_root(wake: &AgentRequestRow) -> &str {
    wake.retry_root_request
        .as_deref()
        .map(str::trim)
        .filter(|root| !root.is_empty())
        .unwrap_or(&wake.request_id)
}

fn wake_identity(wake: &AgentRequestRow) -> (String, String) {
    (
        wake.session_id.clone().expect("validated session_id"),
        parse_queue_hints(wake.metadata.as_deref())
            .and_then(|hints| hints.key)
            .unwrap_or_default(),
    )
}

fn snapshot_keys(wake: &AgentRequestRow) -> BTreeSet<String> {
    let Some(json) = wake.background_completion_notification_keys_json.as_deref() else {
        return BTreeSet::new();
    };
    serde_json::from_str::<Vec<String>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter(|key| {
            crate::background_completion::is_background_completion_notification_message_key(key)
        })
        .collect()
}

fn wake_rank(wake: &AgentRequestRow) -> (i64, &str, &str) {
    (
        wake.retry_count.unwrap_or_default(),
        wake.created_at.as_deref().unwrap_or(""),
        &wake.request_id,
    )
}

fn wake_lifecycle_state(wake: &AgentRequestRow) -> Option<RequestLifecycleState> {
    wake.lifecycle_state
}

fn request_completed(wake: &AgentRequestRow) -> bool {
    wake_lifecycle_state(wake) == Some(RequestLifecycleState::Completed)
}

fn request_failed(wake: &AgentRequestRow) -> bool {
    wake_lifecycle_state(wake) == Some(RequestLifecycleState::Failed)
}

fn request_pending(wake: &AgentRequestRow) -> bool {
    wake_lifecycle_state(wake) == Some(RequestLifecycleState::Pending)
}

fn request_active(wake: &AgentRequestRow) -> bool {
    matches!(
        wake_lifecycle_state(wake),
        Some(
            RequestLifecycleState::Claimed
                | RequestLifecycleState::Processing
                | RequestLifecycleState::InputRequired
        )
    )
}

fn parse_time(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const METADATA: &str = r#"{"queue":{"source":"background_completion","policy":"coalesce","key":"parent-1","queued_after_request_id":"parent-1"},"background_completion_wake_version":1}"#;

    fn wake(
        request_id: &str,
        root: Option<&str>,
        state: &str,
        retry_count: i64,
        max_retries: i64,
    ) -> AgentRequestRow {
        AgentRequestRow {
            request_id: request_id.to_string(),
            session_id: Some("session-1".to_string()),
            retry_root_request: root.map(ToOwned::to_owned),
            metadata: Some(METADATA.to_string()),
            lifecycle_state: Some(
                RequestLifecycleState::parse(state).expect("valid test lifecycle state"),
            ),
            failure_reason: (state == "failed").then(|| "provider unavailable".to_string()),
            created_at: Some(format!("2026-08-12T00:00:0{retry_count}Z")),
            claimed_at: (state != "pending").then(|| "2026-08-12T00:00:03Z".to_string()),
            terminalized_at: (state == "failed").then(|| "2026-08-12T00:00:05Z".to_string()),
            retry_count: Some(retry_count),
            max_retries: Some(max_retries),
            background_completion_input_through_sequence: Some(1),
            background_completion_notification_keys_json: Some(
                r#"["background-completion-notification:child-1:subagent"]"#.to_string(),
            ),
            ..Default::default()
        }
    }

    fn notification(request_id: &str, suffix: &str) -> NotificationRow {
        NotificationRow {
            message_key: format!("background-completion-notification:{suffix}:subagent"),
            request_id: Some(request_id.to_string()),
        }
    }

    fn conversation(latest_request_id: &str) -> ConversationRow {
        ConversationRow {
            doc_id: "conversation-1".to_string(),
            session_id: "session-1".to_string(),
            latest_request_id: latest_request_id.to_string(),
            updated_at: "2026-08-12T00:00:06Z".to_string(),
            title: String::new(),
            preview_text: String::new(),
        }
    }

    #[test]
    fn failed_epoch_surfaces_backoff_and_unacknowledged_notification() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:00:06Z")
            .unwrap()
            .with_timezone(&Utc);
        let diagnostics = summarize(
            vec![wake("wake-1", None, "failed", 0, 3)],
            vec![notification("wake-1", "child-1")],
            vec![conversation("wake-1")],
            now,
        );

        assert_eq!(diagnostics.pending_notifications, 1);
        assert_eq!(diagnostics.acknowledged_notifications, 0);
        assert_eq!(diagnostics.stranded_notifications, 0);
        assert_eq!(diagnostics.oldest_pending_age_seconds, Some(6));
        let epoch = &diagnostics.epochs[0];
        assert_eq!(epoch.state, "retry_backoff");
        assert_eq!(epoch.attempt_count, 1);
        assert_eq!(epoch.last_failure.as_deref(), Some("provider unavailable"));
        assert_eq!(
            epoch.next_retry_at.as_deref(),
            Some("2026-08-12T00:00:10+00:00")
        );
    }

    #[test]
    fn completed_retry_acknowledges_the_whole_epoch() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let diagnostics = summarize(
            vec![
                wake("wake-1", None, "failed", 0, 3),
                wake("wake-2", Some("wake-1"), "completed", 1, 3),
            ],
            vec![notification("wake-1", "child-1")],
            vec![conversation("wake-2")],
            now,
        );

        assert_eq!(diagnostics.pending_notifications, 0);
        assert_eq!(diagnostics.acknowledged_notifications, 1);
        assert_eq!(diagnostics.stranded_notifications, 0);
        let epoch = &diagnostics.epochs[0];
        assert_eq!(epoch.state, "acknowledged");
        assert_eq!(epoch.active_request_id, "wake-2");
        assert_eq!(epoch.attempt_count, 2);
        assert_eq!(epoch.acknowledged_notification_count, 1);
    }

    #[test]
    fn exhausted_epoch_is_stranded() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let diagnostics = summarize(
            vec![wake("wake-1", None, "failed", 3, 3)],
            vec![notification("wake-1", "child-1")],
            vec![conversation("wake-1")],
            now,
        );

        assert_eq!(diagnostics.stranded_notifications, 1);
        assert_eq!(diagnostics.epochs[0].state, "exhausted");
        assert_eq!(diagnostics.epochs[0].next_retry_at, None);
    }

    #[test]
    fn completed_successor_epoch_acknowledges_failed_active_epoch_input() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let first = wake("wake-1", None, "failed", 0, 3);
        let mut successor = wake("wake-2", None, "completed", 0, 3);
        successor.background_completion_notification_keys_json = Some(
            serde_json::to_string(&vec![
                "background-completion-notification:child-1:subagent",
                "background-completion-notification:child-2:subagent",
            ])
            .unwrap(),
        );
        let diagnostics = summarize(
            vec![first, successor],
            vec![
                notification("wake-1", "child-1"),
                notification("wake-2", "child-2"),
            ],
            vec![conversation("wake-2")],
            now,
        );

        assert_eq!(diagnostics.pending_notifications, 0);
        assert_eq!(diagnostics.acknowledged_notifications, 2);
        let first = diagnostics
            .epochs
            .iter()
            .find(|epoch| epoch.root_request_id == "wake-1")
            .unwrap();
        assert_eq!(first.state, "acknowledged_by_successor");
        assert_eq!(first.acknowledged_notification_count, 1);
        assert!(first.pending_notification_keys.is_empty());
    }

    #[test]
    fn failed_wake_displaced_by_later_turn_is_stranded_not_retrying() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let diagnostics = summarize(
            vec![wake("wake-1", None, "failed", 0, 3)],
            vec![notification("wake-1", "child-1")],
            vec![conversation("later-interactive-request")],
            now,
        );

        assert_eq!(diagnostics.pending_notifications, 1);
        assert_eq!(diagnostics.stranded_notifications, 1);
        let epoch = &diagnostics.epochs[0];
        assert_eq!(epoch.state, "retry_ineligible_not_latest");
        assert_eq!(epoch.next_retry_at, None);
        assert_eq!(epoch.pending_notification_keys.len(), 1);
    }
}
