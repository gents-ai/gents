use super::*;
use crate::runtime::agent::ProcessLifecycleObserver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Notify, Semaphore};

#[derive(Default)]
struct RecordingObserver {
    states: StdMutex<Vec<ProcessLifecycleState>>,
}
impl ProcessLifecycleObserver for RecordingObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        self.states.lock().unwrap().push(state);
    }
}
struct PendingReadyWriter {
    retry_first: AtomicBool,
    entered: Notify,
    release: Semaphore,
}
#[async_trait::async_trait]
impl crate::runtime::node_readiness_publisher::NodeReadinessWriter for PendingReadyWriter {
    async fn upsert(
        &self,
        _: &str,
        snapshot: &gents_protocol::node_readiness::NodeReadinessSnapshot,
        _: &str,
    ) -> Result<()> {
        if snapshot.process_state
            == gents_protocol::node_readiness::NodeReadinessProcessState::Ready
        {
            if self.retry_first.swap(false, Ordering::SeqCst) {
                anyhow::bail!("injected readiness retry");
            }
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        Ok(())
    }
}

#[tokio::test]
async fn generated_startup_admission_opens_only_after_a_durable_ready() {
    use gents_lean_vocab::families::runtime_startup_admission_become_ready as family;
    for case in family::cases() {
        let db = gents_inference::test_support::test_db(
            "startup-ready-family",
            gents_automation::lifecycle_hooks::composition(),
        )
        .await;
        let writer = Arc::new(PendingReadyWriter {
            retry_first: AtomicBool::new(case.input.retry_first),
            entered: Notify::new(),
            release: Semaphore::new(0),
        });
        let (status_owner, status) = RuntimeStatusHandle::start_with_readiness_writer(
            db.node.clone(),
            "did:test:startup-ready",
            writer.clone(),
            std::time::Duration::from_millis(1),
            crate::runtime::storage_fault::StorageFaultObservation::new(),
        );
        status.initialize_startup("general").await.unwrap();
        let gate = super::super::router::RuntimeAdmissionGate::closed();
        let observer = Arc::new(RecordingObserver::default());
        let coordinator = Arc::new(RuntimeLifecycleCoordinator::new(
            gate.clone(),
            status.clone(),
            Some(observer.clone()),
        ));
        if case.input.closing {
            coordinator.begin_shutdown().await.unwrap();
        }
        let task = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move { coordinator.become_ready().await })
        };
        let (pending_before_write, gate_before_write) = if case.input.closing {
            (false, gate.is_open().await)
        } else {
            writer.entered.notified().await;
            let pending = !task.is_finished();
            let gate = gate.is_open().await;
            writer.release.add_permits(1);
            (pending, gate)
        };
        let applied = task.await.unwrap().unwrap();
        case.check(family::ReadyObservation {
            pending_before_write,
            gate_before_write,
            applied,
            gate_after_write: gate.is_open().await,
            process: status
                .readiness()
                .observation()
                .process_state()
                .as_str()
                .into(),
            notifications: observer
                .states
                .lock()
                .unwrap()
                .iter()
                .map(|state| state.as_str().to_owned())
                .collect(),
        });
        coordinator.begin_shutdown().await.unwrap();
        coordinator.finish_shutdown().await.unwrap();
        status_owner.close().await.unwrap();
        db.node.shutdown().await.unwrap();
    }
}
