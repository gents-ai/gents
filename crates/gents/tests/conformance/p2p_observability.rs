use gents::{
    JsonP2pSyncStatusAdapter, P2pInboundClassSnapshot, P2pPeerBacklogSnapshot,
    P2pPeerFetchSnapshot, P2pPushBacklogSnapshot, P2pPushRetryMarkerSnapshot,
    P2pRequestDispatchSnapshot, P2pSyncStatusAdapter, P2pSyncStatusSnapshot,
};
use p2p::sync::{DispatchSnapshot, PeerBacklogSnapshot, PushBacklogSnapshot, SyncStatus};
use p2p::{InboundClassSnapshot, PeerFetchStats};
use storage::stores::PushRetryMarkerStats;

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
            recovery_saturated_total: 17,
            rejection_dropped_total: 19,
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
                in_flight: 2,
                in_flight_bytes: 32_768,
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
                in_flight: 1,
                in_flight_bytes: 4096,
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
        provider_rotations: 28,
        pending_dag_resolved: 29,
        pending_dag_registered: 30,
        pending_dag_expired: 31,
        single_flight_suppressed: 37,
        already_merged_fast_path: 53,
        pending_dag_capacity_shed: 59,
        pending_dag_retry_dispatched: 61,
        pending_dag_retry_suppressed: 67,
        pending_dag_fetch_deferred_unavailable: 69,
        pending_dag_fetch_deferred_contention: 68,
        pending_dag_fetch_exhausted: 70,
        pending_dag_terminal_merged: 72,
        next_pending_retry_in_ms: Some(71),
        pending_dag_terminal_quarantined: 73,
        quarantined_pending_dags: 79,
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
                recovery_saturated_total: 17,
                rejection_dropped_total: 19,
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
                    in_flight: 2,
                    in_flight_bytes: 32_768,
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
                    in_flight: 1,
                    in_flight_bytes: 4096,
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
            provider_rotations: 28,
            pending_dag_resolved: 29,
            pending_dag_registered: 30,
            pending_dag_expired: 31,
            single_flight_suppressed: 37,
            already_merged_fast_path: 53,
            pending_dag_capacity_shed: 59,
            pending_dag_retry_dispatched: 61,
            pending_dag_retry_suppressed: 67,
            pending_dag_fetch_deferred_unavailable: 69,
            pending_dag_fetch_deferred_contention: 68,
            pending_dag_fetch_exhausted: 70,
            pending_dag_terminal_merged: 72,
            next_pending_retry_in_ms: Some(71),
            pending_dag_terminal_quarantined: 73,
            quarantined_pending_dags: 79,
        },
        "the observability adapter must map every pinned DefraDB sync field without drift"
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
