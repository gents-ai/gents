use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;

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
    request_doc_id: Option<String>,
}

/// Load the durable completion-delivery state shown by operator status
/// surfaces. Each epoch is one canonical coalescing wake plus its bounded
/// retry descendants; notifications are considered acknowledged only after
/// one attempt in that epoch completes successfully.
pub async fn load_background_completion_diagnostics(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<BackgroundCompletionDiagnostics> {
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            wakes: AgentRequest(filter: {{
                node_did: {{ _eq: "{escaped_node_did}" }},
                execution_origin: {{ _eq: "scheduled" }}
            }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}], limit: {WAKE_SCAN_LIMIT}) {{
                _docID request_id session_id retry_root_request input lifecycle_state
                failure_reason created_at claimed_at terminalized_at
                retry_count max_retries background_completion_input_through_sequence
                background_completion_notification_keys_json
            }}
            notifications: AgentMessage(filter: {{
                node_did: {{ _eq: "{escaped_node_did}" }},
                message_key: {{ _like: "background-completion-notification:%" }}
            }}, order: {{ created_at: DESC }}, limit: {NOTIFICATION_SCAN_LIMIT}) {{
                message_key request_doc_id
            }}
        }}"#
    );
    access
        .transact_readonly("background_completion.diagnostics", |txn| {
            let query = &query;
            Box::pin(async move {
                let response = txn.execute(query).await?;
                let data = response
                    .get("data")
                    .context("background completion diagnostics has no data")?;
                let wakes: Vec<AgentRequestRow> = serde_json::from_value(
                    data.get("wakes")
                        .cloned()
                        .context("diagnostics omitted wakes")?,
                )
                .context("decoding background completion wakes")?;
                let notifications: Vec<NotificationRow> = serde_json::from_value(
                    data.get("notifications")
                        .cloned()
                        .context("diagnostics omitted notifications")?,
                )
                .context("decoding background completion notifications")?;
                let mut sessions = BTreeSet::new();
                for wake in &wakes {
                    let session_id = wake
                        .session_id
                        .as_deref()
                        .filter(|id| !id.trim().is_empty())
                        .with_context(|| {
                            format!(
                                "background completion wake {} has no session_id",
                                wake.request_id
                            )
                        })?;
                    if wake
                        .input
                        .as_ref()
                        .is_some_and(crate::lifecycle::is_background_completion_request)
                    {
                        sessions.insert(session_id);
                    }
                }
                let session_ids = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
                let heads =
                    crate::session::load_latest_requests_in_txn(txn, node_did, &session_ids, None)
                        .await?
                        .into_iter()
                        .map(|(session_id, head)| (session_id, head.observed.request_doc_id))
                        .collect::<BTreeMap<_, _>>();
                Ok(summarize(wakes, notifications, heads, Utc::now()))
            })
        })
        .await
}

fn summarize(
    wakes: Vec<AgentRequestRow>,
    notifications: Vec<NotificationRow>,
    latest_request_by_session: BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> BackgroundCompletionDiagnostics {
    let scanned_wakes = wakes.len();
    let scanned_notifications = notifications.len();
    let wakes = wakes
        .into_iter()
        .filter(|wake| {
            wake.input
                .as_ref()
                .is_some_and(crate::lifecycle::is_background_completion_request)
        })
        .collect::<Vec<_>>();
    let roots_by_request_doc = wakes
        .iter()
        .filter_map(|wake| {
            wake.doc_id
                .as_ref()
                .map(|doc_id| (doc_id.clone(), retry_root(wake).to_string()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut notifications_by_root = BTreeMap::<String, BTreeSet<String>>::new();
    let mut notifications_by_request_doc = BTreeMap::<String, BTreeSet<String>>::new();
    for notification in notifications {
        if !crate::background_completion::is_background_completion_notification_message_key(
            &notification.message_key,
        ) {
            continue;
        }
        let Some(root) = notification
            .request_doc_id
            .as_deref()
            .and_then(|request_doc_id| roots_by_request_doc.get(request_doc_id))
        else {
            continue;
        };
        notifications_by_root
            .entry(root.clone())
            .or_default()
            .insert(notification.message_key.clone());
        notifications_by_request_doc
            .entry(notification.request_doc_id.unwrap_or_default())
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
                notifications_by_request_doc
                    .get(wake.doc_id.as_deref().unwrap_or_default())
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
            .is_some_and(|request_doc_id| latest.doc_id.as_ref() == Some(request_doc_id));
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
                coalescing_key: latest
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .and_then(|queue| queue.key.clone())
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
        wake.input
            .as_ref()
            .and_then(|input| input.queue.as_ref())
            .and_then(|queue| queue.key.clone())
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
        Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
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

    fn input() -> gents_protocol::request_input::RequestInput {
        serde_json::from_value(serde_json::json!({"queue":{"source":"background_completion","policy":"coalesce","key":"parent-1","queued_after_request_id":"parent-1","background_completion_wake_version":1}})).unwrap()
    }

    #[tokio::test]
    async fn diagnostics_reads_actual_scoped_head_across_requesters_without_session_cache(
    ) -> Result<()> {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await?);
        node.add_schema(gents_protocol::schemas::AGENT_REQUEST)
            .await?;
        node.add_schema(gents_protocol::schemas::AGENT_MESSAGE)
            .await?;
        let access = ConfigAccess::local(node.clone());
        // An escaped owner exercises the distinction between raw lookup scope
        // and GraphQL spelling. Diagnostics read authoritative documents
        // directly and maintain no session cache.
        let owner = "did:test:diagnostics\"owner";
        let mut failed = serde_json::json!({
            "node_did":owner, "requester_did":"requester-a", "request_id":"wake", "session_id":"shared-session",
            "purpose":"normal",
            "agent_id":"general", "input":input(), "execution_origin":"scheduled",
            "lifecycle_state":"failed", "created_at":"2026-08-12T00:00:00Z",
            "terminalized_at":"2026-08-12T00:00:05Z", "retry_count":0, "max_retries":3
        });
        let mut wake_doc_id = None;
        for value in [
            failed.clone(),
            serde_json::json!({
                "node_did":"foreign-owner", "requester_did":"requester-z", "request_id":"foreign-later",
                "purpose":"normal",
                "session_id":"shared-session", "agent_id":"general", "execution_origin":"interactive",
                "lifecycle_state":"completed", "created_at":"2026-08-14T00:00:00Z"
            }),
        ] {
            let created = access
                .write(
                    "test.diagnostic.request",
                    &format!(
                        "mutation {{ create_AgentRequest(input:{}){{_docID}} }}",
                        gents_protocol::graphql::graphql_input_literal(&value)?
                    ),
                )
                .await?;
            if value["request_id"] == "wake" {
                wake_doc_id = Some(crate::graphql::created_doc_id(&created, "AgentRequest")?);
            }
        }
        access.write("test.diagnostic.notification", &format!("mutation {{ create_AgentMessage(input:{}){{_docID}} }}", gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "node_did":owner,"session_id":"shared-session","message_key":"background-completion-notification:child:agent",
            "request_doc_id":wake_doc_id.context("wake not created")?,"role":"user","sequence":1,"created_at":"2026-08-12T00:00:01Z"
        }))?)).await?;
        let diagnostics = load_background_completion_diagnostics(&access, owner).await?;
        assert_eq!(diagnostics.epochs.len(), 1);
        assert_eq!(diagnostics.epochs[0].state, "retry_backoff");
        assert!(diagnostics.epochs[0].next_retry_at.is_some());
        assert_eq!(diagnostics.pending_notifications, 1);

        failed["request_id"] = "later-user-turn".into();
        failed["requester_did"] = "requester-b".into();
        failed["execution_origin"] = "interactive".into();
        failed["created_at"] = "2026-08-13T00:00:00Z".into();
        failed["lifecycle_state"] = "completed".into();
        failed.as_object_mut().unwrap().remove("input");
        access
            .write(
                "test.diagnostic.later_turn",
                &format!(
                    "mutation {{ create_AgentRequest(input:{}){{_docID}} }}",
                    gents_protocol::graphql::graphql_input_literal(&failed)?
                ),
            )
            .await?;
        let diagnostics = load_background_completion_diagnostics(&access, owner).await?;
        assert_eq!(
            diagnostics.scanned_wakes, 1,
            "the ordinary head is outside the wake scan"
        );
        assert_eq!(diagnostics.epochs[0].state, "retry_ineligible_not_latest");
        assert_eq!(diagnostics.epochs[0].next_retry_at, None);
        assert_eq!(diagnostics.stranded_notifications, 1);
        Ok(())
    }
}
