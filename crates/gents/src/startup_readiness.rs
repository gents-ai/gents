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
//! released from the barrier, never claimed healthy, its build error recorded
//! in the [`StartupDemotions`] ledger where the router, runtime status, and
//! `/healthz` can see it. The model is
//! `proofs/Proofs/RuntimeReconcile/StartupReadiness.lean`; the emitted case
//! table is fenced by
//! `conformance::generated_startup_readiness_cases_pin_bounded_barrier_release`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Knobs for startup readiness (#559). The defaults are deliberate:
///
/// * `build_failure_budget = 3` — a genuinely transient build error (DNS blip,
///   racing config write) gets real retries, while a deterministic failure
///   demotes within seconds instead of wedging `Ready` forever.
/// * `build_timeout = 60s` — a build that *hangs* (the ChatGptCodex client
///   build does DB and network work) would produce no outcome and escape the
///   budget entirely; the per-attempt timeout converts a hang into a `failed`
///   outcome, so the model's termination theorem covers hangs too and `Ready`
///   never has to be force-flipped by a deadline that would weaken its claim.
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

/// One build attempt's outcome, as the slot loop observes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOutcome {
    /// The daemon began running: the behavior is serving.
    Started,
    /// The build returned an error before the daemon started.
    Failed,
}

/// A behavior's standing with the startup barrier.
///
/// Mirrors `RuntimeReconcile.StartupReadiness.BehaviorStanding`. `Ready` and
/// `Demoted` are absorbing: post-start crashes restart the daemon but never
/// re-enter the barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStanding {
    /// Seeded runnable, not yet started; counts consecutive build failures.
    Pending { failures: u32 },
    /// Started successfully. The only standing that claims health.
    Ready,
    /// Released from the barrier after exhausting the build budget — accounted
    /// for, observable, and never claimed healthy.
    Demoted,
    /// Released because reconcile retired the slot before it started (config
    /// change or removal). Released, unclaimed, accounted.
    Superseded,
}

impl BuildStanding {
    /// The fresh standing every snapshot-runnable behavior is seeded with.
    pub fn seeded() -> Self {
        Self::Pending { failures: 0 }
    }

    /// The barrier no longer waits on this behavior.
    pub fn released(self) -> bool {
        !matches!(self, Self::Pending { .. })
    }

    /// Mirrors `RuntimeReconcile.StartupReadiness.retire`: reconcile retires
    /// the slot; a still-pending behavior is superseded, a settled verdict is
    /// kept. Retirement always releases and never claims health — the second
    /// #559 hang path (a slot retired mid-startup orphaned its pending entry).
    pub fn retire(self) -> Self {
        match self {
            Self::Pending { .. } => Self::Superseded,
            standing => standing,
        }
    }

    /// Mirrors `RuntimeReconcile.StartupReadiness.step`: how the standing
    /// responds to one build outcome under `budget` tolerated failures.
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

/// Startup build-failure demotions, keyed by behavior id.
///
/// Shared between the slot failure policy (writer), the request router (which
/// fails requests to demoted behaviors loudly instead of queueing them into a
/// parked slot), and runtime status (which folds demotions into the
/// runnable/unavailable counts so `/healthz` degrades instead of reading green).
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

    /// A changed behavior gets a fresh slot and a fresh budget
    /// (`RuntimeReconcile.StartupReadiness.change_restores_the_budget`), so its
    /// demotion is cleared when reconcile recreates the slot.
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
