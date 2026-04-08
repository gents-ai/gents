//! P2P event-driven watcher — detects new agent requests arriving via DefraDB gossip.
//!
//! Subscribes to DefraDB's event bus for `Update` events. When a document
//! arrives via P2P replication (`is_relay == true`), queries the AgentRequest
//! collection to check if it's a new request for this agent. This is how the
//! daemon knows Amy has sent a message.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_node::{EmbeddedNode, EventName};
use serde::Deserialize;

/// A new agent request detected by the watcher.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub doc_id: String,
    pub request_id: String,
    pub agent_did: String,
    pub session_id: String,
    pub content: String,
    pub created_at: String,
}

/// Watches for new agent requests arriving via P2P replication.
///
/// Uses DefraDB's event bus (`EventName::Update` with `is_relay == true`)
/// to detect documents arriving from remote peers. For each event, queries
/// the `AgentRequest` collection to check if the document is a pending
/// request for this agent.
pub trait Watcher: Send + Sync {
    /// Block and yield agent requests as they arrive.
    /// Returns `None` when the event bus closes.
    fn next_request(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Result<AgentRequest>>> + Send;
}

/// Event-driven watcher backed by DefraDB's event bus.
pub struct DefraWatcher {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    subscription: events::Subscription,
    processed_request_ids: HashMap<String, Instant>,
}

impl DefraWatcher {
    /// Create a new watcher for the given agent DID.
    ///
    /// Subscribes to `EventName::Update` events on the embedded node's event bus.
    /// Only events with `is_relay == true` (P2P arrivals) are processed.
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str) -> Self {
        let subscription = node.subscribe(&[EventName::Update]);
        Self {
            node,
            agent_did: agent_did.to_string(),
            subscription,
            processed_request_ids: HashMap::new(),
        }
    }

    /// Query DefraDB for an AgentRequest document by doc_id, filtered by
    /// this agent's DID and "pending" status.
    pub async fn try_fetch_request(&self, doc_id: &str) -> Result<Option<AgentRequest>> {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        status: {{ _eq: "pending" }}
                    }}
                ) {{
                    request_id
                    agent_did
                    session_id
                    content
                    created_at
                }}
            }}"#,
            doc_id = doc_id,
            agent_did = self.agent_did,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher query failed: {:?}", resp.errors);
        }

        let docs: Vec<AgentRequestRow> =
            match resp.data.as_ref().and_then(|d| d.get("AgentRequest")) {
                Some(value) => serde_json::from_value(value.clone())?,
                None => Vec::new(),
            };

        match docs.into_iter().next() {
            Some(row) => Ok(Some(AgentRequest {
                doc_id: doc_id.to_string(),
                request_id: row.request_id,
                agent_did: row.agent_did,
                session_id: row.session_id,
                content: row.content,
                created_at: row.created_at,
            })),
            None => Ok(None),
        }
    }

    async fn pending_requests(&self) -> Result<Vec<AgentRequest>> {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{agent_did}" }},
                        status: {{ _eq: "pending" }}
                    }},
                    order: {{ created_at: ASC }}
                ) {{
                    _docID
                    request_id
                    agent_did
                    session_id
                    content
                    created_at
                }}
            }}"#,
            agent_did = self.agent_did,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher pending-request query failed: {:?}", resp.errors);
        }

        let docs: Vec<PendingAgentRequestRow> =
            match resp.data.as_ref().and_then(|d| d.get("AgentRequest")) {
                Some(value) => serde_json::from_value(value.clone())?,
                None => Vec::new(),
            };

        Ok(docs
            .into_iter()
            .map(|row| AgentRequest {
                doc_id: row.doc_id,
                request_id: row.request_id,
                agent_did: row.agent_did,
                session_id: row.session_id,
                content: row.content,
                created_at: row.created_at,
            })
            .collect())
    }
}

/// Maximum number of processed request IDs to track. Expired entries are
/// pruned first; if the map still grows beyond this, clear it and rely on
/// the `status != "pending"` filter in DefraDB queries to avoid reprocessing.
const MAX_PROCESSED_IDS: usize = 10_000;

/// If no P2P gossip event arrives within this interval, poll DefraDB
/// directly for pending requests. This catches gossip stalls, network
/// partitions, and events missed while the daemon was down.
const GOSSIP_FALLBACK_POLL: Duration = Duration::from_secs(30);

/// Keep recently-yielded requests suppressed for one poll interval so a
/// dedup rejection cannot immediately re-enter the queue and starve others.
const PROCESSED_REQUEST_COOLDOWN: Duration = Duration::from_secs(30);

fn prune_processed_requests(processed_request_ids: &mut HashMap<String, Instant>, now: Instant) {
    processed_request_ids.retain(|_, processed_at| {
        now.saturating_duration_since(*processed_at) < PROCESSED_REQUEST_COOLDOWN
    });

    if processed_request_ids.len() > MAX_PROCESSED_IDS {
        tracing::info!(
            count = processed_request_ids.len(),
            "pruning processed request ID set"
        );
        processed_request_ids.clear();
    }
}

fn request_is_cooling_down(
    processed_request_ids: &mut HashMap<String, Instant>,
    request_id: &str,
    now: Instant,
) -> bool {
    match processed_request_ids.get(request_id).copied() {
        Some(processed_at)
            if now.saturating_duration_since(processed_at) < PROCESSED_REQUEST_COOLDOWN =>
        {
            true
        }
        Some(_) => {
            processed_request_ids.remove(request_id);
            false
        }
        None => false,
    }
}

fn mark_processed(
    processed_request_ids: &mut HashMap<String, Instant>,
    request_id: &str,
    now: Instant,
) {
    processed_request_ids.insert(request_id.to_string(), now);
}

fn take_next_eligible_pending_request(
    processed_request_ids: &mut HashMap<String, Instant>,
    requests: Vec<AgentRequest>,
    now: Instant,
) -> Option<AgentRequest> {
    for request in requests {
        if request_is_cooling_down(processed_request_ids, &request.request_id, now) {
            continue;
        }

        mark_processed(processed_request_ids, &request.request_id, now);
        return Some(request);
    }

    None
}

impl Watcher for DefraWatcher {
    async fn next_request(&mut self) -> Option<Result<AgentRequest>> {
        loop {
            let now = Instant::now();
            prune_processed_requests(&mut self.processed_request_ids, now);

            // Always check for pending requests first (picks up
            // recovered requests and anything gossip missed).
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

            // Wait for a P2P event, but time out and poll if gossip stalls.
            let msg =
                match tokio::time::timeout(GOSSIP_FALLBACK_POLL, self.subscription.recv()).await {
                    Ok(Some(msg)) => msg,
                    Ok(None) => {
                        // Event bus closed.
                        return None;
                    }
                    Err(_timeout) => {
                        // No gossip event within the poll interval — loop back
                        // to check for pending requests via direct query.
                        tracing::trace!("gossip quiet, polling for pending requests");
                        continue;
                    }
                };

            // Only process P2P relay events (documents arriving from remote peers).
            let update = match msg.as_update() {
                Some(u) if u.is_relay => u,
                _ => continue,
            };

            let doc_id = &update.doc_id;
            tracing::trace!(doc_id = %doc_id, "P2P update event received");

            // Check dropped events — log but don't fail.
            let dropped = self.subscription.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(
                    dropped = dropped,
                    "event bus dropped messages — may have missed requests"
                );
            }

            // Query to see if this is a pending AgentRequest for us.
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
                Ok(None) => {
                    // Not an AgentRequest for us — continue listening.
                    continue;
                }
                Err(e) => {
                    tracing::error!(error = %e, doc_id = %doc_id, "failed to query agent request");
                    return Some(Err(e));
                }
            }
        }
    }
}

/// Internal deserialization target for AgentRequest query results.
#[derive(Deserialize)]
struct AgentRequestRow {
    request_id: String,
    agent_did: String,
    session_id: String,
    content: String,
    created_at: String,
}

#[derive(Deserialize)]
struct PendingAgentRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    agent_did: String,
    session_id: String,
    content: String,
    created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Instant;

    #[test]
    fn agent_request_clone() {
        let req = AgentRequest {
            doc_id: "abc".into(),
            request_id: "req-1".into(),
            agent_did: "did:key:z123".into(),
            session_id: "sess-1".into(),
            content: "hello".into(),
            created_at: "2026-03-12T00:00:00Z".into(),
        };
        let cloned = req.clone();
        assert_eq!(cloned.doc_id, "abc");
        assert_eq!(cloned.content, "hello");
    }

    #[test]
    fn cooling_down_request_does_not_block_other_pending_sessions() {
        let now = Instant::now();
        let mut processed_request_ids = HashMap::from([("req-1".to_string(), now)]);

        let request = take_next_eligible_pending_request(
            &mut processed_request_ids,
            vec![request("req-1", "sess-1"), request("req-2", "sess-2")],
            now,
        )
        .expect("eligible request");

        assert_eq!(request.request_id, "req-2");
        assert!(processed_request_ids.contains_key("req-1"));
        assert!(processed_request_ids.contains_key("req-2"));
    }

    #[test]
    fn cooled_down_request_becomes_eligible_again() {
        let now = Instant::now();
        let mut processed_request_ids = HashMap::from([("req-1".to_string(), now)]);
        let later = now + PROCESSED_REQUEST_COOLDOWN + Duration::from_millis(1);

        let request = take_next_eligible_pending_request(
            &mut processed_request_ids,
            vec![request("req-1", "sess-1")],
            later,
        )
        .expect("eligible request");

        assert_eq!(request.request_id, "req-1");
        assert_eq!(processed_request_ids.get("req-1").copied(), Some(later));
    }

    fn request(request_id: &str, session_id: &str) -> AgentRequest {
        AgentRequest {
            doc_id: format!("doc-{request_id}"),
            request_id: request_id.to_string(),
            agent_did: "did:key:z123".into(),
            session_id: session_id.to_string(),
            content: "hello".into(),
            created_at: "2026-03-12T00:00:00Z".into(),
        }
    }
}
