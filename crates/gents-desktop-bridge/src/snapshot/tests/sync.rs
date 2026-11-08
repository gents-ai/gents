use std::time::{Duration, SystemTime};

use gents_desktop_core::client::{
    project_sync_health, ClientSyncStateSnapshot, P2PHealth, P2PHealthStatus,
    P2pSyncStatusSnapshot, SyncHealthState,
};
use serde_json::json;

use crate::snapshot::to_sync_health_view;
use crate::types::ClientUpdateEvent;

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn sync_health_view_keeps_database_quarantine_failed() {
    let health = project_sync_health(&ClientSyncStateSnapshot {
        transport: P2PHealth {
            status: P2PHealthStatus::Healthy,
            consecutive_failures: 0,
            connected_peer_count: 1,
            replicator_count: 1,
            last_error: None,
            last_ok_at: Some(t(50)),
            last_failure_at: None,
        },
        database_sync: Some(P2pSyncStatusSnapshot {
            quarantined_pending_dags: 1,
            ..P2pSyncStatusSnapshot::default()
        }),
        database_sync_error: None,
        peer_schema_skew: Default::default(),
        enrollment_activation_errors: Default::default(),
        directory: Vec::new(),
        peers: Vec::new(),
    })
    .unwrap();
    assert_eq!(health.state, SyncHealthState::Failed);
    let view = to_sync_health_view(&health);
    assert_eq!(view.state, "failed");
    assert_eq!(view.quarantined_dag_count, Some(1));
}

#[test]
fn client_update_event_serializes_store_reason_for_projection_wakes() {
    let event = ClientUpdateEvent::coarse("store");
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({ "reason": "store" })
    );
}
