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
    request_id: Option<String>,
}

/// Load the durable completion-delivery state shown by operator status
/// surfaces. Each epoch is one canonical coalescing wake plus its bounded
/// retry descendants; notifications are considered acknowledged only after
/// one attempt in that epoch completes successfully.
pub async fn load_background_completion_diagnostics(
    access: &ConfigAccess,
    agent_did: &str,
) -> Result<BackgroundCompletionDiagnostics> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            wakes: AgentRequest(filter: {{
                agent_did: {{ _eq: "{escaped_agent_did}" }},
                execution_origin: {{ _eq: "scheduled" }}
            }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}], limit: {WAKE_SCAN_LIMIT}) {{
                _docID request_id session_id retry_root_request input lifecycle_state
                failure_reason created_at claimed_at terminalized_at
                retry_count max_retries background_completion_input_through_sequence
                background_completion_notification_keys_json
            }}
            notifications: AgentMessage(filter: {{
                agent_did: {{ _eq: "{escaped_agent_did}" }},
                message_key: {{ _like: "background-completion-notification:%" }}
            }}, order: {{ timestamp: DESC }}, limit: {NOTIFICATION_SCAN_LIMIT}) {{
                message_key request_id
            }}
        }}"#
    );
    access
        .transact("background_completion.diagnostics", |txn| {
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
                let mut heads = BTreeMap::new();
                for session_id in sessions {
                    // The diagnostic scan is bounded, but authority comes from all
                    // actual scoped requests, including ordinary later user turns.
                    if let Some(head) =
                        crate::session::load_latest_request_in_txn(txn, agent_did, session_id, None)
                            .await?
                    {
                        heads.insert(session_id.to_owned(), head.observed.request_doc_id);
                    }
                }
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

    fn input() -> gents_protocol::request_input::RequestInput {
        serde_json::from_value(serde_json::json!({"queue":{"source":"background_completion","policy":"coalesce","key":"parent-1","queued_after_request_id":"parent-1","background_completion_wake_version":1}})).unwrap()
    }

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
            input: Some(input()),
            doc_id: Some(format!("physical-{request_id}")),
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

    fn head(request_id: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("session-1".to_owned(), format!("physical-{request_id}"))])
    }

    #[test]
    fn failed_epoch_surfaces_backoff_and_unacknowledged_notification() {
        let now = DateTime::parse_from_rfc3339("2026-08-12T00:00:06Z")
            .unwrap()
            .with_timezone(&Utc);
        let diagnostics = summarize(
            vec![wake("wake-1", None, "failed", 0, 3)],
            vec![notification("wake-1", "child-1")],
            head("wake-1"),
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
            head("wake-2"),
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
            head("wake-1"),
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
            head("wake-2"),
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
            head("later-interactive-request"),
            now,
        );

        assert_eq!(diagnostics.pending_notifications, 1);
        assert_eq!(diagnostics.stranded_notifications, 1);
        let epoch = &diagnostics.epochs[0];
        assert_eq!(epoch.state, "retry_ineligible_not_latest");
        assert_eq!(epoch.next_retry_at, None);
        assert_eq!(epoch.pending_notification_keys.len(), 1);
    }
    #[test]
    fn retry_eligibility_requires_the_exact_physical_request_head() {
        let failed = wake("same-logical-id", None, "failed", 0, 3);
        let authoritative_heads =
            BTreeMap::from([("session-1".to_owned(), "different-physical-row".to_owned())]);
        let diagnostics = summarize(
            vec![failed],
            vec![notification("same-logical-id", "child-1")],
            authoritative_heads,
            Utc::now(),
        );
        assert_eq!(diagnostics.epochs[0].state, "retry_ineligible_not_latest");
        assert_eq!(diagnostics.epochs[0].next_retry_at, None);
        assert_eq!(diagnostics.stranded_notifications, 1);
    }

    #[test]
    fn scan_limits_remain_visible_even_when_non_wake_requests_are_filtered() {
        let mut ordinary = wake("ordinary", None, "pending", 0, 3);
        ordinary.input = None;
        let diagnostics = summarize(
            vec![ordinary; WAKE_SCAN_LIMIT],
            vec![],
            BTreeMap::new(),
            Utc::now(),
        );
        assert_eq!(diagnostics.scanned_wakes, WAKE_SCAN_LIMIT);
        assert!(diagnostics.scan_truncated);
        assert!(diagnostics.epochs.is_empty());
    }

    #[tokio::test]
    async fn diagnostics_reads_actual_scoped_head_across_requesters_without_session_cache(
    ) -> Result<()> {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await?);
        node.add_schema(gents_protocol::schemas::AGENT_REQUEST)
            .await?;
        node.add_schema(gents_protocol::schemas::AGENT_MESSAGE)
            .await?;
        let access = ConfigAccess::Local(node.clone());
        // An escaped owner exercises the distinction between raw lookup scope
        // and GraphQL spelling. No AgentSession/AgentConversation cache exists.
        let owner = "did:test:diagnostics\"owner";
        let mut failed = serde_json::json!({
            "agent_did":owner, "requester_did":"requester-a", "request_id":"wake", "session_id":"shared-session",
            "behavior_id":"behavior", "input":input(), "execution_origin":"scheduled",
            "lifecycle_state":"failed", "created_at":"2026-08-12T00:00:00Z",
            "terminalized_at":"2026-08-12T00:00:05Z", "retry_count":0, "max_retries":3
        });
        for value in [
            failed.clone(),
            serde_json::json!({
                "agent_did":"foreign-owner", "requester_did":"requester-z", "request_id":"foreign-later",
                "session_id":"shared-session", "behavior_id":"behavior", "execution_origin":"interactive",
                "lifecycle_state":"completed", "created_at":"2026-08-14T00:00:00Z"
            }),
        ] {
            access
                .write(
                    "test.diagnostic.request",
                    &format!(
                        "mutation {{ create_AgentRequest(input:{}){{_docID}} }}",
                        gents_protocol::graphql::graphql_input_literal(&value)?
                    ),
                )
                .await?;
        }
        access.write("test.diagnostic.notification", &format!("mutation {{ create_AgentMessage(input:{}){{_docID}} }}", gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "agent_did":owner,"session_id":"shared-session","message_key":"background-completion-notification:child:subagent",
            "request_id":"wake","role":"user","content":"completed tool","sequence":1,"timestamp":"2026-08-12T00:00:01Z"
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
                "test.diagnostic.later-turn",
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
