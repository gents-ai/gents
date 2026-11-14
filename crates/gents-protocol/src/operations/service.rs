use serde::{Deserialize, Serialize};

/// Lean `ServiceLifecycle.Action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLifecycleAction {
    Install,
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
    Status,
    Uninstall,
}

impl ServiceLifecycleAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Status => "status",
            Self::Uninstall => "uninstall",
        }
    }

    /// Lean `ServiceLifecycle.Action.disrupts`.
    pub fn disrupts(self) -> bool {
        matches!(
            self,
            Self::Install | Self::Stop | Self::Restart | Self::Uninstall
        )
    }

    /// Lean `ServiceLifecycle.Action.contendsWithUpdate`.
    pub fn contends_with_update(self) -> bool {
        matches!(self, Self::Install | Self::Start | Self::Restart)
    }
}

/// Lean `ServiceLifecycle.Decision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLifecycleDecision {
    Admitted,
    WouldDisruptCaller,
    UpdateInProgress,
}

/// Lean `ServiceLifecycle.admission`.
pub fn decide(
    action: ServiceLifecycleAction,
    caller_runs_on_target: bool,
    update_pending: bool,
) -> ServiceLifecycleDecision {
    if action.disrupts() && caller_runs_on_target {
        ServiceLifecycleDecision::WouldDisruptCaller
    } else if action.contends_with_update() && update_pending {
        ServiceLifecycleDecision::UpdateInProgress
    } else {
        ServiceLifecycleDecision::Admitted
    }
}

/// A refused lifecycle action. `home` is the target runtime home as the
/// caller named it, so the next call can be shown whole.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceLifecycleRefusal {
    #[error("service {} would end the runtime executing your turn; submit `gents runtime service update --home {home}` and finish your turn after acceptance, then read `gents runtime service update-status` after reconnect. Use external administration for maintenance an update does not cover", .action.as_str())]
    WouldDisruptCaller {
        action: ServiceLifecycleAction,
        home: String,
    },
    #[error("managed runtime update {operation_id} is in progress and owns the service definition, so service {} waits; run `gents runtime service update-status --home {home} {operation_id}` to read it and resume its supervisor", .action.as_str())]
    UpdateInProgress {
        action: ServiceLifecycleAction,
        home: String,
        operation_id: String,
    },
}

impl ServiceLifecycleRefusal {
    pub fn decision(&self) -> ServiceLifecycleDecision {
        match self {
            Self::WouldDisruptCaller { .. } => ServiceLifecycleDecision::WouldDisruptCaller,
            Self::UpdateInProgress { .. } => ServiceLifecycleDecision::UpdateInProgress,
        }
    }
}
