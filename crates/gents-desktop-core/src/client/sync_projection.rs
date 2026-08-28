//! Honest mobile sync health derived from existing client signals.
//!
//! This module does not own state. Callers pass the latest transport health,
//! and per-peer pairing/route diagnostics.
//! The returned [`SyncHealth::state`] is the only product-facing summary;
//! raw counters and timestamps stay available for diagnostics.
//!
//! Precedence is load-bearing and must not be collapsed by UI:
//!
//! 1. [`SyncHealthState::Failed`] — quarantined or permanently rejected work
//!    (`RemoteUnauthorized`).
//! 2. [`SyncHealthState::Offline`] — transport is wedged or no live peer is
//!    reachable.
//! 3. [`SyncHealthState::Stalled`] — pairing/route retries have crossed the
//!    stuck threshold; this is not "still syncing".
//! 4. [`SyncHealthState::Syncing`] — in-progress retries below the stuck
//!    threshold, or a degraded but still connected transport.
//! 5. [`SyncHealthState::Healthy`] — otherwise.

use std::time::SystemTime;

use super::core::{ClientPeerStatus, P2PHealth, P2PHealthStatus, STUCK_THRESHOLD_ATTEMPTS};
use crate::remote_admin::PairingErrorClass;

/// Product-facing sync summary. See module docs for precedence.
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

/// Combined projection of global P2P and pairing/route retry state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHealth {
    pub state: SyncHealthState,
    /// Onset of the winning [`Self::state`], when a signal carries one.
    pub since: Option<SystemTime>,
    pub offline_since: Option<SystemTime>,
    pub stalled_since: Option<SystemTime>,
    pub last_error_class: Option<PairingErrorClass>,
    pub last_error: Option<String>,
    pub pairing_retry_count: u32,
    pub route_retry_count: u32,
    pub connected_peer_count: usize,
}

/// Derive sync health from the authoritative client signals.
pub fn project_sync_health(health: &P2PHealth, peers: &[ClientPeerStatus]) -> SyncHealth {
    let mut pairing_retry_count = 0;
    let mut route_retry_count = 0;
    let mut stalled_since = None;
    let mut last_error_class = None;
    let mut last_error_class_at = None;
    let mut last_error = health.last_error.clone();
    let mut unauthorized = false;
    let mut unauthorized_since = None;
    let any_configured_peer = !peers.is_empty();
    let mut any_in_progress_retry = false;
    let mut latest_retry_at = None;

    for peer in peers {
        if let Some(error) = &peer.last_error {
            if last_error.is_none() {
                last_error = Some(error.clone());
            }
        }
        for pairing in &peer.pairing {
            pairing_retry_count = pairing_retry_count.max(pairing.pairing_retry_count);
            if pairing.pairing_retry_count > 0 {
                any_in_progress_retry = true;
            }
            latest_retry_at = later(latest_retry_at, pairing.last_retry_at);
            record_error_class(
                pairing.last_retry_error_class,
                pairing.last_retry_at,
                &mut last_error_class,
                &mut last_error_class_at,
                &mut unauthorized,
                &mut unauthorized_since,
            );
            stalled_since = earlier(stalled_since, pairing.stuck_since);
        }
        for route in &peer.routes {
            route_retry_count = route_retry_count.max(route.retry_count);
            if route.retry_count > 0 {
                any_in_progress_retry = true;
            }
            latest_retry_at = later(latest_retry_at, route.last_retry_at);
            record_error_class(
                route.last_retry_error_class,
                route.last_retry_at,
                &mut last_error_class,
                &mut last_error_class_at,
                &mut unauthorized,
                &mut unauthorized_since,
            );
            if route.retry_count >= STUCK_THRESHOLD_ATTEMPTS {
                stalled_since = earlier(stalled_since, route.last_retry_at);
            }
            if last_error.is_none() {
                last_error = route.last_error.clone();
            }
        }
    }

    let offline = is_offline(health, any_configured_peer);
    let offline_since = offline
        .then(|| health.last_failure_at.or(health.last_ok_at))
        .flatten();

    let state = if unauthorized {
        SyncHealthState::Failed
    } else if offline {
        SyncHealthState::Offline
    } else if stalled_since.is_some() {
        SyncHealthState::Stalled
    } else if any_in_progress_retry || health.status == P2PHealthStatus::Degraded {
        SyncHealthState::Syncing
    } else {
        SyncHealthState::Healthy
    };

    let since = match state {
        SyncHealthState::Failed => unauthorized_since.or(health.last_failure_at),
        SyncHealthState::Offline => offline_since,
        SyncHealthState::Stalled => stalled_since,
        SyncHealthState::Syncing => latest_retry_at,
        SyncHealthState::Healthy => health.last_ok_at,
    };

    SyncHealth {
        state,
        since,
        offline_since,
        stalled_since,
        last_error_class,
        last_error,
        pairing_retry_count,
        route_retry_count,
        connected_peer_count: health.connected_peer_count,
    }
}

fn is_offline(health: &P2PHealth, any_configured_peer: bool) -> bool {
    match health.status {
        P2PHealthStatus::Wedged => true,
        P2PHealthStatus::Degraded => health.connected_peer_count == 0,
        P2PHealthStatus::Healthy => health.connected_peer_count == 0 && any_configured_peer,
    }
}

fn record_error_class(
    class: Option<PairingErrorClass>,
    at: Option<SystemTime>,
    last_error_class: &mut Option<PairingErrorClass>,
    last_error_class_at: &mut Option<SystemTime>,
    unauthorized: &mut bool,
    unauthorized_since: &mut Option<SystemTime>,
) {
    if class == Some(PairingErrorClass::RemoteUnauthorized) {
        *unauthorized = true;
        *unauthorized_since = earlier(*unauthorized_since, at);
        *last_error_class = Some(PairingErrorClass::RemoteUnauthorized);
        return;
    }
    if *unauthorized {
        return;
    }
    let Some(class) = class else {
        return;
    };
    if last_error_class.is_none()
        || matches!((at, *last_error_class_at), (Some(next), Some(current)) if next > current)
        || (at.is_some() && last_error_class_at.is_none())
    {
        *last_error_class = Some(class);
        *last_error_class_at = at;
    }
}

fn earlier(left: Option<SystemTime>, right: Option<SystemTime>) -> Option<SystemTime> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn later(left: Option<SystemTime>, right: Option<SystemTime>) -> Option<SystemTime> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::core::{
        ClientRouteStatus, PairingCollectionStatus, STUCK_THRESHOLD_ATTEMPTS,
    };
    use std::time::Duration;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn healthy_transport() -> P2PHealth {
        P2PHealth {
            status: P2PHealthStatus::Healthy,
            consecutive_failures: 0,
            connected_peer_count: 1,
            replicator_count: 1,
            last_error: None,
            last_ok_at: Some(t(50)),
            last_failure_at: None,
        }
    }

    fn peer(
        pairing: Vec<PairingCollectionStatus>,
        routes: Vec<ClientRouteStatus>,
        dial_succeeded: bool,
    ) -> ClientPeerStatus {
        ClientPeerStatus {
            peer_id: "peer-1".into(),
            label: "Studio".into(),
            agent_did: "did:test:agent".into(),
            addr: "/ip4/10.0.0.1/tcp/1".into(),
            dial_succeeded,
            last_error: None,
            pairing,
            routes,
            chat_safe: dial_succeeded,
        }
    }

    fn route(retry_count: u32, class: Option<PairingErrorClass>) -> ClientRouteStatus {
        ClientRouteStatus {
            route_id: "peer-1:client-to-runtime".into(),
            direction: "client-to-runtime".into(),
            directory_id: "peer-1".into(),
            transport_peer_id: Some("transport".into()),
            address: Some("ticket".into()),
            template: Some("machine".into()),
            desired: true,
            applied: retry_count == 0,
            live_match: retry_count == 0,
            filter_summary: "machine".into(),
            last_error: None,
            retry_count,
            last_retry_at: (retry_count > 0).then_some(t(20)),
            last_retry_error_class: class,
        }
    }

    fn stuck_pairing(class: PairingErrorClass, stuck_at: SystemTime) -> PairingCollectionStatus {
        let mut pairing = PairingCollectionStatus::new("AgentSession");
        for _ in 0..STUCK_THRESHOLD_ATTEMPTS {
            pairing.record_retry(class);
        }
        pairing.update_stuck_indicator(stuck_at);
        pairing
    }

    fn retrying_pairing(class: PairingErrorClass, attempts: u32) -> PairingCollectionStatus {
        let mut pairing = PairingCollectionStatus::new("AgentSession");
        for _ in 0..attempts {
            pairing.record_retry(class);
        }
        pairing
    }

    #[test]
    fn healthy_when_transport_ok_and_no_retries() {
        let projected = project_sync_health(&healthy_transport(), &[]);
        assert_eq!(projected.state, SyncHealthState::Healthy);
        assert_eq!(projected.state.as_str(), "healthy");
        assert_eq!(projected.since, Some(t(50)));
        assert!(projected.offline_since.is_none());
        assert!(projected.stalled_since.is_none());
        assert_eq!(projected.pairing_retry_count, 0);
        assert_eq!(projected.route_retry_count, 0);
        assert_eq!(projected.connected_peer_count, 1);
    }

    #[test]
    fn unpaired_client_with_no_peers_is_healthy_not_offline() {
        let health = P2PHealth {
            connected_peer_count: 0,
            replicator_count: 0,
            last_ok_at: Some(t(10)),
            ..healthy_transport()
        };
        let projected = project_sync_health(&health, &[]);
        assert_eq!(projected.state, SyncHealthState::Healthy);
        assert!(projected.offline_since.is_none());
    }

    #[test]
    fn unauthorized_pairing_is_failed() {
        let pairing = retrying_pairing(PairingErrorClass::RemoteUnauthorized, 1);
        let projected =
            project_sync_health(&healthy_transport(), &[peer(vec![pairing], vec![], true)]);
        assert_eq!(projected.state, SyncHealthState::Failed);
        assert_eq!(
            projected.last_error_class,
            Some(PairingErrorClass::RemoteUnauthorized)
        );
        assert_eq!(projected.pairing_retry_count, 1);
    }

    #[test]
    fn unauthorized_route_is_failed_without_waiting_for_stuck_threshold() {
        let projected = project_sync_health(
            &healthy_transport(),
            &[peer(
                vec![],
                vec![route(1, Some(PairingErrorClass::RemoteUnauthorized))],
                true,
            )],
        );
        assert_eq!(projected.state, SyncHealthState::Failed);
        assert_eq!(
            projected.last_error_class,
            Some(PairingErrorClass::RemoteUnauthorized)
        );
    }

    #[test]
    fn stalled_does_not_collapse_into_syncing() {
        let stuck_at = t(80);
        let pairing = stuck_pairing(PairingErrorClass::RpcTimeout, stuck_at);
        let projected =
            project_sync_health(&healthy_transport(), &[peer(vec![pairing], vec![], true)]);
        assert_eq!(projected.state, SyncHealthState::Stalled);
        assert_eq!(projected.stalled_since, Some(stuck_at));
        assert_eq!(projected.since, Some(stuck_at));
        assert_eq!(
            projected.last_error_class,
            Some(PairingErrorClass::RpcTimeout)
        );
        assert!(projected.pairing_retry_count >= STUCK_THRESHOLD_ATTEMPTS);
    }

    #[test]
    fn earliest_stuck_since_wins_across_collections() {
        let earlier_stuck = t(40);
        let later_stuck = t(90);
        let first = stuck_pairing(PairingErrorClass::RpcError, later_stuck);
        let mut second = PairingCollectionStatus::new("AgentConversation");
        for _ in 0..STUCK_THRESHOLD_ATTEMPTS {
            second.record_retry(PairingErrorClass::RpcTimeout);
        }
        second.update_stuck_indicator(earlier_stuck);
        let projected = project_sync_health(
            &healthy_transport(),
            &[peer(vec![first, second], vec![], true)],
        );
        assert_eq!(projected.state, SyncHealthState::Stalled);
        assert_eq!(projected.stalled_since, Some(earlier_stuck));
    }

    #[test]
    fn wedged_transport_is_offline() {
        let health = P2PHealth {
            status: P2PHealthStatus::Wedged,
            consecutive_failures: 3,
            connected_peer_count: 0,
            last_failure_at: Some(t(12)),
            last_ok_at: Some(t(2)),
            last_error: Some("no listen addresses".into()),
            ..P2PHealth::default()
        };
        let projected = project_sync_health(&health, &[peer(vec![], vec![], false)]);
        assert_eq!(projected.state, SyncHealthState::Offline);
        assert_eq!(projected.offline_since, Some(t(12)));
        assert_eq!(projected.since, Some(t(12)));
    }

    #[test]
    fn healthy_transport_with_configured_but_unconnected_peer_is_offline() {
        let health = P2PHealth {
            connected_peer_count: 0,
            last_ok_at: Some(t(5)),
            last_failure_at: None,
            ..healthy_transport()
        };
        let projected = project_sync_health(&health, &[peer(vec![], vec![], false)]);
        assert_eq!(projected.state, SyncHealthState::Offline);
        assert_eq!(projected.offline_since, Some(t(5)));
    }

    #[test]
    fn degraded_zero_connections_is_offline_not_syncing() {
        let health = P2PHealth {
            status: P2PHealthStatus::Degraded,
            consecutive_failures: 1,
            connected_peer_count: 0,
            last_failure_at: Some(t(7)),
            last_ok_at: Some(t(3)),
            last_error: Some("probe failed".into()),
            ..P2PHealth::default()
        };
        let projected = project_sync_health(&health, &[]);
        assert_eq!(projected.state, SyncHealthState::Offline);
        assert_eq!(projected.offline_since, Some(t(7)));
    }

    #[test]
    fn degraded_with_live_peers_is_syncing() {
        let health = P2PHealth {
            status: P2PHealthStatus::Degraded,
            consecutive_failures: 1,
            connected_peer_count: 1,
            last_failure_at: Some(t(7)),
            last_ok_at: Some(t(3)),
            last_error: Some("probe failed".into()),
            replicator_count: 1,
        };
        let projected = project_sync_health(&health, &[peer(vec![], vec![], true)]);
        assert_eq!(projected.state, SyncHealthState::Syncing);
        assert!(projected.offline_since.is_none());
        assert_eq!(projected.last_error.as_deref(), Some("probe failed"));
    }

    #[test]
    fn pairing_retry_below_stuck_threshold_is_syncing() {
        let pairing = retrying_pairing(PairingErrorClass::RpcTimeout, 2);
        let projected =
            project_sync_health(&healthy_transport(), &[peer(vec![pairing], vec![], true)]);
        assert_eq!(projected.state, SyncHealthState::Syncing);
        assert!(projected.stalled_since.is_none());
        assert_eq!(projected.pairing_retry_count, 2);
        assert_eq!(
            projected.last_error_class,
            Some(PairingErrorClass::RpcTimeout)
        );
        assert!(projected.since.is_some());
    }

    #[test]
    fn route_retries_cross_stuck_threshold_without_pairing_stuck() {
        let projected = project_sync_health(
            &healthy_transport(),
            &[peer(
                vec![],
                vec![route(
                    STUCK_THRESHOLD_ATTEMPTS,
                    Some(PairingErrorClass::RpcError),
                )],
                true,
            )],
        );
        assert_eq!(projected.state, SyncHealthState::Stalled);
        assert_eq!(projected.stalled_since, Some(t(20)));
        assert_eq!(projected.route_retry_count, STUCK_THRESHOLD_ATTEMPTS);
        assert_eq!(
            projected.last_error_class,
            Some(PairingErrorClass::RpcError)
        );
    }

    #[test]
    fn retry_counts_are_max_across_peers_and_collections() {
        let mut low = PairingCollectionStatus::new("AgentRequest");
        low.record_retry(PairingErrorClass::LocalError);
        let mut high = PairingCollectionStatus::new("AgentSession");
        for _ in 0..3 {
            high.record_retry(PairingErrorClass::RpcTimeout);
        }
        let second = ClientPeerStatus {
            peer_id: "peer-2".into(),
            label: "Phone".into(),
            agent_did: "did:test:other".into(),
            addr: "/ip4/10.0.0.2/tcp/1".into(),
            dial_succeeded: true,
            last_error: None,
            pairing: vec![high],
            routes: vec![route(5, Some(PairingErrorClass::RpcTimeout))],
            chat_safe: true,
        };
        let projected = project_sync_health(
            &healthy_transport(),
            &[peer(vec![low], vec![route(1, None)], true), second],
        );
        assert_eq!(projected.state, SyncHealthState::Syncing);
        assert_eq!(projected.pairing_retry_count, 3);
        assert_eq!(projected.route_retry_count, 5);
    }

    #[test]
    fn recovery_from_stalled_after_pairing_success() {
        let mut pairing = stuck_pairing(PairingErrorClass::RpcTimeout, t(80));
        assert!(pairing.stuck_since.is_some());
        pairing.record_success();
        let projected =
            project_sync_health(&healthy_transport(), &[peer(vec![pairing], vec![], true)]);
        assert_eq!(projected.state, SyncHealthState::Healthy);
        assert!(projected.stalled_since.is_none());
        assert_eq!(projected.pairing_retry_count, 0);
        assert!(projected.last_error_class.is_none());
    }

    #[test]
    fn recovery_from_offline_after_reconnect() {
        let projected = project_sync_health(&healthy_transport(), &[peer(vec![], vec![], true)]);
        assert_eq!(projected.state, SyncHealthState::Healthy);
        assert!(projected.offline_since.is_none());
        assert_eq!(projected.connected_peer_count, 1);
    }

    #[test]
    fn offline_without_transport_timestamps_does_not_invent_a_since_time() {
        let health = P2PHealth {
            status: P2PHealthStatus::Wedged,
            consecutive_failures: 3,
            connected_peer_count: 0,
            last_ok_at: None,
            last_failure_at: None,
            last_error: Some("wedged".into()),
            replicator_count: 0,
        };
        let projected = project_sync_health(&health, &[]);
        assert_eq!(projected.state, SyncHealthState::Offline);
        assert_eq!(projected.offline_since, None);
        assert_eq!(projected.since, None);
    }

    #[test]
    fn latest_retry_error_class_wins_independent_of_peer_order() {
        let mut older = retrying_pairing(PairingErrorClass::RpcTimeout, 1);
        older.last_retry_at = Some(t(10));
        let mut newer = retrying_pairing(PairingErrorClass::LocalError, 1);
        newer.last_retry_at = Some(t(20));
        let mut first_peer = peer(vec![older], vec![], true);
        first_peer.peer_id = "peer-a".into();
        let mut second_peer = peer(vec![newer], vec![], true);
        second_peer.peer_id = "peer-b".into();

        let projected = project_sync_health(&healthy_transport(), &[first_peer, second_peer]);

        assert_eq!(projected.state, SyncHealthState::Syncing);
        assert_eq!(projected.since, Some(t(20)));
        assert_eq!(
            projected.last_error_class,
            Some(PairingErrorClass::LocalError)
        );
    }
}
