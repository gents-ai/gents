use super::diagnostics::compact_field;
use super::*;

pub(crate) fn refreshed_runtime_generation(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    agent_did: &str,
) -> Option<i64> {
    runtime.block_on(core.refresh_store()).ok()?;
    core.store()
        .snapshot()
        .latest_runtime(agent_did)
        .and_then(|row| row.router_generation.or(row.active_generation))
}

pub(crate) fn wait_for_stable_runtime_ready(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
    stable_for: Duration,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut stable_since = None;
    let mut stable_generation = None;
    let mut last_state = "runtime=<missing>".to_string();

    loop {
        runtime.block_on(core.refresh_store())?;
        let snapshot = core.store().snapshot();
        let runtime_row = snapshot.latest_runtime(agent_did);
        let ready = runtime_row.is_some_and(|row| {
            let generation = row.router_generation.or(row.active_generation);
            last_state = format!(
                "generation={generation:?} runnable={:?} unavailable={:?} result={:?} error={}",
                row.runnable_behavior_count,
                row.unavailable_behavior_count,
                row.last_reconcile_result,
                compact_field(row.last_reconcile_error.as_deref())
            );
            generation.is_some()
                && row.runnable_behavior_count == Some(1)
                && row.unavailable_behavior_count == Some(0)
                && row
                    .last_reconcile_error
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
        });
        let generation =
            runtime_row.and_then(|row| row.router_generation.or(row.active_generation));

        if ready {
            match (stable_generation, generation) {
                (Some(stable), Some(current)) if stable == current => {}
                (_, Some(current)) => {
                    stable_generation = Some(current);
                    stable_since = Some(Instant::now());
                }
                _ => {
                    stable_generation = None;
                    stable_since = None;
                }
            }
            if stable_since.is_some_and(|since| since.elapsed() >= stable_for) {
                return Ok(());
            }
        } else {
            stable_generation = None;
            stable_since = None;
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for stable runtime ready for {label}; last={last_state}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
