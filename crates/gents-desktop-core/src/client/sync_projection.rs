//! Product sync status projected from one coherent client observation.
//!
//! DefraDB owns replication work, retry clocks, exhaustion, and quarantine.
//! The client owner snapshots those facts with transport connectivity; this
//! module only chooses the small product label shown in the UI. Per-deployment
//! pairing and route readiness remain separate authorities and are not rebuilt
//! here as global sync state.

#[cfg(test)]
use gents::P2pSyncStatusSnapshot;

use super::core::{ClientSyncStateSnapshot, P2PHealthStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncHealthState {
    Healthy,
    Syncing,
    Offline,
    Failed,
}

impl SyncHealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Syncing => "syncing",
            Self::Offline => "offline",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHealth {
    pub state: SyncHealthState,
    pub last_error: Option<String>,
    /// Configured peers currently observed as connected, excluding unrelated
    /// transport connections.
    pub connected_peer_count: usize,
    pub pending_dag_count: Option<usize>,
    pub persisted_pending_dag_count: Option<usize>,
    pub push_retry_marker_count: Option<usize>,
    pub exhausted_fetch_count: Option<u64>,
    pub quarantined_dag_count: Option<usize>,
}

pub fn project_sync_health(sync: &ClientSyncStateSnapshot) -> Option<SyncHealth> {
    let transport = &sync.transport;
    let database = sync.database_sync.as_ref();
    let database_error = sync.database_sync_error.as_ref();
    let connected_peer_count = sync.peers.iter().filter(|peer| peer.dial_succeeded).count();
    let offline = is_offline(
        transport.status,
        !sync.peers.is_empty(),
        connected_peer_count,
    );
    if database.is_none()
        && database_error.is_none()
        && transport.status == P2PHealthStatus::Healthy
        && !offline
    {
        return None;
    }
    let failed = database.is_some_and(|status| status.quarantined_pending_dags > 0);
    let push_retry_marker_count = database.map(|status| {
        status
            .push_retry_markers
            .document_markers
            .saturating_add(status.push_retry_markers.collection_markers)
    });
    let syncing = database.is_some_and(|status| {
        status.pending_dags > 0
            || status.persisted_pending_dags > 0
            || push_retry_marker_count.is_some_and(|count| count > 0)
            || status.pending_resync_in_flight
            || status.push_backlog.queued_items > 0
            || status.push_backlog.active_jobs > 0
    });

    let state = if failed || database_error.is_some() {
        SyncHealthState::Failed
    } else if offline {
        SyncHealthState::Offline
    } else if syncing || transport.status == P2PHealthStatus::Degraded {
        SyncHealthState::Syncing
    } else {
        SyncHealthState::Healthy
    };
    let last_error = if database_error.is_some() {
        database_error.cloned()
    } else if failed {
        Some("DefraDB quarantined a document DAG that could not be merged".to_string())
    } else {
        transport.last_error.clone()
    };

    Some(SyncHealth {
        state,
        last_error,
        connected_peer_count,
        pending_dag_count: database.map(|status| status.pending_dags),
        persisted_pending_dag_count: database.map(|status| status.persisted_pending_dags),
        push_retry_marker_count,
        exhausted_fetch_count: database.map(|status| status.pending_dag_fetch_exhausted),
        quarantined_dag_count: database.map(|status| status.quarantined_pending_dags),
    })
}

fn is_offline(
    transport_status: P2PHealthStatus,
    any_configured_peer: bool,
    connected_peer_count: usize,
) -> bool {
    match transport_status {
        P2PHealthStatus::Wedged => true,
        P2PHealthStatus::Degraded => connected_peer_count == 0,
        P2PHealthStatus::Healthy => connected_peer_count == 0 && any_configured_peer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::core::{ClientPeerStatus, P2PHealth, PairingCollectionStatus};
    use crate::remote_admin::PairingErrorClass;
    use gents::P2pPushRetryMarkerSnapshot;
    use std::time::{Duration, SystemTime};

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn transport(status: P2PHealthStatus) -> P2PHealth {
        P2PHealth {
            status,
            connected_peer_count: usize::from(status == P2PHealthStatus::Healthy),
            replicator_count: 1,
            last_ok_at: Some(t(50)),
            ..P2PHealth::default()
        }
    }

    fn peer(dial_succeeded: bool) -> ClientPeerStatus {
        ClientPeerStatus {
            peer_id: "peer-1".into(),
            label: "Studio".into(),
            agent_did: "did:test:agent".into(),
            addr: "/ip4/10.0.0.1/tcp/1".into(),
            dial_succeeded,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
        }
    }

    fn project(
        transport: P2PHealth,
        peers: Vec<ClientPeerStatus>,
        database_sync: Option<P2pSyncStatusSnapshot>,
    ) -> Option<SyncHealth> {
        project_sync_health(&ClientSyncStateSnapshot {
            transport,
            database_sync,
            database_sync_error: None,
            directory: Vec::new(),
            peers,
        })
    }

    #[test]
    fn missing_database_observation_has_no_projected_health() {
        assert_eq!(
            project(transport(P2PHealthStatus::Healthy), Vec::new(), None),
            None
        );
    }

    #[test]
    fn database_decode_error_is_visible_without_reclassifying_transport() {
        let health = project_sync_health(&ClientSyncStateSnapshot {
            transport: transport(P2PHealthStatus::Healthy),
            database_sync: None,
            database_sync_error: Some("incompatible sync status".into()),
            directory: Vec::new(),
            peers: Vec::new(),
        })
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Failed);
        assert_eq!(
            health.last_error.as_deref(),
            Some("incompatible sync status")
        );
        assert_eq!(health.pending_dag_count, None);
    }

    #[test]
    fn no_configured_peer_is_healthy_not_offline() {
        let health = project(
            transport(P2PHealthStatus::Healthy),
            Vec::new(),
            Some(P2pSyncStatusSnapshot::default()),
        )
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Healthy);
    }

    #[test]
    fn configured_peer_connectivity_not_unrelated_global_connections_owns_offline() {
        let mut observed = transport(P2PHealthStatus::Healthy);
        observed.connected_peer_count = 7;
        let health = project(
            observed,
            vec![peer(false)],
            Some(P2pSyncStatusSnapshot::default()),
        )
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Offline);
        assert_eq!(health.connected_peer_count, 0);
        assert_eq!(health.pending_dag_count, Some(0));
    }

    #[test]
    fn degraded_transport_with_a_live_peer_is_syncing() {
        let mut observed = transport(P2PHealthStatus::Degraded);
        observed.last_error = Some("probe failed".into());
        let health = project(
            observed,
            vec![peer(true)],
            Some(P2pSyncStatusSnapshot::default()),
        )
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Syncing);
        assert_eq!(health.last_error.as_deref(), Some("probe failed"));
    }

    #[test]
    fn wedged_transport_is_visible_before_database_observation() {
        let mut observed = transport(P2PHealthStatus::Wedged);
        observed.last_ok_at = None;
        observed.last_error = Some("probe failed".into());
        let health = project(observed, Vec::new(), None).unwrap();
        assert_eq!(health.state, SyncHealthState::Offline);
        assert_eq!(health.last_error.as_deref(), Some("probe failed"));
        assert_eq!(health.pending_dag_count, None);
    }

    #[test]
    fn pairing_retry_does_not_rebuild_database_sync_state() {
        let mut retry = PairingCollectionStatus::new("AgentSession");
        retry.record_retry(PairingErrorClass::RpcTimeout);
        let mut configured = peer(true);
        configured.pairing.push(retry);
        let health = project(
            transport(P2PHealthStatus::Healthy),
            vec![configured],
            Some(P2pSyncStatusSnapshot::default()),
        )
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Healthy);
    }

    #[test]
    fn database_pending_work_is_syncing_and_copies_exact_gauges() {
        let health = project(
            transport(P2PHealthStatus::Healthy),
            Vec::new(),
            Some(P2pSyncStatusSnapshot {
                pending_dags: 2,
                persisted_pending_dags: 3,
                pending_resync_in_flight: true,
                push_retry_markers: P2pPushRetryMarkerSnapshot {
                    document_markers: 4,
                    collection_markers: 5,
                    ..Default::default()
                },
                ..P2pSyncStatusSnapshot::default()
            }),
        )
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Syncing);
        assert_eq!(health.pending_dag_count, Some(2));
        assert_eq!(health.persisted_pending_dag_count, Some(3));
        assert_eq!(health.push_retry_marker_count, Some(9));
    }

    #[test]
    fn historical_provider_exhaustion_does_not_invent_a_stalled_state() {
        let health = project(
            transport(P2PHealthStatus::Healthy),
            Vec::new(),
            Some(P2pSyncStatusSnapshot {
                pending_dags: 1,
                pending_dag_fetch_exhausted: 3,
                push_retry_markers: P2pPushRetryMarkerSnapshot {
                    scheduled_peers: 1,
                    ..Default::default()
                },
                ..P2pSyncStatusSnapshot::default()
            }),
        )
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Syncing);
        assert_eq!(health.exhausted_fetch_count, Some(3));
        assert_eq!(health.last_error, None);
    }

    #[test]
    fn database_quarantine_is_failed() {
        let health = project(
            transport(P2PHealthStatus::Healthy),
            Vec::new(),
            Some(P2pSyncStatusSnapshot {
                quarantined_pending_dags: 2,
                ..P2pSyncStatusSnapshot::default()
            }),
        )
        .unwrap();
        assert_eq!(health.state, SyncHealthState::Failed);
        assert_eq!(health.quarantined_dag_count, Some(2));
    }
}
