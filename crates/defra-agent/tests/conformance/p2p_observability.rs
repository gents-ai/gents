use defra_agent::{JsonP2pSyncStatusAdapter, P2pSyncStatusAdapter};
use p2p::sync::{CidRetrySnapshot, PeerBacklogSnapshot, PushBacklogSnapshot, SyncStatus};

/// Compile-time + wire-shape fence for the pinned DefraDB diagnostics API.
///
/// These exhaustive upstream struct literals intentionally fail to compile if
/// a dependency bump adds, drops, or renames a field. Serialization followed
/// by the production adapter then fences the JSON names and value mapping.
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
            peer_capacity_parks_total: 13,
            per_cid_retry_counts: vec![CidRetrySnapshot {
                cid: "bafy-retry".to_string(),
                retry_count: 19,
            }],
            per_peer: vec![PeerBacklogSnapshot {
                peer_id: "peer-a".to_string(),
                queued_items: 4,
                queued_bytes: 2_048,
                active_jobs: 1,
                consecutive_failures: 3,
                cooldown_remaining_ms: 750,
            }],
        },
        encode_cache_hits_total: 37,
        encode_cache_entries: 5,
        broadcast_coalesced_total: 41,
        push_updates_coalesced_total: 43,
        gossip_direction_filtered_total: 47,
        pending_dags: 13,
        pending_dag_capacity: 1_000,
        persisted_pending_dags: 17,
        persisted_pending_dag_capacity: 4_000,
        pending_resync_in_flight: true,
        retained_background_tasks: 6,
        missing_link_retries: 23,
        pending_dag_resolved: 29,
        pending_dag_expired: 31,
        single_flight_suppressed: 37,
        already_merged_fast_path: 53,
        pending_dag_capacity_shed: 59,
        pending_dag_retry_dispatched: 61,
        pending_dag_retry_suppressed: 67,
        next_pending_retry_in_ms: Some(71),
    };

    let wire = serde_json::to_value(upstream).expect("serialize pinned DefraDB SyncStatus");
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
    assert_eq!(
        adapted.push_backlog.per_cid_retry_counts[0].cid,
        "bafy-retry"
    );
    assert_eq!(adapted.push_backlog.per_cid_retry_counts[0].retry_count, 19);
    assert_eq!(adapted.push_backlog.per_peer[0].peer_id, "peer-a");
    assert_eq!(adapted.push_backlog.per_peer[0].consecutive_failures, 3);
    assert_eq!(adapted.encode_cache_hits_total, 37);
    assert_eq!(adapted.encode_cache_entries, 5);
    assert_eq!(adapted.broadcast_coalesced_total, 41);
    assert_eq!(adapted.push_updates_coalesced_total, 43);
    assert_eq!(adapted.pending_dags, 13);
    assert_eq!(adapted.persisted_pending_dags, 17);
    assert!(adapted.pending_resync_in_flight);
    assert_eq!(adapted.retained_background_tasks, 6);
    assert_eq!(adapted.missing_link_retries, 23);
    assert_eq!(adapted.push_backlog.peer_capacity_parks_total, 13);
    assert_eq!(adapted.gossip_direction_filtered_total, 47);
    assert_eq!(adapted.single_flight_suppressed, 37);
    assert_eq!(adapted.already_merged_fast_path, 53);
    assert_eq!(adapted.pending_dag_capacity_shed, 59);
    assert_eq!(adapted.pending_dag_retry_dispatched, 61);
    assert_eq!(adapted.pending_dag_retry_suppressed, 67);
    assert_eq!(adapted.next_pending_retry_in_ms, Some(71));
}
