use std::sync::atomic::Ordering;

use anyhow::{Context, Result};

use super::{ObservedStore, ObserverMetrics};
use crate::client::query::is_transcript_content_collection;

/// Applies one subscription envelope from `node`'s runtime. A document change
/// is a request event of that node (`ClientDataPublication.observerEvent`):
/// it rejects that node's patches read before it and queues a fresh snapshot
/// of the node, but leaves every snapshot reload in flight to publish, so a
/// steady event stream cannot keep rows from catching up.
pub(super) fn deliver(
    store: &ObservedStore,
    metrics: &ObserverMetrics,
    node: &str,
    collection: &str,
    envelope: &serde_json::Value,
) -> Result<()> {
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
        store.invalidate_projection(collection == gents_protocol::schemas::AGENT_TOOL_CALL_NAME);
        metrics
            .transcript_invalidations
            .fetch_add(1, Ordering::Relaxed);
    } else {
        store.observe_node_event(node);
        store.request_resync(Some(node));
    }
    Ok(())
}
