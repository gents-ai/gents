use gents::{ProcessLifecycleObserver, ProcessLifecycleState};
use std::sync::Mutex;
use tokio::sync::watch;

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

    pub async fn wait_for(&self, expected: ProcessLifecycleState) {
        let mut process_state_rx = self.process_state_tx.subscribe();
        loop {
            if self.states().contains(&expected) {
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
