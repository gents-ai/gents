use std::future::{poll_fn, Future};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use anyhow::Result;
use tokio::sync::Notify;
use tokio::task::{JoinError, JoinSet};

#[derive(Debug, thiserror::Error)]
pub(super) enum AuxiliaryTasksError {
    #[error("auxiliary execution is shutting down")]
    ShuttingDown,
}

#[derive(Default)]
struct Tasks {
    jobs: JoinSet<Result<()>>,
    closed: bool,
}

#[derive(Default)]
struct State {
    tasks: Mutex<Tasks>,
    changed: Notify,
}

/// A caller awaiting a tool can start audit work while its daemon is busy.
/// The daemon still owns every join and drains them before shutdown completes.
#[derive(Clone, Default)]
pub(super) struct AuxiliaryTasks(Arc<State>);

impl AuxiliaryTasks {
    pub(super) fn spawn<F>(&self, job: F) -> Result<(), AuxiliaryTasksError>
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        let mut tasks = self.0.tasks.lock().expect("auxiliary task lock poisoned");
        if tasks.closed {
            return Err(AuxiliaryTasksError::ShuttingDown);
        }
        tasks.jobs.spawn(job);
        self.0.changed.notify_one();
        Ok(())
    }

    pub(super) fn close(&self) {
        self.0
            .tasks
            .lock()
            .expect("auxiliary task lock poisoned")
            .closed = true;
        self.0.changed.notify_one();
    }

    pub(super) async fn join_next(&self) -> Option<std::result::Result<Result<()>, JoinError>> {
        loop {
            let changed = self.0.changed.notified();
            let (joined, closed) = poll_fn(|cx| {
                let mut tasks = self.0.tasks.lock().expect("auxiliary task lock poisoned");
                let closed = tasks.closed;
                if tasks.jobs.is_empty() {
                    Poll::Ready((None, closed))
                } else {
                    tasks.jobs.poll_join_next(cx).map(|joined| (joined, closed))
                }
            })
            .await;
            if joined.is_some() || closed {
                return joined;
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn an_active_join_does_not_block_another_callers_audit() {
        let tasks = AuxiliaryTasks::default();
        let pending = Arc::new(Notify::new());
        tasks
            .spawn({
                let pending = pending.clone();
                async move {
                    pending.notified().await;
                    Ok(())
                }
            })
            .unwrap();
        let joined = tokio::spawn({
            let tasks = tasks.clone();
            async move { tasks.join_next().await }
        });
        tokio::task::yield_now().await;
        tasks.spawn(async { Ok(()) }).unwrap();
        tokio::time::timeout(Duration::from_secs(5), joined)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap();
        tasks.close();
        assert!(tasks.spawn(async { Ok(()) }).is_err());
        pending.notify_one();
        tasks.join_next().await.unwrap().unwrap().unwrap();
        assert!(tasks.join_next().await.is_none());
    }
}
