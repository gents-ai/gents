use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

use super::STALL_CEILING;
use crate::config_client::ConfigApplyTxn;

#[derive(Default)]
pub struct Checkpoint {
    reached: Arc<Notify>,
    release: Arc<Notify>,
}

impl Checkpoint {
    pub async fn hold(&self) {
        self.reached.notify_one();
        self.release.notified().await;
    }

    pub async fn reached(&self, name: &str) {
        tokio::time::timeout(STALL_CEILING, self.reached.notified())
            .await
            .unwrap_or_else(|_| panic!("actor stalled before {name}"));
    }

    pub fn release(&self) {
        self.release.notify_one();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommitReceipt {
    Delivered,
    LostAfterCommit,
    TimeoutBeforeCommit,
}

#[derive(Default)]
pub struct EmbeddedWriteActor {
    pub begun: Checkpoint,
    pub mutated: Checkpoint,
    pub committing: Checkpoint,
    pub discarded: Checkpoint,
    queued: Arc<Notify>,
    acquired: Arc<AtomicBool>,
    calls: AtomicUsize,
}

impl EmbeddedWriteActor {
    pub async fn queued(&self) {
        tokio::time::timeout(STALL_CEILING, self.queued.notified())
            .await
            .expect("writer queued at the embedded gate");
        assert!(!self.acquired.load(Ordering::Acquire));
    }

    pub fn attempts(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }

    pub async fn callback<F>(&self, body: F) -> F::Output
    where
        F: Future,
    {
        if self.calls.fetch_add(1, Ordering::AcqRel) != 0 {
            return body.await;
        }
        assert!(self.acquired.load(Ordering::Acquire));
        self.begun.hold().await;
        ConfigApplyTxn::with_each_successful_mutation_pause(
            self.mutated.reached.clone(),
            self.mutated.release.clone(),
            body,
        )
        .await
        .0
    }

    pub async fn run<F>(&self, receipt: CommitReceipt, operation: F) -> F::Output
    where
        F: Future,
    {
        let finalization = async {
            if receipt == CommitReceipt::TimeoutBeforeCommit {
                return ConfigApplyTxn::with_unlanded_commit_timeout(
                    self.committing.reached.clone(),
                    self.discarded.reached.clone(),
                    operation,
                )
                .await;
            }
            let commit = ConfigApplyTxn::with_embedded_before_commit_pause(
                self.committing.reached.clone(),
                self.committing.release.clone(),
                operation,
            );
            if receipt == CommitReceipt::LostAfterCommit {
                let (result, fired) = ConfigApplyTxn::with_post_commit_receipt_loss(commit).await;
                assert!(fired, "commit reached receipt delivery");
                return result;
            }
            commit.await
        };
        ConfigApplyTxn::with_write_gate_observation(
            self.queued.clone(),
            self.acquired.clone(),
            finalization,
        )
        .await
    }
}
