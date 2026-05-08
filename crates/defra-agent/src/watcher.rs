use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use defra_node::{EmbeddedNode, EventName};

mod cooldown;
mod query;
#[cfg(test)]
mod tests;

use cooldown::{
    mark_processed, prune_processed_requests, request_is_cooling_down,
    take_next_eligible_pending_request, GOSSIP_FALLBACK_POLL, PROCESSED_REQUEST_COOLDOWN,
};

#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub doc_id: String,
    pub request_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
    pub session_id: String,
    pub content: String,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub max_tokens: Option<i64>,
    pub metadata: Option<String>,
    pub execution_origin: Option<String>,
    pub created_at: String,
    pub subagent_depth: u32,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
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
        let subscription = node.subscribe(&[EventName::Update]);
        Self {
            node,
            agent_did: agent_did.to_string(),
            subscription,
            processed_request_ids: HashMap::new(),
        }
    }
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

            let update = match msg.as_update() {
                Some(u) if u.is_relay => u,
                _ => continue,
            };

            let doc_id = &update.doc_id;
            tracing::trace!(doc_id = %doc_id, "P2P update event received");

            let dropped = self.subscription.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(
                    dropped = dropped,
                    "event bus dropped messages — may have missed requests"
                );
            }

            match self.try_fetch_request(doc_id).await {
                Ok(Some(request)) => {
                    let now = Instant::now();
                    if request_is_cooling_down(
                        &mut self.processed_request_ids,
                        &request.request_id,
                        now,
                    ) {
                        tracing::debug!(
                            request_id = %request.request_id,
                            doc_id = %doc_id,
                            cooldown_secs = PROCESSED_REQUEST_COOLDOWN.as_secs(),
                            "skipping cooling-down P2P request"
                        );
                        continue;
                    }
                    tracing::info!(
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        "new agent request detected via P2P"
                    );
                    mark_processed(&mut self.processed_request_ids, &request.request_id, now);
                    return Some(Ok(request));
                }
                Ok(None) => continue,
                Err(e) => {
                    tracing::error!(error = %e, doc_id = %doc_id, "failed to query agent request");
                    return Some(Err(e));
                }
            }
        }
    }
}
