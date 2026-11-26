use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use gents_protocol::session::SessionScope;

use super::{ObservedStore, ObserverMetrics};
use crate::client_projection::query::{is_transcript_content_collection, CollectionRoot};

/// `ObserverResync.Change`: what a delivered change is, by its collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Change {
    ToolControl,
    Transcript,
    ToolCall,
    Collection,
    SessionScope,
    Node,
}

impl Change {
    fn of(collection: &str, root: Option<CollectionRoot>, scoped: bool) -> Self {
        match collection {
            "AgentToolControl" | "AgentToolControlClaim" => Self::ToolControl,
            gents_protocol::schemas::AGENT_TOOL_CALL_NAME => Self::ToolCall,
            _ if is_transcript_content_collection(collection) => Self::Transcript,
            _ if root.is_some() => Self::Collection,
            gents_protocol::schemas::AGENT_REQUEST_NAME
            | gents_protocol::schemas::AGENT_SESSION_NAME
                if scoped =>
            {
                Self::SessionScope
            }
            _ => Self::Node,
        }
    }
}

/// The session scopes the sessions or requests `rows` name, when every one
/// of them is an `ObserverResync.Change.sessionScope` change: not deleted,
/// caused by no trigger, and naming its session.
fn session_scopes(node_did: &str, rows: &[serde_json::Value]) -> Option<BTreeSet<SessionScope>> {
    let text = |row: &serde_json::Value, field: &str| match row.get(field) {
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        _ => None,
    };
    rows.iter()
        .map(|row| {
            let deleted = row.get("_deleted").and_then(serde_json::Value::as_bool);
            let triggered = row
                .get("caused_by_trigger_id")
                .is_some_and(|trigger| !trigger.is_null());
            if deleted != Some(false) || triggered {
                return None;
            }
            Some(SessionScope {
                node_did: node_did.to_owned(),
                session_id: text(row, "session_id")?,
                requester_did: text(row, "requester_did"),
            })
        })
        .collect()
}

/// `ObserverResync.Delivery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Delivery {
    InvalidateTranscript,
    InvalidateProvenance,
    ReloadCollection,
    ReloadSessionScope,
    Resync,
}

/// `ObserverResync.deliver`.
pub(super) fn route(change: Change) -> Delivery {
    match change {
        Change::ToolControl | Change::Transcript => Delivery::InvalidateTranscript,
        Change::ToolCall => Delivery::InvalidateProvenance,
        Change::Collection => Delivery::ReloadCollection,
        Change::SessionScope => Delivery::ReloadSessionScope,
        Change::Node => Delivery::Resync,
    }
}

/// Applies one subscription envelope from the runtime of `store`'s node. A
/// document change is a request event of that node's store
/// (`ClientDataPublication.requestEvent`): it rejects the store's patches read
/// before it and queues a fresh read of its collection or of the node, but
/// leaves every reload in flight to publish, so a steady event stream cannot
/// keep rows from catching up.
pub(super) fn deliver(
    store: &ObservedStore,
    metrics: &ObserverMetrics,
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
    let root = CollectionRoot::of(collection);
    let scopes = matches!(
        collection,
        gents_protocol::schemas::AGENT_REQUEST_NAME | gents_protocol::schemas::AGENT_SESSION_NAME
    )
    .then(|| session_scopes(store.node_did(), rows))
    .flatten()
    .filter(|scopes| !scopes.is_empty());
    let change = Change::of(collection, root, scopes.is_some());
    match route(change) {
        Delivery::InvalidateTranscript => {
            store.invalidate_projection(false);
            if change == Change::Transcript {
                metrics
                    .transcript_invalidations
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        Delivery::InvalidateProvenance => {
            store.invalidate_projection(true);
            metrics
                .transcript_invalidations
                .fetch_add(1, Ordering::Relaxed);
        }
        Delivery::ReloadCollection => {
            store.observe_request_event();
            store.request_collection_reload(
                root.context("a collection change names its snapshot root")?,
            );
        }
        Delivery::ReloadSessionScope => {
            store.observe_request_event();
            store.request_session_reload(
                scopes.context("a session scope change names its session scope")?,
            );
        }
        Delivery::Resync => {
            store.observe_request_event();
            store.request_resync();
        }
    }
    Ok(())
}

#[cfg(test)]
mod conformance {
    #[test]
    fn deliver_matches_lean() {
        gents_lean_vocab::families::observer_resync_deliver::replay(|i| super::route(i.change));
    }
}
