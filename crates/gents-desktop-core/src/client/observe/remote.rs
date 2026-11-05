use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

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
    abort: AbortHandle,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

impl Source {
    pub(super) fn stop(&mut self) {
        self.ready = false;
        self.denied = true;
        self.abort.abort();
    }
}

#[derive(Default)]
pub(super) struct Sources {
    pub entries: BTreeMap<String, Source>,
    generation: u64,
}

impl Sources {
    pub(super) fn reconcile(
        &mut self,
        peers: &[PeerRecord],
        requester: &str,
        interaction: crate::local_runtime::OperatorIdentityInteraction,
        notices: &mpsc::UnboundedSender<Notice>,
        store: &Arc<ObservedStore>,
        metrics: &Arc<ObserverMetrics>,
    ) {
        let endpoints: BTreeMap<_, _> = peers
            .iter()
            .filter_map(|peer| {
                interaction
                    .endpoint(peer, requester)
                    .map(|endpoint| (peer.node_did.clone(), endpoint))
            })
            .collect();
        self.entries.retain(|node, source| {
            endpoints
                .get(node)
                .is_some_and(|endpoint| endpoint.to_string() == source.endpoint.to_string())
                && (source.denied || !source.abort.is_finished())
        });
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
            permission_denied(&error)
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

pub(super) fn permission_denied(error: &anyhow::Error) -> bool {
    gents::store::config_client::TransactionFailure::config_client_error(error)
        .is_some_and(gents::store::config_client::ConfigClientError::is_permission_denied)
}
