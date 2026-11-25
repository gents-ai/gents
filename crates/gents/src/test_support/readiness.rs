use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use gents_protocol::node_readiness::{
    AgentReadinessUnavailableReason, NodeReadinessProcessState, NodeReadinessSnapshot,
};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify, Semaphore};

use crate::runtime::node_readiness_publisher::{FatalNodeReadinessWrite, NodeReadinessWriter};

pub(crate) struct AcceptingReadinessWriter;

#[async_trait::async_trait]
impl NodeReadinessWriter for AcceptingReadinessWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        _snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct RejectRouterGenerationWriter;

#[async_trait::async_trait]
impl NodeReadinessWriter for RejectRouterGenerationWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        if snapshot.router_generation > 0 {
            return Err(FatalNodeReadinessWrite.into());
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct CapturingReadinessWriter {
    snapshots: Mutex<Vec<NodeReadinessSnapshot>>,
}

impl CapturingReadinessWriter {
    pub(crate) fn snapshots(&self) -> Vec<NodeReadinessSnapshot> {
        self.snapshots.lock().unwrap().clone()
    }

    pub(crate) fn process_states(&self) -> Vec<NodeReadinessProcessState> {
        self.snapshots
            .lock()
            .unwrap()
            .iter()
            .map(|snapshot| snapshot.process_state)
            .collect()
    }
}

#[async_trait::async_trait]
impl NodeReadinessWriter for CapturingReadinessWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        self.snapshots.lock().unwrap().push(snapshot.clone());
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("native transaction")]
struct NativeTransactionFailure(#[source] storage::corekv::Error);

fn latched_write_ahead_log_error() -> anyhow::Error {
    anyhow::Error::new(NativeTransactionFailure(storage::corekv::Error::from(
        std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "write-ahead log left in an unknown state: No space left on device (os error 28)",
        ),
    )))
}

pub(crate) struct StorageFullReadinessWriter;

#[async_trait::async_trait]
impl NodeReadinessWriter for StorageFullReadinessWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        _snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        Err(latched_write_ahead_log_error())
    }
}

#[derive(Default)]
pub(crate) struct StorageFullAfterReadyWriter {
    pub(crate) ready: Notify,
    pub(crate) ready_seen: AtomicBool,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for StorageFullAfterReadyWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        if self.ready_seen.load(Ordering::SeqCst)
            && matches!(
                snapshot.process_state,
                NodeReadinessProcessState::ShuttingDown | NodeReadinessProcessState::Shutdown
            )
        {
            return Err(latched_write_ahead_log_error());
        }
        if snapshot.process_state == NodeReadinessProcessState::Ready {
            self.ready_seen.store(true, Ordering::SeqCst);
            self.ready.notify_waiters();
        }
        Ok(())
    }
}

pub(crate) struct GatedGenerationSourceWriter {
    generation: u64,
    attempted: Arc<Notify>,
    release: Arc<Semaphore>,
    fail_with_fatal: bool,
}

impl GatedGenerationSourceWriter {
    pub(crate) fn new(
        generation: u64,
        attempted: Arc<Notify>,
        release: Arc<Semaphore>,
        fail_with_fatal: bool,
    ) -> Self {
        Self {
            generation,
            attempted,
            release,
            fail_with_fatal,
        }
    }
}

#[async_trait::async_trait]
impl NodeReadinessWriter for GatedGenerationSourceWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        if snapshot.active_generation == self.generation {
            self.attempted.notify_one();
            self.release.acquire().await.unwrap().forget();
            if self.fail_with_fatal {
                return Err(FatalNodeReadinessWrite.into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum WritePlan {
    Success,
}

pub(crate) struct ControlledReadinessWriter {
    pub(crate) plans: Mutex<VecDeque<WritePlan>>,
    pub(crate) attempts: mpsc::UnboundedSender<NodeReadinessProcessState>,
    pub(crate) persisted: AsyncMutex<Vec<NodeReadinessProcessState>>,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for ControlledReadinessWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        let plan = self
            .plans
            .lock()
            .expect("writer plans mutex")
            .pop_front()
            .expect("test supplied a write plan");
        self.attempts
            .send(snapshot.process_state)
            .expect("attempt observer remains alive");
        match plan {
            WritePlan::Success => {}
        }
        self.persisted.lock().await.push(snapshot.process_state);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct CandidateCommitWriter {
    pub(crate) results: Mutex<VecDeque<String>>,
    pub(crate) persisted: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for CandidateCommitWriter {
    async fn upsert(&self, _: &str, snapshot: &NodeReadinessSnapshot, _: &str) -> Result<()> {
        let result = self
            .results
            .lock()
            .unwrap()
            .pop_front()
            .expect("modeled write outcome");
        match result.as_str() {
            "ok" => {
                self.persisted.lock().unwrap().push(
                    serde_json::to_value(snapshot.process_state)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .into(),
                );
                Ok(())
            }
            "full" => Err(gents_protocol::database_error::StorageFull.into()),
            "timeout" => std::future::pending().await,
            "fail" => anyhow::bail!("injected write failure"),
            other => panic!("unmodeled write result {other}"),
        }
    }
}

#[derive(Default)]
pub(crate) struct ScriptedStartupWriter {
    pub(crate) source: Mutex<VecDeque<String>>,
    pub(crate) ready: Mutex<VecDeque<String>>,
    pub(crate) source_attempts: AtomicU64,
    pub(crate) ready_attempts: AtomicU64,
    pub(crate) persisted: Mutex<Vec<NodeReadinessSnapshot>>,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for ScriptedStartupWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        let (script, attempts) = match snapshot.process_state {
            NodeReadinessProcessState::Uninitialized | NodeReadinessProcessState::Recovering
                if snapshot.active_generation > 0 =>
            {
                (&self.source, &self.source_attempts)
            }
            NodeReadinessProcessState::Ready => (&self.ready, &self.ready_attempts),
            _ => {
                self.persisted
                    .lock()
                    .expect("scripted writer mutex poisoned")
                    .push(snapshot.clone());
                return Ok(());
            }
        };
        attempts.fetch_add(1, Ordering::SeqCst);
        let result = script
            .lock()
            .expect("scripted writer mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| "ok".to_owned());
        match result.as_str() {
            "ok" => {
                self.persisted
                    .lock()
                    .expect("scripted writer mutex poisoned")
                    .push(snapshot.clone());
                Ok(())
            }
            "fail" => anyhow::bail!("injected startup persistence failure"),
            other => panic!("unmodeled write result {other}"),
        }
    }
}

pub(crate) struct PendingReadyWriter {
    pub(crate) retry_first: AtomicBool,
    pub(crate) entered: Notify,
    pub(crate) release: Semaphore,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for PendingReadyWriter {
    async fn upsert(&self, _: &str, snapshot: &NodeReadinessSnapshot, _: &str) -> Result<()> {
        if snapshot.process_state == NodeReadinessProcessState::Ready {
            if self.retry_first.swap(false, Ordering::SeqCst) {
                anyhow::bail!("injected readiness retry");
            }
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        Ok(())
    }
}

pub(crate) struct DemotionReadinessWriter {
    pub(crate) durable: bool,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for DemotionReadinessWriter {
    async fn upsert(&self, _: &str, snapshot: &NodeReadinessSnapshot, _: &str) -> Result<()> {
        if !self.durable
            && snapshot.agents.iter().any(|entry| {
                entry.reason == Some(AgentReadinessUnavailableReason::ExecutorStartFailed)
            })
        {
            return Err(FatalNodeReadinessWrite.into());
        }
        Ok(())
    }
}
