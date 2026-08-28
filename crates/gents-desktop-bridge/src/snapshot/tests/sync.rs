use std::time::{Duration, SystemTime};

use gents::agent::p2p_reconcile::session_hydration::{
    ClientHydrationPhase, ClientHydrationProgress,
};
use gents_desktop_core::client::{
    project_sync_health, ClientPeerStatus, P2PHealth, P2PHealthStatus, PairingCollectionStatus,
    SyncHealthState, STUCK_THRESHOLD_ATTEMPTS,
};
use gents_desktop_core::remote_admin::PairingErrorClass;
use serde_json::json;

use crate::contract::EVENT_REASONS;
use crate::snapshot::{to_hydration_view, to_pairing_collection_view, to_sync_health_view};
use crate::types::ClientUpdateEvent;

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn serving_progress() -> ClientHydrationProgress {
    ClientHydrationProgress {
        session_id: "session-1".into(),
        agent_did: "did:test:agent".into(),
        phase: ClientHydrationPhase::Serving,
        merged_count: 4,
        served_count: Some(11),
    }
}

fn connected_peer(pairing: Vec<PairingCollectionStatus>) -> ClientPeerStatus {
    ClientPeerStatus {
        peer_id: "peer-1".into(),
        label: "Studio".into(),
        agent_did: "did:test:agent".into(),
        addr: "/ip4/10.0.0.1/tcp/1".into(),
        dial_succeeded: true,
        last_error: None,
        pairing,
        routes: Vec::new(),
        chat_safe: true,
    }
}

#[test]
fn hydration_view_copies_receiver_counts_exactly() {
    let view = to_hydration_view(&serving_progress());
    assert_eq!(view.session_id, "session-1");
    assert_eq!(view.agent_did, "did:test:agent");
    assert_eq!(view.phase, "serving");
    assert_eq!(view.merged_count, 4);
    assert_eq!(view.served_count, Some(11));
}

#[test]
fn sync_health_view_keeps_failed_from_collapsing_into_syncing() {
    let mut pairing = PairingCollectionStatus::new("AgentSession");
    pairing.record_retry(PairingErrorClass::RemoteUnauthorized);
    let health = project_sync_health(
        &P2PHealth {
            status: P2PHealthStatus::Healthy,
            consecutive_failures: 0,
            connected_peer_count: 1,
            replicator_count: 1,
            last_error: None,
            last_ok_at: Some(t(50)),
            last_failure_at: None,
        },
        &[connected_peer(vec![pairing])],
    );
    assert_eq!(health.state, SyncHealthState::Failed);
    let view = to_sync_health_view(&health);
    assert_eq!(view.state, "failed");
    assert_eq!(view.last_error_class.as_deref(), Some("RemoteUnauthorized"));
    assert_eq!(view.pairing_retry_count, 1);
}

#[test]
fn pairing_collection_view_preserves_retry_stuck_and_timestamps() {
    let stuck_at = t(80);
    let mut pairing = PairingCollectionStatus::new("AgentConversation");
    for _ in 0..STUCK_THRESHOLD_ATTEMPTS {
        pairing.record_retry(PairingErrorClass::RpcTimeout);
    }
    pairing.update_stuck_indicator(stuck_at);
    let view = to_pairing_collection_view(&pairing);
    assert_eq!(view.collection_id, "AgentConversation");
    assert_eq!(view.pairing_retry_count, STUCK_THRESHOLD_ATTEMPTS);
    assert_eq!(view.last_retry_error_class.as_deref(), Some("RpcTimeout"));
    assert_eq!(
        view.stuck_since.as_deref(),
        crate::snapshot::system_time_rfc3339(Some(stuck_at)).as_deref()
    );
    assert!(view.last_retry_at.is_some());
}

#[test]
fn client_update_event_serializes_store_reason_for_projection_wakes() {
    let event = ClientUpdateEvent::coarse("store");
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({ "reason": "store" })
    );
    assert!(EVENT_REASONS.contains(&"store"));
    assert!(!EVENT_REASONS.contains(&"hydration"));
}
