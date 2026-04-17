use crate::telemetry::DesktopLogCategory;

#[derive(Debug, Clone)]
pub struct IdentityState {
    pub initials: &'static str,
    pub label: String,
    pub did_short: String,
}

#[derive(Debug, Clone)]
pub struct StatusBarState {
    pub peered_now: usize,
    pub peered_target: usize,
    pub p2p_state: String,
    pub p2p_warning: bool,
    pub active_agent: String,
    pub runtime_state: String,
    pub gossip_lag_ms: u32,
    pub replication_state: String,
    pub error_count: usize,
    pub frame_counter: u64,
    pub did_short: String,
    pub build_label: String,
}

impl StatusBarState {
    pub fn advance_frame(&mut self) {
        self.frame_counter = self.frame_counter.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogsFilter {
    #[default]
    All,
    Category(DesktopLogCategory),
}

impl LogsFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Category(category) => category.label(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LogsState {
    pub filter: LogsFilter,
}

#[derive(Debug, Clone, Default)]
pub struct OnboardingState {
    pub first_launch_redirect_done: bool,
}
