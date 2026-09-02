//! Product sync status projected from one coherent client observation.
//!
//! DefraDB owns replication work, retry clocks, exhaustion, and quarantine.
//! The client owner snapshots those facts with transport connectivity; this
//! module only chooses the small product label shown in the UI. Per-deployment
//! pairing and route readiness remain separate authorities and are not rebuilt
//! here as global sync state.

use std::time::SystemTime;

use super::core::{ClientSyncStateSnapshot, DatabaseSyncStatus, P2PHealthStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncHealthState {
    Healthy,
    Syncing,
    Stalled,
    Offline,
    Failed,
}

impl SyncHealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Syncing => "syncing",
            Self::Stalled => "stalled",
            Self::Offline => "offline",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHealth {
    pub state: SyncHealthState,
    pub since: Option<SystemTime>,
    pub offline_since: Option<SystemTime>,
    pub last_error: Option<String>,
    /// Configured peers currently observed as connected, excluding unrelated
    /// transport connections.
    pub connected_peer_count: usize,
    pub pending_dag_count: usize,
    pub persisted_pending_dag_count: usize,
    pub push_retry_marker_count: usize,
    pub exhausted_fetch_count: u64,
    pub quarantined_dag_count: usize,
}

pub fn project_sync_health(sync: &ClientSyncStateSnapshot) -> SyncHealth {
    let transport = &sync.transport;
    let database = sync.database_sync.as_ref();
    let connected_peer_count = sync.peers.iter().filter(|peer| peer.dial_succeeded).count();
    let offline = is_offline(
        transport.status,
        !sync.peers.is_empty(),
        connected_peer_count,
    );
    let offline_since = offline
        .then(|| transport.last_failure_at.or(transport.last_ok_at))
        .flatten();
    let failed = database.is_some_and(DatabaseSyncStatus::has_quarantined_work);
    let stalled = database.is_some_and(DatabaseSyncStatus::has_exhausted_unresolved_work);
    let syncing = database.is_some_and(DatabaseSyncStatus::has_active_work);

    let state = if failed {
        SyncHealthState::Failed
    } else if offline {
        SyncHealthState::Offline
    } else if stalled {
        SyncHealthState::Stalled
    } else if syncing || transport.status == P2PHealthStatus::Degraded {
        SyncHealthState::Syncing
    } else {
        SyncHealthState::Healthy
    };
    let last_error = if failed {
        Some("DefraDB quarantined a document DAG that could not be merged".to_string())
    } else if stalled {
        Some("DefraDB exhausted every provider for an unresolved document DAG".to_string())
    } else {
        transport
            .last_error
            .clone()
            .or_else(|| sync.peers.iter().find_map(|peer| peer.last_error.clone()))
    };

    SyncHealth {
        state,
        since: match state {
            SyncHealthState::Offline => offline_since,
            SyncHealthState::Healthy => transport.last_ok_at,
            SyncHealthState::Syncing | SyncHealthState::Stalled | SyncHealthState::Failed => None,
        },
        offline_since,
        last_error,
        connected_peer_count,
        pending_dag_count: database
            .map(|status| status.pending_dags)
            .unwrap_or_default(),
        persisted_pending_dag_count: database
            .map(|status| status.persisted_pending_dags)
            .unwrap_or_default(),
        push_retry_marker_count: database
            .map(DatabaseSyncStatus::push_retry_marker_count)
            .unwrap_or_default(),
        exhausted_fetch_count: database
            .map(|status| status.pending_dag_fetch_exhausted)
            .unwrap_or_default(),
        quarantined_dag_count: database
            .map(|status| status.quarantined_pending_dags)
            .unwrap_or_default(),
    }
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
    use crate::client::core::{
        ClientPeerStatus, DatabasePushRetryMarkerStatus, P2PHealth, PairingCollectionStatus,
    };
    use crate::remote_admin::PairingErrorClass;
    use std::time::Duration;

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
        database_sync: Option<DatabaseSyncStatus>,
    ) -> SyncHealth {
        project_sync_health(&ClientSyncStateSnapshot {
            transport,
            database_sync,
            directory: Vec::new(),
            peers,
        })
    }

    #[test]
    fn no_configured_peer_is_healthy_not_offline() {
        let health = project(transport(P2PHealthStatus::Healthy), Vec::new(), None);
        assert_eq!(health.state, SyncHealthState::Healthy);
        assert_eq!(health.since, Some(t(50)));
    }

    #[test]
    fn configured_peer_connectivity_not_unrelated_global_connections_owns_offline() {
        let mut observed = transport(P2PHealthStatus::Healthy);
        observed.connected_peer_count = 7;
        let health = project(observed, vec![peer(false)], None);
        assert_eq!(health.state, SyncHealthState::Offline);
        assert_eq!(health.connected_peer_count, 0);
        assert_eq!(health.offline_since, Some(t(50)));
    }

    #[test]
    fn degraded_transport_with_a_live_peer_is_syncing() {
        let mut observed = transport(P2PHealthStatus::Degraded);
        observed.last_error = Some("probe failed".into());
        let health = project(observed, vec![peer(true)], None);
        assert_eq!(health.state, SyncHealthState::Syncing);
        assert_eq!(health.last_error.as_deref(), Some("probe failed"));
    }

    #[test]
    fn wedged_transport_is_offline_without_inventing_an_onset() {
        let mut observed = transport(P2PHealthStatus::Wedged);
        observed.last_ok_at = None;
        let health = project(observed, Vec::new(), None);
        assert_eq!(health.state, SyncHealthState::Offline);
        assert_eq!(health.since, None);
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
            Some(DatabaseSyncStatus::default()),
        );
        assert_eq!(health.state, SyncHealthState::Healthy);
    }

    #[test]
    fn database_pending_work_is_syncing_and_copies_exact_gauges() {
        let health = project(
            transport(P2PHealthStatus::Healthy),
            Vec::new(),
            Some(DatabaseSyncStatus {
                pending_dags: 2,
                persisted_pending_dags: 3,
                pending_resync_in_flight: true,
                push_retry_markers: DatabasePushRetryMarkerStatus {
                    document_markers: 4,
                    collection_markers: 5,
                    ..Default::default()
                },
                ..DatabaseSyncStatus::default()
            }),
        );
        assert_eq!(health.state, SyncHealthState::Syncing);
        assert_eq!(health.pending_dag_count, 2);
        assert_eq!(health.persisted_pending_dag_count, 3);
        assert_eq!(health.push_retry_marker_count, 9);
    }

    #[test]
    fn database_provider_exhaustion_is_stalled_even_with_sender_retry_work() {
        let health = project(
            transport(P2PHealthStatus::Healthy),
            Vec::new(),
            Some(DatabaseSyncStatus {
                pending_dags: 1,
                pending_dag_fetch_exhausted: 3,
                push_retry_markers: DatabasePushRetryMarkerStatus {
                    scheduled_peers: 1,
                    ..Default::default()
                },
                ..DatabaseSyncStatus::default()
            }),
        );
        assert_eq!(health.state, SyncHealthState::Stalled);
        assert_eq!(health.exhausted_fetch_count, 3);
        assert_eq!(
            health.last_error.as_deref(),
            Some("DefraDB exhausted every provider for an unresolved document DAG")
        );
    }

    #[test]
    fn database_quarantine_is_failed() {
        let health = project(
            transport(P2PHealthStatus::Healthy),
            Vec::new(),
            Some(DatabaseSyncStatus {
                quarantined_pending_dags: 2,
                ..DatabaseSyncStatus::default()
            }),
        );
        assert_eq!(health.state, SyncHealthState::Failed);
        assert_eq!(health.quarantined_dag_count, 2);
    }
}
