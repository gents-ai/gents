use std::collections::HashMap;
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
    pub doc_id: String,
    pub request_id: String,
    pub agent_did: String,
    pub requester_did: Option<String>,
    pub behavior_id: String,
    pub session_id: String,
    pub content: String,
    pub max_total_tokens: Option<i64>,
    pub input: gents_protocol::request_input::RequestInput,
    pub execution_origin: Option<String>,
    pub created_at: String,
    pub deadline: Option<String>,
    pub execution_generation: Option<String>,
    pub execution_lease_expires_at: Option<String>,
    pub execution_progress_seq: u64,
    pub subagent_depth: u32,
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
    pub workspace_owner_agent_did: Option<String>,
    pub workspace_authority: Option<String>,
    pub workspace_seal_hash: Option<String>,
}

impl AgentRequest {
    pub(crate) fn has_automated_trigger_lineage(&self) -> bool {
        let has_trigger_id = self
            .caused_by_trigger_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        has_trigger_id
            && matches!(
                self.caused_by_trigger_kind.as_deref().map(str::trim),
                Some("event" | "schedule")
            )
    }
}

impl TryFrom<gents_protocol::row::AgentRequestRow> for AgentRequest {
    type Error = anyhow::Error;

    fn try_from(row: gents_protocol::row::AgentRequestRow) -> Result<Self> {
        let subagent_depth = row
            .subagent_depth
            .map(u32::try_from)
            .transpose()
            .context("agent request subagent_depth must fit in u32")?
            .unwrap_or(0);
        let request = Self {
            doc_id: row.doc_id.context("agent request is missing _docID")?,
            request_id: row.request_id,
            agent_did: row
                .agent_did
                .context("agent request is missing agent_did")?,
            requester_did: normalize_optional_string(row.requester_did),
            behavior_id: row
                .behavior_id
                .context("agent request is missing behavior_id")?,
            session_id: row
                .session_id
                .context("agent request is missing session_id")?,
            content: row.content.context("agent request is missing content")?,
            max_total_tokens: row.max_total_tokens,
            input: row.input.unwrap_or_default(),
            execution_origin: normalize_optional_string(row.execution_origin),
            created_at: row
                .created_at
                .context("agent request is missing created_at")?,
            deadline: normalize_optional_string(row.deadline),
            execution_generation: normalize_optional_string(row.execution_generation),
            execution_lease_expires_at: normalize_optional_string(row.execution_lease_expires_at),
            execution_progress_seq: row
                .execution_progress_seq
                .map(u64::try_from)
                .transpose()
                .context("agent request execution_progress_seq must fit in u64")?
                .unwrap_or(0),
            subagent_depth,
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
            workspace_owner_agent_did: row.workspace_owner_agent_did,
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
        !req.behavior_id.is_empty() && req.behavior_id.trim() == req.behavior_id,
        "agent request behavior_id must be a canonical nonblank identifier"
    );
    if req.max_total_tokens.is_some_and(|limit| limit < 0) {
        anyhow::bail!("agent request max_total_tokens must be non-negative");
    }
    let has_parent_req = req.caused_by_parent_request_id.is_some();
    let has_parent_tc = req.caused_by_parent_tool_call_id.is_some();
    let has_parent_req_doc = req.caused_by_parent_request_doc_id.is_some();
    let has_parent_tc_doc = req.caused_by_parent_tool_call_doc_id.is_some();
    let request_only_control_link = has_parent_req
        && !has_parent_tc
        && req.input.queue.as_ref().is_some_and(|queue| {
            matches!(
                queue.source,
                gents_protocol::request_input::QueueSource::Steering
                    | gents_protocol::request_input::QueueSource::Goal
                    | gents_protocol::request_input::QueueSource::BackgroundCompletion
            )
        });
    if has_parent_req != has_parent_req_doc || has_parent_tc != has_parent_tc_doc {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    if has_parent_req != has_parent_tc && !request_only_control_link {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    if req.subagent_depth > 0
        && !request_only_control_link
        && !(has_parent_req && has_parent_tc && has_parent_req_doc && has_parent_tc_doc)
    {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    let is_top_level = !has_parent_req;
    if is_top_level && req.subagent_depth != 0 {
        return Err(IllegalToolCallTransition::ParentLinkageIncoherent.into());
    }
    if !is_top_level && req.subagent_depth == 0 && !request_only_control_link {
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
    agent_did: String,
    subscription: events::Subscription,
    processed_request_ids: HashMap<String, Instant>,
}

impl DefraWatcher {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str) -> Self {
        Self::with_subscription_source(node.clone(), node, agent_did)
    }

    pub fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        node: Arc<EmbeddedNode>,
        agent_did: &str,
    ) -> Self {
        let subscription = subs.subscribe_updates();
        Self {
            node,
            agent_did: agent_did.to_string(),
            subscription,
            processed_request_ids: HashMap::new(),
        }
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

fn request_update_wakeup(message: &events::Message) -> Option<&events::Update> {
    message.as_update()
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

            let msg =
                match tokio::time::timeout(GOSSIP_FALLBACK_POLL, self.subscription.recv()).await {
                    Ok(Some(msg)) => msg,
                    Ok(None) => return None,
                    Err(_timeout) => {
                        tracing::trace!("gossip quiet, polling for pending requests");
                        continue;
                    }
                };

            let Some(update) = request_update_wakeup(&msg) else {
                continue;
            };

            let doc_id = &update.doc_id;
            tracing::trace!(doc_id = %doc_id, is_relay = update.is_relay, "DefraDB update event received");

            let dropped = self.subscription.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(
                    dropped = dropped,
                    "event bus dropped messages — may have missed requests"
                );
            }

            // An event is a rescan signal, not permission to bypass the
            // durable queue. Fetching this doc directly allowed a stream of
            // newly-created descendants to overtake an older completion wake
            // in another session. The next loop iteration performs the global
            // FIFO + aged-wake scan and still benefits from immediate gossip.
            continue;
        }
    }
}
