//! Authorized canonical transcript reads.
//!
//! Headers carry no body bytes.  This module is the one session reader that
//! obtains strict header/segment documents and hands the facts to the protocol
//! reconstruction owner.  In particular, a fork header may resolve segments
//! from its origin session, so segment reads are scoped by node/requester,
//! not by the child session label.

fn replay_violation(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(gents_loop::loop_stream::ReplayEvidenceViolation(
        message.into(),
    ))
}

macro_rules! replay_ensure {
    ($condition:expr, $($message:tt)+) => {
        if !($condition) {
            return Err(replay_violation(format!($($message)+)));
        }
    };
}

#[derive(Debug, thiserror::Error)]
pub enum CanonicalOutputReadError {
    #[error("canonical header is unresolved or unauthorized: {header_doc_id}")]
    MissingCanonicalHeader { header_doc_id: String },
    #[error("canonical header lookup is ambiguous: {header_doc_id}")]
    AmbiguousCanonicalHeader { header_doc_id: String },
}

/// The physical request and node scope supplied by the owned restore.
/// Its commit CID is a historical request version, not the latest lease write.
#[derive(Clone, Copy)]
pub(crate) struct CanonicalReplayScope<'a> {
    pub(crate) node_did: &'a str,
    pub(crate) requester_did: Option<&'a str>,
    pub(crate) session_id: &'a str,
    pub(crate) request_id: &'a str,
    pub(crate) request_doc_id: &'a str,
    pub(crate) request_commit_cid: &'a str,
    pub(crate) expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
}

/// One canonical assistant, retaining physical identity even
/// when another row happens to reconstruct to equal native bytes.
pub(crate) struct CanonicalAssistantCandidate {
    pub(crate) header_doc_id: String,
    pub(crate) request_doc_id: String,
    #[cfg(test)]
    pub(crate) sequence: u32,
    pub(crate) message: gents_protocol::message::Message,
    pub(crate) coordinate: CanonicalProviderCoordinate,
    capture: Option<CanonicalReplayCapture>,
}

#[cfg(test)]
impl CanonicalAssistantCandidate {
    pub(crate) fn has_capture(&self) -> bool {
        self.capture.is_some()
    }
}

struct CanonicalReplayCapture {
    issuer: gents_loop::claude_messages_body::ReplayIssuer,
    wire: gents_loop::claude_messages_body::ReplayWire,
    body: serde_json::Value,
}

/// Exact source of a provider-owned assistant, established by its physical
/// referenced closes rather than by message bytes or a message-key suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalProviderCoordinate {
    pub(crate) scope: gents_protocol::rendered_request::CaptureScope,
    pub(crate) turn_index: u32,
    pub(crate) attempt: u32,
}

struct ReconstructedScopedMessage {
    header: super::canonical_rows::TranscriptMessageRow,
    origin: super::canonical_rows::TranscriptMessageRow,
    message: gents_protocol::message::Message,
    segments: Vec<super::canonical_rows::OutputSegmentRow>,
}

#[cfg(test)]
tokio::task_local! {
    static REQUEST_OUTPUT_SCANS: std::sync::Arc<std::sync::atomic::AtomicUsize>;
    static SOURCE_OUTPUT_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(test)]
pub(crate) fn record_request_output_scan() {
    let _ = REQUEST_OUTPUT_SCANS
        .try_with(|scans| scans.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(test)]
pub(crate) async fn count_request_output_scans<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let scans = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = REQUEST_OUTPUT_SCANS.scope(scans.clone(), future).await;
    (output, scans.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
pub(crate) async fn count_source_output_queries<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = SOURCE_OUTPUT_QUERIES.scope(queries.clone(), future).await;
    (output, queries.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
tokio::task_local! {
    static CAPTURE_ROW_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(test)]
pub(crate) async fn count_capture_row_queries<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = CAPTURE_ROW_QUERIES.scope(queries.clone(), future).await;
    (output, queries.load(std::sync::atomic::Ordering::Relaxed))
}

mod access;
mod headers;
pub(super) mod reader;
mod reconstruct;
pub(super) mod replay_candidates;
mod replay_capture;
pub(super) mod replay_request;
pub(super) mod replay_tags;
pub(super) mod sequenced;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod paging_tests;
