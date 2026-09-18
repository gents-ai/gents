//! The live-stream sink seam.
//!
//! `StreamProcessor` writes streamed tokens and reasoning to a durable
//! response document as they arrive. `gents`'s `DefraStreamWriter` (native)
//! implements the actual write; the loop side only needs these four
//! operations, moved here so `StreamProcessor` can be generic over `W:
//! StreamWriter` with no DefraDB dependency.

pub trait StreamWriter: Send + Sync {
    fn write_tokens(
        &self,
        doc_id: &str,
        tokens: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;

    fn write_reasoning(
        &self,
        doc_id: &str,
        reasoning: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;

    fn flush_pending(
        &self,
        doc_id: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;

    /// When the next batch of buffered tokens is due to be written, if any
    /// are buffered. A driver uses it to wake exactly at the deadline
    /// instead of polling, so a stream that goes quiet mid-batch still
    /// lands its tokens on time.
    ///
    /// `None` means nothing is pending. The default says so, because a
    /// writer that does not batch has nothing to be due: only a batching
    /// writer needs to answer this.
    fn next_flush_deadline(
        &self,
        doc_id: &str,
    ) -> impl std::future::Future<Output = Option<tokio::time::Instant>> + Send {
        let _ = doc_id;
        std::future::ready(None)
    }

    /// Reset the live tail after a durable commit (a full turn, or a retract),
    /// so a resumed session's live view starts clean from the durable text.
    fn reset_tail(
        &self,
        doc_id: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}
