//! Subagent-backed `TriggerSource`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::agent::p2p_reconcile::PeerAdmissionAuthority;
use crate::background_tools::{
    fail_running_subagent_tool_call, load_behavior_allow_cross_deployment,
    load_parent_subagent_authorization, subagent_spawn_denial, subagent_tool_not_allowed_payload,
};
use crate::config_client::ConfigAccess;
use crate::event_delivery_contract::{EventDeliveryRuntimeContract, EventDeliverySourceContract};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::run_timeline_fetch::load_accepted_tool_arguments;
use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ConcurrencyMode, ResolvedTask};
use crate::tool_call_lifecycle::subagent_request::{
    create_subagent_request_with_request_id_and_workspace,
    create_subagent_request_with_trusted_parent_request_id_and_workspace,
};
use crate::tool_call_lifecycle::subagent_workspace::{
    resolve_child_workspace, ParentWorkspaceStamp, SpawnWorkspaceError,
};
use crate::tool_call_lifecycle::{
    AwaitMode, CancelPolicy, FailureClass, IllegalToolCallTransition, ToolCallState,
};
use crate::UpdateSubscriptionSource;

use super::{FireIntent, FireResult, TriggerKind, TriggerSource};

const TOOL_CALL_COLLECTION: &str = "AgentToolCall";
const SUBAGENT_SOURCE_RESCAN_INTERVAL: Duration = Duration::from_secs(5);

struct StaticPeerAdmission {
    authorized_peer_dids: HashSet<String>,
}

#[async_trait]
impl PeerAdmissionAuthority for StaticPeerAdmission {
    async fn fresh_member_authorized(&self, member_did: &str) -> anyhow::Result<bool> {
        Ok(self.authorized_peer_dids.contains(member_did))
    }

    async fn fresh_member_authorized_for_agent(
        &self,
        member_did: &str,
        _owner_agent: &str,
    ) -> anyhow::Result<bool> {
        self.fresh_member_authorized(member_did).await
    }
}

pub struct SubagentSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    peer_admission: Arc<dyn PeerAdmissionAuthority>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
    subscription: Option<events::Subscription>,
    cancel: CancellationToken,
    collection_id_to_name: HashMap<String, String>,
    processed_tool_calls: HashSet<String>,
    warned_incoherent_tool_calls: HashSet<String>,
    rescan_tick: tokio::time::Interval,
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    tool_call_key: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
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
    #[serde(default)]
    spawn_target_did: Option<String>,
    #[serde(default)]
    spawn_behavior_id: Option<String>,
    #[serde(default)]
    delegated_workspace: Option<gents_protocol::output::DelegatedWorkspace>,
    #[serde(default)]
    delegated_input: Option<gents_protocol::output::DelegatedToolInput>,
    #[serde(default)]
    cancel_cascade_intent_at: Option<String>,
}

#[derive(Clone)]
struct PinnedTarget {
    target_agent_did: String,
    behavior_id: String,
}

#[derive(Debug, Deserialize)]
struct ToolCallDocIdRow {
    #[serde(rename = "_docID")]
    doc_id: String,
}

impl ToolCallRow {
    fn cancel_policy(&self) -> Option<CancelPolicy> {
        self.cancel_policy
            .as_deref()
            .and_then(CancelPolicy::from_persisted)
    }
}

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    name: String,
    prompt: String,
    #[serde(default)]
    deadline: Option<String>,
    #[serde(default)]
    workspace: Option<crate::background_tools::SpawnWorkspaceArg>,
}

impl SpawnArgs {
    fn target_name(&self) -> &str {
        &self.name
    }
}

fn subagent_source_rescan_tick(interval: Duration) -> tokio::time::Interval {
    let interval = if interval.is_zero() {
        SUBAGENT_SOURCE_RESCAN_INTERVAL
    } else {
        interval
    };
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick
}

impl SubagentSource {
    pub fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        peer_admission: Arc<dyn PeerAdmissionAuthority>,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_subscription_source(node.clone(), snapshot_rx, node, peer_admission, cancel)
    }

    pub fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        peer_admission: Arc<dyn PeerAdmissionAuthority>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            node,
            peer_admission,
            subscription_source: subs,
            subscription: None,
            cancel,
            collection_id_to_name: HashMap::new(),
            processed_tool_calls: HashSet::new(),
            warned_incoherent_tool_calls: HashSet::new(),
            rescan_tick: subagent_source_rescan_tick(SUBAGENT_SOURCE_RESCAN_INTERVAL),
        }
    }

    #[doc(hidden)]
    pub fn with_subscription_source_for_test(
        subs: Arc<dyn UpdateSubscriptionSource>,
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        authorized_peer_dids: HashSet<String>,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_subscription_source(
            subs,
            snapshot_rx,
            node,
            Arc::new(StaticPeerAdmission {
                authorized_peer_dids,
            }),
            cancel,
        )
    }

    #[doc(hidden)]
    pub fn with_rescan_interval(mut self, interval: Duration) -> Self {
        self.rescan_tick = subagent_source_rescan_tick(interval);
        self
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
                    request_doc_id
                    agent_did
                    tool_call_id
                    tool_name
                    lifecycle_state
                    started_at
                    deadline_at
                    await_mode
                    cancel_policy
                    child_request_id
                    spawn_target_did
                    spawn_behavior_id
                    delegated_workspace
                    delegated_input
                    cancel_cascade_intent_at
                }}
            }}"#
        );
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "query AgentToolCall for SubagentSource",
        )
        .await?;
        let rows: Vec<ToolCallRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get(TOOL_CALL_COLLECTION))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        Ok(rows.into_iter().next())
    }

    /// The bridge row is lifecycle-only.  Its spawn arguments remain in the
    /// accepted provider header, so resolve them through the shared canonical
    /// reader by exact physical tool document identity in the parent session.
    /// There is no retired `AgentToolCall.args` fallback and no
    /// logical-id/proximity join.
    async fn load_spawn_args(
        &self,
        row: &ToolCallRow,
        parent: Option<&AgentRequestRow>,
    ) -> anyhow::Result<SpawnArgs> {
        if let Some(input) = &row.delegated_input {
            return serde_json::from_str(&input.arguments).with_context(|| {
                format!(
                    "decode delegated spawn arguments for physical bridge {}",
                    row.doc_id
                )
            });
        }
        let parent = parent.context("subagent bridge parent request is not local")?;
        let parent_agent_did = parent
            .agent_did
            .as_deref()
            .context("subagent bridge parent request lacks agent_did")?;
        let parent_session_id = non_empty(parent.session_id.as_deref())
            .context("subagent bridge parent request lacks session_id")?;
        let matches = load_accepted_tool_arguments(
            &ConfigAccess::Local(self.node.clone()),
            parent_agent_did,
            parent_session_id,
            parent.requester_did.as_deref(),
            &row.doc_id,
        )
        .await
        .with_context(|| {
            format!(
                "resolve canonical spawn admission for physical tool {}",
                row.doc_id
            )
        })?;
        anyhow::ensure!(
            matches.len() == 1,
            "canonical session did not resolve exactly one physical spawn bridge {}",
            row.doc_id
        );
        let args = &matches[0];
        anyhow::ensure!(
            !args.trim().is_empty(),
            "canonical spawn admission has no arguments for physical bridge {}",
            row.doc_id
        );
        serde_json::from_str(args).with_context(|| {
            format!(
                "decode canonical spawn arguments for physical bridge {}",
                row.doc_id
            )
        })
    }

    async fn load_parent_request(
        &self,
        request_doc_id: &str,
    ) -> anyhow::Result<Option<AgentRequestRow>> {
        let escaped_request_doc_id = escape_graphql_string(request_doc_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ _docID: {{ _eq: "{escaped_request_doc_id}" }} }},
                    limit: 1
                ) {{
                    request_id
                    agent_did
                    requester_did
                    session_id
                    subagent_depth
                    workspace_id
                    workspace_authority
                    workspace_owner_agent_did
                    workspace_seal_hash
                }}
            }}"#
        );
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "query parent AgentRequest for SubagentSource",
        )
        .await?;
        let value = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .context("AgentRequest field missing from parent request query")?;
        let rows: Vec<AgentRequestRow> = serde_json::from_value(value.clone())
            .context("decode parent AgentRequest rows for SubagentSource")?;
        for row in &rows {
            anyhow::ensure!(
                row.agent_did.is_some(),
                "parent AgentRequest {} has no agent_did",
                row.request_id
            );
        }
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
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "query child AgentRequest for SubagentSource",
        )
        .await?;
        Ok(response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| !rows.is_empty()))
    }

    async fn load_running_bridge_doc_ids(&self) -> anyhow::Result<Vec<String>> {
        let query = r#"{
            AgentToolCall(
                filter: {
                    lifecycle_state: { _eq: "running" },
                    child_request_id: { _ne: "" }
                }
            ) { _docID }
        }"#;
        let response = graphql_with_transaction_retry(
            &self.node,
            query,
            "query running AgentToolCall bridge rows for SubagentSource rescan",
        )
        .await?;
        let rows: Vec<ToolCallDocIdRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get(TOOL_CALL_COLLECTION))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        Ok(rows.into_iter().map(|row| row.doc_id).collect())
    }

    async fn rescan_running_bridge_rows(&mut self) -> Option<FireIntent> {
        let doc_ids = match self.load_running_bridge_doc_ids().await {
            Ok(doc_ids) => doc_ids,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "subagent source periodic rescan failed to load running bridge rows",
                );
                return None;
            }
        };
        tracing::debug!(
            local_did = %self.snapshot_rx.borrow().local_did,
            bridge_count = doc_ids.len(),
            "subagent source periodic rescan loaded running bridge rows",
        );

        for doc_id in doc_ids {
            match self.build_intent_for_tool_call_doc(&doc_id).await {
                Ok(Some(intent)) => {
                    tracing::info!(
                        doc_id = %doc_id,
                        "subagent source periodic rescan emitted fire intent",
                    );
                    return Some(intent);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        doc_id = %doc_id,
                        %error,
                        "subagent source periodic rescan failed to process AgentToolCall row",
                    );
                }
            }
        }
        None
    }

    /// Interrupt the exact child this bridge receipt names, by its physical
    /// document and principal scope.
    async fn interrupt_created_child(&self, bridge_doc_id: &str, child_request_id: &str) {
        let result = async {
            let child = crate::descendant_graph::resolve_bridge_receipt_child(
                crate::descendant_graph::DescendantGraphAccess::Local(self.node.as_ref()),
                bridge_doc_id,
            )
            .await?
            .context("created child does not corroborate its bridge receipt")?;
            crate::interrupt::interrupt_request_by_doc_id(
                &self.node,
                child
                    .doc_id
                    .as_deref()
                    .context("created child omitted _docID")?,
                child
                    .agent_did
                    .as_deref()
                    .context("created child omitted agent_did")?,
                child.requester_did.as_deref(),
            )
            .await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(
                child_request_id,
                %error,
                "subagent source failed to interrupt a child created for a settled bridge",
            );
        }
    }

    async fn fail_unauthorized_tool_call(
        &self,
        row: &ToolCallRow,
        path: &str,
        requested: &str,
        message: impl Into<String>,
        allowed_targets: &[String],
    ) -> anyhow::Result<bool> {
        let payload = subagent_tool_not_allowed_payload(
            &row.tool_name,
            path,
            requested,
            message,
            allowed_targets,
        );
        fail_running_subagent_tool_call(
            &self.node,
            &row.doc_id,
            &payload,
            FailureClass::ServiceUnavailable,
        )
        .await
    }

    async fn fail_workspace_tool_call(
        &self,
        row: &ToolCallRow,
        error: SpawnWorkspaceError,
    ) -> anyhow::Result<bool> {
        fail_running_subagent_tool_call(&self.node, &row.doc_id, &error.payload(), error.class)
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

        let Some(processed_key) = row.tool_call_key.clone().filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        if self.processed_tool_calls.contains(&processed_key) {
            return Ok(None);
        }

        let parent_request_id = match non_empty(row.request_id.as_deref()) {
            Some(value) => value.to_string(),
            None => return Ok(None),
        };
        let parent_request_doc_id = match non_empty(row.request_doc_id.as_deref()) {
            Some(value) => value.to_string(),
            None => {
                if self
                    .warned_incoherent_tool_calls
                    .insert(processed_key.clone())
                {
                    tracing::warn!(
                        doc_id = %row.doc_id,
                        parent_request_id = %parent_request_id,
                        tool_call_id = %row.tool_call_id,
                        "subagent source quarantined AgentToolCall without exact parent document binding",
                    );
                }
                return Ok(None);
            }
        };
        let parent_tool_call_id = match non_empty(Some(&row.tool_call_id)) {
            Some(value) => value.to_string(),
            None => return Ok(None),
        };
        let parent = self.load_parent_request(&parent_request_doc_id).await?;
        let spawn_args = self.load_spawn_args(&row, parent.as_ref()).await?;
        let Some(row_spawn_target_did) = non_empty(row.spawn_target_did.as_deref()) else {
            return Ok(None);
        };
        let Some(row_spawn_behavior_id) = non_empty(row.spawn_behavior_id.as_deref()) else {
            return Ok(None);
        };
        let Some(await_mode) = row
            .await_mode
            .as_deref()
            .and_then(AwaitMode::from_persisted)
        else {
            return Ok(None);
        };
        let Some(bridge_cancel_policy) = row.cancel_policy() else {
            return Ok(None);
        };

        let snapshot = self.snapshot_rx.borrow().clone();
        let bridge_authoring_did = non_empty(row.agent_did.as_deref())
            .ok_or(IllegalToolCallTransition::ParentLinkageIncoherent)?;
        let parent_authoring_did = match parent.as_ref() {
            Some(parent)
                if parent.request_id == parent_request_id
                    && Some(bridge_authoring_did) == parent.agent_did.as_deref() =>
            {
                parent
                    .agent_did
                    .clone()
                    .expect("parent request agent_did validated at query boundary")
            }
            Some(_) => anyhow::bail!(IllegalToolCallTransition::ParentLinkageIncoherent),
            None => bridge_authoring_did.to_string(),
        };
        let parent_is_local = parent.as_ref().is_some_and(|parent| {
            !snapshot.local_did.trim().is_empty()
                && parent.agent_did.as_deref().map(str::trim) == Some(snapshot.local_did.trim())
        });
        let trusted_paired_peer = if parent_is_local {
            false
        } else {
            if !self
                .peer_admission
                .fresh_member_authorized(&parent_authoring_did)
                .await?
            {
                anyhow::bail!(IllegalToolCallTransition::ParentLinkageIncoherent);
            }
            true
        };
        let Some(tool_name) = non_empty(Some(&row.tool_name)) else {
            return Ok(None);
        };
        let target = if trusted_paired_peer {
            let local_did = snapshot.local_did.trim();
            if local_did.is_empty() || row_spawn_target_did != local_did {
                tracing::debug!(
                    parent_request_id = %parent_request_id,
                    parent_tool_call_id = %parent_tool_call_id,
                    parent_authoring_did = %parent_authoring_did,
                    spawn_target_did = %row_spawn_target_did,
                    local_did = %local_did,
                    "subagent source skipping trusted spawn: immutable spawn target is not this host DID",
                );
                return Ok(None);
            }
            if snapshot.behavior(&row_spawn_behavior_id).is_none() {
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    target_behavior_id = %row_spawn_behavior_id,
                    local_did = %snapshot.local_did,
                    "trusted peer target behavior is not in this host runtime snapshot",
                );
                return Ok(None);
            }
            if !load_behavior_allow_cross_deployment(&self.node, local_did, &row_spawn_behavior_id)
                .await
                .unwrap_or(false)
            {
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    target_behavior_id = %row_spawn_behavior_id,
                    "trusted peer target behavior has not enabled cross-deployment spawning",
                );
                return Ok(None);
            }
            PinnedTarget {
                target_agent_did: row_spawn_target_did.to_owned(),
                behavior_id: row_spawn_behavior_id.to_owned(),
            }
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
            let Some(target) = authorization
                .resolve_target(spawn_args.target_name())
                .cloned()
            else {
                return Ok(None);
            };
            if target.target_agent_did != row_spawn_target_did {
                let failed = self
                    .fail_unauthorized_tool_call(
                        &row,
                        "/name",
                        spawn_args.target_name(),
                        "resolved target does not match immutable spawn_target_did",
                        &authorization.allowed_target_names(),
                    )
                    .await?;
                self.processed_tool_calls.insert(processed_key);
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    parent_tool_call_id = %parent_tool_call_id,
                    spawn_target_did = %row_spawn_target_did,
                    resolved_target_did = %target.target_agent_did,
                    failed_tool_call = failed,
                    "subagent source rejected target that conflicts with immutable admission",
                );
                return Ok(None);
            }
            if target.behavior_id != row_spawn_behavior_id {
                let failed = self
                    .fail_unauthorized_tool_call(
                        &row,
                        "/name",
                        spawn_args.target_name(),
                        "resolved target does not match immutable spawn_behavior_id",
                        &authorization.allowed_target_names(),
                    )
                    .await?;
                self.processed_tool_calls.insert(processed_key);
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    parent_tool_call_id = %parent_tool_call_id,
                    spawn_behavior_id = %row_spawn_behavior_id,
                    resolved_behavior_id = %target.behavior_id,
                    failed_tool_call = failed,
                    "subagent source rejected behavior that conflicts with immutable admission",
                );
                return Ok(None);
            }
            PinnedTarget {
                target_agent_did: target.target_agent_did,
                behavior_id: target.behavior_id,
            }
        };

        if snapshot.behavior(&target.behavior_id).is_none() {
            tracing::warn!(
                parent_request_id = %parent_request_id,
                parent_tool_call_id = %parent_tool_call_id,
                target_name = %spawn_args.target_name(),
                target_behavior_id = %target.behavior_id,
                local_did = %snapshot.local_did,
                "subagent source target behavior is not in the active runtime snapshot; skipping spawn",
            );
            return Ok(None);
        }

        if !trusted_paired_peer {
            let local_did = snapshot.local_did.trim();
            let target_owner_did = row_spawn_target_did;
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

        let parent_depth = match parent.as_ref() {
            Some(parent) => {
                let row_depth = parent
                    .subagent_depth
                    .and_then(|depth| u32::try_from(depth).ok())
                    .ok_or(IllegalToolCallTransition::ParentLinkageIncoherent)?;
                row_depth
            }
            None if trusted_paired_peer => row
                .delegated_input
                .as_ref()
                .map(|input| input.parent_subagent_depth)
                .context("trusted subagent bridge omitted immutable parent depth")?,
            None => return Ok(None),
        };
        let deadline =
            effective_deadline(row.deadline_at.as_deref(), spawn_args.deadline.as_deref());
        let child_agent_did = target.target_agent_did.clone();
        let parent_workspace =
            delegated_parent_workspace(&parent_authoring_did, row.delegated_workspace.as_ref())?;
        let operator_tool_root = crate::workspace::process_operator_tool_root();
        let workspace = match resolve_child_workspace(
            &self.node,
            &parent_workspace,
            spawn_args.workspace.as_ref(),
            None,
            &child_agent_did,
            &parent_tool_call_id,
            &parent_request_id,
            operator_tool_root.as_deref(),
        )
        .await
        {
            Ok(lineage) => lineage,
            Err(error) => {
                let failed = self.fail_workspace_tool_call(&row, error).await?;
                self.processed_tool_calls.insert(processed_key);
                tracing::warn!(
                    parent_request_id = %parent_request_id,
                    parent_tool_call_id = %parent_tool_call_id,
                    failed_tool_call = failed,
                    "subagent source rejected spawn because workspace could not be resolved"
                );
                return Ok(None);
            }
        };
        let request_id = if trusted_paired_peer {
            create_subagent_request_with_trusted_parent_request_id_and_workspace(
                &self.node,
                child_request_id.clone(),
                parent_request_id.clone(),
                parent_request_doc_id.clone(),
                parent_tool_call_id.clone(),
                row.doc_id.clone(),
                parent_depth,
                child_agent_did,
                target.behavior_id.clone(),
                spawn_args.prompt.clone(),
                deadline,
                parent_authoring_did.clone(),
                workspace,
            )
            .await?
        } else {
            create_subagent_request_with_request_id_and_workspace(
                &self.node,
                child_request_id.clone(),
                parent_request_id.clone(),
                parent_request_doc_id.clone(),
                parent_tool_call_id.clone(),
                row.doc_id.clone(),
                parent_depth,
                child_agent_did,
                target.behavior_id.clone(),
                spawn_args.prompt.clone(),
                deadline,
                workspace,
            )
            .await?
        };

        // Only an explicit bridge settlement reaches the child; the parent's
        // interrupt or terminal state never does.
        let latest_bridge = self.load_tool_call(doc_id).await;

        // Lean `SpawnClaimFence`: a bridge that left `running` or carries a
        // cancel intent would have failed the running-bridge gate above, so its
        // child is fenced before any claim, whatever the cancel policy.
        let bridge_fenced = match &latest_bridge {
            Ok(Some(latest)) => {
                latest.lifecycle_state.as_deref() != Some("running")
                    || non_empty(latest.cancel_cascade_intent_at.as_deref()).is_some()
            }
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(
                    child_request_id = %request_id,
                    %error,
                    "subagent source failed to re-read bridge after child create; the claim gate and cancel mirror remain the fence",
                );
                false
            }
        };
        if bridge_fenced {
            tracing::info!(
                child_request_id = %request_id,
                parent_request_id = %parent_request_id,
                "subagent source: bridge settled while the child was created; interrupting it before claim",
            );
            self.interrupt_created_child(doc_id, &request_id).await;
        }

        self.processed_tool_calls.insert(processed_key);
        let fired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let task = ResolvedTask {
            task_id: format!("subagent:{parent_tool_call_id}"),
            name: Some(format!("Subagent {target}", target = target.behavior_id)),
            behavior_id: target.behavior_id,
            prompt_template: spawn_args.prompt,
            goal_objective_template: None,
            goal_token_budget: None,
            output_schema_ref: None,
            hooks: Vec::new(),
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
            correlation: None,
            group_vars: None,
            trigger_context: None,
            args_vars: None,
            durable_fire_key: crate::trigger_engine::durable_fire_key("subagent", &[&request_id]),
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

fn delegated_parent_workspace(
    parent_authoring_did: &str,
    delegated_workspace: Option<&gents_protocol::output::DelegatedWorkspace>,
) -> anyhow::Result<ParentWorkspaceStamp> {
    let lineage = match delegated_workspace {
        Some(workspace) => crate::lifecycle::WorkspaceLineage {
            workspace_id: Some(workspace.workspace_id.clone()),
            workspace_owner_agent_did: Some(workspace.workspace_owner_agent_did.clone()),
            workspace_authority: Some(workspace.workspace_authority.clone()),
            workspace_seal_hash: workspace.workspace_seal_hash.clone(),
        },
        None => crate::lifecycle::WorkspaceLineage::default(),
    };
    lineage.require_authority_if_workspace_id()?;
    Ok(ParentWorkspaceStamp::from_fields(
        parent_authoring_did,
        lineage.workspace_id.as_deref(),
        lineage.workspace_owner_agent_did.as_deref(),
        lineage.workspace_authority.as_deref(),
        lineage.workspace_seal_hash.as_deref(),
    ))
}

impl EventDeliveryRuntimeContract for SubagentSource {
    const EVENT_DELIVERY_CONTRACT: EventDeliverySourceContract = EventDeliverySourceContract {
        name: "SubagentSource",
        dedupe_policy: "monotone_once",
        rescan_bounded_by: 1,
        deviation: None,
    };
}

impl TriggerSource for SubagentSource {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>> {
        Box::pin(async move {
            self.ensure_subscription();
            loop {
                let mut message = None;
                let mut dropped = 0;
                let rescan_due = {
                    let subscription = self
                        .subscription
                        .as_mut()
                        .expect("subagent source subscription opened before polling");
                    let rescan_due = tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => return None,
                        res = self.snapshot_rx.changed() => {
                            if res.is_err() {
                                return None;
                            }
                            continue;
                        }
                        _ = self.rescan_tick.tick() => true,
                        msg = subscription.recv() => {
                            match msg {
                                Some(received) => {
                                    message = Some(received);
                                    false
                                }
                                None => {
                                    tracing::warn!(
                                        "subagent source subscription channel closed; source exiting",
                                    );
                                    return None;
                                }
                            }
                        }
                    };
                    if !rescan_due {
                        dropped = subscription.check_and_reset_dropped();
                    }
                    rescan_due
                };
                if rescan_due {
                    if let Some(intent) = self.rescan_running_bridge_rows().await {
                        return Some(intent);
                    }
                    continue;
                }
                let message = message.expect("subscription recv branch sets message");

                if dropped > 0 {
                    tracing::warn!(
                        dropped,
                        "subagent source dropped messages; periodic rescan will recover child spawns",
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

#[cfg(test)]
mod accepted_target_drift_tests {
    use super::*;
    use crate::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };
    use crate::{Collection, ConfigAccess};
    use serde_json::json;

    #[tokio::test]
    async fn local_source_fails_accepted_bridge_when_target_name_drifts_from_immutable_did() {
        let admitted = published_admission(PublishedAdmissionOptions {
            name: "source-target-drift".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            cancel_policy: CancelPolicy::Cascade,
            spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
                tool_call_id: "bridge-native-tool".into(),
                child_request_id: "child-source-target-drift".into(),
                spawn_target_did: "overridden-by-fixture".into(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: AwaitMode::Background,
            }),
            ..Default::default()
        })
        .await
        .expect("publish canonically accepted spawn bridge");
        let node = admitted.node;
        let path = admitted.path;
        let agent_did = admitted.agent_did;
        let mut tool = admitted.tool;
        let tool_doc_id = tool.doc_id().expect("accepted bridge document").to_string();
        // The production background hook publishes the one immutable receipt
        // after dispatch and before the source may terminalize a denial.
        tool.publish_background_receipt("child started")
            .await
            .expect("publish accepted background receipt");

        crate::test_support::install_test_behavior(&node, &agent_did, "general").await;
        let documents = [
            (
                Collection::Tools,
                json!({
                    "agent_did": agent_did,
                    "tools_id": "general:tools",
                    "subagents": {
                        "target_ids": ["target-drift"],
                        "spawn_enabled": true,
                        "background_enabled": true,
                        "allow_cross_principal": true
                    }
                }),
            ),
            (
                Collection::SubagentTarget,
                json!({
                    "agent_did": agent_did,
                    "target_id": "target-drift",
                    "name": "child",
                    "target_agent_did": "did:key:zDifferentAuthorizedTarget",
                    "behavior_id": "general"
                }),
            ),
        ];
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        ConfigAccess::transact_local(&node, None, "test.accepted_target_drift", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();

        let snapshot = ActiveRuntimeSnapshot {
            generation: 1,
            principal: None,
            local_did: agent_did.clone(),
            default_behavior_id: "general".into(),
            behaviors: HashMap::new(),
            tool_surfaces: HashMap::new(),
            backend_admission_configs: HashMap::new(),
            unavailable_behaviors: HashMap::new(),
            active_schedules: HashMap::new(),
            unavailable_schedules: HashSet::new(),
            active_event_triggers: HashMap::new(),
            unavailable_event_triggers: HashSet::new(),
            active_tasks: HashMap::new(),
            dispatchers: Default::default(),
            behavior_executor_capacities: HashMap::new(),
            behavior_executor_queue_capacities: HashMap::new(),
        };
        let (_snapshot_tx, snapshot_rx) = watch::channel(Arc::new(snapshot));
        let mut source = SubagentSource::with_subscription_source_for_test(
            node.clone(),
            snapshot_rx,
            node.clone(),
            HashSet::new(),
            CancellationToken::new(),
        );
        assert!(source
            .build_intent_for_tool_call_doc(&tool_doc_id)
            .await
            .expect("source must evaluate accepted target drift")
            .is_none());

        let escaped = escape_graphql_string(&tool_doc_id);
        let response = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state tool_failure_class }} AgentRequest(filter: {{ request_id: {{ _eq: "child-source-target-drift" }} }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = response.data.expect("target-drift observation");
        let tool_rows = data["AgentToolCall"].as_array().expect("bridge rows");
        assert_eq!(tool_rows.len(), 1);
        assert_eq!(tool_rows[0]["lifecycle_state"], "failed");
        assert_eq!(tool_rows[0]["tool_failure_class"], "serviceUnavailable");
        assert!(data["AgentRequest"]
            .as_array()
            .expect("child rows")
            .is_empty());

        drop(source);
        drop(tool);
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(test)]
mod delegated_workspace_tests {
    use super::*;

    #[test]
    fn immutable_workspace_snapshot_builds_parent_stamp() {
        let stamp = delegated_parent_workspace(
            "did:parent",
            Some(&gents_protocol::output::DelegatedWorkspace {
                workspace_id: "workspace-1".into(),
                workspace_owner_agent_did: "did:workspace-owner".into(),
                workspace_authority: "readOnly".into(),
                workspace_seal_hash: Some("sealed-1".into()),
            }),
        )
        .unwrap();
        assert_eq!(stamp.workspace_id.as_deref(), Some("workspace-1"));
        assert_eq!(
            stamp.workspace_owner_agent_did.as_deref(),
            Some("did:workspace-owner")
        );
        assert_eq!(stamp.workspace_authority.as_deref(), Some("readOnly"));
        assert_eq!(stamp.workspace_seal_hash.as_deref(), Some("sealed-1"));
    }

    #[test]
    fn absent_snapshot_is_only_an_unbound_parent_stamp() {
        let stamp = delegated_parent_workspace("did:parent", None).unwrap();
        assert!(!stamp.has_workspace_id());
        assert!(stamp.workspace_owner_agent_did.is_none());
        assert!(stamp.workspace_authority.is_none());
    }

    #[test]
    fn malformed_immutable_workspace_snapshot_is_rejected() {
        assert!(delegated_parent_workspace(
            "did:parent",
            Some(&gents_protocol::output::DelegatedWorkspace {
                workspace_id: "workspace-1".into(),
                workspace_owner_agent_did: "".into(),
                workspace_authority: "readOnly".into(),
                workspace_seal_hash: None,
            }),
        )
        .is_err());
    }
}
