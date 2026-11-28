use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use tokio::sync::Notify;

use super::STALL_CEILING;
use crate::config_client::ConfigApplyTxn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionCheckpoint {
    Begin(&'static str),
    Commit(&'static str),
}

#[derive(Default)]
struct Signals {
    reached: Arc<Notify>,
    release: Arc<Notify>,
    operation: Mutex<Option<TransactionCheckpoint>>,
}

tokio::task_local! {
    static ATTEMPTS: (&'static [&'static str], Arc<Signals>);
}

pub(crate) async fn observe(checkpoint: TransactionCheckpoint) {
    let name = match checkpoint {
        TransactionCheckpoint::Begin(name) | TransactionCheckpoint::Commit(name) => name,
    };
    let Ok(Some(signals)) =
        ATTEMPTS.try_with(|(names, signals)| names.contains(&name).then(|| signals.clone()))
    else {
        return;
    };
    *signals.operation.lock().unwrap() = Some(checkpoint);
    signals.reached.notify_one();
    signals.release.notified().await;
}

pub struct PausedTransaction<'a, T> {
    operation: Pin<Box<dyn Future<Output = T> + 'a>>,
    signals: Arc<Signals>,
    held: bool,
    settled: Option<T>,
}

impl<'a, T> PausedTransaction<'a, T> {
    pub fn before_commit<F>(future: F) -> Self
    where
        F: Future<Output = T> + 'a,
    {
        let signals = Arc::new(Signals::default());
        Self {
            operation: Box::pin(ConfigApplyTxn::with_embedded_before_commit_pause(
                signals.reached.clone(),
                signals.release.clone(),
                future,
            )),
            signals,
            held: false,
            settled: None,
        }
    }

    pub fn each_mutation<F>(future: F) -> Self
    where
        F: Future<Output = T> + 'a,
    {
        let signals = Arc::new(Signals::default());
        let observing = ConfigApplyTxn::with_each_successful_mutation_pause(
            signals.reached.clone(),
            signals.release.clone(),
            future,
        );
        Self {
            operation: Box::pin(async move { observing.await.0 }),
            signals,
            held: false,
            settled: None,
        }
    }

    pub fn named_attempts<F>(names: &'static [&'static str], future: F) -> Self
    where
        F: Future<Output = T> + 'a,
    {
        let signals = Arc::new(Signals::default());
        Self {
            operation: Box::pin(ATTEMPTS.scope((names, signals.clone()), future)),
            signals,
            held: false,
            settled: None,
        }
    }

    pub fn checkpoint(&self) -> Option<TransactionCheckpoint> {
        *self.signals.operation.lock().unwrap()
    }

    pub async fn next_checkpoint(&mut self) -> Option<TransactionCheckpoint> {
        self.advance().await;
        self.checkpoint()
    }

    pub async fn advance(&mut self) -> bool {
        if self.settled.is_some() {
            return false;
        }
        if self.held {
            *self.signals.operation.lock().unwrap() = None;
            self.signals.release.notify_one();
            self.held = false;
        }
        tokio::time::timeout(STALL_CEILING, async {
            tokio::select! {
                () = self.signals.reached.notified() => self.held = true,
                result = &mut self.operation => self.settled = Some(result),
            }
        })
        .await
        .expect("transaction stalled between checkpoints");
        self.held
    }

    pub async fn finish(mut self) -> T {
        while self.advance().await {}
        self.settled.unwrap()
    }
}
