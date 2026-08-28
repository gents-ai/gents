use std::sync::Mutex;
use std::time::Duration;

use gents::{ProcessLifecycleObserver, ProcessLifecycleState};
use tokio::sync::watch;

const PROCESS_STATE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

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
        let wait = async {
            loop {
                if *process_state_rx.borrow_and_update() == expected {
                    return Ok::<(), watch::error::RecvError>(());
                }
                process_state_rx.changed().await?;
            }
        };
        match tokio::time::timeout(PROCESS_STATE_WAIT_TIMEOUT, wait).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                panic!("process lifecycle observer closed while waiting for {expected:?}: {error}")
            }
            Err(error) => panic!(
                "timed out after {PROCESS_STATE_WAIT_TIMEOUT:?} waiting for observed process state \
                 {expected:?}; observed states: {:?}: {error}",
                self.states()
            ),
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
