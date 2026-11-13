use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct P2pSyncStatusSnapshot {
    pub push_backlog: P2pPushBacklogSnapshot,
    pub push_retry_markers: P2pPushRetryMarkerSnapshot,
    pub broadcast_coalesced_total: u64,
    pub push_updates_coalesced_total: u64,
    pub gossip_direction_filtered_total: u64,
    pub gossip_unfollowed_dropped_total: u64,
    pub pending_dags: usize,
    pub pending_dag_capacity: usize,
    pub pending_dag_high_water: u64,
    pub persisted_pending_dags: usize,
    pub persisted_pending_dag_capacity: usize,
    pub persisted_pending_dag_high_water: u64,
    pub pending_resync_in_flight: bool,
    pub retained_background_tasks: usize,
    pub request_dispatch: P2pRequestDispatchSnapshot,
    pub inbound_budget: Vec<P2pInboundClassSnapshot>,
    pub peer_fetches: Vec<P2pPeerFetchSnapshot>,
    pub non_authoritative_broadcast_tasks: usize,
    pub non_authoritative_broadcast_high_water: usize,
    pub non_authoritative_broadcast_rejected_total: u64,
    pub missing_link_retries: u64,
    pub car_requested_cids: u64,
    pub car_present_cids: u64,
    pub car_served_cids: u64,
    pub car_filtered_cids: u64,
    pub car_served_bytes: u64,
    pub car_received_already_held: u64,
    /// `car_served_cids` over `car_requested_cids`; 0 before any request.
    pub car_served_per_requested: f64,
    pub provider_rotations: u64,
    pub provider_demotions_total: u64,
    pub provider_cooldown_skips_total: u64,
    pub pending_dag_resolved: u64,
    pub pending_dag_registered: u64,
    pub pending_dag_expired: u64,
    pub single_flight_suppressed: u64,
    pub already_merged_fast_path: u64,
    pub pending_dag_capacity_shed: u64,
    pub gossip_heads_deferred: u64,
    pub gossip_heads_shed: u64,
    pub pending_dag_retry_dispatched: u64,
    pub pending_dag_retry_suppressed: u64,
    pub pending_dag_fetch_deferred_unavailable: u64,
    pub pending_dag_fetch_deferred_contention: u64,
    pub pending_dag_fetch_deferred_busy: u64,
    pub car_busy_responses: u64,
    pub provider_busy_rotations: u64,
    pub pending_dag_fetch_exhausted: u64,
    pub pending_dag_terminal_merged: u64,
    pub next_pending_retry_in_ms: Option<u64>,
    pub pending_dag_terminal_quarantined: u64,
    pub quarantined_pending_dags: usize,
    pub peers: Vec<P2pPeerSummarySnapshot>,
    /// DAG-fetch arbitration estimates (defradb.rs observe-only estimators);
    /// absent from pins that predate them.
    #[serde(default)]
    pub pending_dag_attempt_ewma_median_ms: Option<u64>,
    #[serde(default)]
    pub pending_dag_attempt_ewma_max_ms: Option<u64>,
    #[serde(default)]
    pub pending_dag_no_progress_median_ppm: Option<u64>,
    #[serde(default)]
    pub pending_dag_tie_reorders: u64,
    #[serde(default)]
    pub pending_dag_peer_occupancy: Vec<P2pPendingPeerOccupancySnapshot>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pInboundClassSnapshot {
    pub class: String,
    pub capacity_bytes: usize,
    pub held_bytes: usize,
    pub held_bytes_high_water: usize,
    pub busy_resets: u64,
    pub oversized_resets: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pPeerFetchSnapshot {
    pub peer: String,
    pub fetches: u64,
    pub bytes: u64,
    pub delivery_rate_bytes_per_sec: u64,
    pub min_rtt_micros: Option<u64>,
    pub success_ppm: u64,
    pub timeout_ppm: u64,
    pub partial_ppm: u64,
    pub busy_ppm: u64,
    pub in_flight: u64,
    pub in_flight_bytes: u64,
    /// Time-decayed (60 s half-life) per-provider estimates; absent until a
    /// fetch finishes and from pins that predate them.
    #[serde(default)]
    pub success_latency_micros: Option<u64>,
    #[serde(default)]
    pub useful_ewma_ppm: Option<u64>,
    #[serde(default)]
    pub attempt_cost_micros: Option<u64>,
    #[serde(default)]
    pub expected_useful_micros: Option<u64>,
    #[serde(default)]
    pub ewma_age_millis: Option<u64>,
}

/// One source peer's share of the pending-DAG map: entries held now and a
/// ~177 s average of them in Q8 (entries × 256).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pPendingPeerOccupancySnapshot {
    pub peer: String,
    pub held: usize,
    pub occupancy_q8: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct P2pPeerSummarySnapshot {
    pub peer_id: String,
    pub connected: bool,
    pub last_seen_at: String,
    pub last_block_received_at: Option<String>,
    pub last_head_acked_at: Option<String>,
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
    pub admission_saturated_total: u64,
    pub recovery_saturated_total: u64,
    pub rejection_dropped_total: u64,
    pub gossip_refusals_dropped_total: u64,
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

pub async fn observe_embedded_sync_status(
    node: &defra_node::Node,
) -> anyhow::Result<P2pSyncStatusSnapshot> {
    use anyhow::Context;
    let p2p =
        gents_store::identity::node_p2p(node).context("embedded node has no P2P transport")?;
    let upstream = tokio::time::timeout(std::time::Duration::from_secs(5), p2p.sync_status())
        .await
        .context("native P2P sync observation timed out")??;
    Ok(JsonP2pSyncStatusAdapter.adapt(&upstream)?)
}

#[cfg(test)]
mod tests {

    use super::{
        JsonP2pSyncStatusAdapter, P2pInboundClassSnapshot, P2pPeerBacklogSnapshot,
        P2pPeerFetchSnapshot, P2pPeerSummarySnapshot, P2pPendingPeerOccupancySnapshot,
        P2pPushBacklogSnapshot, P2pPushRetryMarkerSnapshot, P2pRequestDispatchSnapshot,
        P2pSyncStatusAdapter, P2pSyncStatusSnapshot,
    };

    use p2p::sync::{DispatchSnapshot, PeerBacklogSnapshot, PushBacklogSnapshot, SyncStatus};
    use p2p::{InboundClassSnapshot, PeerFetchStats, PeerSummary};
    use storage::stores::PushRetryMarkerStats;

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

    fn pinned_upstream_wire() -> serde_json::Value {
        let upstream = SyncStatus {
            push_backlog: PushBacklogSnapshot {
                queue_item_capacity: 128,
                queue_byte_capacity: 1_048_576,
                per_peer_active_cap: 2,
                worker_count: 8,
                queued_items: 7,
                queued_bytes: 4_096,
                active_jobs: 3,
                enqueued_total: 101,
                coalesced_total: 11,
                rejected_items_total: 5,
                rejected_bytes_total: 2,
                completed_total: 79,
                failed_total: 4,
                stale_head_retirements_total: 17,
                head_hints_enqueued_document: 103,
                head_hints_enqueued_collection: 107,
                head_hints_sent_document: 109,
                head_hints_sent_collection: 113,
                head_hints_acked_document: 127,
                head_hints_acked_collection: 131,
                head_hints_nacked_capacity: 137,
                head_hints_nacked_other: 139,
                head_hints_failed_transport: 149,
                head_hints_failed_local: 151,
                peer_capacity_parks_total: 13,
                per_peer: vec![PeerBacklogSnapshot {
                    peer_id: "peer-a".to_string(),
                    queued_items: 4,
                    queued_bytes: 2_048,
                    active_jobs: 1,
                    consecutive_failures: 3,
                    cooldown_remaining_ms: 750,
                }],
            },
            broadcast_coalesced_total: 41,
            push_updates_coalesced_total: 43,
            gossip_direction_filtered_total: 47,
            gossip_unfollowed_dropped_total: 48,
            pending_dags: 13,
            pending_dag_capacity: 1_000,
            pending_dag_high_water: 14,
            persisted_pending_dags: 17,
            persisted_pending_dag_capacity: 4_000,
            persisted_pending_dag_high_water: 18,
            pending_resync_in_flight: true,
            retained_background_tasks: 6,
            request_dispatch: DispatchSnapshot {
                request_capacity: 32,
                active_requests: 2,
                active_requests_high_water: 11,
                recovery_capacity: 8,
                active_recovery: 1,
                active_recovery_high_water: 5,
                rejection_capacity: 8,
                active_rejections: 0,
                active_rejections_high_water: 3,
                completion_capacity: 16,
                active_completions: 1,
                active_completions_high_water: 7,
                saturated_total: 13,
                admission_saturated_total: 81,
                recovery_saturated_total: 17,
                rejection_dropped_total: 19,
                gossip_refusals_dropped_total: 23,
            },
            inbound_budget: vec![
                InboundClassSnapshot {
                    class: "control",
                    capacity_bytes: 33_554_432,
                    held_bytes: 1024,
                    held_bytes_high_water: 4096,
                    busy_resets: 2,
                    oversized_resets: 3,
                },
                InboundClassSnapshot {
                    class: "replication",
                    capacity_bytes: 67_108_864,
                    held_bytes: 2048,
                    held_bytes_high_water: 8192,
                    busy_resets: 5,
                    oversized_resets: 7,
                },
                InboundClassSnapshot {
                    class: "sync",
                    capacity_bytes: 67_108_864,
                    held_bytes: 4096,
                    held_bytes_high_water: 16_384,
                    busy_resets: 11,
                    oversized_resets: 13,
                },
                InboundClassSnapshot {
                    class: "bulk",
                    capacity_bytes: 134_217_728,
                    held_bytes: 8192,
                    held_bytes_high_water: 32_768,
                    busy_resets: 17,
                    oversized_resets: 19,
                },
            ],
            peer_fetches: vec![
                PeerFetchStats {
                    peer: "peer-fetch-a".into(),
                    fetches: 23,
                    bytes: 1_048_576,
                    delivery_rate_bytes_per_sec: 524_288,
                    min_rtt_micros: Some(850),
                    success_ppm: 800_000,
                    timeout_ppm: 100_000,
                    partial_ppm: 100_000,
                    busy_ppm: 0,
                    in_flight: 2,
                    in_flight_bytes: 32_768,
                    estimates: Default::default(),
                },
                PeerFetchStats {
                    peer: "peer-fetch-b".into(),
                    fetches: 0,
                    bytes: 0,
                    delivery_rate_bytes_per_sec: 0,
                    min_rtt_micros: None,
                    success_ppm: 0,
                    timeout_ppm: 0,
                    partial_ppm: 0,
                    busy_ppm: 1_000_000,
                    in_flight: 1,
                    in_flight_bytes: 4096,
                    estimates: Default::default(),
                },
            ],
            non_authoritative_broadcast_tasks: 7,
            non_authoritative_broadcast_high_water: 8,
            non_authoritative_broadcast_rejected_total: 9,
            missing_link_retries: 23,
            car_requested_cids: 24,
            car_present_cids: 25,
            car_served_cids: 26,
            car_filtered_cids: 27,
            car_served_bytes: 80,
            car_received_already_held: 81,
            car_served_per_requested: 0.5,
            provider_rotations: 28,
            provider_demotions_total: 74,
            provider_cooldown_skips_total: 75,
            pending_dag_resolved: 29,
            pending_dag_registered: 30,
            pending_dag_expired: 31,
            single_flight_suppressed: 37,
            already_merged_fast_path: 53,
            pending_dag_capacity_shed: 59,
            gossip_heads_deferred: 83,
            gossip_heads_shed: 89,
            pending_dag_retry_dispatched: 61,
            pending_dag_retry_suppressed: 67,
            pending_dag_fetch_deferred_unavailable: 69,
            pending_dag_fetch_deferred_contention: 68,
            pending_dag_fetch_deferred_busy: 76,
            car_busy_responses: 77,
            provider_busy_rotations: 78,
            pending_dag_fetch_exhausted: 70,
            pending_dag_terminal_merged: 72,
            next_pending_retry_in_ms: Some(71),
            pending_dag_terminal_quarantined: 73,
            quarantined_pending_dags: 79,
            pending_dag_attempt_ewma_median_ms: None,
            pending_dag_attempt_ewma_max_ms: None,
            pending_dag_no_progress_median_ppm: None,
            pending_dag_tie_reorders: 0,
            pending_dag_peer_occupancy: Vec::new(),
            peers: vec![PeerSummary {
                peer_id: "peer-a".to_string(),
                connected: true,
                last_seen_at: "2026-10-22T14:00:05Z".parse().unwrap(),
                last_block_received_at: Some("2026-10-22T14:00:03Z".parse().unwrap()),
                last_head_acked_at: None,
            }],
        };

        let mut wire = serde_json::to_value(upstream).expect("serialize pinned DefraDB SyncStatus");
        wire.as_object_mut()
            .expect("DefraDB sync status object")
            .insert(
                "push_retry_markers".into(),
                serde_json::to_value(PushRetryMarkerStats {
                    document_markers: 3,
                    collection_markers: 5,
                    scheduled_peers: 2,
                    oldest_scheduled_retry_unix: Some(1_700_000_000),
                })
                .expect("serialize pinned DefraDB PushRetryMarkerStats"),
            );
        wire
    }

    #[test]
    fn pinned_defradb_sync_status_maps_field_for_field_into_the_typed_snapshot() {
        let wire = pinned_upstream_wire();
        let adapted = JsonP2pSyncStatusAdapter
            .adapt(&wire)
            .expect("adapt pinned DefraDB SyncStatus");

        assert_eq!(
            adapted,
            P2pSyncStatusSnapshot {
                push_backlog: P2pPushBacklogSnapshot {
                    queue_item_capacity: 128,
                    queue_byte_capacity: 1_048_576,
                    per_peer_active_cap: 2,
                    worker_count: 8,
                    queued_items: 7,
                    queued_bytes: 4_096,
                    active_jobs: 3,
                    enqueued_total: 101,
                    coalesced_total: 11,
                    rejected_items_total: 5,
                    rejected_bytes_total: 2,
                    completed_total: 79,
                    failed_total: 4,
                    stale_head_retirements_total: 17,
                    head_hints_enqueued_document: 103,
                    head_hints_enqueued_collection: 107,
                    head_hints_sent_document: 109,
                    head_hints_sent_collection: 113,
                    head_hints_acked_document: 127,
                    head_hints_acked_collection: 131,
                    head_hints_nacked_capacity: 137,
                    head_hints_nacked_other: 139,
                    head_hints_failed_transport: 149,
                    head_hints_failed_local: 151,
                    peer_capacity_parks_total: 13,
                    per_peer: vec![P2pPeerBacklogSnapshot {
                        peer_id: "peer-a".to_string(),
                        queued_items: 4,
                        queued_bytes: 2_048,
                        active_jobs: 1,
                        consecutive_failures: 3,
                        cooldown_remaining_ms: 750,
                    }],
                },
                push_retry_markers: P2pPushRetryMarkerSnapshot {
                    document_markers: 3,
                    collection_markers: 5,
                    scheduled_peers: 2,
                    oldest_scheduled_retry_unix: Some(1_700_000_000),
                },
                broadcast_coalesced_total: 41,
                push_updates_coalesced_total: 43,
                gossip_direction_filtered_total: 47,
                gossip_unfollowed_dropped_total: 48,
                pending_dags: 13,
                pending_dag_capacity: 1_000,
                pending_dag_high_water: 14,
                persisted_pending_dags: 17,
                persisted_pending_dag_capacity: 4_000,
                persisted_pending_dag_high_water: 18,
                pending_resync_in_flight: true,
                retained_background_tasks: 6,
                request_dispatch: P2pRequestDispatchSnapshot {
                    request_capacity: 32,
                    active_requests: 2,
                    active_requests_high_water: 11,
                    recovery_capacity: 8,
                    active_recovery: 1,
                    active_recovery_high_water: 5,
                    rejection_capacity: 8,
                    active_rejections: 0,
                    active_rejections_high_water: 3,
                    completion_capacity: 16,
                    active_completions: 1,
                    active_completions_high_water: 7,
                    saturated_total: 13,
                    admission_saturated_total: 81,
                    recovery_saturated_total: 17,
                    rejection_dropped_total: 19,
                    gossip_refusals_dropped_total: 23,
                },
                inbound_budget: vec![
                    P2pInboundClassSnapshot {
                        class: "control".into(),
                        capacity_bytes: 33_554_432,
                        held_bytes: 1024,
                        held_bytes_high_water: 4096,
                        busy_resets: 2,
                        oversized_resets: 3,
                    },
                    P2pInboundClassSnapshot {
                        class: "replication".into(),
                        capacity_bytes: 67_108_864,
                        held_bytes: 2048,
                        held_bytes_high_water: 8192,
                        busy_resets: 5,
                        oversized_resets: 7,
                    },
                    P2pInboundClassSnapshot {
                        class: "sync".into(),
                        capacity_bytes: 67_108_864,
                        held_bytes: 4096,
                        held_bytes_high_water: 16_384,
                        busy_resets: 11,
                        oversized_resets: 13,
                    },
                    P2pInboundClassSnapshot {
                        class: "bulk".into(),
                        capacity_bytes: 134_217_728,
                        held_bytes: 8192,
                        held_bytes_high_water: 32_768,
                        busy_resets: 17,
                        oversized_resets: 19,
                    },
                ],
                peer_fetches: vec![
                    P2pPeerFetchSnapshot {
                        peer: "peer-fetch-a".into(),
                        fetches: 23,
                        bytes: 1_048_576,
                        delivery_rate_bytes_per_sec: 524_288,
                        min_rtt_micros: Some(850),
                        success_ppm: 800_000,
                        timeout_ppm: 100_000,
                        partial_ppm: 100_000,
                        busy_ppm: 0,
                        in_flight: 2,
                        in_flight_bytes: 32_768,
                        ..Default::default()
                    },
                    P2pPeerFetchSnapshot {
                        peer: "peer-fetch-b".into(),
                        fetches: 0,
                        bytes: 0,
                        delivery_rate_bytes_per_sec: 0,
                        min_rtt_micros: None,
                        success_ppm: 0,
                        timeout_ppm: 0,
                        partial_ppm: 0,
                        busy_ppm: 1_000_000,
                        in_flight: 1,
                        in_flight_bytes: 4096,
                        ..Default::default()
                    },
                ],
                non_authoritative_broadcast_tasks: 7,
                non_authoritative_broadcast_high_water: 8,
                non_authoritative_broadcast_rejected_total: 9,
                missing_link_retries: 23,
                car_requested_cids: 24,
                car_present_cids: 25,
                car_served_cids: 26,
                car_filtered_cids: 27,
                car_served_bytes: 80,
                car_received_already_held: 81,
                car_served_per_requested: 0.5,
                provider_rotations: 28,
                provider_demotions_total: 74,
                provider_cooldown_skips_total: 75,
                pending_dag_resolved: 29,
                pending_dag_registered: 30,
                pending_dag_expired: 31,
                single_flight_suppressed: 37,
                already_merged_fast_path: 53,
                pending_dag_capacity_shed: 59,
                gossip_heads_deferred: 83,
                gossip_heads_shed: 89,
                pending_dag_retry_dispatched: 61,
                pending_dag_retry_suppressed: 67,
                pending_dag_fetch_deferred_unavailable: 69,
                pending_dag_fetch_deferred_contention: 68,
                pending_dag_fetch_deferred_busy: 76,
                car_busy_responses: 77,
                provider_busy_rotations: 78,
                pending_dag_fetch_exhausted: 70,
                pending_dag_terminal_merged: 72,
                next_pending_retry_in_ms: Some(71),
                pending_dag_terminal_quarantined: 73,
                quarantined_pending_dags: 79,
                peers: vec![P2pPeerSummarySnapshot {
                    peer_id: "peer-a".into(),
                    connected: true,
                    last_seen_at: "2026-10-22T14:00:05Z".into(),
                    last_block_received_at: Some("2026-10-22T14:00:03Z".into()),
                    last_head_acked_at: None,
                }],
                pending_dag_attempt_ewma_median_ms: None,
                pending_dag_attempt_ewma_max_ms: None,
                pending_dag_no_progress_median_ppm: None,
                pending_dag_tie_reorders: 0,
                pending_dag_peer_occupancy: Vec::new(),
            },
            "the observability adapter must map every pinned DefraDB sync field without drift"
        );
    }

    /// The DAG-fetch arbitration estimates DefraDB adds to `SyncStatus` and each
    /// peer fetch entry must decode when populated.
    #[test]
    fn arbitration_estimates_decode_when_the_pin_carries_them() {
        let mut wire = pinned_upstream_wire();
        let status = wire.as_object_mut().expect("DefraDB sync status object");
        for (field, value) in [
            (
                "pending_dag_attempt_ewma_median_ms",
                serde_json::json!(4_500),
            ),
            ("pending_dag_attempt_ewma_max_ms", serde_json::json!(35_000)),
            (
                "pending_dag_no_progress_median_ppm",
                serde_json::json!(null),
            ),
            ("pending_dag_tie_reorders", serde_json::json!(12)),
            (
                "pending_dag_peer_occupancy",
                serde_json::json!([{ "peer": "peer-a", "held": 3, "occupancy_q8": 640 }]),
            ),
        ] {
            status.insert(field.into(), value);
        }
        let fetch = wire["peer_fetches"][0]
            .as_object_mut()
            .expect("peer fetch entry object");
        for (field, value) in [
            ("success_latency_micros", serde_json::json!(84_000)),
            ("useful_ewma_ppm", serde_json::json!(170_000)),
            ("attempt_cost_micros", serde_json::json!(490_000)),
            ("expected_useful_micros", serde_json::json!(2_882_352)),
            ("ewma_age_millis", serde_json::json!(null)),
        ] {
            fetch.insert(field.into(), value);
        }

        let adapted = JsonP2pSyncStatusAdapter
            .adapt(&wire)
            .expect("adapt a DefraDB SyncStatus carrying arbitration estimates");
        assert_eq!(adapted.pending_dag_attempt_ewma_median_ms, Some(4_500));
        assert_eq!(adapted.pending_dag_attempt_ewma_max_ms, Some(35_000));
        assert_eq!(adapted.pending_dag_no_progress_median_ppm, None);
        assert_eq!(adapted.pending_dag_tie_reorders, 12);
        assert_eq!(
            adapted.pending_dag_peer_occupancy,
            vec![P2pPendingPeerOccupancySnapshot {
                peer: "peer-a".into(),
                held: 3,
                occupancy_q8: 640,
            }]
        );
        let fetch = &adapted.peer_fetches[0];
        assert_eq!(
            (
                fetch.success_latency_micros,
                fetch.useful_ewma_ppm,
                fetch.attempt_cost_micros,
                fetch.expected_useful_micros,
                fetch.ewma_age_millis,
            ),
            (
                Some(84_000),
                Some(170_000),
                Some(490_000),
                Some(2_882_352),
                None
            )
        );
        assert_eq!(adapted.peer_fetches[1].success_latency_micros, None);

        wire["pending_dag_peer_occupancy"][0]["unmodeled"] = serde_json::json!(1);
        assert!(
            JsonP2pSyncStatusAdapter.adapt(&wire).is_err(),
            "an occupancy row with an unknown field must fail the adapter"
        );
    }

    #[test]
    fn unknown_upstream_sync_fields_are_rejected_not_dropped() {
        let mut wire = pinned_upstream_wire();
        wire.as_object_mut()
            .expect("DefraDB sync status object")
            .insert("unmodeled_future_stat".into(), serde_json::json!(1));

        let error = JsonP2pSyncStatusAdapter
            .adapt(&wire)
            .expect_err("unknown upstream fields must fail the adapter, not vanish");
        assert!(
            error.to_string().contains("unmodeled_future_stat"),
            "rejection should name the unknown field, got {error}"
        );
    }

    #[test]
    fn mistyped_upstream_sync_fields_are_rejected() {
        let mut wire = pinned_upstream_wire();
        wire["push_backlog"]["queued_items"] = serde_json::json!("7");

        assert!(
            JsonP2pSyncStatusAdapter.adapt(&wire).is_err(),
            "a string where a counter is expected must fail the adapter instead of coercing"
        );
    }
}
