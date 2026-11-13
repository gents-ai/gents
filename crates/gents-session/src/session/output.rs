//! Authorized canonical transcript reads.
//!
//! Headers carry no body bytes.  This module is the one session reader that
//! obtains strict header/segment documents and hands the facts to the protocol
//! reconstruction owner.  A fork header resolves the closures copied into its
//! own session, so a reader granted only the fork needs nothing from the
//! source session; its origin chain is provenance only.  A fork published
//! before those copies existed still resolves through its origin.

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

impl CanonicalOutputReadError {
    /// The header read failure carried as the root or a context of an
    /// erased canonical read error.
    pub fn in_error(error: &anyhow::Error) -> Option<&Self> {
        error.downcast_ref::<Self>()
    }
}

/// The physical request and node scope supplied by the owned restore.
/// Its commit CID is a historical request version, not the latest lease write.
#[derive(Clone, Copy)]
pub struct CanonicalReplayScope<'a> {
    pub node_did: &'a str,
    pub requester_did: Option<&'a str>,
    pub session_id: &'a str,
    pub request_id: &'a str,
    pub request_doc_id: &'a str,
    pub request_commit_cid: &'a str,
    pub expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
}

/// One canonical assistant, retaining physical identity even
/// when another row happens to reconstruct to equal native bytes.
pub struct CanonicalAssistantCandidate {
    pub(crate) header_doc_id: String,
    pub request_doc_id: String,
    #[cfg(test)]
    pub(crate) sequence: u32,
    pub(crate) message: gents_protocol::message::Message,
    pub coordinate: CanonicalProviderCoordinate,
    pub(crate) block_indices: Vec<usize>,
    pub(crate) complete: bool,
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
pub struct CanonicalProviderCoordinate {
    pub scope: gents_protocol::rendered_request::CaptureScope,
    pub turn_index: u32,
    pub attempt: u32,
}

struct ReconstructedScopedMessage {
    header: gents_protocol::output::rows::TranscriptMessageRow,
    origin: gents_protocol::output::rows::TranscriptMessageRow,
    message: gents_protocol::message::Message,
    segments: Vec<gents_protocol::output::rows::OutputSegmentRow>,
}

#[cfg(any(test, feature = "test-fixtures"))]
tokio::task_local! {
    static REQUEST_OUTPUT_SCANS: std::sync::Arc<std::sync::atomic::AtomicUsize>;
    static SOURCE_OUTPUT_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
    static SOURCE_ROWS_DECODED: std::sync::Arc<std::sync::atomic::AtomicUsize>;
    static SOURCE_PAYLOAD_BYTES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn record_source_read(rows: usize, payload_bytes: usize) {
    let _ = SOURCE_ROWS_DECODED
        .try_with(|decoded| decoded.fetch_add(rows, std::sync::atomic::Ordering::Relaxed));
    let _ = SOURCE_PAYLOAD_BYTES
        .try_with(|bytes| bytes.fetch_add(payload_bytes, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn count_source_rows_decoded<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let rows = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = SOURCE_ROWS_DECODED.scope(rows.clone(), future).await;
    (output, rows.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn count_source_payload_bytes<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let bytes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = SOURCE_PAYLOAD_BYTES.scope(bytes.clone(), future).await;
    (output, bytes.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn record_request_output_scan() {
    let _ = REQUEST_OUTPUT_SCANS
        .try_with(|scans| scans.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn count_request_output_scans<F>(future: F) -> (F::Output, usize)
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

pub mod access;
mod headers;
pub mod reader;
mod reconstruct;
pub mod replay_candidates;
mod replay_capture;
pub mod replay_request;
pub mod replay_tags;
pub mod sequenced;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod paging_tests;
