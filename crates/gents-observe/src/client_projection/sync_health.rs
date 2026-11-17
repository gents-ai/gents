use super::observe::RuntimeConnectionStatus;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncHealthCause {
    RuntimeConnecting,
    RuntimeDisconnected,
    RuntimeRefused,
    RuntimeNoEndpoint,
}

impl SyncHealthCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeConnecting => "runtime_connecting",
            Self::RuntimeDisconnected => "runtime_disconnected",
            Self::RuntimeRefused => "runtime_refused",
            Self::RuntimeNoEndpoint => "runtime_no_endpoint",
        }
    }
    pub fn next_action(self) -> SyncNextAction {
        match self {
            Self::RuntimeConnecting | Self::RuntimeDisconnected => SyncNextAction::Wait,
            Self::RuntimeRefused => SyncNextAction::RestoreAccess,
            Self::RuntimeNoEndpoint => SyncNextAction::ConnectRuntime,
        }
    }
    fn state(self) -> SyncHealthState {
        if self.next_action() == SyncNextAction::Wait {
            SyncHealthState::Waiting
        } else {
            SyncHealthState::Blocked
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncNextAction {
    Wait,
    RestoreAccess,
    ConnectRuntime,
}

impl SyncNextAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::RestoreAccess => "restore_access",
            Self::ConnectRuntime => "connect_runtime",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHealth {
    pub state: SyncHealthState,
    pub cause: Option<SyncHealthCause>,
    pub next_action: Option<SyncNextAction>,
    pub runtime_count: usize,
    pub connected_runtime_count: usize,
}

pub fn project_sync_health<I>(runtimes: I) -> SyncHealth
where
    I: IntoIterator<Item = Option<RuntimeConnectionStatus>>,
{
    let runtimes = runtimes.into_iter().collect::<Vec<_>>();
    let connected_runtime_count = runtimes
        .iter()
        .filter(|status| **status == Some(RuntimeConnectionStatus::Connected))
        .count();
    let causes = runtimes
        .iter()
        .copied()
        .filter_map(runtime_cause)
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
        runtime_count: runtimes.len(),
        connected_runtime_count,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_actions_follow_the_model() {
        gents_lean_vocab::families::client_sync_health_cause_next_action::replay::<
            SyncHealthCause,
            SyncNextAction,
        >(|input| input.cause.next_action());
    }

    #[test]
    fn connection_health_follows_the_model() {
        for case in gents_lean_vocab::families::client_sync_health_cases_projection::cases() {
            let statuses = case.input.runtimes.iter().map(|status| {
                status.as_deref().map(|status| match status {
                    "connecting" => RuntimeConnectionStatus::Connecting,
                    "connected" => RuntimeConnectionStatus::Connected,
                    "disconnected" => RuntimeConnectionStatus::Disconnected,
                    "refused" => RuntimeConnectionStatus::Refused,
                    "noEndpoint" => RuntimeConnectionStatus::NoEndpoint,
                    other => panic!("unexpected runtime status {other}"),
                })
            });
            let view = project_sync_health(statuses);
            case.check(
                gents_lean_vocab::families::client_sync_health_cases_projection::ViewOutput {
                    state: view.state.as_str().into(),
                    cause: view.cause.map(|cause| cause.as_str().into()),
                    next_action: view.next_action.map(|action| action.as_str().into()),
                    runtime_count: view.runtime_count as u64,
                    connected_runtime_count: view.connected_runtime_count as u64,
                },
            );
        }
    }
}
