use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Barrier, Notify, Semaphore};

use super::*;
use crate::runtime::agent::ProcessLifecycleObserver;

#[derive(Default)]
struct RecordingObserver {
    states: StdMutex<Vec<ProcessLifecycleState>>,
}

impl ProcessLifecycleObserver for RecordingObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        self.states.lock().expect("observer mutex").push(state);
    }
}

struct FailFirstReadyWriter {
    calls: AtomicUsize,
    ready_attempted: Barrier,
    release: Semaphore,
}

#[async_trait::async_trait]
impl crate::runtime::node_readiness_publisher::NodeReadinessWriter for FailFirstReadyWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        _snapshot: &gents_protocol::node_readiness::NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            self.ready_attempted.wait().await;
            self.release.acquire().await.unwrap().forget();
            anyhow::bail!("injected first Ready persistence failure");
        }
        Ok(())
    }
}

#[tokio::test]
async fn admission_gate_waits_for_durable_ready_retry_ack() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let writer = Arc::new(FailFirstReadyWriter {
        calls: AtomicUsize::new(0),
        ready_attempted: Barrier::new(2),
        release: Semaphore::new(0),
    });
    let (status_owner, status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        "did:test:ready-gate-persistence",
        writer.clone(),
        std::time::Duration::from_millis(5),
    );
    status.initialize_startup("general").await.unwrap();
    let gate = super::super::router::RuntimeAdmissionGate::closed();
    let coordinator = Arc::new(RuntimeLifecycleCoordinator::new(gate.clone(), status, None));
    let becoming_ready = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move { coordinator.become_ready().await })
    };

    writer.ready_attempted.wait().await;
    assert!(
        !gate.is_open().await,
        "Ready write failure opened admission"
    );
    assert!(
        !becoming_ready.is_finished(),
        "Ready lifecycle acknowledged before persistence retry"
    );
    writer.release.add_permits(1);
    assert!(becoming_ready.await.unwrap().unwrap());
    assert!(gate.is_open().await, "durable Ready did not open admission");

    coordinator.begin_shutdown().await.unwrap();
    coordinator.finish_shutdown().await.unwrap();
    status_owner.close().await.unwrap();
    node.shutdown().await;
}

#[tokio::test]
async fn shutdown_wins_over_a_late_startup_barrier_release() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let status = RuntimeStatusHandle::new(node.clone(), "did:test:lifecycle-race");
    status.initialize_startup("general").await.unwrap();
    let gate = super::super::router::RuntimeAdmissionGate::closed();
    let observer = Arc::new(RecordingObserver::default());
    let coordinator = Arc::new(RuntimeLifecycleCoordinator::new(
        gate.clone(),
        status,
        Some(observer.clone()),
    ));
    let waiting = Arc::new(Barrier::new(2));
    let release = Arc::new(Notify::new());

    let ready_task = {
        let coordinator = coordinator.clone();
        let waiting = waiting.clone();
        let release = release.clone();
        tokio::spawn(async move {
            waiting.wait().await;
            release.notified().await;
            coordinator.become_ready().await
        })
    };

    waiting.wait().await;
    coordinator.begin_shutdown().await.unwrap();
    release.notify_one();

    assert!(
        !ready_task.await.unwrap().unwrap(),
        "closing lifecycle reopened Ready"
    );
    assert!(
        !gate.is_open().await,
        "closing lifecycle reopened admission"
    );
    assert_eq!(
        *observer.states.lock().expect("observer mutex"),
        vec![ProcessLifecycleState::ShuttingDown],
        "Ready must never become observable after shutdown begins"
    );
    node.shutdown().await;
}
