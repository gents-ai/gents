use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::store::config_client::{ConfigAccess, GraphqlEndpoint, TransactionFailure};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::Instant;

use super::{ObservedStore, ObserverMetrics, RuntimeConnectionEvent, RESYNC_RETRY_DELAY};
use crate::client::query::{is_transcript_content_collection, OBSERVED_COLLECTIONS};
use crate::client::PeerRecord;

pub(super) struct Notice {
    pub node: String,
    pub generation: u64,
    pub ready: bool,
    pub denied: bool,
}

pub(super) struct Source {
    pub endpoint: GraphqlEndpoint,
    pub generation: u64,
    pub ready: bool,
    pub denied: bool,
    reprobe_at: Option<Instant>,
    abort: AbortHandle,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

#[derive(Default)]
pub(super) struct Sources {
    pub entries: BTreeMap<String, Source>,
    denials: BTreeMap<String, u32>,
    generation: u64,
}

const REPROBE_BASE_MS: u64 = 1000;
const REPROBE_CAP_MS: u64 = 30_000;

/// `RuntimeConnectionObservation.reprobeDelayMs`.
pub(super) fn reprobe_delay(denials: u32) -> Duration {
    Duration::from_millis(
        1_u64
            .checked_shl(denials)
            .and_then(|factor| factor.checked_mul(REPROBE_BASE_MS))
            .map_or(REPROBE_CAP_MS, |delay| delay.min(REPROBE_CAP_MS)),
    )
}

/// `RuntimeConnectionObservation.nextDenials`.
pub(super) fn next_denials(denials: u32, event: RuntimeConnectionEvent) -> u32 {
    match event {
        RuntimeConnectionEvent::PermissionDenied => denials.saturating_add(1),
        RuntimeConnectionEvent::SnapshotSucceeded => 0,
        RuntimeConnectionEvent::SourceStarted
        | RuntimeConnectionEvent::SubscriptionsReady
        | RuntimeConnectionEvent::TransportFailed => denials,
    }
}

impl Sources {
    /// Publishes `event` for `node` and advances its re-probe schedule. A
    /// denial stops the source and schedules its restart at the same endpoint.
    pub(super) fn observe(
        &mut self,
        node: &str,
        event: RuntimeConnectionEvent,
        store: &ObservedStore,
        now: Instant,
    ) {
        store.observe_runtime_connection(node, event);
        let denials = self.denials.get(node).copied().unwrap_or_default();
        if matches!(event, RuntimeConnectionEvent::PermissionDenied) {
            if let Some(source) = self.entries.get_mut(node) {
                source.ready = false;
                source.denied = true;
                source.reprobe_at = Some(now + reprobe_delay(denials));
                source.abort.abort();
            }
        }
        self.denials
            .insert(node.to_owned(), next_denials(denials, event));
    }

    pub(super) fn next_reprobe(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|source| source.reprobe_at)
            .min()
    }

    /// Drops denied sources whose re-probe is due so the next reconcile
    /// restarts them; returns whether any was dropped.
    pub(super) fn take_due_reprobes(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, source| source.reprobe_at.is_none_or(|due| due > now));
        self.entries.len() != before
    }

    pub(super) fn reconcile(
        &mut self,
        peers: &[PeerRecord],
        capability: &gents::store::config_client::ClientSigningCapability,
        notices: &mpsc::UnboundedSender<Notice>,
        store: &Arc<ObservedStore>,
        metrics: &Arc<ObserverMetrics>,
    ) {
        let endpoints: BTreeMap<_, _> = peers
            .iter()
            .filter_map(|peer| {
                crate::local_runtime::operator_endpoint(peer, capability)
                    .map(|endpoint| (peer.node_did.clone(), endpoint))
            })
            .collect();
        self.entries.retain(|node, source| {
            endpoints
                .get(node)
                .is_some_and(|endpoint| endpoint.to_string() == source.endpoint.to_string())
                && (source.denied || !source.abort.is_finished())
        });
        self.denials.retain(|node, _| endpoints.contains_key(node));
        for (node, endpoint) in endpoints {
            if self.entries.contains_key(&node) {
                continue;
            }
            store.observe_runtime_connection(&node, RuntimeConnectionEvent::SourceStarted);
            self.generation += 1;
            let current = self.generation;
            let task = tokio::spawn(observe(
                node.clone(),
                endpoint.clone(),
                current,
                notices.clone(),
                store.clone(),
                metrics.clone(),
            ));
            self.entries.insert(
                node,
                Source {
                    endpoint,
                    generation: current,
                    ready: false,
                    denied: false,
                    reprobe_at: None,
                    abort: task.abort_handle(),
                },
            );
        }
    }
}

async fn observe(
    node: String,
    endpoint: GraphqlEndpoint,
    generation: u64,
    notices: mpsc::UnboundedSender<Notice>,
    store: Arc<ObservedStore>,
    metrics: Arc<ObserverMetrics>,
) {
    loop {
        let denied = if let Err(error) =
            connected(&node, &endpoint, generation, &notices, &store, &metrics).await
        {
            tracing::warn!(node_did = %node, error = %error, "runtime subscription disconnected");
            error.permission_denied()
        } else {
            false
        };
        store.observe_request_event(None);
        if notices
            .send(Notice {
                node: node.clone(),
                generation,
                ready: false,
                denied,
            })
            .is_err()
        {
            return;
        }
        if denied {
            return;
        }
        tokio::time::sleep(RESYNC_RETRY_DELAY).await;
    }
}

/// DefraDB emits changes only after subscription response headers; arm every
/// root before allowing a snapshot. Dropping the JoinSet cancels every stream on
/// disconnect, credential renewal or source replacement.
async fn connected(
    node: &str,
    endpoint: &GraphqlEndpoint,
    generation: u64,
    notices: &mpsc::UnboundedSender<Notice>,
    store: &Arc<ObservedStore>,
    metrics: &Arc<ObserverMetrics>,
) -> Result<()> {
    let access = ConfigAccess::Graphql(endpoint.clone());
    let mut streams = Vec::with_capacity(OBSERVED_COLLECTIONS.len() + 2);
    for collection in OBSERVED_COLLECTIONS
        .iter()
        .copied()
        .chain(["AgentToolControl", "AgentToolControlClaim"])
    {
        let query =
            format!("subscription {{ {collection}(showDeleted: true) {{ _docID _deleted }} }}");
        streams.push((collection, access.subscribe(&query).await?));
    }
    let mut tasks = JoinSet::new();
    for (collection, mut stream) in streams {
        let node = node.to_owned();
        let store = store.clone();
        let metrics = metrics.clone();
        tasks.spawn(async move {
            while let Some(envelope) = stream.recv().await? {
                if envelope
                    .get("errors")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|errors| !errors.is_empty())
                {
                    anyhow::bail!("runtime GraphQL subscription returned errors");
                }
                let rows = envelope
                    .get("data")
                    .and_then(|data| data.get(collection))
                    .and_then(serde_json::Value::as_array)
                    .context("runtime subscription omitted selected root")?;
                metrics
                    .events_received
                    .fetch_add(rows.len() as u64, Ordering::Relaxed);
                if matches!(collection, "AgentToolControl" | "AgentToolControlClaim") {
                    store.invalidate_projection(false);
                } else if is_transcript_content_collection(collection) {
                    store.invalidate_projection(
                        collection == gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
                    );
                    metrics
                        .transcript_invalidations
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    store.observe_request_event(None);
                    store.request_resync(Some(&node));
                }
            }
            Ok::<(), anyhow::Error>(())
        });
    }
    notices
        .send(Notice {
            node: node.to_owned(),
            generation,
            ready: true,
            denied: false,
        })
        .context("runtime observer stopped")?;
    if let Some(result) = tasks.join_next().await {
        result??;
    }
    Ok(())
}
