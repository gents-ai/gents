//! Background subagent completion projection.
//!
//! R4b keeps background spawns non-blocking by leaving the parent bridge row
//! running until the child request reaches a terminal state. This module owns
//! the observer path that projects that terminal state into the parent
//! `AgentToolCall`, appends a compact transcript notification, and enqueues the
//! coalesced same-session wake-up request.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use defra_node::{EmbeddedNode, EventName};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::background_tools::{
    child_request_completed, load_authorized_child_edge, load_child_final_response,
    load_child_terminal_row, load_parent_subagent_context, project_child_terminal, ChildEdge,
};
use crate::graphql::escape_graphql_string;
use crate::lifecycle::queue::{
    enqueue_session_request, parse_queue_hints, QueueHints, QueuePolicy, QueueSource,
};
use crate::lifecycle::ExecutionOrigin;
use crate::session;
use crate::tool_call_lifecycle::{AwaitMode, ChildTerminal, ToolCallLifecycle};
use crate::watcher::{validate_agent_request_subagent_coherence, AgentRequest};

const AGENT_REQUEST_COLLECTION: &str = "AgentRequest";
const BACKGROUND_COMPLETION_WAKE_PROMPT: &str =
    "Review pending subagent completion notifications in this session and continue the task if needed.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundCompletionOutcome {
    Projected {
        child_request_id: String,
        parent_request_id: String,
        parent_tool_call_id: String,
        parent_session_id: String,
        notification_sequence: u32,
        wake_request_id: String,
    },
    NotTerminal,
    NotBackground,
    MissingFinalResponse,
    AlreadyProjected,
    Unlinked,
}

pub async fn project_background_subagent_completion(
    node: Arc<EmbeddedNode>,
    child_request_id: &str,
) -> Result<BackgroundCompletionOutcome> {
    let Some(linkage) = load_child_linkage(node.as_ref(), child_request_id).await? else {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    };
    let Some(parent_request_id) = non_empty(linkage.caused_by_parent_request_id.as_deref()) else {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    };
    if non_empty(linkage.caused_by_parent_tool_call_id.as_deref()).is_none() {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    }

    let Some(terminal_row) = load_child_terminal_row(node.as_ref(), child_request_id).await? else {
        return Ok(BackgroundCompletionOutcome::Unlinked);
    };
    let completed = child_request_completed(&terminal_row);
    let terminal = if completed {
        None
    } else {
        let Some(terminal) = project_child_terminal(&terminal_row) else {
            return Ok(BackgroundCompletionOutcome::NotTerminal);
        };
        Some(terminal)
    };

    let parent_context = load_parent_subagent_context(node.as_ref(), parent_request_id).await?;
    let edge = load_authorized_child_edge(node.as_ref(), &parent_context, child_request_id).await?;
    if edge.await_mode != AwaitMode::Background {
        return Ok(BackgroundCompletionOutcome::NotBackground);
    }

    let (status, summary, bridge_result, terminal) = if completed {
        let Some(final_response) =
            load_projected_final_response(node.as_ref(), &parent_context.session_id, &edge).await?
        else {
            return Ok(BackgroundCompletionOutcome::MissingFinalResponse);
        };
        let summary = compact_summary(&final_response);
        ("completed".to_string(), summary, Some(final_response), None)
    } else {
        let terminal = terminal.expect("non-completed child terminal checked above");
        let status = child_terminal_status(&terminal).to_string();
        let summary = child_terminal_summary(&terminal);
        (status, summary, None, Some(terminal))
    };

    let mut transitioned = false;
    if edge.lifecycle_state == "running" {
        let mut lifecycle = match ToolCallLifecycle::load(
            node.clone(),
            &parent_context.session_id,
            &edge.parent_tool_call_id,
        )
        .await?
        {
            Some(lifecycle) => lifecycle,
            None => return Ok(BackgroundCompletionOutcome::Unlinked),
        };

        transitioned = match (bridge_result.clone(), terminal.clone()) {
            (Some(final_response), None) => lifecycle.bridge_complete(final_response).await?,
            (None, Some(terminal)) => lifecycle.bridge_failure(terminal).await?,
            _ => false,
        };
    } else if !bridge_state_is_terminal(&edge.lifecycle_state) {
        return Ok(BackgroundCompletionOutcome::AlreadyProjected);
    }

    let side_effects = ensure_projection_side_effects(
        node.as_ref(),
        &parent_context.session_id,
        &parent_context.request_id,
        &edge,
        &status,
        &summary,
    )
    .await?;

    let outcome = if transitioned || side_effects.created_notification || side_effects.created_wake
    {
        BackgroundCompletionOutcome::Projected {
            child_request_id: edge.child_request_id,
            parent_request_id: parent_context.request_id,
            parent_tool_call_id: edge.parent_tool_call_id,
            parent_session_id: parent_context.session_id,
            notification_sequence: side_effects.notification_sequence,
            wake_request_id: side_effects.wake_request_id,
        }
    } else {
        BackgroundCompletionOutcome::AlreadyProjected
    };
    Ok(outcome)
}

async fn load_projected_final_response(
    node: &EmbeddedNode,
    parent_session_id: &str,
    edge: &ChildEdge,
) -> Result<Option<String>> {
    if let Some(final_response) = load_child_final_response(node, edge).await? {
        return Ok(Some(final_response));
    }
    if edge.lifecycle_state == "completed" {
        return match session::load_tool_call_result(
            node,
            parent_session_id,
            &edge.parent_tool_call_id,
        )
        .await
        {
            Ok(result) if !result.trim().is_empty() => Ok(Some(result)),
            Ok(_) => Ok(None),
            Err(error) => Err(error),
        };
    }
    Ok(None)
}

struct SideEffects {
    notification_sequence: u32,
    wake_request_id: String,
    created_notification: bool,
    created_wake: bool,
}

pub(crate) async fn append_background_tool_completion(
    node: &EmbeddedNode,
    parent_session_id: &str,
    parent_request_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
) -> Result<()> {
    let (notification_timestamp, created_notification) =
        match existing_tool_completion_notification(node, parent_session_id, tool_call_id).await? {
            Some(existing) => (existing.timestamp, false),
            None => {
                let notification =
                    render_tool_completion(tool_call_id, tool_name, status, result, reason);
                let sequence =
                    session::append_message(node, parent_session_id, "user", &notification).await?;
                let timestamp = load_message_timestamp(node, parent_session_id, sequence).await?;
                (timestamp, true)
            }
        };

    let queue_key = format!("background_completion:{parent_session_id}");
    if existing_wakeup_after(node, parent_session_id, &queue_key, &notification_timestamp)
        .await?
        .is_some()
    {
        return Ok(());
    }

    let parent_request = load_agent_request_for_queue(node, parent_request_id)
        .await?
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} not found"))?;
    let _wake = enqueue_session_request(
        node,
        &parent_request,
        BACKGROUND_COMPLETION_WAKE_PROMPT,
        ExecutionOrigin::Scheduled,
        QueueHints {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(queue_key),
            queued_after_request_id: Some(parent_request_id.to_string()),
        },
    )
    .await?;

    if created_notification {
        tracing::debug!(
            parent_session_id,
            parent_request_id,
            tool_call_id,
            "appended background tool completion notification"
        );
    }
    Ok(())
}

async fn ensure_projection_side_effects(
    node: &EmbeddedNode,
    parent_session_id: &str,
    parent_request_id: &str,
    edge: &ChildEdge,
    status: &str,
    summary: &str,
) -> Result<SideEffects> {
    let (notification_sequence, notification_timestamp, created_notification) =
        match existing_notification(node, parent_session_id, &edge.child_request_id).await? {
            Some(existing) => (existing.sequence, existing.timestamp, false),
            None => {
                let notification = render_notification(edge, status, summary);
                let sequence =
                    session::append_message(node, parent_session_id, "user", &notification).await?;
                let timestamp = load_message_timestamp(node, parent_session_id, sequence).await?;
                (sequence, timestamp, true)
            }
        };

    let queue_key = format!("background_completion:{parent_session_id}");
    if let Some(wake_request_id) =
        existing_wakeup_after(node, parent_session_id, &queue_key, &notification_timestamp).await?
    {
        return Ok(SideEffects {
            notification_sequence,
            wake_request_id,
            created_notification,
            created_wake: false,
        });
    }

    let parent_request = load_agent_request_for_queue(node, parent_request_id)
        .await?
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} not found"))?;
    let wake = enqueue_session_request(
        node,
        &parent_request,
        BACKGROUND_COMPLETION_WAKE_PROMPT,
        ExecutionOrigin::Scheduled,
        QueueHints {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(queue_key),
            queued_after_request_id: Some(parent_request_id.to_string()),
        },
    )
    .await?;

    Ok(SideEffects {
        notification_sequence,
        wake_request_id: wake.request_id,
        created_notification,
        created_wake: true,
    })
}

fn bridge_state_is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "timedOut" | "cancelled")
}

struct ExistingNotification {
    sequence: u32,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct NotificationMessageRow {
    sequence: u32,
    content: String,
    timestamp: String,
}

async fn existing_notification(
    node: &EmbeddedNode,
    parent_session_id: &str,
    child_request_id: &str,
) -> Result<Option<ExistingNotification>> {
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                content
                timestamp
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent AgentMessage notifications for session {parent_session_id} failed: {:?}",
            response.errors
        );
    }

    let marker = format!(
        r#"child_request_id="{}""#,
        xml_escape_attr(child_request_id)
    );
    let rows: Vec<NotificationMessageRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    for row in rows {
        if row.content.contains("<subagent-notification") && row.content.contains(&marker) {
            return Ok(Some(ExistingNotification {
                sequence: row.sequence,
                timestamp: parse_utc_timestamp(&row.timestamp, "AgentMessage.timestamp")?,
            }));
        }
    }

    Ok(None)
}

async fn existing_tool_completion_notification(
    node: &EmbeddedNode,
    parent_session_id: &str,
    tool_call_id: &str,
) -> Result<Option<ExistingNotification>> {
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                content
                timestamp
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentMessage for background tool completion session={parent_session_id} failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<NotificationMessageRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let needle = format!(
        r#"<tool-completion tool_call_id="{}""#,
        xml_escape_attr(tool_call_id)
    );
    for row in rows {
        if row.content.contains(&needle) {
            return Ok(Some(ExistingNotification {
                sequence: row.sequence,
                timestamp: parse_utc_timestamp(&row.timestamp, "AgentMessage.timestamp")?,
            }));
        }
    }
    Ok(None)
}

#[derive(Debug, Deserialize)]
struct MessageTimestampRow {
    timestamp: String,
}

async fn load_message_timestamp(
    node: &EmbeddedNode,
    parent_session_id: &str,
    sequence: u32,
) -> Result<DateTime<Utc>> {
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    sequence: {{ _eq: {sequence} }}
                }},
                limit: 1
            ) {{ timestamp }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentMessage timestamp session={parent_session_id} sequence={sequence} failed: {:?}",
            response.errors
        );
    }
    let row: MessageTimestampRow = first_row(response.data.as_ref(), "AgentMessage")
        .ok_or_else(|| anyhow!("AgentMessage session={parent_session_id} sequence={sequence} not found after append"))?;
    parse_utc_timestamp(&row.timestamp, "AgentMessage.timestamp")
}

#[derive(Debug, Deserialize)]
struct WakeupRow {
    request_id: String,
    metadata: Option<String>,
    created_at: String,
}

async fn existing_wakeup_after(
    node: &EmbeddedNode,
    parent_session_id: &str,
    queue_key: &str,
    notification_timestamp: &DateTime<Utc>,
) -> Result<Option<String>> {
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                metadata
                created_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query scheduled wake-ups for session {parent_session_id} failed: {:?}",
            response.errors
        );
    }

    let rows: Vec<WakeupRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    for row in rows {
        let matches_key = parse_queue_hints(row.metadata.as_deref()).is_some_and(|hints| {
            hints.source == QueueSource::BackgroundCompletion
                && hints.policy == QueuePolicy::Coalesce
                && hints.key.as_deref() == Some(queue_key)
        });
        if !matches_key {
            continue;
        }

        let created_at = parse_utc_timestamp(&row.created_at, "AgentRequest.created_at")?;
        if created_at >= *notification_timestamp {
            return Ok(Some(row.request_id));
        }
    }
    Ok(None)
}

fn parse_utc_timestamp(value: &str, field: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .map_err(|error| anyhow!("{field} is not RFC3339: {error}"))?
        .with_timezone(&Utc))
}

pub(crate) async fn run_background_completion_observer(
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut observer = BackgroundCompletionObserver::new(node, cancel);
    observer.run().await
}

struct BackgroundCompletionObserver {
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
    subscription: events::Subscription,
    collection_id_to_name: HashMap<String, String>,
    processed_child_request_ids: HashSet<String>,
}

impl BackgroundCompletionObserver {
    fn new(node: Arc<EmbeddedNode>, cancel: CancellationToken) -> Self {
        let subscription = node.subscribe(&[EventName::Update]);
        Self {
            node,
            cancel,
            subscription,
            collection_id_to_name: HashMap::new(),
            processed_child_request_ids: HashSet::new(),
        }
    }

    async fn run(&mut self) -> Result<()> {
        self.project_ready_children().await?;
        loop {
            let message = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Ok(()),
                msg = self.subscription.recv() => {
                    match msg {
                        Some(message) => message,
                        None => anyhow::bail!("subagent completion subscription channel closed"),
                    }
                }
            };

            let dropped = self.subscription.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(
                    dropped,
                    "subagent completion observer dropped messages; scanning terminal children"
                );
                self.project_ready_children().await?;
            }

            let Some(update) = message.as_update() else {
                continue;
            };
            let Some(collection_name) = self.resolve_collection_name(&update.collection_id).await
            else {
                continue;
            };
            if collection_name != AGENT_REQUEST_COLLECTION {
                continue;
            }

            let Some(child_request_id) =
                load_request_id_by_doc_id(self.node.as_ref(), &update.doc_id).await?
            else {
                continue;
            };
            self.project_child_if_needed(child_request_id).await;
        }
    }

    async fn project_ready_children(&mut self) -> Result<()> {
        for child_request_id in load_terminal_child_request_ids(self.node.as_ref()).await? {
            self.project_child_if_needed(child_request_id).await;
        }
        Ok(())
    }

    async fn project_child_if_needed(&mut self, child_request_id: String) {
        if self.processed_child_request_ids.contains(&child_request_id) {
            return;
        }

        match project_background_subagent_completion(self.node.clone(), &child_request_id).await {
            Ok(BackgroundCompletionOutcome::Projected { .. })
            | Ok(BackgroundCompletionOutcome::AlreadyProjected) => {
                self.processed_child_request_ids.insert(child_request_id);
            }
            Ok(
                BackgroundCompletionOutcome::NotTerminal
                | BackgroundCompletionOutcome::NotBackground
                | BackgroundCompletionOutcome::MissingFinalResponse
                | BackgroundCompletionOutcome::Unlinked,
            ) => {}
            Err(error) => {
                tracing::warn!(
                    child_request_id = %child_request_id,
                    error = %error,
                    "failed to project background subagent completion"
                );
            }
        }
    }

    async fn resolve_collection_name(&mut self, collection_id: &str) -> Option<String> {
        if let Some(name) = self.collection_id_to_name.get(collection_id) {
            return Some(name.clone());
        }

        let names = match self.node.list_collections() {
            Ok(names) => names,
            Err(error) => {
                tracing::warn!(
                    collection_id = %collection_id,
                    %error,
                    "subagent completion observer failed to list collections"
                );
                return None;
            }
        };

        for name in names {
            let def = match self.node.get_collection(&name) {
                Ok(Some(def)) => def,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        collection_name = %name,
                        %error,
                        "subagent completion observer failed to fetch collection definition",
                    );
                    continue;
                }
            };
            self.collection_id_to_name
                .insert(def.collection_id.clone(), def.name.clone());
        }

        self.collection_id_to_name.get(collection_id).cloned()
    }
}

#[derive(Debug, Deserialize)]
struct ChildLinkageRow {
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
}

async fn load_child_linkage(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<ChildLinkageRow>> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query child AgentRequest linkage {child_request_id} failed: {:?}",
            response.errors
        );
    }
    Ok(first_row(response.data.as_ref(), "AgentRequest"))
}

#[derive(Debug, Deserialize)]
struct RequestIdRow {
    request_id: String,
}

async fn load_request_id_by_doc_id(node: &EmbeddedNode, doc_id: &str) -> Result<Option<String>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{ request_id }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentRequest doc {doc_id} failed: {:?}",
            response.errors
        );
    }
    Ok(first_row::<RequestIdRow>(response.data.as_ref(), "AgentRequest").map(|row| row.request_id))
}

#[derive(Debug, Deserialize)]
struct TerminalChildRow {
    request_id: String,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
}

async fn load_terminal_child_request_ids(node: &EmbeddedNode) -> Result<Vec<String>> {
    let query = r#"{
        AgentRequest(
            filter: {
                lifecycle_state: { _in: ["completed", "failed", "dead", "interrupted", "superseded"] }
            }
        ) {
            request_id
            caused_by_parent_request_id
            caused_by_parent_tool_call_id
        }
    }"#;
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query terminal child AgentRequests failed: {:?}",
            response.errors
        );
    }

    let rows: Vec<TerminalChildRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter(|row| {
            non_empty(row.caused_by_parent_request_id.as_deref()).is_some()
                && non_empty(row.caused_by_parent_tool_call_id.as_deref()).is_some()
        })
        .map(|row| row.request_id)
        .collect())
}

#[derive(Debug, Deserialize)]
struct AgentRequestQueueRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    session_id: String,
    content: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
    execution_origin: Option<String>,
    created_at: String,
    deadline: Option<String>,
    subagent_depth: Option<u32>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
}

async fn load_agent_request_for_queue(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<AgentRequest>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                temperature
                top_p
                top_k
                max_tokens
                metadata
                execution_origin
                created_at
                deadline
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent AgentRequest {request_id} for wake-up failed: {:?}",
            response.errors
        );
    }
    let Some(row) = first_row::<AgentRequestQueueRow>(response.data.as_ref(), "AgentRequest")
    else {
        return Ok(None);
    };

    let request = AgentRequest {
        doc_id: row.doc_id,
        request_id: row.request_id,
        agent_did: row.agent_did,
        behavior_id: normalize_optional_string(row.behavior_id),
        session_id: row.session_id,
        content: row.content,
        temperature: row.temperature,
        top_p: row.top_p,
        top_k: row.top_k,
        max_tokens: row.max_tokens,
        metadata: row.metadata,
        execution_origin: normalize_optional_string(row.execution_origin),
        created_at: row.created_at,
        deadline: normalize_optional_string(row.deadline),
        subagent_depth: row.subagent_depth.unwrap_or(0),
        caused_by_parent_request_id: normalize_optional_string(row.caused_by_parent_request_id),
        caused_by_parent_tool_call_id: normalize_optional_string(row.caused_by_parent_tool_call_id),
    };
    validate_agent_request_subagent_coherence(&request)?;
    Ok(Some(request))
}

fn render_notification(edge: &ChildEdge, status: &str, summary: &str) -> String {
    format!(
        r#"<subagent-notification child_request_id="{child_request_id}" child_session_id="{child_session_id}" behavior_id="{behavior_id}" parent_tool_call_id="{parent_tool_call_id}" status="{status}">
<summary>{summary}</summary>
</subagent-notification>"#,
        child_request_id = xml_escape_attr(&edge.child_request_id),
        child_session_id = xml_escape_attr(&edge.child_session_id),
        behavior_id = xml_escape_attr(&edge.behavior_id),
        parent_tool_call_id = xml_escape_attr(&edge.parent_tool_call_id),
        status = xml_escape_attr(status),
        summary = xml_escape_text(summary),
    )
}

fn render_tool_completion(
    tool_call_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
) -> String {
    let reason_element = reason
        .map(|reason| format!("\n  <reason>{}</reason>", xml_escape_text(reason)))
        .unwrap_or_default();
    format!(
        r#"<tool-completion tool_call_id="{tool_call_id}" tool_name="{tool_name}" status="{status}">
  <result>{result}</result>{reason_element}
</tool-completion>"#,
        tool_call_id = xml_escape_attr(tool_call_id),
        tool_name = xml_escape_attr(tool_name),
        status = xml_escape_attr(status),
        result = xml_escape_text(&compact_summary(result)),
        reason_element = reason_element,
    )
}

fn child_terminal_status(terminal: &ChildTerminal) -> &'static str {
    match terminal {
        ChildTerminal::Failed { .. } => "failed",
        ChildTerminal::Dead => "dead",
        ChildTerminal::Interrupted => "interrupted",
        ChildTerminal::Superseded => "superseded",
    }
}

fn child_terminal_summary(terminal: &ChildTerminal) -> String {
    match terminal {
        ChildTerminal::Failed { reason, .. } => compact_summary(reason),
        ChildTerminal::Dead => "child request reached the dead terminal state".to_string(),
        ChildTerminal::Interrupted => "child request was interrupted".to_string(),
        ChildTerminal::Superseded => "child request was superseded".to_string(),
    }
}

fn compact_summary(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    const LIMIT: usize = 4000;
    if normalized.len() <= LIMIT {
        return normalized;
    }

    let boundary = normalized
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= LIMIT)
        .last()
        .unwrap_or(0);
    let mut truncated = normalized[..boundary].to_string();
    truncated.push_str("...");
    truncated
}

fn xml_escape_attr(value: &str) -> String {
    xml_escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn first_row<T>(data: Option<&serde_json::Value>, collection: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    data.and_then(|data| data.get(collection))
        .and_then(|value| serde_json::from_value::<Vec<T>>(value.clone()).ok())
        .and_then(|mut rows| rows.pop())
}
