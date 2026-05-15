use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures::FutureExt;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::config::AgentBehavior;
use crate::retry::RetryPolicy;

pub(super) async fn supervise_behaviors_with_runner<F, Fut>(
    behaviors: Vec<Arc<AgentBehavior>>,
    mut shutdown: watch::Receiver<bool>,
    retry_policy: RetryPolicy,
    runner: F,
) -> Result<()>
where
    F: Fn(Arc<AgentBehavior>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut join_set = JoinSet::new();
    let mut running = std::collections::HashSet::new();
    let mut failure_counts = std::collections::HashMap::<String, u32>::new();

    for behavior in behaviors {
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
                running.remove(&behavior.name);

                if shutdown.has_changed().unwrap_or(false) {
                    return Ok(());
                }

                match outcome {
                    Ok(Ok(())) => {
                        if running.is_empty() {
                            return Err(anyhow!("all behaviors exited cleanly"));
                        }
                    }
                    Ok(Err(error)) => {
                        let attempt = failure_counts.entry(behavior.name.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            behavior_id = %behavior.name,
                            error = %error,
                            delay_ms = delay.as_millis() as u64,
                            "behavior task failed, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(anyhow!("all behaviors failed"));
                        }
                        wait_for_restart(delay, &mut shutdown).await?;
                        spawn_behavior(&mut join_set, &mut running, behavior, shutdown.clone(), runner.clone());
                    }
                    Err(_) => {
                        let attempt = failure_counts.entry(behavior.name.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            behavior_id = %behavior.name,
                            delay_ms = delay.as_millis() as u64,
                            "behavior task panicked, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(anyhow!("all behaviors failed"));
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
    join_set: &mut JoinSet<(Arc<AgentBehavior>, std::thread::Result<Result<()>>)>,
    running: &mut std::collections::HashSet<String>,
    behavior: Arc<AgentBehavior>,
    shutdown: watch::Receiver<bool>,
    runner: F,
) where
    F: Fn(Arc<AgentBehavior>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let name = behavior.name.clone();
    running.insert(name);
    join_set.spawn(async move {
        let outcome = AssertUnwindSafe(runner(behavior.clone(), shutdown))
            .catch_unwind()
            .await;
        (behavior, outcome)
    });
}

async fn wait_for_restart(delay: Duration, shutdown: &mut watch::Receiver<bool>) -> Result<()> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        _ = shutdown.changed() => bail!("shutdown requested"),
    }
}
