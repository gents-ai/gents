use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::event_delivery_contract::{EventDeliveryRuntimeContract, EventDeliverySourceContract};
use crate::tool_call_lifecycle::IllegalToolCallTransition;
use crate::UpdateSubscriptionSource;

mod cooldown;
mod query;
#[cfg(test)]
mod tests;

use cooldown::{
    prune_processed_requests, take_next_eligible_pending_request, GOSSIP_FALLBACK_POLL,
    PROCESSED_REQUEST_COOLDOWN,
};
pub(crate) use query::{agent_request_from_mutation_response, AGENT_REQUEST_FIELDS};

#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub purpose: gents_protocol::request_admission::RequestPurpose,
    pub doc_id: String,
    pub request_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
    pub agent_id: String,
    pub session_id: String,
    pub content: String,
    pub retry_parent_request_doc_id: Option<String>,
    pub max_total_tokens: Option<i64>,
    pub input: gents_protocol::request_input::RequestInput,
    pub execution_origin: Option<String>,
    pub created_at: String,
    pub deadline: Option<String>,
    pub execution_generation: Option<String>,
    pub execution_lease_secs: Option<i64>,
    pub execution_lease_expires_at: Option<String>,
    pub request_hop: u32,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_request_doc_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    pub caused_by_trigger_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
    pub caused_by_source_doc_id: Option<String>,
    pub caused_by_correlation: Option<String>,
    pub caused_by_trigger_context: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_owner_node_did: Option<String>,
    pub workspace_authority: Option<String>,
    pub workspace_seal_hash: Option<String>,
}

impl AgentRequest {
    pub(crate) fn has_automated_trigger_lineage(&self) -> bool {
        automated_trigger_lineage(
            self.caused_by_trigger_kind.as_deref(),
            self.caused_by_trigger_id.as_deref(),
        )
    }
}

pub(crate) fn automated_trigger_lineage(kind: Option<&str>, id: Option<&str>) -> bool {
    id.is_some_and(|value| !value.trim().is_empty())
        && matches!(kind.map(str::trim), Some("event" | "schedule"))
}

impl TryFrom<gents_protocol::row::AgentRequestRow> for AgentRequest {
    type Error = anyhow::Error;

    fn try_from(row: gents_protocol::row::AgentRequestRow) -> Result<Self> {
        let request_hop = row
            .request_hop
            .map(u32::try_from)
            .transpose()
            .context("agent request request_hop must fit in u32")?
            .unwrap_or(0);
        let execution_lease_secs = match row.execution_lease_secs {
            Some(secs) if secs < 0 => {
                anyhow::bail!("agent request execution_lease_secs must be non-negative");
            }
            other => other,
        };
        let request = Self {
            purpose: row.purpose.context("agent request is missing purpose")?,
            doc_id: row.doc_id.context("agent request is missing _docID")?,
            request_id: row.request_id,
            node_did: row.node_did.context("agent request is missing node_did")?,
            requester_did: normalize_optional_string(row.requester_did),
            agent_id: row.agent_id.context("agent request is missing agent_id")?,
            session_id: row
                .session_id
                .context("agent request is missing session_id")?,
            content: row.content.context("agent request is missing content")?,
            retry_parent_request_doc_id: normalize_optional_string(row.retry_parent_request_doc_id),
            max_total_tokens: row.max_total_tokens,
            input: row.input.unwrap_or_default(),
            execution_origin: normalize_optional_string(row.execution_origin),
            created_at: row
                .created_at
                .context("agent request is missing created_at")?,
            deadline: normalize_optional_string(row.deadline),
            execution_generation: normalize_optional_string(row.execution_generation),
            execution_lease_secs,
            execution_lease_expires_at: normalize_optional_string(row.execution_lease_expires_at),
            request_hop,
            caused_by_parent_request_id: row.caused_by_parent_request_id,
            caused_by_parent_request_doc_id: row.caused_by_parent_request_doc_id,
            caused_by_parent_tool_call_id: row.caused_by_parent_tool_call_id,
            caused_by_parent_tool_call_doc_id: row.caused_by_parent_tool_call_doc_id,
            caused_by_trigger_id: normalize_optional_string(row.caused_by_trigger_id),
            caused_by_trigger_kind: normalize_optional_string(row.caused_by_trigger_kind),
            caused_by_source_doc_id: normalize_optional_string(row.caused_by_source_doc_id),
            caused_by_correlation: normalize_optional_string(row.caused_by_correlation),
            caused_by_trigger_context: normalize_optional_string(row.caused_by_trigger_context),
            workspace_id: normalize_optional_string(row.workspace_id),
            workspace_owner_node_did: row.workspace_owner_node_did,
            workspace_authority: normalize_optional_string(row.workspace_authority),
            workspace_seal_hash: normalize_optional_string(row.workspace_seal_hash),
        };
        validate_agent_request(&request)?;
        Ok(request)
    }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub fn validate_agent_request(req: &AgentRequest) -> Result<()> {
    anyhow::ensure!(
        !req.agent_id.is_empty() && req.agent_id.trim() == req.agent_id,
        "agent request agent_id must be a canonical nonblank identifier"
    );
    if req.max_total_tokens.is_some_and(|limit| limit < 0) {
        anyhow::bail!("agent request max_total_tokens must be non-negative");
    }
    let has_parent_req = req.caused_by_parent_request_id.is_some();
    let has_parent_tc = req.caused_by_parent_tool_call_id.is_some();
    let has_parent_req_doc = req.caused_by_parent_request_doc_id.is_some();
    let has_parent_tc_doc = req.caused_by_parent_tool_call_doc_id.is_some();
    let auxiliary_parent_link = req.purpose.is_auxiliary();
    if auxiliary_parent_link {
        anyhow::ensure!(
            has_parent_req
                && has_parent_req_doc
                && !has_parent_tc
                && !has_parent_tc_doc
                && req.request_hop == 0,
            "auxiliary request requires parent-only provenance, not delegated authority"
        );
    }
    let request_only_control_link = has_parent_req
        && !has_parent_tc
        && (auxiliary_parent_link
            || req.input.queue.as_ref().is_some_and(|queue| {
                matches!(
                    queue.source,
                    gents_protocol::request_input::QueueSource::User
                        | gents_protocol::request_input::QueueSource::Steering
                        | gents_protocol::request_input::QueueSource::Goal
                        | gents_protocol::request_input::QueueSource::BackgroundCompletion
                )
            }));
    if has_parent_req != has_parent_req_doc || has_parent_tc != has_parent_tc_doc {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    if has_parent_req != has_parent_tc && !request_only_control_link {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    if req.request_hop > 0
        && !request_only_control_link
        && !(has_parent_req && has_parent_tc && has_parent_req_doc && has_parent_tc_doc)
    {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    let is_top_level = !has_parent_req;
    if is_top_level && req.request_hop != 0 {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    if !is_top_level && req.request_hop == 0 && !request_only_control_link {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    Ok(())
}

pub trait Watcher: Send + Sync {
    fn next_request(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Result<AgentRequest>>> + Send;
}

pub struct DefraWatcher {
    node: Arc<EmbeddedNode>,
    node_did: String,
    subscription: events::DocumentChangeSubscription,
    processed_request_ids: cooldown::ProcessedRequests,
}

impl DefraWatcher {
    pub fn new(node: Arc<EmbeddedNode>, node_did: &str) -> Result<Self> {
        Self::with_subscription_source(node.clone(), node, node_did)
    }

    pub fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        node: Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<Self> {
        let subscription =
            subs.subscribe_collection_changes(crate::collection_ids(&node, ["AgentRequest"])?);
        Ok(Self {
            node,
            node_did: node_did.to_string(),
            subscription,
            processed_request_ids: RapidHashMap::new(),
        })
    }
}

impl EventDeliveryRuntimeContract for DefraWatcher {
    const EVENT_DELIVERY_CONTRACT: EventDeliverySourceContract = EventDeliverySourceContract {
        name: "Watcher",
        dedupe_policy: "ttl_cooldown",
        rescan_bounded_by: 1,
        deviation: None,
    };
}

async fn wait_for_request_change(
    subscription: &mut events::DocumentChangeSubscription,
) -> Result<Option<events::DocumentChangeBatch>, tokio::time::error::Elapsed> {
    tokio::time::timeout(GOSSIP_FALLBACK_POLL, subscription.recv()).await
}

impl Watcher for DefraWatcher {
    async fn next_request(&mut self) -> Option<Result<AgentRequest>> {
        loop {
            let now = Instant::now();
            prune_processed_requests(&mut self.processed_request_ids, now);

            match self.pending_requests().await {
                Ok(requests) => {
                    let pending_count = requests.len();
                    if let Some(request) = take_next_eligible_pending_request(
                        &mut self.processed_request_ids,
                        requests,
                        now,
                    ) {
                        return Some(Ok(request));
                    }

                    if pending_count > 0 {
                        tracing::debug!(
                            pending_count,
                            cooldown_secs = PROCESSED_REQUEST_COOLDOWN.as_secs(),
                            "all pending requests are cooling down"
                        );
                    }
                }
                Err(e) => return Some(Err(e)),
            }

            let batch = match wait_for_request_change(&mut self.subscription).await {
                Ok(Some(batch)) => batch,
                Ok(None) => return None,
                Err(_timeout) => {
                    tracing::trace!("gossip quiet, polling for pending requests");
                    continue;
                }
            };

            if batch.resync_required {
                tracing::warn!(
                    updates = batch.updates,
                    "request watcher document-change capacity exceeded; durable queue scan remains authoritative"
                );
            } else {
                tracing::trace!(
                    updates = batch.updates,
                    changed_documents = batch.changes.len(),
                    "coalesced DefraDB document changes into one request wakeup"
                );
            }

            continue;
        }
    }
}
