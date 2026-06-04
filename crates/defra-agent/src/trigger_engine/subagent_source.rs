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
use defra_node::EmbeddedNode;
use serde::Deserialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::background_tools::{
    fail_running_subagent_tool_call, load_behavior_allow_cross_deployment,
    load_parent_subagent_authorization, subagent_spawn_denial, subagent_tool_not_allowed_payload,
};
use crate::event_delivery_contract::{EventDeliveryRuntimeContract, EventDeliverySourceContract};
use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ConcurrencyMode, ResolvedTask};
use crate::tool_call_lifecycle::subagent_request::{
    create_subagent_request_with_request_id, create_subagent_request_with_trusted_parent_request_id,
};
use crate::tool_call_lifecycle::{
    AwaitMode, CancelPolicy, FailureClass, IllegalToolCallTransition, ToolCallState,
};
use crate::UpdateSubscriptionSource;

use super::{FireIntent, FireResult, TriggerKind, TriggerSource};

const TOOL_CALL_COLLECTION: &str = "AgentToolCall";

pub struct SubagentSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
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
    cancel_policy: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
}

impl ToolCallRow {
    /// Resolve the bridge `cancel_policy`, defaulting to `Cascade` to match
    /// `recovery::cancel_policy` (an absent/unknown policy is treated as the
    /// cascade default everywhere else).
    fn cancel_policy(&self) -> CancelPolicy {
        self.cancel_policy
            .as_deref()
            .and_then(CancelPolicy::from_persisted)
            .unwrap_or(CancelPolicy::Cascade)
    }
}

#[derive(Debug, Deserialize)]
struct ParentRequestRow {
    agent_did: String,
    #[serde(default)]
    subagent_depth: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ParentTerminalRow {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
}

/// True when the parent request reached a CANCEL-WORTHY terminal state — the
/// states for which the live cascade (`transition/bridge.rs`) and recovery
/// cascade (`recovery.rs`) drive a Cascade child to `.interrupted`.
///
/// This is `recovery::request_is_terminal` MINUS clean `completed`: a parent that
/// completed NORMALLY does not cascade-cancel its tools anywhere else, so a
/// background/detached child whose parent simply finished must be allowed to keep
/// running. We treat `error | superseded | dead | interrupted` (status) and
/// `failed | superseded | dead | interrupted` (lifecycle_state) as cancel-worthy.
fn parent_reached_cancel_worthy_terminal(row: &ParentTerminalRow) -> bool {
    matches!(
        row.status.as_deref(),
        Some("error" | "superseded" | "dead" | "interrupted")
    ) || matches!(
        row.lifecycle_state.as_deref(),
        Some("failed" | "superseded" | "dead" | "interrupted")
    )
}

/// Bridge args persisted by the spawn hook. After the named-target redesign
/// (#377) these carry both the model-facing `name` and the RESOLVED target
/// `(agent_did, behavior_id)`, so the claiming node never needs to re-resolve
/// the friendly name. The `target`/`target_behavior_id` aliases keep older
/// fixtures that wrote a bare behavior id under `behavior_id` working.
#[derive(Debug, Deserialize)]
struct SpawnArgs {
    #[serde(default)]
    name: Option<String>,
    /// Resolved owning DID of the target behavior. Absent on legacy fixtures.
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(alias = "target", alias = "target_behavior_id")]
    behavior_id: String,
    #[serde(alias = "message", alias = "content")]
    prompt: String,
    #[serde(default)]
    deadline: Option<String>,
}

impl SpawnArgs {
    /// Model-facing target name for authorization/error reporting. Falls back to
    /// the behavior id for legacy fixtures that omit a name.
    fn target_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.behavior_id)
    }
}

impl SubagentSource {
    pub fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_subscription_source(node.clone(), snapshot_rx, node, cancel)
    }

    pub fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            node,
            subscription_source: subs,
            subscription: None,
            cancel,
            collection_id_to_name: HashMap::new(),
            processed_tool_calls: HashSet::new(),
        }
    }

    fn ensure_subscription(&mut self) {
        if self.subscription.is_none() {
            self.subscription = Some(self.subscription_source.subscribe_updates());
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
                    cancel_policy
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

    async fn load_parent_terminal(
        &self,
        request_id: &str,
    ) -> anyhow::Result<Option<ParentTerminalRow>> {
        let escaped_request_id = escape_graphql_string(request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    limit: 1
                ) {{
                    status
                    lifecycle_state
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!(
                "query parent AgentRequest terminal state for SubagentSource failed: {:?}",
                response.errors
            );
        }
        let rows: Vec<ParentTerminalRow> = response
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

        let snapshot = self.snapshot_rx.borrow().clone();
        let parent_authoring_did = parent.agent_did.clone();
        let trusted_paired_peer = snapshot.paired_peer_dids.contains(&parent_authoring_did);
        let tool_name = non_empty(Some(&row.tool_name)).unwrap_or("spawn_subagent");
        if trusted_paired_peer {
            // The trusted-paired-peer branch is a CROSS-DEPLOYMENT spawn (the
            // parent DID is a paired peer, not this node). It bypasses
            // `subagent_spawn_denial`, so it must gate on the TARGET behavior's
            // `subagent_allow_cross_deployment` flag directly (#377). With the
            // flag off (default) we refuse to materialize the cross-deployment
            // child. The behavior id is the target behavior on THIS node.
            let allow_cross_deployment = match load_behavior_allow_cross_deployment(
                &self.node,
                &spawn_args.behavior_id,
            )
            .await
            {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        parent_request_id = %parent_request_id,
                        parent_authoring_did = %parent_authoring_did,
                        target_behavior_id = %spawn_args.behavior_id,
                        %error,
                        "subagent source could not load target behavior cross-deployment flag; refusing cross-deployment child",
                    );
                    return Ok(None);
                }
            };
            if !allow_cross_deployment {
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    parent_authoring_did = %parent_authoring_did,
                    target_behavior_id = %spawn_args.behavior_id,
                    "cross-deployment child refused: subagent_allow_cross_deployment is off for target behavior {behavior_id}",
                    behavior_id = spawn_args.behavior_id,
                );
                return Ok(None);
            }
            tracing::debug!(
                parent_request_id = %parent_request_id,
                parent_authoring_did = %parent_authoring_did,
                "subagent source claiming cross-deployment spawn from paired peer",
            );
        } else {
            let authorization = match load_parent_subagent_authorization(
                &self.node,
                &parent_request_id,
            )
            .await
            {
                Ok(authorization) => authorization,
                Err(error) => {
                    let failed = self
                        .fail_unauthorized_tool_call(
                            &row,
                            "/name",
                            spawn_args.target_name(),
                            "subagent authorization could not be verified for this behavior",
                            &[],
                        )
                        .await?;
                    self.processed_tool_calls.insert(processed_key);
                    tracing::warn!(
                        parent_request_id = %parent_request_id,
                        parent_tool_call_id = %parent_tool_call_id,
                        target_name = %spawn_args.target_name(),
                        failed_tool_call = failed,
                        %error,
                        "subagent source could not verify parent subagent authorization; rejecting spawn",
                    );
                    return Ok(None);
                }
            };
            if let Some(denial) = subagent_spawn_denial(
                &authorization,
                spawn_args.target_name(),
                await_mode,
                tool_name,
                snapshot.local_did.as_str(),
            ) {
                let failed = self
                    .fail_unauthorized_tool_call(
                        &row,
                        denial.path,
                        &denial.requested,
                        denial.message,
                        &authorization.allowed_target_names(),
                    )
                    .await?;
                self.processed_tool_calls.insert(processed_key);
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    parent_behavior_id = %authorization.behavior_id,
                    parent_tool_call_id = %parent_tool_call_id,
                    target_name = %spawn_args.target_name(),
                    await_mode = %await_mode.as_str(),
                    failed_tool_call = failed,
                    "subagent source rejected unauthorized subagent spawn",
                );
                return Ok(None);
            }
        }

        if snapshot.behavior(&spawn_args.behavior_id).is_none() {
            tracing::warn!(
                parent_request_id = %parent_request_id,
                parent_tool_call_id = %parent_tool_call_id,
                target_name = %spawn_args.target_name(),
                target_behavior_id = %spawn_args.behavior_id,
                "subagent source target behavior is not in the active runtime snapshot; skipping spawn",
            );
            return Ok(None);
        }

        // The child is owned by the RESOLVED target's `agent_did` carried in the
        // bridge args (#377). Legacy fixtures that omit `agent_did` fall back to
        // the parent's DID.
        let resolved_target_did = spawn_args
            .agent_did
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        // DID-anchored single-creator gate (audit Finding 2). On the non-trusted
        // path a node may ONLY materialize a child addressed to its OWN DID.
        // `request_id` is `@index` (not unique), so without this anchor two peers
        // that both replicate the bridge could each create the same child. The
        // trusted-paired-peer path is the explicit, vetted cross-deployment
        // exception and keeps taking local ownership below.
        if !trusted_paired_peer {
            let local_did = snapshot.local_did.trim();
            let target_owner_did = resolved_target_did
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| parent.agent_did.trim());
            if local_did.is_empty() || target_owner_did != local_did {
                tracing::debug!(
                    parent_request_id = %parent_request_id,
                    parent_tool_call_id = %parent_tool_call_id,
                    target_name = %spawn_args.target_name(),
                    target_owner_did = %target_owner_did,
                    local_did = %local_did,
                    "subagent source skipping spawn: this node does not own the target DID (single-creator gate)",
                );
                return Ok(None);
            }
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
        // The trusted-paired-peer claiming path takes local ownership; otherwise
        // the child is owned by the RESOLVED target DID (which the single-creator
        // gate above has already confirmed equals our local DID).
        let child_agent_did = if trusted_paired_peer && !snapshot.local_did.trim().is_empty() {
            snapshot.local_did.clone()
        } else {
            resolved_target_did.unwrap_or_else(|| parent.agent_did.clone())
        };
        let request_id = if trusted_paired_peer {
            create_subagent_request_with_trusted_parent_request_id(
                &self.node,
                child_request_id.clone(),
                parent_request_id.clone(),
                parent_tool_call_id.clone(),
                parent_depth,
                child_agent_did,
                spawn_args.behavior_id.clone(),
                spawn_args.prompt.clone(),
                deadline,
            )
            .await?
        } else {
            create_subagent_request_with_request_id(
                &self.node,
                child_request_id.clone(),
                parent_request_id.clone(),
                parent_tool_call_id.clone(),
                parent_depth,
                child_agent_did,
                spawn_args.behavior_id.clone(),
                spawn_args.prompt.clone(),
                deadline,
            )
            .await?
        };

        // Orphan-child-escapes-cancel race (audit Finding 1). The parent may have
        // been cancelled/interrupted in the window between the spawn hook writing
        // the `running` bridge and this child create. The cascade's
        // `interrupt_request(child_request_id)` would have no-oped because the
        // child did not exist yet, so we re-check AFTER the create and interrupt
        // the just-created child if a genuine cancel signal is present.
        //
        // CRUCIALLY, this re-check must be consistent with the live cascade
        // (`transition/bridge.rs::bridge_cancel_cascade`) and the recovery cascade
        // (`recovery.rs::cascade_child_request_id`): BOTH gate the child interrupt
        // on `cancel_policy == Cascade` and refuse to cascade for detached
        // children (`if self.cancel_policy != CancelPolicy::Cascade { return None }`
        // / `cascade_child_request_id` returns `None` unless Cascade). A
        // DETACHED/background-detached child outlives its parent. So we ONLY
        // interrupt when the bridge policy is Cascade AND a real cancel signal is
        // present. A parent that completed NORMALLY is NOT a cancel signal — a
        // cleanly-completed parent never cascade-cancels its tools anywhere else.
        let bridge_cancel_policy = row.cancel_policy();
        if bridge_cancel_policy != CancelPolicy::Cascade {
            // Detached/background-detached: the child is intentionally decoupled
            // from parent lifetime and must survive parent completion/cancel.
            tracing::debug!(
                child_request_id = %request_id,
                parent_request_id = %parent_request_id,
                cancel_policy = bridge_cancel_policy.as_str(),
                "subagent source: detached child, skipping orphan cancel re-check (child outlives parent)",
            );
        } else {
            // A bridge that itself reached `.cancelled` is a genuine cancel signal
            // (the live cascade cancels the bridge then drives the child). Other
            // non-running bridge states (e.g. `completed`/`failed`) are NOT cancel
            // signals on their own. A vanished bridge is treated as cancel-worthy
            // so a truly orphaned child is reclaimed rather than left running
            // uncancellable.
            let bridge_cancelled = match self.load_tool_call(doc_id).await {
                Ok(Some(latest)) => {
                    latest.lifecycle_state.as_deref() == Some(ToolCallState::Cancelled.as_str())
                }
                Ok(None) => true,
                Err(error) => {
                    tracing::warn!(
                        child_request_id = %request_id,
                        %error,
                        "subagent source failed to re-read bridge lifecycle after child create; assuming not cancelled",
                    );
                    false
                }
            };
            // Parent interrupt latch — the exact signal the live cascade fires on.
            let parent_interrupted = match crate::interrupt::fetch_interrupt_requested_at(
                &self.node,
                &parent_request_id,
            )
            .await
            {
                Ok(value) => value.is_some(),
                Err(error) => {
                    tracing::warn!(
                        parent_request_id = %parent_request_id,
                        %error,
                        "subagent source failed to re-read parent interrupt latch after child create; assuming not interrupted",
                    );
                    false
                }
            };
            // The parent may have reached a CANCEL-WORTHY terminal state
            // (error/superseded/dead/interrupted — NOT clean `completed`) in the
            // materialize window without setting the interrupt latch. Recovery's
            // cascade treats these as Failed/Cancelled and interrupts the Cascade
            // child; mirror that here. A vanished parent row is treated as
            // cancel-worthy so the orphan is reclaimed.
            let parent_cancel_worthy_terminal = match self
                .load_parent_terminal(&parent_request_id)
                .await
            {
                Ok(Some(row)) => parent_reached_cancel_worthy_terminal(&row),
                Ok(None) => true,
                Err(error) => {
                    tracing::warn!(
                        parent_request_id = %parent_request_id,
                        %error,
                        "subagent source failed to re-read parent terminal state after child create; assuming not terminal",
                    );
                    false
                }
            };
            if bridge_cancelled || parent_interrupted || parent_cancel_worthy_terminal {
                tracing::info!(
                    child_request_id = %request_id,
                    parent_request_id = %parent_request_id,
                    bridge_cancelled,
                    parent_interrupted,
                    parent_cancel_worthy_terminal,
                    "subagent source: Cascade bridge with real cancel signal in materialize window; interrupting just-created orphan child",
                );
                if let Err(error) =
                    crate::interrupt::interrupt_request(&self.node, &request_id).await
                {
                    tracing::warn!(
                        child_request_id = %request_id,
                        %error,
                        "subagent source failed to interrupt orphaned child after cancel-before-materialize race",
                    );
                }
            }
        }

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

impl EventDeliveryRuntimeContract for SubagentSource {
    const EVENT_DELIVERY_CONTRACT: EventDeliverySourceContract = EventDeliverySourceContract {
        name: "SubagentSource",
        dedupe_policy: "monotone_once",
        rescan_bounded_by: 0,
        deviation: Some("subagent_source_lacks_live_rescan"),
    };
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
