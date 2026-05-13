//! Subagent-backed `TriggerSource`.
//!
//! Observes `AgentToolCall` rows with a non-empty `child_request_id` and
//! materializes the linked child `AgentRequest`. The child is created before
//! the intent reaches `TriggerEngine`; the returned `FireIntent` carries the
//! pre-materialized request id so dispatch records a `Fired` result without
//! creating a second request.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName};
use serde::Deserialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ConcurrencyMode, ResolvedTask};
use crate::subagent_tools::{
    fail_running_subagent_tool_call, load_parent_subagent_authorization, subagent_spawn_denial,
    subagent_tool_not_allowed_payload,
};
use crate::tool_call_lifecycle::subagent_request::create_subagent_request_with_request_id;
use crate::tool_call_lifecycle::{AwaitMode, FailureClass, IllegalToolCallTransition};

use super::{FireIntent, FireResult, TriggerKind, TriggerSource};

const TOOL_CALL_COLLECTION: &str = "AgentToolCall";

pub(crate) struct SubagentSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    subscription: Option<events::Subscription>,
    cancel: CancellationToken,
    collection_id_to_name: HashMap<String, String>,
    processed_tool_calls: HashSet<String>,
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    tool_call_key: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    args: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    deadline_at: Option<String>,
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParentRequestRow {
    agent_did: String,
    #[serde(default)]
    subagent_depth: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    #[serde(alias = "target", alias = "target_behavior_id")]
    behavior_id: String,
    #[serde(alias = "message", alias = "content")]
    prompt: String,
    #[serde(default)]
    deadline: Option<String>,
}

impl SubagentSource {
    pub(crate) fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            node,
            subscription: None,
            cancel,
            collection_id_to_name: HashMap::new(),
            processed_tool_calls: HashSet::new(),
        }
    }

    fn ensure_subscription(&mut self) {
        if self.subscription.is_none() {
            self.subscription = Some(self.node.subscribe(&[EventName::Update]));
            tracing::info!("subagent source opened global Update subscription");
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
                    "subagent source failed to list collections; dropping event"
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
                        "subagent source failed to fetch collection definition while resolving id",
                    );
                    continue;
                }
            };
            self.collection_id_to_name
                .insert(def.collection_id.clone(), def.name.clone());
        }

        self.collection_id_to_name.get(collection_id).cloned()
    }

    async fn load_tool_call(&self, doc_id: &str) -> anyhow::Result<Option<ToolCallRow>> {
        let escaped_doc_id = escape_graphql_string(doc_id);
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    limit: 1
                ) {{
                    _docID
                    tool_call_key
                    request_id
                    tool_call_id
                    tool_name
                    args
                    lifecycle_state
                    started_at
                    deadline_at
                    await_mode
                    child_request_id
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!(
                "query AgentToolCall for SubagentSource failed: {:?}",
                response.errors
            );
        }
        let rows: Vec<ToolCallRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get(TOOL_CALL_COLLECTION))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        Ok(rows.into_iter().next())
    }

    async fn load_parent_request(
        &self,
        request_id: &str,
    ) -> anyhow::Result<Option<ParentRequestRow>> {
        let escaped_request_id = escape_graphql_string(request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    limit: 1
                ) {{
                    agent_did
                    subagent_depth
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!(
                "query parent AgentRequest for SubagentSource failed: {:?}",
                response.errors
            );
        }
        let rows: Vec<ParentRequestRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        Ok(rows.into_iter().next())
    }

    async fn child_request_exists(&self, request_id: &str) -> anyhow::Result<bool> {
        let escaped_request_id = escape_graphql_string(request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    limit: 1
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!(
                "query child AgentRequest for SubagentSource failed: {:?}",
                response.errors
            );
        }
        Ok(response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| !rows.is_empty()))
    }

    async fn fail_unauthorized_tool_call(
        &self,
        row: &ToolCallRow,
        path: &str,
        requested: &str,
        message: impl Into<String>,
        allowed_targets: &[String],
    ) -> anyhow::Result<bool> {
        let tool_name = non_empty(Some(&row.tool_name)).unwrap_or("spawn_subagent");
        let payload =
            subagent_tool_not_allowed_payload(tool_name, path, requested, message, allowed_targets);
        fail_running_subagent_tool_call(
            &self.node,
            &row.doc_id,
            row.started_at.as_deref(),
            row.deadline_at.as_deref(),
            &payload,
            FailureClass::ServiceUnavailable,
        )
        .await
    }

    async fn build_intent_for_tool_call_doc(
        &mut self,
        doc_id: &str,
    ) -> anyhow::Result<Option<FireIntent>> {
        let Some(row) = self.load_tool_call(doc_id).await? else {
            return Ok(None);
        };
        let child_request_id = match non_empty(row.child_request_id.as_deref()) {
            Some(value) => value.to_string(),
            None => return Ok(None),
        };
        if row.lifecycle_state.as_deref() != Some("running") {
            return Ok(None);
        }

        let processed_key = row
            .tool_call_key
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| row.doc_id.clone());
        if self.processed_tool_calls.contains(&processed_key) {
            return Ok(None);
        }

        let parent_request_id = match non_empty(row.request_id.as_deref()) {
            Some(value) => value.to_string(),
            None => return Ok(None),
        };
        let parent_tool_call_id = match non_empty(Some(&row.tool_call_id)) {
            Some(value) => value.to_string(),
            None => return Ok(None),
        };
        let spawn_args: SpawnArgs = serde_json::from_str(&row.args)?;
        let await_mode = row
            .await_mode
            .as_deref()
            .and_then(AwaitMode::from_persisted)
            .unwrap_or(AwaitMode::Foreground);

        let Some(parent) = self.load_parent_request(&parent_request_id).await? else {
            anyhow::bail!(IllegalToolCallTransition::ParentLinkageIncoherent);
        };

        let authorization = match load_parent_subagent_authorization(&self.node, &parent_request_id)
            .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                let failed = self
                    .fail_unauthorized_tool_call(
                        &row,
                        "/behavior_id",
                        &spawn_args.behavior_id,
                        "subagent authorization could not be verified for this behavior",
                        &[],
                    )
                    .await?;
                self.processed_tool_calls.insert(processed_key);
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    parent_tool_call_id = %parent_tool_call_id,
                    target_behavior_id = %spawn_args.behavior_id,
                    failed_tool_call = failed,
                    %error,
                    "subagent source could not verify parent subagent authorization; rejecting spawn",
                );
                return Ok(None);
            }
        };
        let tool_name = non_empty(Some(&row.tool_name)).unwrap_or("spawn_subagent");
        if let Some(denial) = subagent_spawn_denial(
            &authorization,
            &spawn_args.behavior_id,
            await_mode,
            tool_name,
        ) {
            let failed = self
                .fail_unauthorized_tool_call(
                    &row,
                    denial.path,
                    &denial.requested,
                    denial.message,
                    &authorization.allowed_targets,
                )
                .await?;
            self.processed_tool_calls.insert(processed_key);
            tracing::warn!(
                parent_request_id = %parent_request_id,
                parent_behavior_id = %authorization.behavior_id,
                parent_tool_call_id = %parent_tool_call_id,
                target_behavior_id = %spawn_args.behavior_id,
                await_mode = %await_mode.as_str(),
                failed_tool_call = failed,
                "subagent source rejected unauthorized subagent spawn",
            );
            return Ok(None);
        }

        let snapshot = self.snapshot_rx.borrow().clone();
        if snapshot.behavior(&spawn_args.behavior_id).is_none() {
            tracing::warn!(
                parent_request_id = %parent_request_id,
                parent_tool_call_id = %parent_tool_call_id,
                target_behavior_id = %spawn_args.behavior_id,
                "subagent source target behavior is not in the active runtime snapshot; skipping spawn",
            );
            return Ok(None);
        }

        if self.child_request_exists(&child_request_id).await? {
            self.processed_tool_calls.insert(processed_key);
            return Ok(None);
        }

        let parent_depth = parent
            .subagent_depth
            .and_then(|depth| u32::try_from(depth).ok())
            .unwrap_or(0);
        let deadline =
            effective_deadline(row.deadline_at.as_deref(), spawn_args.deadline.as_deref());
        let request_id = create_subagent_request_with_request_id(
            &self.node,
            child_request_id.clone(),
            parent_request_id.clone(),
            parent_tool_call_id.clone(),
            parent_depth,
            parent.agent_did,
            spawn_args.behavior_id.clone(),
            spawn_args.prompt.clone(),
            deadline,
        )
        .await?;

        self.processed_tool_calls.insert(processed_key);
        let fired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let task = ResolvedTask {
            task_id: format!("subagent:{parent_tool_call_id}"),
            name: Some(format!(
                "Subagent {target}",
                target = spawn_args.behavior_id
            )),
            behavior_id: spawn_args.behavior_id,
            prompt_template: spawn_args.prompt,
            output_schema_ref: None,
        };
        let event_vars = serde_json::json!({
            "fired_at": fired_at,
            "trigger_id": parent_tool_call_id,
            "trigger_kind": "subagent",
            "parent_request_id": parent_request_id,
            "child_request_id": request_id,
        });
        Ok(Some(FireIntent {
            trigger_id: None,
            trigger_kind: TriggerKind::Manual,
            task,
            concurrency: ConcurrencyMode::Parallel,
            event_vars,
            doc_vars: None,
            args_vars: None,
            pre_materialized_request_id: Some(request_id),
            on_result: Box::new(move |result| match result {
                FireResult::Fired { request_id } => {
                    tracing::debug!(
                        child_request_id = %request_id,
                        "subagent source reported pre-materialized child request fired"
                    );
                }
                FireResult::Skipped { reason } => {
                    tracing::warn!(%reason, "subagent source pre-materialized fire skipped");
                }
                FireResult::Errored { error } => {
                    tracing::warn!(%error, "subagent source pre-materialized fire errored");
                }
            }),
        }))
    }
}

impl TriggerSource for SubagentSource {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>> {
        Box::pin(async move {
            self.ensure_subscription();
            loop {
                let message = {
                    let subscription = self
                        .subscription
                        .as_mut()
                        .expect("subagent source subscription opened before polling");
                    tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => return None,
                        res = self.snapshot_rx.changed() => {
                            if res.is_err() {
                                return None;
                            }
                            continue;
                        }
                        msg = subscription.recv() => {
                            match msg {
                                Some(message) => message,
                                None => {
                                    tracing::warn!(
                                        "subagent source subscription channel closed; source exiting",
                                    );
                                    return None;
                                }
                            }
                        }
                    }
                };

                let dropped = self
                    .subscription
                    .as_mut()
                    .expect("subagent source subscription remains open")
                    .check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(
                        dropped,
                        "subagent source dropped messages; may have missed child spawns",
                    );
                }

                let Some(update) = message.as_update() else {
                    continue;
                };
                let collection_id = update.collection_id.clone();
                let doc_id = update.doc_id.clone();
                let Some(collection_name) = self.resolve_collection_name(&collection_id).await
                else {
                    continue;
                };
                if collection_name != TOOL_CALL_COLLECTION {
                    continue;
                }

                match self.build_intent_for_tool_call_doc(&doc_id).await {
                    Ok(Some(intent)) => return Some(intent),
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(
                            doc_id = %doc_id,
                            %error,
                            "subagent source failed to process AgentToolCall event",
                        );
                        continue;
                    }
                }
            }
        })
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn parse_deadline(value: Option<&str>) -> Option<DateTime<Utc>> {
    non_empty(value)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn effective_deadline(
    tool_deadline: Option<&str>,
    args_deadline: Option<&str>,
) -> Option<DateTime<Utc>> {
    match (parse_deadline(tool_deadline), parse_deadline(args_deadline)) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}
