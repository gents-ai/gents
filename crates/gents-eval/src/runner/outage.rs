use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::runner::{RunOptions, TrialExecutor, TrialSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutageStep {
    Admit,
    Wait,
    Stop,
}

pub(crate) fn outage_step(provider_answers: bool, waited_secs: u64, limit_secs: u64) -> OutageStep {
    if provider_answers {
        OutageStep::Admit
    } else if waited_secs < limit_secs {
        OutageStep::Wait
    } else {
        OutageStep::Stop
    }
}

pub(crate) fn breaker_count(consecutive: u32, retryable: bool, provider_answers: bool) -> u32 {
    if !retryable {
        0
    } else if provider_answers {
        consecutive.saturating_add(1)
    } else {
        consecutive
    }
}

pub(super) enum Admission {
    Admitted,
    Stopped,
    ProviderDown,
}

pub(super) struct ProviderGate {
    suspect: AtomicBool,
    down: AtomicBool,
    turn: tokio::sync::Mutex<()>,
    limit: Duration,
    gap: Duration,
}

impl ProviderGate {
    pub(super) fn new(options: &RunOptions) -> Self {
        Self {
            suspect: AtomicBool::new(false),
            down: AtomicBool::new(false),
            turn: tokio::sync::Mutex::new(()),
            limit: options.provider_wait_limit,
            gap: options.provider_probe_gap,
        }
    }

    pub(super) fn suspect(&self) {
        self.suspect.store(true, Ordering::Relaxed);
    }

    pub(super) async fn admit(
        &self,
        run_id: &str,
        executor: &dyn TrialExecutor,
        spec: &TrialSpec,
        stopped: &CancellationToken,
    ) -> Admission {
        if !self.suspect.load(Ordering::Relaxed) {
            return Admission::Admitted;
        }
        let _turn = tokio::select! {
            biased;
            () = stopped.cancelled() => return Admission::Stopped,
            turn = self.turn.lock() => turn,
        };
        if self.down.load(Ordering::Relaxed) {
            return Admission::ProviderDown;
        }
        if !self.suspect.load(Ordering::Relaxed) {
            return Admission::Admitted;
        }
        let started = Instant::now();
        loop {
            let answers = tokio::select! {
                biased;
                () = stopped.cancelled() => return Admission::Stopped,
                answers = executor.provider_answers(spec) => answers,
            };
            let waited_secs = started.elapsed().as_secs();
            let limit_secs = self.limit.as_secs();
            match outage_step(answers, waited_secs, limit_secs) {
                OutageStep::Admit => {
                    self.suspect.store(false, Ordering::Relaxed);
                    tracing::info!(
                        run_id,
                        waited_secs,
                        "the provider answers again; the eval run continues"
                    );
                    return Admission::Admitted;
                }
                OutageStep::Wait => {
                    tracing::warn!(
                        run_id,
                        waited_secs,
                        limit_secs,
                        "the provider does not answer; the eval run starts no trial until it does"
                    );
                    tokio::select! {
                        biased;
                        () = stopped.cancelled() => return Admission::Stopped,
                        () = tokio::time::sleep(self.gap) => {}
                    }
                }
                OutageStep::Stop => {
                    self.down.store(true, Ordering::Relaxed);
                    return Admission::ProviderDown;
                }
            }
        }
    }
}
