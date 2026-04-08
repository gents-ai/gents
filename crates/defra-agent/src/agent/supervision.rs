use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures::FutureExt;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::config::ProfileConfig;
use crate::retry::RetryPolicy;

pub(super) async fn supervise_profiles_with_runner<F, Fut>(
    profiles: Vec<Arc<ProfileConfig>>,
    mut shutdown: watch::Receiver<bool>,
    retry_policy: RetryPolicy,
    runner: F,
) -> Result<()>
where
    F: Fn(Arc<ProfileConfig>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut join_set = JoinSet::new();
    let mut running = std::collections::HashSet::new();
    let mut failure_counts = std::collections::HashMap::<String, u32>::new();

    for profile in profiles {
        spawn_profile(
            &mut join_set,
            &mut running,
            profile,
            shutdown.clone(),
            runner.clone(),
        );
    }

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            Some(joined) = join_set.join_next() => {
                let (profile, outcome) = joined?;
                running.remove(&profile.name);

                if shutdown.has_changed().unwrap_or(false) {
                    return Ok(());
                }

                match outcome {
                    Ok(Ok(())) => {
                        if running.is_empty() {
                            return Err(anyhow!("all profiles exited cleanly"));
                        }
                    }
                    Ok(Err(error)) => {
                        let attempt = failure_counts.entry(profile.name.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            profile = %profile.name,
                            error = %error,
                            delay_ms = delay.as_millis() as u64,
                            "profile task failed, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(anyhow!("all profiles failed"));
                        }
                        wait_for_restart(delay, &mut shutdown).await?;
                        spawn_profile(&mut join_set, &mut running, profile, shutdown.clone(), runner.clone());
                    }
                    Err(_) => {
                        let attempt = failure_counts.entry(profile.name.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            profile = %profile.name,
                            delay_ms = delay.as_millis() as u64,
                            "profile task panicked, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(anyhow!("all profiles failed"));
                        }
                        wait_for_restart(delay, &mut shutdown).await?;
                        spawn_profile(&mut join_set, &mut running, profile, shutdown.clone(), runner.clone());
                    }
                }
            }
            else => return Ok(()),
        }
    }
}

fn spawn_profile<F, Fut>(
    join_set: &mut JoinSet<(Arc<ProfileConfig>, std::thread::Result<Result<()>>)>,
    running: &mut std::collections::HashSet<String>,
    profile: Arc<ProfileConfig>,
    shutdown: watch::Receiver<bool>,
    runner: F,
) where
    F: Fn(Arc<ProfileConfig>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let name = profile.name.clone();
    running.insert(name);
    join_set.spawn(async move {
        let outcome = AssertUnwindSafe(runner(profile.clone(), shutdown))
            .catch_unwind()
            .await;
        (profile, outcome)
    });
}

async fn wait_for_restart(delay: Duration, shutdown: &mut watch::Receiver<bool>) -> Result<()> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        _ = shutdown.changed() => bail!("shutdown requested"),
    }
}
