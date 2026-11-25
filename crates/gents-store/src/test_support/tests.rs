use super::observe_until;
use std::time::Duration;

const CEILING: Duration = Duration::from_secs(10);

/// A collection another actor writes every second, read through a query
/// whose answer never satisfies the wait (the rows its reader cannot see):
/// the writes wake the wait, and it still fails at the ceiling with the
/// answer it last saw.
#[tokio::test(start_paused = true)]
async fn a_wait_whose_answer_never_changes_fails_at_the_ceiling_despite_writes() {
    let mut writes = tokio::time::interval(Duration::from_secs(1));
    let mut reads = 0u32;
    let outcome = observe_until(
        CEILING,
        async || {
            writes.tick().await;
        },
        async || {
            reads += 1;
            Err::<(), _>("AgentRequest: []")
        },
    )
    .await;
    assert_eq!(outcome, Err("AgentRequest: []".to_owned()));
    assert!(
        reads > 2,
        "the periodic writes woke the wait: {reads} reads"
    );
}

/// A wait whose answer keeps changing makes progress, so it outlives the
/// ceiling and resolves once the answer satisfies it.
#[tokio::test(start_paused = true)]
async fn a_wait_whose_answer_changes_outlives_the_ceiling() {
    let mut writes = tokio::time::interval(Duration::from_secs(1));
    let mut step = 0u64;
    let finish = 3 * CEILING.as_secs();
    let outcome = observe_until(
        CEILING,
        async || {
            writes.tick().await;
        },
        async || {
            step += 1;
            if step > finish {
                Ok(step)
            } else {
                Err(step)
            }
        },
    )
    .await;
    assert_eq!(outcome, Ok(finish + 1));
}
