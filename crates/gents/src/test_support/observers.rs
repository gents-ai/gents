use std::sync::Mutex;

use gents_automation::triggers::snapshot_observer::RuntimeSnapshotObserver;
use tokio::sync::watch;

use crate::runtime::agent::{ProcessLifecycleObserver, ProcessLifecycleState};
use crate::runtime::startup_readiness::StartupBuildFailureObserver;

pub struct RecordingProcessObserver {
    states: Mutex<Vec<ProcessLifecycleState>>,
    process_state_tx: watch::Sender<ProcessLifecycleState>,
}

impl Default for RecordingProcessObserver {
    fn default() -> Self {
        let (process_state_tx, _) = watch::channel(ProcessLifecycleState::Uninitialized);
        Self {
            states: Mutex::new(Vec::new()),
            process_state_tx,
        }
    }
}

impl RecordingProcessObserver {
    pub fn states(&self) -> Vec<ProcessLifecycleState> {
        self.states
            .lock()
            .expect("recording observer mutex poisoned")
            .clone()
    }

    pub fn contains(&self, expected: ProcessLifecycleState) -> bool {
        self.states
            .lock()
            .expect("recording observer mutex poisoned")
            .contains(&expected)
    }

    pub async fn wait_for(&self, expected: ProcessLifecycleState) {
        let mut process_state_rx = self.process_state_tx.subscribe();
        loop {
            if self.contains(expected) {
                return;
            }
            process_state_rx.changed().await.unwrap_or_else(|error| {
                panic!(
                    "process lifecycle observer closed while waiting for {expected:?}; \
                     observed states: {:?}: {error}",
                    self.states()
                )
            });
        }
    }
}

impl ProcessLifecycleObserver for RecordingProcessObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        self.states
            .lock()
            .expect("recording observer mutex poisoned")
            .push(state);
        self.process_state_tx.send_replace(state);
    }
}

pub struct RuntimeEventObserver {
    process_state_tx: watch::Sender<ProcessLifecycleState>,
    generation_tx: watch::Sender<u64>,
    event_sources_tx: watch::Sender<Option<(u64, String)>>,
}

impl RuntimeEventObserver {
    pub fn new(
        process_state_tx: watch::Sender<ProcessLifecycleState>,
        generation_tx: watch::Sender<u64>,
        event_sources_tx: watch::Sender<Option<(u64, String)>>,
    ) -> Self {
        Self {
            process_state_tx,
            generation_tx,
            event_sources_tx,
        }
    }
}

impl ProcessLifecycleObserver for RuntimeEventObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        self.process_state_tx.send_replace(state);
    }
}

impl RuntimeSnapshotObserver for RuntimeEventObserver {
    fn on_event_sources_reconciled(
        &self,
        generation: u64,
        fingerprint: &str,
        result: Result<(), &str>,
    ) {
        assert!(result.is_ok(), "subscription seeding failed: {result:?}");
        self.event_sources_tx
            .send_replace(Some((generation, fingerprint.to_owned())));
    }

    fn on_generation_published(
        &self,
        generation: u64,
        _configuration_fingerprint: &str,
        _runnable_agent_dids: &[String],
    ) {
        self.generation_tx.send_replace(generation);
    }
}

#[derive(Clone, Debug)]
pub struct BuildFailure {
    pub agent_did: String,
    pub failure_number: u32,
    pub budget: u32,
    pub error: String,
}

pub struct RecordingBuildFailureObserver {
    failures: Mutex<Vec<BuildFailure>>,
    failure_count_tx: watch::Sender<usize>,
}

impl Default for RecordingBuildFailureObserver {
    fn default() -> Self {
        let (failure_count_tx, _) = watch::channel(0);
        Self {
            failures: Mutex::new(Vec::new()),
            failure_count_tx,
        }
    }
}

impl RecordingBuildFailureObserver {
    pub fn failures(&self) -> Vec<BuildFailure> {
        self.failures
            .lock()
            .expect("build failure observer mutex poisoned")
            .clone()
    }

    pub async fn wait_for_failure_count(&self, expected: usize) {
        let mut failure_count_rx = self.failure_count_tx.subscribe();
        loop {
            if self
                .failures
                .lock()
                .expect("build failure observer mutex poisoned")
                .len()
                >= expected
            {
                return;
            }
            failure_count_rx
                .changed()
                .await
                .expect("build failure observer closed");
        }
    }
}

impl StartupBuildFailureObserver for RecordingBuildFailureObserver {
    fn on_build_failure(&self, agent_did: &str, failure_number: u32, budget: u32, error: &str) {
        let count = {
            let mut failures = self
                .failures
                .lock()
                .expect("build failure observer mutex poisoned");
            failures.push(BuildFailure {
                agent_did: agent_did.to_string(),
                failure_number,
                budget,
                error: error.to_string(),
            });
            failures.len()
        };
        self.failure_count_tx.send_replace(count);
    }
}
