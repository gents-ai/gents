use std::time::Duration;

#[doc(hidden)]
pub trait StartupBuildFailureObserver: Send + Sync {
    fn on_build_failure(&self, agent_id: &str, failure_number: u32, budget: u32, error: &str);
}

#[derive(Debug, Clone)]
/// Host startup policy for waiting on agent builds. Embedders may set these
/// values without changing an agent's inference or tool execution budget.
pub struct StartupReadinessOptions {
    pub build_failure_budget: u32,
    pub build_timeout: Duration,
}

impl Default for StartupReadinessOptions {
    fn default() -> Self {
        Self {
            build_failure_budget: 3,
            build_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOutcome {
    Started,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStanding {
    Pending { failures: u32 },
    Ready,
    Demoted,
    Superseded,
}

impl BuildStanding {
    pub fn seeded() -> Self {
        Self::Pending { failures: 0 }
    }

    pub fn released(self) -> bool {
        !matches!(self, Self::Pending { .. })
    }

    pub fn retire(self) -> Self {
        match self {
            Self::Pending { .. } => Self::Superseded,
            standing => standing,
        }
    }

    pub fn step(self, budget: u32, outcome: BuildOutcome) -> Self {
        match (self, outcome) {
            (Self::Pending { .. }, BuildOutcome::Started) => Self::Ready,
            (Self::Pending { failures }, BuildOutcome::Failed) => {
                if failures + 1 < budget {
                    Self::Pending {
                        failures: failures + 1,
                    }
                } else {
                    Self::Demoted
                }
            }
            (standing, _) => standing,
        }
    }
}
