//! Startup readiness under bounded build failures (#559).
//! `conformance::generated_startup_readiness_cases_pin_bounded_barrier_release`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone)]
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

#[derive(Debug, Default)]
pub struct StartupDemotions {
    inner: Mutex<HashMap<String, String>>,
}

impl StartupDemotions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, behavior_id: &str, reason: impl Into<String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.insert(behavior_id.to_string(), reason.into());
        }
    }

    pub fn reason(&self, behavior_id: &str) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.get(behavior_id).cloned())
    }

    pub fn clear(&self, behavior_id: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.remove(behavior_id);
        }
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.inner
            .lock()
            .map(|inner| inner.clone())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.is_empty())
            .unwrap_or(true)
    }
}
