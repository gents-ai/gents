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

pub trait CanonicalStreamWriter<L>: StreamWriter
where
    L: RequestLifecycleControl,
{
    type AcceptedToolCall: Send;

    fn publish_authored_message(
        &self,
        lifecycle: &L,
        key: &str,
        message: &Message,
    ) -> impl std::future::Future<Output = anyhow::Result<String>> + Send;

    /// Atomically validates and publishes one candidate; `None` fences a stale
    /// candidate and its remaining ordered batch out of provider input.
    fn try_publish_steering_message(
        &self,
        lifecycle: &L,
        key: &str,
        message: &Message,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<String>>> + Send {
        async move {
            self.publish_authored_message(lifecycle, key, message)
                .await
                .map(Some)
        }
    }

    fn finish_natural_turn(
        &self,
        _lifecycle: &mut L,
        _message_doc_id: &str,
        _pending_request_doc_ids: &[String],
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        async { Ok(true) }
    }

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

    fn publish_native_turn(
        &self,
        lifecycle: &L,
        turn: usize,
        attempt: u32,
        message: &Message,
    ) -> impl std::future::Future<
        Output = anyhow::Result<CanonicalPublishedTurn<Self::AcceptedToolCall>>,
    > + Send;
}

pub trait StreamWriter: Send + Sync {
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

    /// When the next batch of buffered tokens is due, if any; a driver waits
    /// on this instead of polling so a quiet stream still lands its tokens.
    /// `None` means nothing is pending (the default: only a batching writer
    /// has anything due).
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
