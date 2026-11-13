use super::core::{ClientSyncStateSnapshot, EnrollmentActivationStage, P2PHealthStatus};
use super::RuntimeConnectionStatus;

/// `ClientSyncHealth.State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncHealthState {
    Healthy,
    Waiting,
    Blocked,
}

impl SyncHealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
        }
    }
}

/// `ClientSyncHealth.Cause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncHealthCause {
    RuntimeConnecting,
    RuntimeDisconnected,
    RuntimeRefused,
    RuntimeNoEndpoint,
    EnrollmentAuthentication,
    EnrollmentRoute,
    DiscoveryDegraded,
    DiscoveryOffline,
}

impl SyncHealthCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeConnecting => "runtime_connecting",
            Self::RuntimeDisconnected => "runtime_disconnected",
            Self::RuntimeRefused => "runtime_refused",
            Self::RuntimeNoEndpoint => "runtime_no_endpoint",
            Self::EnrollmentAuthentication => "enrollment_authentication",
            Self::EnrollmentRoute => "enrollment_route",
            Self::DiscoveryDegraded => "discovery_degraded",
            Self::DiscoveryOffline => "discovery_offline",
        }
    }

    pub fn next_action(self) -> SyncNextAction {
        match self {
            Self::RuntimeConnecting
            | Self::RuntimeDisconnected
            | Self::EnrollmentRoute
            | Self::DiscoveryDegraded
            | Self::DiscoveryOffline => SyncNextAction::Wait,
            Self::RuntimeRefused => SyncNextAction::RestoreAccess,
            Self::RuntimeNoEndpoint => SyncNextAction::ConnectRuntime,
            Self::EnrollmentAuthentication => SyncNextAction::EnrollAgain,
        }
    }

    fn state(self) -> SyncHealthState {
        if self.next_action() == SyncNextAction::Wait {
            SyncHealthState::Waiting
        } else {
            SyncHealthState::Blocked
        }
    }

    fn is_discovery(self) -> bool {
        matches!(self, Self::DiscoveryDegraded | Self::DiscoveryOffline)
    }
}

/// `ClientSyncHealth.NextAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncNextAction {
    Wait,
    RestoreAccess,
    ConnectRuntime,
    EnrollAgain,
}

impl SyncNextAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::RestoreAccess => "restore_access",
            Self::ConnectRuntime => "connect_runtime",
            Self::EnrollAgain => "enroll_again",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHealth {
    pub state: SyncHealthState,
    pub cause: Option<SyncHealthCause>,
    pub next_action: Option<SyncNextAction>,
    pub last_error: Option<String>,
    pub runtime_count: usize,
    pub connected_runtime_count: usize,
    pub activation_error_count: usize,
}

pub fn project_sync_health<I>(sync: &ClientSyncStateSnapshot, runtimes: I) -> SyncHealth
where
    I: IntoIterator<Item = Option<RuntimeConnectionStatus>>,
{
    let runtimes = runtimes.into_iter().collect::<Vec<_>>();
    let connected_runtime_count = runtimes
        .iter()
        .filter(|status| **status == Some(RuntimeConnectionStatus::Connected))
        .count();
    let runtimes_connected = !runtimes.is_empty() && connected_runtime_count == runtimes.len();
    let dialed_peers = sync.peers.iter().filter(|peer| peer.dial_succeeded).count();
    let causes = runtimes
        .iter()
        .copied()
        .filter_map(runtime_cause)
        .chain(
            sync.enrollment_activation_errors
                .values()
                .map(|error| match error.stage {
                    EnrollmentActivationStage::Authentication => {
                        SyncHealthCause::EnrollmentAuthentication
                    }
                    EnrollmentActivationStage::Route => SyncHealthCause::EnrollmentRoute,
                }),
        )
        .chain(
            discovery_cause(sync.transport.status, sync.peers.len(), dialed_peers)
                .filter(|_| !runtimes_connected),
        )
        .collect::<Vec<_>>();
    let cause = causes
        .iter()
        .find(|cause| cause.state() == SyncHealthState::Blocked)
        .or_else(|| causes.first())
        .copied();
    SyncHealth {
        state: cause.map_or(SyncHealthState::Healthy, SyncHealthCause::state),
        cause,
        next_action: cause.map(SyncHealthCause::next_action),
        last_error: cause
            .filter(|cause| cause.is_discovery())
            .and_then(|_| sync.transport.last_error.clone()),
        runtime_count: runtimes.len(),
        connected_runtime_count,
        activation_error_count: sync.enrollment_activation_errors.len(),
    }
}

fn runtime_cause(status: Option<RuntimeConnectionStatus>) -> Option<SyncHealthCause> {
    match status {
        None | Some(RuntimeConnectionStatus::Connecting) => {
            Some(SyncHealthCause::RuntimeConnecting)
        }
        Some(RuntimeConnectionStatus::Connected) => None,
        Some(RuntimeConnectionStatus::Disconnected) => Some(SyncHealthCause::RuntimeDisconnected),
        Some(RuntimeConnectionStatus::Refused) => Some(SyncHealthCause::RuntimeRefused),
        Some(RuntimeConnectionStatus::NoEndpoint) => Some(SyncHealthCause::RuntimeNoEndpoint),
    }
}

fn discovery_cause(
    transport: P2PHealthStatus,
    configured_peers: usize,
    dialed_peers: usize,
) -> Option<SyncHealthCause> {
    match transport {
        P2PHealthStatus::Wedged => Some(SyncHealthCause::DiscoveryOffline),
        P2PHealthStatus::Degraded if dialed_peers == 0 => Some(SyncHealthCause::DiscoveryOffline),
        P2PHealthStatus::Degraded => Some(SyncHealthCause::DiscoveryDegraded),
        P2PHealthStatus::Healthy if dialed_peers == 0 && configured_peers > 0 => {
            Some(SyncHealthCause::DiscoveryOffline)
        }
        P2PHealthStatus::Healthy => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::core::{ClientPeerStatus, EnrollmentActivationError, P2PHealth};

    fn status(name: &str) -> RuntimeConnectionStatus {
        match name {
            "connecting" => RuntimeConnectionStatus::Connecting,
            "connected" => RuntimeConnectionStatus::Connected,
            "disconnected" => RuntimeConnectionStatus::Disconnected,
            "refused" => RuntimeConnectionStatus::Refused,
            "noEndpoint" => RuntimeConnectionStatus::NoEndpoint,
            other => panic!("unexpected runtime connection status {other}"),
        }
    }

    fn peer(index: usize, dial_succeeded: bool) -> ClientPeerStatus {
        ClientPeerStatus {
            peer_id: format!("peer-{index}"),
            label: format!("Peer {index}"),
            node_did: format!("did:test:{index}"),
            addr: format!("/ip4/10.0.0.{index}/tcp/1"),
            dial_succeeded,
            last_error: None,
            routes: Vec::new(),
        }
    }

    fn snapshot(case: &gents_lean_vocab::LeanClientSyncHealthCase) -> ClientSyncStateSnapshot {
        let status = match case.transport.as_str() {
            "healthy" => P2PHealthStatus::Healthy,
            "degraded" => P2PHealthStatus::Degraded,
            "wedged" => P2PHealthStatus::Wedged,
            other => panic!("unexpected transport {other}"),
        };
        ClientSyncStateSnapshot {
            transport: P2PHealth {
                status,
                last_error: Some("probe failed".into()),
                ..P2PHealth::default()
            },
            enrollment_activation_errors: case
                .enrollment
                .iter()
                .enumerate()
                .map(|(index, stage)| {
                    (
                        format!("request-{index}"),
                        EnrollmentActivationError {
                            request_digest: format!("digest-{index}"),
                            authorization_sequence: 1,
                            authorization_expires_at: "2030-01-01T00:00:00Z".into(),
                            stage: match stage.as_str() {
                                "authentication" => EnrollmentActivationStage::Authentication,
                                "route" => EnrollmentActivationStage::Route,
                                other => panic!("unexpected stage {other}"),
                            },
                        },
                    )
                })
                .collect(),
            directory: Vec::new(),
            peers: (0..case.configured_peers)
                .map(|index| peer(index as usize, index < case.dialed_peers))
                .collect(),
        }
    }

    #[test]
    fn generated_sync_health_cases_match_lean() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().client_sync_health_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let sync = snapshot(case);
            let health = project_sync_health(
                &sync,
                case.runtimes
                    .iter()
                    .map(|runtime| runtime.as_deref().map(status)),
            );
            let label = format!(
                "runtimes={:?} enrollment={:?} transport={} configured={} dialed={}",
                case.runtimes,
                case.enrollment,
                case.transport,
                case.configured_peers,
                case.dialed_peers
            );
            assert_eq!(health.state.as_str(), case.state.as_str(), "{label}");
            assert_eq!(
                health.cause.map(SyncHealthCause::as_str),
                case.cause.as_deref(),
                "{label}"
            );
            assert_eq!(
                health.next_action.map(SyncNextAction::as_str),
                case.next_action.as_deref(),
                "{label}"
            );
            assert_eq!(
                health.last_error.is_some(),
                health.cause.is_some_and(SyncHealthCause::is_discovery),
                "{label}"
            );
            assert_eq!(health.activation_error_count, case.enrollment.len());
            assert_eq!(health.runtime_count, case.runtime_count as usize);
            assert_eq!(
                health.connected_runtime_count,
                case.connected_runtime_count as usize
            );
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn sync_health_cause_next_action_matches_lean() {
        gents_lean_vocab::families::client_sync_health_cause_next_action::replay(|i| {
            SyncHealthCause::next_action(i.cause)
        });
    }
}
