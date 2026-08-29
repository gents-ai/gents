//! Startup readiness under bounded build failures (#559).
//!
//! The startup barrier seeds every snapshot-runnable behavior and the process
//! reports `Ready` only when none is pending. A behavior used to leave pending
//! in exactly one way — its daemon started — so a behavior that was runnable at
//! snapshot time but persistently failed to *build* its completion client
//! wedged the barrier forever: the slot hot-restarted, `wait_ready()` never
//! returned, the process never reported `Ready`, and (because the trigger
//! engine is gated on the same barrier) every schedule and event trigger was
//! silently disabled.
//!
//! Build attempts now consume a budget. Exhausting it **demotes** the behavior:
//! released from the barrier, never claimed healthy, and durably recorded by
//! the runtime behavior-readiness publisher used by admission and clients. The model is
//! `proofs/Proofs/RuntimeReconcile/StartupReadiness.lean`; the emitted case
//! table is fenced by
//! `conformance::generated_startup_readiness_cases_pin_bounded_barrier_release`.

use std::time::Duration;

/// Observes failed startup behavior-build attempts.
///
/// This is diagnostic plumbing for embedders and deterministic test fixtures;
/// it does not participate in the build-failure verdict.
#[doc(hidden)]
pub trait StartupBuildFailureObserver: Send + Sync {
    fn on_build_failure(&self, behavior_id: &str, failure_number: u32, budget: u32, error: &str);
}

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
