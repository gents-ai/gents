use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use tokio::sync::watch;
use tokio::task::{JoinError, JoinSet};

use crate::config::ResolvedAgent;
use crate::retry::RetryPolicy;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SupervisionError {
    #[error("all agents exited cleanly")]
    AllExitedCleanly,
    #[error("all agents failed")]
    AllFailed,
    #[error("shutdown requested")]
    ShutdownRequested,
    #[error(transparent)]
    Join(#[from] JoinError),
}

pub(super) async fn supervise_behaviors_with_runner<F, Fut>(
    agents: Vec<Arc<ResolvedAgent>>,
    mut shutdown: watch::Receiver<bool>,
    retry_policy: RetryPolicy,
    runner: F,
) -> Result<(), SupervisionError>
where
    F: Fn(Arc<ResolvedAgent>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let mut join_set = JoinSet::new();
    let mut running = rapidhash::RapidHashSet::default();
    let mut failure_counts = rapidhash::RapidHashMap::<String, u32>::default();

    for behavior in agents {
        spawn_behavior(
            &mut join_set,
            &mut running,
            behavior,
            shutdown.clone(),
            runner.clone(),
        );
    }

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            Some(joined) = join_set.join_next() => {
                let (behavior, outcome) = joined?;
                running.remove(&behavior.agent_id);

                if shutdown.has_changed().unwrap_or(false) {
                    return Ok(());
                }

                match outcome {
                    Ok(Ok(())) => {
                        if running.is_empty() {
                            return Err(SupervisionError::AllExitedCleanly);
                        }
                    }
                    Ok(Err(error)) => {
                        let attempt = failure_counts.entry(behavior.agent_id.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            agent_id = %behavior.agent_id,
                            error = %error,
                            delay_ms = delay.as_millis() as u64,
                            "behavior task failed, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(SupervisionError::AllFailed);
                        }
                        wait_for_restart(delay, &mut shutdown).await?;
                        spawn_behavior(&mut join_set, &mut running, behavior, shutdown.clone(), runner.clone());
                    }
                    Err(_) => {
                        let attempt = failure_counts.entry(behavior.agent_id.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            agent_id = %behavior.agent_id,
                            delay_ms = delay.as_millis() as u64,
                            "behavior task panicked, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(SupervisionError::AllFailed);
                        }
                        wait_for_restart(delay, &mut shutdown).await?;
                        spawn_behavior(&mut join_set, &mut running, behavior, shutdown.clone(), runner.clone());
                    }
                }
            }
            else => return Ok(()),
        }
    }
}

fn spawn_behavior<F, Fut>(
    join_set: &mut JoinSet<(Arc<ResolvedAgent>, std::thread::Result<anyhow::Result<()>>)>,
    running: &mut rapidhash::RapidHashSet<String>,
    behavior: Arc<ResolvedAgent>,
    shutdown: watch::Receiver<bool>,
    runner: F,
) where
    F: Fn(Arc<ResolvedAgent>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let name = behavior.agent_id.clone();
    running.insert(name);
    join_set.spawn(async move {
        let outcome = AssertUnwindSafe(runner(behavior.clone(), shutdown))
            .catch_unwind()
            .await;
        (behavior, outcome)
    });
}

async fn wait_for_restart(
    delay: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), SupervisionError> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        _ = shutdown.changed() => Err(SupervisionError::ShutdownRequested),
    }
}
