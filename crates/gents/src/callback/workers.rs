use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use super::documents::{load_invocation, CallbackInvocationDoc};
use super::run::run_owned_invocation;
use super::CallbackEngine;

pub(super) const PLUGIN_CALLBACK_CONCURRENCY: usize = 32;

/// Process-local execution accounting is separate from the durable claim:
/// recovery must not interrupt an invocation whose attempt is still executing.
pub(crate) struct CallbackWorkers {
    active: Arc<Mutex<HashMap<String, String>>>,
    permits: Arc<Semaphore>,
    tasks: TaskTracker,
    cancel: CancellationToken,
}

struct ActiveInvocation {
    active: Arc<Mutex<HashMap<String, String>>>,
    id: String,
}

impl Drop for ActiveInvocation {
    fn drop(&mut self) {
        self.active
            .lock()
            .expect("callback workers poisoned")
            .remove(&self.id);
    }
}

impl CallbackWorkers {
    pub(super) fn new(limit: usize, cancel: CancellationToken) -> Self {
        Self {
            active: Arc::default(),
            permits: Arc::new(Semaphore::new(limit)),
            tasks: TaskTracker::new(),
            cancel,
        }
    }

    pub(super) fn contains(&self, id: &str) -> bool {
        self.active
            .lock()
            .expect("callback workers poisoned")
            .contains_key(id)
    }

    fn schedule(
        &self,
        id: String,
        callback: String,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> bool {
        if self.cancel.is_cancelled() || self.contains(&id) {
            return false;
        }
        // Capacity leaves the durable invocation pending; admission must keep
        // draining arrivals so another callback's ready stage can be delivered.
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            return false;
        };
        let mut active = self.active.lock().expect("callback workers poisoned");
        if active.contains_key(&id) {
            return false;
        }
        active.insert(id.clone(), callback);
        drop(active);
        let active = ActiveInvocation {
            active: self.active.clone(),
            id,
        };
        self.tasks.spawn(async move {
            let _active = active;
            let _permit = permit;
            task.await;
        });
        true
    }

    pub(super) fn prioritize(&self, invocations: &mut [CallbackInvocationDoc]) {
        let mut counts = HashMap::<String, usize>::new();
        for callback in self
            .active
            .lock()
            .expect("callback workers poisoned")
            .values()
        {
            *counts.entry(callback.clone()).or_default() += 1;
        }
        // Preserve arrival order within equal ranks; a saturated callback must
        // not consume every newly free slot ahead of other ready stages.
        invocations
            .sort_by_key(|invocation| counts.get(&invocation.callback_id).copied().unwrap_or(0));
    }

    pub(super) async fn drain(&self) {
        self.tasks.close();
        self.tasks.wait().await;
    }

    pub(super) fn dispatch(
        &self,
        node: Arc<defra_node::EmbeddedNode>,
        invocation: &CallbackInvocationDoc,
        callback: &crate::document_config::Callback,
        ceiling: Option<std::path::PathBuf>,
        plugins: Arc<crate::plugin::executor::PluginExecutor>,
    ) {
        let id = invocation.invocation_id.clone();
        let owner = invocation.owner_agent_did.clone();
        let callback = callback.clone();
        self.schedule(id.clone(), callback.callback_id.clone(), async move {
            let result = async {
                // A queued snapshot can finish before its slot opens.
                if let Some(current) = load_invocation(&node, &id, &owner).await? {
                    run_owned_invocation(&node, &current, &callback, ceiling.as_deref(), &plugins)
                        .await?;
                }
                anyhow::Ok(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(invocation_id = %id, %error, "callback invocation run failed");
            }
        });
    }
}

impl CallbackEngine {
    pub(super) async fn execute_invocation(
        &self,
        invocation: &CallbackInvocationDoc,
        callback: &crate::document_config::Callback,
    ) -> anyhow::Result<()> {
        if let Some(workers) = &self.workers {
            if matches!(
                callback.handler,
                crate::document_config::CallbackHandler::Plugin { .. }
            ) {
                workers.dispatch(
                    self.node.clone(),
                    invocation,
                    callback,
                    self.ceiling.clone(),
                    self.plugins.clone(),
                );
                return Ok(());
            }
        }
        run_owned_invocation(
            &self.node,
            invocation,
            callback,
            self.ceiling.as_deref(),
            &self.plugins,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn execution_overlaps_with_a_bound_and_deduplicates_active_attempts() {
        let workers = CallbackWorkers::new(2, CancellationToken::new());
        let (entered_tx, mut entered) = tokio::sync::mpsc::unbounded_channel();
        let mut releases = Vec::new();
        for id in ["one", "two"] {
            let (release, waiting) = oneshot::channel();
            releases.push(release);
            let entered = entered_tx.clone();
            assert!(workers.schedule(id.into(), "busy".into(), async move {
                entered.send(id).unwrap();
                waiting.await.unwrap();
            }));
        }
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(5), entered.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(!workers.schedule("one".into(), "busy".into(), async {
            panic!("duplicate active invocation")
        }));
        assert!(!workers.schedule("three".into(), "other".into(), async {
            panic!("exceeded capacity")
        }));
        assert!(!workers.contains("three"));
        assert_eq!(workers.permits.available_permits(), 0);
        assert!(entered.try_recv().is_err());
        releases.remove(0).send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while workers.contains("one") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let entered_tx = entered_tx.clone();
        assert!(
            workers.schedule("three".into(), "other".into(), async move {
                entered_tx.send("three").unwrap();
            })
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), entered.recv())
                .await
                .unwrap()
                .unwrap(),
            "three"
        );
        releases.remove(0).send(()).unwrap();
        workers.drain().await;
        assert!(workers.active.lock().unwrap().is_empty());
        assert_eq!(workers.permits.available_permits(), 2);
    }

    #[tokio::test]
    async fn shutdown_stops_admission_and_drains_the_running_attempt() {
        let cancel = CancellationToken::new();
        let workers = CallbackWorkers::new(1, cancel.clone());
        let (release, waiting) = oneshot::channel();
        assert!(
            workers.schedule("running".into(), "busy".into(), async move {
                waiting.await.unwrap();
            })
        );
        cancel.cancel();
        assert!(!workers.schedule("pending".into(), "busy".into(), async {
            panic!("started after shutdown")
        }));
        assert!(!workers.contains("pending"));
        let drained = workers.drain();
        tokio::pin!(drained);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut drained)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), drained)
            .await
            .unwrap();
        assert!(!workers.contains("running"));
    }
}
