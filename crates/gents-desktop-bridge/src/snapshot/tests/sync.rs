use gents_desktop_core::client::{
    project_sync_health, ClientSyncStateSnapshot, P2PHealth, P2PHealthStatus,
    RuntimeConnectionStatus,
};
use serde_json::json;

use crate::snapshot::to_sync_health_view;
use crate::types::ClientUpdateEvent;

#[test]
fn sync_health_view_names_the_cause_and_next_action() {
    let sync = ClientSyncStateSnapshot {
        transport: P2PHealth {
            status: P2PHealthStatus::Degraded,
            last_error: Some("probe failed".into()),
            ..P2PHealth::default()
        },
        enrollment_activation_errors: Default::default(),
        directory: Vec::new(),
        peers: Vec::new(),
    };
    let view = to_sync_health_view(&project_sync_health(
        &sync,
        [Some(RuntimeConnectionStatus::Connected)],
    ));
    assert_eq!(view.state, "healthy");
    assert_eq!(view.cause, None);
    assert_eq!((view.runtime_count, view.connected_runtime_count), (1, 1));

    let view = to_sync_health_view(&project_sync_health(&sync, []));
    assert_eq!(view.state, "waiting");
    assert_eq!(view.cause.as_deref(), Some("discovery_offline"));
    assert_eq!(view.next_action.as_deref(), Some("wait"));
    assert_eq!(view.last_error.as_deref(), Some("probe failed"));

    let view = to_sync_health_view(&project_sync_health(
        &sync,
        [Some(RuntimeConnectionStatus::Refused)],
    ));
    assert_eq!(view.cause.as_deref(), Some("runtime_refused"));
    assert_eq!(view.next_action.as_deref(), Some("restore_access"));
    assert_eq!(view.last_error, None);
}

#[test]
fn client_update_event_serializes_store_reason_for_projection_wakes() {
    let event = ClientUpdateEvent::coarse("store");
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({ "reason": "store" })
    );
}
