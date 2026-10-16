use std::future::Future;

/// Keep a retained stream write-gate acquisition progressing while the
/// consumer flushes through the same gate; retain at most one produced item
/// until earlier output has been flushed.
pub async fn flush_while_polling<F, P>(flush: F, progress: P) -> (F::Output, Option<P::Output>)
where
    F: Future,
    P: Future,
{
    tokio::pin!(flush);
    tokio::select! {
        biased;
        result = &mut flush => (result, None),
        item = progress => (flush.await, Some(item)),
    }
}

#[cfg(test)]
mod tests;
