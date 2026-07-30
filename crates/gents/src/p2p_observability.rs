//! Downstream contract for DefraDB P2P sync diagnostics.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pSyncStatusSnapshot {
    pub push_backlog: P2pPushBacklogSnapshot,
    pub encode_cache_hits_total: u64,
    pub encode_cache_entries: usize,
    pub broadcast_coalesced_total: u64,
    pub push_updates_coalesced_total: u64,
    pub gossip_direction_filtered_total: u64,
    pub pending_dags: usize,
    pub pending_dag_capacity: usize,
    pub persisted_pending_dags: usize,
    pub persisted_pending_dag_capacity: usize,
    pub pending_resync_in_flight: bool,
    pub retained_background_tasks: usize,
    pub missing_link_retries: u64,
    pub pending_dag_resolved: u64,
    pub pending_dag_expired: u64,
    pub single_flight_suppressed: u64,
    pub already_merged_fast_path: u64,
    pub pending_dag_capacity_shed: u64,
    pub pending_dag_retry_dispatched: u64,
    pub pending_dag_retry_suppressed: u64,
    pub next_pending_retry_in_ms: Option<u64>,
    pub pending_dag_terminal_quarantined: u64,
    pub quarantined_pending_dags: usize,
}

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
    pub stale_head_retirements_total: u64,
    pub peer_capacity_parks_total: u64,
    pub per_cid_retry_counts: Vec<P2pCidRetrySnapshot>,
    pub per_peer: Vec<P2pPeerBacklogSnapshot>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pCidRetrySnapshot {
    pub cid: String,
    pub retry_count: u64,
}

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

pub trait P2pSyncStatusAdapter {
    type Error;

    fn adapt(&self, upstream: &Value) -> Result<P2pSyncStatusSnapshot, Self::Error>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct JsonP2pSyncStatusAdapter;

impl P2pSyncStatusAdapter for JsonP2pSyncStatusAdapter {
    type Error = serde_json::Error;

    fn adapt(&self, upstream: &Value) -> Result<P2pSyncStatusSnapshot, Self::Error> {
        serde_json::from_value(upstream.clone())
    }
}
