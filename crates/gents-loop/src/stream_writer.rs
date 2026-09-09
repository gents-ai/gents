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

    /// Reset the live tail after a durable commit (a full turn, or a retract),
    /// so a resumed session's live view starts clean from the durable text.
    fn reset_tail(
        &self,
        doc_id: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}
