//! Downstream contract for DefraDB P2P sync diagnostics.
//!
//! DefraDB owns the sync implementation and its wire snapshot. This module is
//! the deliberately small adapter seam where the agent binds that upstream
//! contract into operator-facing status and metrics. Keeping a typed snapshot
//! here prevents those surfaces from reaching into untyped JSON independently.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pSyncStatusSnapshot {
    pub push_backlog: P2pPushBacklogSnapshot,
    pub push_retry_markers: P2pPushRetryMarkerSnapshot,
    pub broadcast_coalesced_total: u64,
    pub push_updates_coalesced_total: u64,
    pub gossip_direction_filtered_total: u64,
    pub pending_dags: usize,
    pub pending_dag_capacity: usize,
    pub pending_dag_high_water: u64,
    pub persisted_pending_dags: usize,
    pub persisted_pending_dag_capacity: usize,
    pub persisted_pending_dag_high_water: u64,
    pub pending_resync_in_flight: bool,
    pub retained_background_tasks: usize,
    pub request_dispatch: P2pRequestDispatchSnapshot,
    pub non_authoritative_broadcast_tasks: usize,
    pub non_authoritative_broadcast_high_water: usize,
    pub non_authoritative_broadcast_rejected_total: u64,
    pub missing_link_retries: u64,
    pub car_requested_cids: u64,
    pub car_present_cids: u64,
    pub car_served_cids: u64,
    pub car_filtered_cids: u64,
    pub provider_rotations: u64,
    pub pending_dag_resolved: u64,
    pub pending_dag_registered: u64,
    pub pending_dag_expired: u64,
    pub single_flight_suppressed: u64,
    pub already_merged_fast_path: u64,
    pub pending_dag_capacity_shed: u64,
    pub pending_dag_retry_dispatched: u64,
    pub pending_dag_retry_suppressed: u64,
    pub pending_dag_fetch_deferred_unavailable: u64,
    pub pending_dag_fetch_deferred_contention: u64,
    pub pending_dag_fetch_exhausted: u64,
    pub pending_dag_terminal_merged: u64,
    pub next_pending_retry_in_ms: Option<u64>,
    pub pending_dag_terminal_quarantined: u64,
    pub quarantined_pending_dags: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pRequestDispatchSnapshot {
    pub request_capacity: usize,
    pub active_requests: usize,
    pub active_requests_high_water: usize,
    pub recovery_capacity: usize,
    pub active_recovery: usize,
    pub active_recovery_high_water: usize,
    pub rejection_capacity: usize,
    pub active_rejections: usize,
    pub active_rejections_high_water: usize,
    pub completion_capacity: usize,
    pub active_completions: usize,
    pub active_completions_high_water: usize,
    pub saturated_total: u64,
    pub recovery_saturated_total: u64,
    pub rejection_dropped_total: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pPushRetryMarkerSnapshot {
    pub document_markers: usize,
    pub collection_markers: usize,
    pub scheduled_peers: usize,
    pub oldest_scheduled_retry_unix: Option<u64>,
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
    pub head_hints_enqueued_document: u64,
    pub head_hints_enqueued_collection: u64,
    pub head_hints_sent_document: u64,
    pub head_hints_sent_collection: u64,
    pub head_hints_acked_document: u64,
    pub head_hints_acked_collection: u64,
    pub head_hints_nacked_capacity: u64,
    pub head_hints_nacked_other: u64,
    pub head_hints_failed_transport: u64,
    pub head_hints_failed_local: u64,
    pub peer_capacity_parks_total: u64,
    pub per_peer: Vec<P2pPeerBacklogSnapshot>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_status_is_rejected() {
        let upstream = serde_json::json!({
            "push_backlog": { "head_hints_acked_document": 13 },
            "push_retry_markers": {
                "document_markers": 3,
                "scheduled_peers": 2
            },
            "car_filtered_cids": 2,
            "provider_rotations": 3,
            "request_dispatch": {
                "request_capacity": 32,
                "active_requests_high_water": 4,
                "recovery_capacity": 8,
                "active_recovery_high_water": 2
            },
            "pending_dag_fetch_deferred_unavailable": 4,
            "pending_dag_fetch_deferred_contention": 6,
            "pending_dag_fetch_exhausted": 5
        });

        assert!(JsonP2pSyncStatusAdapter.adapt(&upstream).is_err());
    }
}
