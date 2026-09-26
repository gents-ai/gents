//! The live-stream sink seam.
//!
//! `StreamProcessor` writes streamed tokens and reasoning to a durable
//! response document as they arrive. `gents`'s `DefraStreamWriter` (native)
//! implements the actual write; the loop side only needs these four
//! operations, moved here so `StreamProcessor` can be generic over `W:
//! StreamWriter` with no DefraDB dependency.

use gents_protocol::message::Message;
use gents_protocol::rendered_request::CaptureScope;

use crate::request_lifecycle::RequestLifecycleControl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAttemptClose {
    Retracted,
    Partial,
}

pub struct CanonicalPublishedTurn<Accepted> {
    pub message_doc_id: String,
    pub accepted_tools: Vec<Accepted>,
}

/// Native durable publication supplied to the generic stream processor.
pub trait CanonicalStreamWriter<L: RequestLifecycleControl>: StreamWriter {
    type AcceptedToolCall: Send;
    type SpawnAdmissionPlan: Send;

    fn publish_authored_message(
        &self,
        lifecycle: &L,
        key: &str,
        message: &Message,
    ) -> impl std::future::Future<Output = anyhow::Result<String>> + Send;

    fn start_provider_attempt(
        &self,
        request_doc_id: &str,
        turn: usize,
        attempt: u32,
        capture_scope: CaptureScope,
    ) -> impl std::future::Future<Output = ()> + Send;

    fn flush_native_partial(
        &self,
        lifecycle: &L,
        message: &Message,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;

    fn close_provider_attempt(
        &self,
        lifecycle: &L,
        turn: usize,
        attempt: u32,
        close: ProviderAttemptClose,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;

    fn publish_native_turn_with_spawn_admissions(
        &self,
        lifecycle: &L,
        turn: usize,
        attempt: u32,
        message: &Message,
        spawn_admissions: &[Self::SpawnAdmissionPlan],
    ) -> impl std::future::Future<
        Output = anyhow::Result<CanonicalPublishedTurn<Self::AcceptedToolCall>>,
    > + Send;
}

pub trait StreamWriter: Send + Sync {
    /// Schedule private metadata without exposing it as visible reasoning.
    fn mark_pending_output(
        &self,
        _doc_id: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        std::future::ready(Ok(true))
    }

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
