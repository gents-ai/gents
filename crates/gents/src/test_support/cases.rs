use std::future::Future;

/// Generated cases each drive their own node, and their cost is CPU on the
/// task that drives them, so a test runtime needs this many workers for the
/// cases to run in parallel.
pub const CASE_WORKERS: usize = 8;

/// Drives every case on its own task, at most [`CASE_WORKERS`] at once, and
/// returns their results in case order. A case that panics fails the caller
/// with that case's own panic.
pub async fn drive_cases<I, C, T, F, Fut>(cases: I, drive: F) -> Vec<T>
where
    I: IntoIterator<Item = C>,
    T: Send + 'static,
    F: Fn(C) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
{
    let mut pending = cases.into_iter().enumerate();
    let mut running = tokio::task::JoinSet::new();
    let mut results = Vec::new();
    loop {
        while running.len() < CASE_WORKERS {
            let Some((index, case)) = pending.next() else {
                break;
            };
            let case = drive(case);
            running.spawn(async move { (index, case.await) });
        }
        let Some(joined) = running.join_next().await else {
            break;
        };
        match joined {
            Ok(result) => results.push(result),
            Err(error) => match error.try_into_panic() {
                Ok(panic) => std::panic::resume_unwind(panic),
                Err(error) => panic!("case task did not finish: {error}"),
            },
        }
    }
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}
