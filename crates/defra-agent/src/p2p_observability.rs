//! Downstream contract for DefraDB P2P sync diagnostics.
//!
//! DefraDB owns the sync implementation and its wire snapshot. This module is
//! the deliberately small adapter seam where the agent binds that upstream
//! contract into operator-facing status and metrics. Keeping a typed snapshot
//! here prevents those surfaces from reaching into untyped JSON independently.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Typed view of the pinned DefraDB `p2p::sync::SyncStatus` JSON contract.
///
/// Fields added by defradb.rs#1101/#1102 belong here only after their upstream
/// names land. The pinned-struct conformance test then forces this adapter to
/// be reviewed on the same change as the dependency revision.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pSyncStatusSnapshot {
    pub push_backlog: P2pPushBacklogSnapshot,
    pub pending_dags: usize,
    pub pending_dag_capacity: usize,
    pub persisted_pending_dags: usize,
    pub persisted_pending_dag_capacity: usize,
    pub pending_resync_in_flight: bool,
    pub retained_background_tasks: usize,
    pub missing_link_retries: u64,
    pub pending_dag_resolved: u64,
    pub pending_dag_expired: u64,
}

/// Typed view of DefraDB's bounded outbound push backlog.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pPushBacklogSnapshot {
    pub queue_item_capacity: usize,
    pub queue_byte_capacity: usize,
    pub per_peer_active_cap: usize,
    pub worker_count: usize,
    pub queued_items: usize,
    pub queued_bytes: usize,
    pub active_jobs: usize,
    pub enqueued_total: u64,
    pub coalesced_total: u64,
    pub rejected_items_total: u64,
    pub rejected_bytes_total: u64,
    pub completed_total: u64,
    pub failed_total: u64,
    pub per_peer: Vec<P2pPeerBacklogSnapshot>,
}

/// Per-peer occupancy and cooldown state from the outbound push backlog.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pPeerBacklogSnapshot {
    pub peer_id: String,
    pub queued_items: usize,
    pub queued_bytes: usize,
    pub active_jobs: usize,
    pub consecutive_failures: u32,
    pub cooldown_remaining_ms: u64,
}

/// Converts the upstream diagnostics representation into the agent contract.
///
/// The trait keeps the eventual #1101/#1102 pin update to one thin mapping and
/// lets conformance tests drive the same boundary as production metrics.
pub trait P2pSyncStatusAdapter {
    type Error;

    fn adapt(&self, upstream: &Value) -> Result<P2pSyncStatusSnapshot, Self::Error>;
}

/// Adapter for the JSON returned by `GET /api/v0/p2p/sync/status`.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonP2pSyncStatusAdapter;

impl P2pSyncStatusAdapter for JsonP2pSyncStatusAdapter {
    type Error = serde_json::Error;

    fn adapt(&self, upstream: &Value) -> Result<P2pSyncStatusSnapshot, Self::Error> {
        serde_json::from_value(upstream.clone())
    }
}
