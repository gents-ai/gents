use gents::{JsonP2pSyncStatusAdapter, P2pSyncStatusAdapter};
use p2p::sync::{DispatchSnapshot, PeerBacklogSnapshot, PushBacklogSnapshot, SyncStatus};
use storage::stores::PushRetryMarkerStats;

#[test]
fn pinned_defradb_sync_status_satisfies_agent_observability_contract() {
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
    let adapted = JsonP2pSyncStatusAdapter
        .adapt(&wire)
        .expect("adapt pinned DefraDB SyncStatus");

    assert_eq!(adapted.push_backlog.queue_item_capacity, 128);
    assert_eq!(adapted.push_backlog.queued_items, 7);
    assert_eq!(adapted.push_backlog.queued_bytes, 4_096);
    assert_eq!(adapted.push_backlog.active_jobs, 3);
    assert_eq!(adapted.push_backlog.rejected_items_total, 5);
    assert_eq!(adapted.push_backlog.rejected_bytes_total, 2);
    assert_eq!(adapted.push_backlog.stale_head_retirements_total, 17);
    assert_eq!(adapted.push_backlog.per_peer[0].peer_id, "peer-a");
    assert_eq!(adapted.push_backlog.per_peer[0].consecutive_failures, 3);
    assert_eq!(adapted.push_backlog.head_hints_enqueued_document, 103);
    assert_eq!(adapted.push_backlog.head_hints_acked_collection, 131);
    assert_eq!(adapted.push_retry_markers.document_markers, 3);
    assert_eq!(adapted.push_retry_markers.collection_markers, 5);
    assert_eq!(adapted.broadcast_coalesced_total, 41);
    assert_eq!(adapted.push_updates_coalesced_total, 43);
    assert_eq!(adapted.pending_dags, 13);
    assert_eq!(adapted.persisted_pending_dags, 17);
    assert!(adapted.pending_resync_in_flight);
    assert_eq!(adapted.retained_background_tasks, 6);
    assert_eq!(adapted.request_dispatch.active_requests_high_water, 11);
    assert_eq!(adapted.request_dispatch.active_recovery_high_water, 5);
    assert_eq!(adapted.request_dispatch.saturated_total, 13);
    assert_eq!(adapted.missing_link_retries, 23);
    assert_eq!(adapted.push_backlog.peer_capacity_parks_total, 13);
    assert_eq!(adapted.gossip_direction_filtered_total, 47);
    assert_eq!(adapted.single_flight_suppressed, 37);
    assert_eq!(adapted.already_merged_fast_path, 53);
    assert_eq!(adapted.pending_dag_capacity_shed, 59);
    assert_eq!(adapted.pending_dag_retry_dispatched, 61);
    assert_eq!(adapted.pending_dag_retry_suppressed, 67);
    assert_eq!(adapted.pending_dag_fetch_deferred_unavailable, 69);
    assert_eq!(adapted.pending_dag_fetch_deferred_contention, 68);
    assert_eq!(adapted.pending_dag_fetch_exhausted, 70);
    assert_eq!(adapted.pending_dag_terminal_merged, 72);
    assert_eq!(adapted.next_pending_retry_in_ms, Some(71));
    assert_eq!(adapted.pending_dag_terminal_quarantined, 73);
    assert_eq!(adapted.quarantined_pending_dags, 79);
}
