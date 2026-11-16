use std::future::Future;
use std::time::Duration;

use gents_documents::inference_defaults::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS;

/// Records a consumer matches to size its own wait on a stopping runtime.
pub const SHUTDOWN_EVENT_TARGET: &str = "gents::shutdown";

/// How often an outstanding stage names itself while it is still running.
pub const STAGE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(5);

/// DefraDB's node close runs four sequential stages, each bounded on its own
/// by the same default: the request drain, the network drain, the worker
/// pool's shutdown deadline and the store's close timeout. Only the store's
/// bound is a type this crate can read, so the stage count is the external
/// premise carried here; any one stage can consume the whole of it.
const NODE_CLOSE_STAGES: u32 = 4;

/// The agent drain waits for an in-flight turn to settle its own durable
/// state, which the provider stream's liveness timeout is what bounds.
pub fn agent_drain_budget() -> Duration {
    Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS)
}

pub fn node_close_budget() -> Duration {
    storage::RegolithStoreOptions::default()
        .close_timeout
        .saturating_mul(NODE_CLOSE_STAGES)
}

/// What a consumer must allow a runtime to stop in before calling it stalled.
pub fn shutdown_budget() -> Duration {
    agent_drain_budget().saturating_add(node_close_budget())
}

/// Reports an ordered teardown stage without bounding it. A stage may only be
/// observed, never cut short: the durable completion writes and the store
/// release both require the work under them to have actually finished, so the
/// record names the outstanding owner and the stage keeps running.
pub async fn observe_stage<F>(stage: &'static str, budget: Duration, work: F) -> F::Output
where
    F: Future,
{
    let started = tokio::time::Instant::now();
    let budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        target: SHUTDOWN_EVENT_TARGET,
        stage,
        budget_ms,
        "server shutdown stage started"
    );
    let mut work = std::pin::pin!(work);
    let mut observations = tokio::time::interval_at(
        started + STAGE_OBSERVATION_INTERVAL,
        STAGE_OBSERVATION_INTERVAL,
    );
    let output = loop {
        tokio::select! {
            output = &mut work => break output,
            _ = observations.tick() => {
                let elapsed = started.elapsed();
                let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
                if elapsed >= budget {
                    tracing::error!(
                        target: SHUTDOWN_EVENT_TARGET,
                        stage,
                        elapsed_ms,
                        budget_ms,
                        "server shutdown stage exceeded its budget"
                    );
                } else {
                    tracing::warn!(
                        target: SHUTDOWN_EVENT_TARGET,
                        stage,
                        elapsed_ms,
                        budget_ms,
                        "server shutdown stage still outstanding"
                    );
                }
            }
        }
    };
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        target: SHUTDOWN_EVENT_TARGET,
        stage,
        elapsed_ms,
        budget_ms,
        "server shutdown stage completed"
    );
    output
}

#[cfg(test)]
mod tests;
