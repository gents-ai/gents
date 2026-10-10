//! Authorized canonical transcript reads.
//!
//! Headers carry no body bytes.  This module is the one session reader that
//! obtains strict header/segment documents and hands the facts to the protocol
//! reconstruction owner.  In particular, a fork header may resolve segments
//! from its origin session, so segment reads are scoped by node/requester,
//! not by the child session label.

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{
    MessagePublication, MessageRole, OutputOutcome, OutputSource, OutputWriter, PayloadRef,
    SourceClose,
};
use std::collections::{BTreeMap, BTreeSet};

use super::canonical_rows::{
    decode_output_segment_row, decode_scoped_request_output_segments,
    decode_transcript_message_row, request_output_segments_query, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use super::query::session_scope_filter;
use super::SequencedMessage;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};

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
    /// Read only by tests that pin the candidate's transcript position.
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
}

/// Run `future`, counting the request-wide output segment scans canonical
/// reconstruction issues on this task.
#[cfg(test)]
pub(crate) async fn count_request_output_scans<F: std::future::Future>(
    future: F,
) -> (F::Output, usize) {
    let scans = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = REQUEST_OUTPUT_SCANS.scope(scans.clone(), future).await;
    (output, scans.load(std::sync::atomic::Ordering::Relaxed))
}

#[derive(Clone, Copy)]
enum ReadAccess<'a, 'txn> {
    Node(&'a EmbeddedNode),
    Txn(&'a ConfigApplyTxn<'txn>),
    Config(&'a ConfigAccess),
}

/// One scoped read only, never a replacement for an authoritative transaction.
/// Cache positive immutable facts, not missing dependencies: a later node query
/// can observe replication filling a gap. The caller fixes agent/requester scope.
#[derive(Default)]
struct ReadCache {
    headers: BTreeMap<String, super::canonical_rows::TranscriptMessageRow>,
    requests: BTreeMap<String, Vec<super::canonical_rows::OutputSegmentRow>>,
    /// Rows the caller read by an exact scoped query in the same transaction;
    /// each still passes coordinate validation before it enters `headers`.
    observed: BTreeMap<String, super::canonical_rows::TranscriptMessageRow>,
    /// Bulk readers cache scoped session headers; single-header readers query twins only.
    sessions: Option<BTreeMap<String, Vec<serde_json::Value>>>,
    /// One bounded bulk pass reconstructs every message of a request
    /// against the same scan. A reused scan that lacks a referenced close is
    /// dropped and re-read, so replication filling that gap is still observed.
    reuse_node_scans: bool,
}

impl ReadAccess<'_, '_> {
    async fn query(self, document: &str, operation: &str) -> Result<serde_json::Value> {
        let response = match self {
            Self::Node(node) => {
                crate::graphql::graphql_response_with_transaction_retry(node, document, operation)
                    .await?
            }
            Self::Txn(txn) => return txn.execute(document).await,
            Self::Config(access) => {
                let value = access.execute(document).await?;
                anyhow::ensure!(
                    value
                        .get("errors")
                        .is_none_or(|errors| errors.is_null()
                            || errors.as_array().is_some_and(Vec::is_empty)),
                    "{operation} failed: {}",
                    value["errors"]
                );
                return Ok(value);
            }
        };
        anyhow::ensure!(
            !response.has_errors(),
            "{operation} failed: {:?}",
            response.errors
        );
        Ok(serde_json::json!({"data": response.data}))
    }
}

/// Resolve one exact header inside an existing authoritative transaction.  This
/// is deliberately keyed by physical document identity: terminal selection and
/// recovery must never fall back to a latest session message.
pub(crate) async fn load_canonical_message_in_txn(
    txn: &ConfigApplyTxn<'_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    reconstruct_scoped_message(
        ReadAccess::Txn(txn),
        header_doc_id,
        node_did,
        requester_did,
        &mut ReadCache::default(),
    )
    .await
}

/// Exact reconstruction of many headers of one scope in one authoritative
/// transaction. Each request-wide segment scan and resolved header is read
/// once per reader: terminalization holds the process-wide write gate while
/// it validates every accepted tool reply, and a fresh read per header scans
/// the request's whole output once per tool, starving lease renewal.
///
/// Its cache is a snapshot: once the transaction creates an `AgentMessage` or
/// `AgentOutputSegment`, the reader must not be used again. A caller that
/// publishes canonical output constructs a new reader after publishing.
pub(crate) struct TxnCanonicalReader<'a, 'txn> {
    txn: &'a ConfigApplyTxn<'txn>,
    node_did: &'a str,
    requester_did: Option<&'a str>,
    cache: ReadCache,
}

impl<'a, 'txn> TxnCanonicalReader<'a, 'txn> {
    pub(crate) fn new(
        txn: &'a ConfigApplyTxn<'txn>,
        node_did: &'a str,
        requester_did: Option<&'a str>,
    ) -> Self {
        Self {
            txn,
            node_did,
            requester_did,
            cache: ReadCache {
                sessions: Some(BTreeMap::new()),
                ..ReadCache::default()
            },
        }
    }

    /// Resolve these headers, read by an exact scoped query in this
    /// transaction, without a physical-ID lookup: DefraDB reads a whole
    /// collection (or the node's part of it) to find one `_docID`.
    pub(crate) fn observe_headers(
        &mut self,
        headers: &[super::canonical_rows::TranscriptMessageRow],
    ) {
        for header in headers {
            self.cache
                .observed
                .insert(header.doc_id.clone(), header.clone());
        }
    }

    pub(crate) async fn load_message(
        &mut self,
        header_doc_id: &str,
    ) -> Result<(
        gents_protocol::output::TranscriptMessage,
        gents_protocol::message::Message,
    )> {
        reconstruct_scoped_message(
            ReadAccess::Txn(self.txn),
            header_doc_id,
            self.node_did,
            self.requester_did,
            &mut self.cache,
        )
        .await
    }
}

/// Resolve an exact authorized canonical header through either the local or
/// HTTP read adapter. Exports and clients share the same dependency owner as
/// transaction-backed runtime readers; there is no serialized-content fallback.
pub async fn load_canonical_message(
    access: &ConfigAccess,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    reconstruct_scoped_message(
        ReadAccess::Config(access),
        header_doc_id,
        node_did,
        requester_did,
        &mut ReadCache::default(),
    )
    .await
}

/// Borrowed-node form of the same exact, scoped reconstruction boundary.
pub async fn load_canonical_message_from_node(
    node: &EmbeddedNode,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    reconstruct_scoped_message(
        ReadAccess::Node(node),
        header_doc_id,
        node_did,
        requester_did,
        &mut ReadCache::default(),
    )
    .await
}

#[cfg(test)]
/// Reconstruct one exact payload reference inside an authorized physical
/// request. Callers must first obtain the reference from its canonical owner;
/// this reader never searches by logical labels or chooses a latest stream.
pub(crate) async fn load_canonical_payload_from_node(
    node: &EmbeddedNode,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    reference: &PayloadRef,
) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
    load_canonical_payload(
        ReadAccess::Node(node),
        request_doc_id,
        node_did,
        requester_did,
        reference,
        None,
    )
    .await
}

pub(crate) async fn load_canonical_payload_with_access(
    access: &ConfigAccess,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    reference: &PayloadRef,
    expected_source: &OutputSource,
) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
    load_canonical_payload(
        ReadAccess::Config(access),
        request_doc_id,
        node_did,
        requester_did,
        reference,
        Some(expected_source),
    )
    .await
}

pub(crate) async fn load_canonical_payload_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    reference: &PayloadRef,
    expected_source: &gents_protocol::output::OutputSource,
) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
    load_canonical_payload(
        ReadAccess::Txn(txn),
        request_doc_id,
        node_did,
        requester_did,
        reference,
        Some(expected_source),
    )
    .await
}

async fn load_canonical_payload(
    access: ReadAccess<'_, '_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    reference: &PayloadRef,
    expected_source: Option<&gents_protocol::output::OutputSource>,
) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "canonical payload request id is blank"
    );
    let response = access
        .query(
            &request_output_segments_query(request_doc_id),
            "load_canonical_payload",
        )
        .await?;
    let rows = decode_scoped_request_output_segments(
        rows_value(&response, "AgentOutputSegment")?,
        node_did,
        None,
        requester_did,
    )?;
    if let Some(expected_source) = expected_source {
        anyhow::ensure!(
            rows.iter().any(|row| {
                row.doc_id == reference.close_doc_id
                    && row.segment.source == *expected_source
                    && row.segment.close.is_some()
            }),
            "canonical payload close does not belong to the expected tool source"
        );
    }
    let observed = rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    gents_protocol::output::reconstruction::reconstruct_stream(&observed, &[], &[], reference)
        .map_err(anyhow::Error::from)
}

/// Load only canonical headers bound to one exact request.  Callers that make
/// lifecycle decisions can inspect immutable publication membership without
/// accidentally triggering a "latest message" projection or reading payload
/// bytes.  Duplicate sequences are an integrity failure.
pub(crate) async fn load_request_headers_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
) -> Result<Vec<super::canonical_rows::TranscriptMessageRow>> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "request header lookup requires request document id"
    );
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{ AgentMessage(filter: {{ {scope}, request_doc_id: {{ _eq: "{}" }} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        crate::graphql::escape_graphql_string(request_doc_id)
    );
    let response = txn.execute(&query).await?;
    let headers = rows_value(&response, "AgentMessage")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let mut sequences = std::collections::BTreeSet::new();
    for header in &headers {
        anyhow::ensure!(
            header.message.session_id == session_id
                && header.message.node_did == node_did
                && header.message.requester_did.as_deref() == requester_did
                && header.message.request_doc_id.as_deref() == Some(request_doc_id),
            "request header crossed exact scope"
        );
        anyhow::ensure!(
            sequences.insert(header.message.sequence),
            "duplicate canonical message sequence for one request scope"
        );
    }
    Ok(headers)
}

fn rows_value<'a>(
    response: &'a serde_json::Value,
    collection: &str,
) -> Result<&'a Vec<serde_json::Value>> {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(serde_json::Value::as_array)
        .with_context(|| format!("canonical output query omitted {collection} rows"))
}

async fn load_header(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<super::canonical_rows::TranscriptMessageRow> {
    anyhow::ensure!(
        !header_doc_id.trim().is_empty(),
        "canonical header id is blank"
    );
    if let Some(header) = cache.headers.get(header_doc_id) {
        return Ok(header.clone());
    }
    let mut headers = if let Some(observed) = cache.observed.get(header_doc_id) {
        vec![observed.clone()]
    } else {
        let requester = requester_did
            .map(|did| format!(r#""{}""#, crate::graphql::escape_graphql_string(did)))
            .unwrap_or_else(|| "null".into());
        let query = format!(
            r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(header_doc_id),
            crate::graphql::escape_graphql_string(node_did)
        );
        let response = access.query(&query, "load_canonical_header").await?;
        rows_value(&response, "AgentMessage")?
            .iter()
            .map(decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?
    };
    match headers.len() {
        0 => Err(anyhow::Error::new(
            CanonicalOutputReadError::MissingCanonicalHeader {
                header_doc_id: header_doc_id.to_owned(),
            },
        )),
        1 => {
            let header = headers.pop().expect("one canonical header checked");
            anyhow::ensure!(
                header.doc_id == header_doc_id
                    && header.message.node_did == node_did
                    && header.message.requester_did.as_deref() == requester_did,
                "canonical header scope mismatch"
            );
            // A pinned physical ID does not make a second header at the same
            // logical coordinate harmless. Include visible key/sequence twins
            // in the shared identity validator before caching the observation.
            let mut observed =
                coordinate_twins(access, &header, node_did, requester_did, cache).await?;
            observed.push(header.clone());
            let facts = observed
                .iter()
                .map(|row| gents_protocol::output::origin::ObservedMessage {
                    doc_id: &row.doc_id,
                    message: &row.message,
                })
                .collect::<Vec<_>>();
            gents_protocol::output::origin::lookup_message(
                &facts,
                &[],
                header_doc_id,
                node_did,
                requester_did,
            )
            .map_err(anyhow::Error::new)?;
            cache.headers.insert(header.doc_id.clone(), header.clone());
            Ok(header)
        }
        _ => Err(anyhow::Error::new(
            CanonicalOutputReadError::AmbiguousCanonicalHeader {
                header_doc_id: header_doc_id.to_owned(),
            },
        )),
    }
}

/// Scoped headers sharing `header`'s key or sequence in its session. A
/// bulk transaction reader reads its session once: validating each of a request's
/// headers by its own query costs the session size per header.
async fn coordinate_twins(
    access: ReadAccess<'_, '_>,
    header: &super::canonical_rows::TranscriptMessageRow,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<Vec<super::canonical_rows::TranscriptMessageRow>> {
    let session_id = &header.message.session_id;
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let key = &header.message.message_key;
    let sequence = header.message.sequence;
    if cache.sessions.is_none() {
        let escaped = crate::graphql::escape_graphql_string(key);
        let query = format!(
            r#"{{
            key_matches: AgentMessage(filter: {{ {scope}, message_key: {{ _eq: "{escaped}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }}
            sequence_matches: AgentMessage(filter: {{ {scope}, sequence: {{ _eq: {sequence} }} }}) {{ {AGENT_MESSAGE_FIELDS} }}
        }}"#
        );
        let response = access
            .query(&query, "validate_canonical_header_coordinate")
            .await?;
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for selection in ["key_matches", "sequence_matches"] {
            for value in rows_value(&response, selection)? {
                let row = decode_transcript_message_row(value)?;
                if seen.insert(row.doc_id.clone()) {
                    rows.push(row);
                }
            }
        }
        return Ok(rows);
    }
    let sessions = cache.sessions.as_mut().expect("bulk coordinate cache");
    if !sessions.contains_key(session_id) {
        let query =
            format!(r#"{{ AgentMessage(filter: {{ {scope} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#);
        let response = access
            .query(&query, "validate_canonical_header_coordinate")
            .await?;
        let rows = rows_value(&response, "AgentMessage")?.clone();
        sessions.insert(session_id.clone(), rows);
    }
    sessions[session_id]
        .iter()
        .filter(|row| {
            row.get("message_key").and_then(serde_json::Value::as_str) == Some(key.as_str())
                || row.get("sequence").and_then(serde_json::Value::as_u64)
                    == Some(u64::from(sequence))
        })
        .map(decode_transcript_message_row)
        .collect()
}

fn validate_fork(
    child: &super::canonical_rows::TranscriptMessageRow,
    origin: &super::canonical_rows::TranscriptMessageRow,
) -> Result<()> {
    use gents_protocol::output::origin::{validate_fork_metadata, ObservedMessage};
    validate_fork_metadata(
        ObservedMessage {
            doc_id: &child.doc_id,
            message: &child.message,
        },
        ObservedMessage {
            doc_id: &origin.doc_id,
            message: &origin.message,
        },
    )
    .map_err(anyhow::Error::new)
}

async fn validate_origin_chain(
    access: ReadAccess<'_, '_>,
    first: &super::canonical_rows::TranscriptMessageRow,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<super::canonical_rows::TranscriptMessageRow> {
    let mut active = BTreeSet::new();
    active.insert(first.doc_id.clone());
    let mut child = first.clone();
    loop {
        let MessagePublication::Fork {
            origin_message_doc_id,
        } = &child.message.publication
        else {
            return Ok(child);
        };
        anyhow::ensure!(
            active.insert(origin_message_doc_id.clone()),
            "canonical fork origin cycle at {origin_message_doc_id}"
        );
        let origin = load_header(
            access,
            origin_message_doc_id,
            node_did,
            requester_did,
            cache,
        )
        .await?;
        validate_fork(&child, &origin)?;
        child = origin;
    }
}

/// Scoped segments of one request, read once per reader in a transaction.
async fn request_rows(
    access: ReadAccess<'_, '_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &ReadCache,
    local: &mut BTreeMap<String, Vec<super::canonical_rows::OutputSegmentRow>>,
) -> Result<Vec<super::canonical_rows::OutputSegmentRow>> {
    if let Some(rows) = cache
        .requests
        .get(request_doc_id)
        .or_else(|| local.get(request_doc_id))
    {
        return Ok(rows.clone());
    }
    #[cfg(test)]
    let _ = REQUEST_OUTPUT_SCANS
        .try_with(|scans| scans.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let response = access
        .query(
            &request_output_segments_query(request_doc_id),
            "load_output_source",
        )
        .await?;
    let rows = decode_scoped_request_output_segments(
        rows_value(&response, "AgentOutputSegment")?,
        node_did,
        None,
        requester_did,
    )?;
    local.insert(request_doc_id.to_owned(), rows.clone());
    Ok(rows)
}

/// `request_hint` is the physical request whose output the header's payloads
/// are expected to close; closes found there need no `_docID` lookup, which
/// DefraDB serves by reading the node's whole output.
async fn load_referenced_segments(
    access: ReadAccess<'_, '_>,
    header: &super::canonical_rows::TranscriptMessageRow,
    request_hint: Option<&str>,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<Vec<super::canonical_rows::OutputSegmentRow>> {
    let requester = requester_did
        .map(|did| format!(r#""{}""#, crate::graphql::escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let close_ids = header
        .message
        .payload_references()
        .into_iter()
        .map(|reference| reference.close_doc_id.clone())
        .collect::<BTreeSet<_>>();
    let mut local = BTreeMap::new();
    let hinted = match request_hint {
        Some(request) if !close_ids.is_empty() => {
            request_rows(access, request, node_did, requester_did, cache, &mut local).await?
        }
        _ => Vec::new(),
    };
    let mut closures = Vec::new();
    for close_id in close_ids {
        let found = hinted
            .iter()
            .filter(|row| row.doc_id == close_id)
            .cloned()
            .collect::<Vec<_>>();
        if !found.is_empty() {
            closures.extend(found);
            continue;
        }
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&close_id),
            crate::graphql::escape_graphql_string(node_did)
        );
        let response = access.query(&query, "load_output_closure").await?;
        let rows = rows_value(&response, "AgentOutputSegment")?;
        closures.extend(
            rows.iter()
                .map(decode_output_segment_row)
                .collect::<Result<Vec<_>>>()?,
        );
    }
    for closure in &closures {
        if cache
            .requests
            .get(&closure.segment.request_doc_id)
            .is_some_and(|rows| !rows.iter().any(|row| row.doc_id == closure.doc_id))
        {
            cache.requests.remove(&closure.segment.request_doc_id);
        }
    }
    let mut sources = Vec::new();
    for closure in &closures {
        if !sources.iter().any(
            |(request, source): &(String, gents_protocol::output::OutputSource)| {
                request == &closure.segment.request_doc_id && source == &closure.segment.source
            },
        ) {
            sources.push((
                closure.segment.request_doc_id.clone(),
                closure.segment.source.clone(),
            ));
        }
    }
    // Do not key these observations by document ID: contradictory observations
    // of one physical ID must reach the shared integrity checker, not become
    // last-writer-wins here. Exact duplicates are harmless to reconstruction.
    let mut records = Vec::new();
    for (request_doc_id, source) in sources {
        let rows = request_rows(
            access,
            &request_doc_id,
            node_did,
            requester_did,
            cache,
            &mut local,
        )
        .await?;
        records.extend(rows.into_iter().filter(|row| row.segment.source == source));
    }
    // A transaction has a fixed view; a node read may still be receiving
    // additional facts, so reuse its scan only within one opted-in pass.
    if matches!(access, ReadAccess::Txn(_)) || cache.reuse_node_scans {
        cache.requests.extend(local);
    }
    Ok(records)
}

async fn reconstruct_scoped_message(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    let reconstructed = reconstruct_scoped_message_with_facts(
        access,
        header_doc_id,
        node_did,
        requester_did,
        cache,
    )
    .await?;
    Ok((reconstructed.header.message, reconstructed.message))
}

async fn reconstruct_scoped_message_with_facts(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<ReconstructedScopedMessage> {
    let header = load_header(access, header_doc_id, node_did, requester_did, cache).await?;
    let origin = validate_origin_chain(access, &header, node_did, requester_did, cache).await?;
    let request_hint = origin.message.request_doc_id.clone();
    let reconstruct = |segments: &[super::canonical_rows::OutputSegmentRow]| {
        let observed = segments
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        if origin.doc_id != header.doc_id {
            // Fork equality alone is insufficient: the origin's publication/source
            // constraints must hold too. Fork publication deliberately relaxes those
            // constraints on the child, not on the original authored message.
            reconstruct_message(&observed, &[], &[], &origin.message).map_err(|error| {
                (
                    error,
                    format!("reconstructing fork origin {}", origin.doc_id),
                )
            })?;
        }
        reconstruct_message(&observed, &[], &[], &header.message).map_err(|error| {
            (
                error,
                format!("reconstructing exact canonical message {header_doc_id}"),
            )
        })
    };
    // A node scan reused from an earlier message may predate this message's
    // payload segments; an incomplete result from it is re-read once.
    let reused = if cache.reuse_node_scans && !matches!(access, ReadAccess::Txn(_)) {
        cache.requests.keys().cloned().collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let mut segments = load_referenced_segments(
        access,
        &header,
        request_hint.as_deref(),
        node_did,
        requester_did,
        cache,
    )
    .await?;
    let mut reconstructed = reconstruct(&segments);
    if let Err((error, _)) = &reconstructed {
        let stale = segments
            .iter()
            .map(|row| &row.segment.request_doc_id)
            .chain(request_hint.as_ref())
            .filter(|request| reused.contains(*request))
            .cloned()
            .collect::<BTreeSet<_>>();
        if error.is_incomplete() && !stale.is_empty() {
            for request in &stale {
                cache.requests.remove(request);
            }
            segments = load_referenced_segments(
                access,
                &header,
                request_hint.as_deref(),
                node_did,
                requester_did,
                cache,
            )
            .await?;
            reconstructed = reconstruct(&segments);
        }
    }
    let message =
        reconstructed.map_err(|(error, context)| anyhow::Error::new(error).context(context))?;
    Ok(ReconstructedScopedMessage {
        header,
        origin,
        message,
        segments,
    })
}

/// Resolve physical assistant origins reachable from the bounded session view.
/// Forks retain their origin's request/session scope; an arbitrary historical
/// request supplied by the caller is never a lookup root.
/// A source boundary limits this read, but does not itself prove that an
/// independently stored checkpoint was causally derived from the whole view.
/// The caller must not select a candidate by comparing native message bytes.
#[cfg(test)]
pub(crate) async fn load_canonical_assistant_candidates(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
) -> Result<Vec<CanonicalAssistantCandidate>> {
    load_canonical_assistant_candidates_with(node, node, scope, boundary, None).await
}

/// Physical-request reads for replay. A failed store read is an error, never
/// empty rows, so it cannot pass for a missing request.
#[async_trait::async_trait]
pub(crate) trait ReplayRequestReader: Sync {
    async fn request_rows(&self, query: &str) -> Result<serde_json::Value>;
    async fn request_commits(
        &self,
        request_doc_id: &str,
    ) -> Result<Vec<crate::graphql::CompositeCommit>>;
}

#[async_trait::async_trait]
impl ReplayRequestReader for EmbeddedNode {
    async fn request_rows(&self, query: &str) -> Result<serde_json::Value> {
        ReadAccess::Node(self)
            .query(query, "load_canonical_replay_request")
            .await
    }

    async fn request_commits(
        &self,
        request_doc_id: &str,
    ) -> Result<Vec<crate::graphql::CompositeCommit>> {
        crate::graphql::composite_commits(self, request_doc_id, "canonical replay request commits")
            .await
    }
}

pub(crate) async fn load_canonical_assistant_candidates_with(
    node: &EmbeddedNode,
    requests: &impl ReplayRequestReader,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
    selected_tags: Option<&[gents_loop::claude_messages_body::ReplayTag]>,
) -> Result<Vec<CanonicalAssistantCandidate>> {
    let (request_commits, high_water) =
        validated_canonical_replay_boundary(node, requests, scope, boundary).await?;
    // None records the empty canonical view at capture time. A later current
    // read cannot turn that historical empty boundary into an unbounded scan.
    let Some(high_water) = high_water else {
        return Ok(Vec::new());
    };
    let mut cache = ReadCache {
        reuse_node_scans: true,
        ..ReadCache::default()
    };
    let mut request_facts = BTreeMap::from([(
        (
            scope.request_doc_id.to_owned(),
            scope.node_did.to_owned(),
            scope.requester_did.map(str::to_owned),
            scope.session_id.to_owned(),
        ),
        (scope.request_id.to_owned(), request_commits),
    )]);
    let mut capture_cache = crate::rendered_request::CaptureReadCache::default();

    let session_filter =
        session_scope_filter(scope.node_did, scope.session_id, scope.requester_did);
    let query = format!(
        r#"{{ AgentMessage(filter: {{ {session_filter}, sequence: {{ _le: {} }} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        high_water.sequence
    );
    let response = ReadAccess::Node(node)
        .query(&query, "load_canonical_replay_candidates")
        .await?;
    let headers = rows_value(&response, "AgentMessage")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let mut ids = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for row in &headers {
        replay_ensure!(
            row.message.node_did == scope.node_did
                && row.message.requester_did.as_deref() == scope.requester_did
                && row.message.session_id == scope.session_id
                && i64::from(row.message.sequence) <= high_water.sequence,
            "canonical replay header crossed its scoped high-water view"
        );
        replay_ensure!(
            ids.insert(row.doc_id.clone())
                && sequences.insert(row.message.sequence)
                && keys.insert(row.message.message_key.clone()),
            "canonical replay header has a physical, sequence, or key twin"
        );
        cache.headers.insert(row.doc_id.clone(), row.clone());
    }
    replay_ensure!(
        ids.contains(&high_water.doc_id),
        "canonical replay high-water header is absent from its bounded view"
    );

    let mut candidates = Vec::new();
    for row in headers
        .into_iter()
        .filter(|row| row.message.role == MessageRole::Assistant)
    {
        let reconstructed = reconstruct_scoped_message_with_facts(
            ReadAccess::Node(node),
            &row.doc_id,
            scope.node_did,
            scope.requester_did,
            &mut cache,
        )
        .await?;
        let origin_header = &reconstructed.origin.message;
        if origin_header.outcome != OutputOutcome::Complete
            || !matches!(
                origin_header.publication,
                MessagePublication::RequestExecution { .. }
            )
        {
            continue;
        }
        let coordinate =
            match provider_coordinate_for_candidate(&reconstructed, scope.expected_scope_kind) {
                Ok(coordinate) => coordinate,
                Err(error) => {
                    tracing::debug!(header_doc_id = %row.doc_id, error = %error,
                    "historical assistant has no usable provider replay coordinate");
                    continue;
                }
            };
        let Some(coordinate) = coordinate else {
            // An authored assistant belongs in permissive history, but is not
            // a provider continuation candidate merely because its role and
            // publication match a provider-owned assistant.
            continue;
        };
        let request_doc_id = origin_header
            .request_doc_id
            .as_deref()
            .context("canonical provider header has no physical request")?;
        // Filter only after bounded-header and physical-coordinate validation
        // so unrequested history cannot bypass canonical-view checks.
        if let Some(tags) = selected_tags {
            let selected = tags.iter().any(|tag| {
                if tag.request_doc_id != request_doc_id {
                    return false;
                }
                matches!(
                    &tag.source,
                    OutputSource::ProviderTurn {
                        scope: tag_scope,
                        turn_index,
                        attempt,
                    } if tag_scope == &coordinate.scope
                        && *turn_index == coordinate.turn_index
                        && *attempt == coordinate.attempt
                )
            });
            if !selected {
                continue;
            }
        }
        let request_scope = (
            request_doc_id.to_owned(),
            origin_header.node_did.clone(),
            origin_header.requester_did.clone(),
            origin_header.session_id.clone(),
        );
        let loaded = match request_facts.get(&request_scope) {
            Some(facts) => Ok(facts.clone()),
            None => {
                load_replay_physical_request(
                    requests,
                    request_doc_id,
                    &origin_header.node_did,
                    origin_header.requester_did.as_deref(),
                    &origin_header.session_id,
                )
                .await
            }
        };
        let (request_id, request_commits) = match loaded {
            Ok(request) => request,
            // A historical request that is verifiably missing or out of scope
            // makes only its turn non-replayable; the current request is
            // validated above. A failed store read says nothing about the
            // request and must not silently strip reasoning, so it propagates.
            Err(error)
                if request_doc_id != scope.request_doc_id
                    && error
                        .downcast_ref::<gents_loop::loop_stream::ReplayEvidenceViolation>()
                        .is_some() =>
            {
                tracing::warn!(
                    header_doc_id = %row.doc_id,
                    error = %format!("{error:#}"),
                    "accepted turn request is unverifiable; its reasoning is not replayed"
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        request_facts
            .entry(request_scope)
            .or_insert_with(|| (request_id.clone(), request_commits.clone()));
        let capture = replay_capture_for_candidate(
            node,
            &origin_header.node_did,
            origin_header.requester_did.as_deref(),
            &origin_header.session_id,
            &request_id,
            request_doc_id,
            &request_commits,
            coordinate,
            &mut capture_cache,
        )
        .await?;
        candidates.push(CanonicalAssistantCandidate {
            header_doc_id: reconstructed.origin.doc_id,
            request_doc_id: request_doc_id.to_owned(),
            #[cfg(test)]
            sequence: row.message.sequence,
            message: reconstructed.message,
            coordinate,
            capture,
        });
    }
    Ok(candidates)
}

/// Validate a restored replay's physical request and bounded transcript view
/// even when no current assistant needs provider evidence.
pub(crate) async fn validate_canonical_replay_boundary(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
) -> Result<()> {
    validated_canonical_replay_boundary(node, node, scope, boundary)
        .await
        .map(|_| ())
}

async fn validated_canonical_replay_boundary(
    node: &EmbeddedNode,
    requests: &impl ReplayRequestReader,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
) -> Result<(
    BTreeSet<String>,
    Option<crate::provider_context_reduction::TranscriptFactRef>,
)> {
    crate::provider_context_reduction::validate_source_boundary(
        boundary,
        scope.request_doc_id,
        scope.request_commit_cid,
    )
    .map_err(|error| replay_violation(error.to_string()))?;
    replay_ensure!(
        !scope.node_did.is_empty()
            && !scope.session_id.is_empty()
            && !scope.request_id.is_empty()
            && !scope.request_doc_id.is_empty()
            && !scope.request_commit_cid.is_empty()
            && scope.requester_did.is_none_or(|did| !did.is_empty()),
        "canonical replay scope is incomplete"
    );
    let request_commits = validate_replay_request(requests, scope).await?;
    let Some(high_water) = boundary.canonical_through.as_ref() else {
        return Ok((request_commits, None));
    };
    let mut cache = ReadCache::default();
    let high_water_header = load_header(
        ReadAccess::Node(node),
        &high_water.doc_id,
        scope.node_did,
        scope.requester_did,
        &mut cache,
    )
    .await
    .map_err(|error| {
        if error.downcast_ref::<CanonicalOutputReadError>().is_some() {
            replay_violation(error.to_string())
        } else {
            error
        }
    })?;
    replay_ensure!(
        high_water_header.message.session_id == scope.session_id
            && i64::from(high_water_header.message.sequence) == high_water.sequence,
        "canonical replay high-water header disagrees with its physical scope or sequence"
    );
    let high_water_commit = crate::graphql::newest_document_composite_commit(
        node,
        &high_water.doc_id,
        "canonical replay high-water header",
    )
    .await?
    .ok_or_else(|| {
        replay_violation("canonical replay high-water header has no composite commit")
    })?;
    replay_ensure!(
        high_water_commit.cid == high_water.commit_cid,
        "canonical replay high-water CID is not the physical header version"
    );
    Ok((request_commits, Some(high_water.clone())))
}

/// Resolve all requested tags from one physically bounded canonical candidate
/// view. Preserve input order and every physical match (including zero or
/// multiple matches) for the replay owner to reject, without a cross-boundary
/// cache or a coordinate-to-single-winner map.
pub(crate) async fn resolve_canonical_replay_tags(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
    tags: &[gents_loop::claude_messages_body::ReplayTag],
) -> Result<
    Vec<(
        gents_loop::claude_messages_body::ReplayTag,
        Vec<gents_loop::claude_messages_body::ResolvedReplayEvidence>,
    )>,
> {
    use gents_loop::claude_messages_body::{reasoning_witness, ResolvedReplayEvidence};

    for tag in tags {
        replay_ensure!(
            matches!(&tag.source, OutputSource::ProviderTurn { .. }),
            "canonical replay tag is not a provider source"
        );
    }
    let candidates =
        load_canonical_assistant_candidates_with(node, node, scope, boundary, Some(tags)).await?;
    tags.iter()
        .map(|tag| {
            let OutputSource::ProviderTurn {
                scope: tag_scope,
                turn_index,
                attempt,
            } = &tag.source
            else {
                unreachable!("all tag sources validated above")
            };
            let evidence = candidates
                .iter()
                .filter(|candidate| {
                    candidate.request_doc_id == tag.request_doc_id
                        && candidate.coordinate.scope == *tag_scope
                        && candidate.coordinate.turn_index == *turn_index
                        && candidate.coordinate.attempt == *attempt
                })
                .filter(|candidate| candidate.capture.is_some())
                .map(|candidate| {
                    let gents_protocol::message::Message::Assistant { content, .. } =
                        &candidate.message
                    else {
                        return Err(replay_violation(
                            "canonical replay candidate is not an assistant message",
                        ));
                    };
                    let capture = candidate.capture.as_ref().expect("capture filtered above");
                    // An undecodable capture leaves the turn without evidence
                    // of its producing prefix, which makes it non-replayable.
                    let captured = gents_loop::provider_input::replay_frontier::flatten(
                        &capture.body,
                        capture.wire,
                    )
                    .ok();
                    Ok(ResolvedReplayEvidence {
                        origin: gents_loop::claude_messages_body::ReplayOrigin::AcceptedProvider,
                        reasoning: reasoning_witness(content),
                        issuer: capture.issuer.clone(),
                        wire: capture.wire,
                        physical_header: candidate.header_doc_id.clone(),
                        complete: true,
                        captured,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((tag.clone(), evidence))
        })
        .collect()
}

/// Test convenience; production uses the batch owner even for a single turn.
#[cfg(test)]
pub(crate) async fn resolve_canonical_replay_tag(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
    tag: &gents_loop::claude_messages_body::ReplayTag,
) -> Result<Vec<gents_loop::claude_messages_body::ResolvedReplayEvidence>> {
    Ok(
        resolve_canonical_replay_tags(node, scope, boundary, std::slice::from_ref(tag))
            .await?
            .pop()
            .expect("singleton replay resolution returns one entry")
            .1,
    )
}

async fn validate_replay_request(
    requests: &impl ReplayRequestReader,
    scope: CanonicalReplayScope<'_>,
) -> Result<BTreeSet<String>> {
    let (request_id, commits) = load_replay_physical_request(
        requests,
        scope.request_doc_id,
        scope.node_did,
        scope.requester_did,
        scope.session_id,
    )
    .await?;
    replay_ensure!(
        request_id == scope.request_id,
        "canonical replay request has a different logical request ID"
    );
    replay_ensure!(
        commits.contains(scope.request_commit_cid),
        "canonical replay request boundary CID is not a commit of its physical request"
    );
    Ok(commits)
}

/// A missing, ambiguous or out-of-scope request is a `ReplayEvidenceViolation`;
/// a failed store read keeps its own error so callers cannot mistake it for one.
async fn load_replay_physical_request(
    requests: &impl ReplayRequestReader,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<(String, BTreeSet<String>)> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id purpose session_id node_did requester_did }} }}"#,
        crate::graphql::escape_graphql_string(request_doc_id)
    );
    let response = requests.request_rows(&query).await?;
    let rows = rows_value(&response, "AgentRequest")?;
    replay_ensure!(
        rows.len() == 1,
        "canonical replay request is missing or ambiguous"
    );
    let row = &rows[0];
    replay_ensure!(
        required_row_str(row, "_docID")? == request_doc_id
            && required_row_str(row, "purpose")? == "normal"
            && required_row_str(row, "session_id")? == session_id
            && required_row_str(row, "node_did")? == node_did
            && row.get("requester_did").is_some()
            && row["requester_did"].as_str() == requester_did,
        "canonical replay request crossed its physical node/session scope"
    );
    let commits = requests
        .request_commits(request_doc_id)
        .await?
        .into_iter()
        .map(|commit| commit.cid)
        .collect::<BTreeSet<_>>();
    Ok((required_row_str(row, "request_id")?.to_owned(), commits))
}

fn required_row_str<'a>(row: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| replay_violation(format!("canonical replay row omitted {field}")))
}

fn provider_coordinate_for_candidate(
    reconstructed: &ReconstructedScopedMessage,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> Result<Option<CanonicalProviderCoordinate>> {
    provider_coordinate_for_candidate_inner(reconstructed, expected_scope_kind)
        .map_err(|error| replay_violation(error.to_string()))
}

fn provider_coordinate_for_candidate_inner(
    reconstructed: &ReconstructedScopedMessage,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> Result<Option<CanonicalProviderCoordinate>> {
    let header = &reconstructed.origin.message;
    let request_doc_id = header
        .request_doc_id
        .as_deref()
        .context("canonical replay execution assistant has no physical request")?;
    let MessagePublication::RequestExecution {
        execution_generation,
    } = &header.publication
    else {
        anyhow::bail!("canonical replay candidate is not an execution publication");
    };
    let close_ids = header
        .payload_references()
        .into_iter()
        .map(|reference| reference.close_doc_id.clone())
        .collect::<BTreeSet<_>>();
    // An accepted empty assistant has no payload reference to a close. Its
    // message key is not independent provider evidence; never parse it to
    // manufacture an issuer or select a capture.
    if close_ids.is_empty() {
        return Ok(None);
    }
    let mut provider = None;
    let mut authored = false;
    for close_id in close_ids {
        let matches = reconstructed
            .segments
            .iter()
            .filter(|record| record.doc_id == close_id)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matches.len() == 1,
            "canonical replay reference has a missing or conflicting physical close {close_id}"
        );
        let close = &matches[0].segment;
        anyhow::ensure!(
            close.node_did == header.node_did
                && close.requester_did == header.requester_did
                && close.session_id == header.session_id
                && close.request_doc_id == request_doc_id
                && matches!(&close.writer, OutputWriter::RequestExecution { execution_generation: writer } if writer == execution_generation)
                && matches!(
                    &close.close,
                    Some(SourceClose::Closed {
                        outcome: OutputOutcome::Complete,
                        ..
                    })
                ),
            "canonical replay close crossed its request execution or is not complete"
        );
        match &close.source {
            OutputSource::Authored { .. } => authored = true,
            OutputSource::ProviderTurn {
                scope: capture_scope,
                turn_index,
                attempt,
            } => {
                anyhow::ensure!(
                    capture_scope.kind == expected_scope_kind,
                    "canonical replay assistant references a different provider loop scope"
                );
                let coordinate = CanonicalProviderCoordinate {
                    scope: *capture_scope,
                    turn_index: *turn_index,
                    attempt: *attempt,
                };
                match provider {
                    None => provider = Some(coordinate),
                    Some(expected) => anyhow::ensure!(
                        expected == coordinate,
                        "canonical replay assistant combines different provider turns"
                    ),
                }
            }
            _ => anyhow::bail!("canonical replay assistant references an invalid close source"),
        }
    }
    anyhow::ensure!(
        !(authored && provider.is_some()),
        "canonical replay assistant combines authored and provider sources"
    );
    Ok(provider)
}

#[allow(clippy::too_many_arguments)]
async fn replay_capture_for_candidate(
    node: &EmbeddedNode,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_id: &str,
    request_doc_id: &str,
    request_commits: &BTreeSet<String>,
    coordinate: CanonicalProviderCoordinate,
    cache: &mut crate::rendered_request::CaptureReadCache,
) -> Result<Option<CanonicalReplayCapture>> {
    let CanonicalProviderCoordinate {
        scope: capture_scope,
        turn_index,
        attempt,
    } = coordinate;
    let capture_scope_label = capture_scope.to_string();
    let capture_key = gents_loop::rendered_request::capture_key(
        node_did,
        session_id,
        request_doc_id,
        &capture_scope_label,
        usize::try_from(turn_index)?,
        attempt,
    )?;
    // Query by the existing exact key, but do not filter by the remaining
    // tuple: a contradictory row under that key must be detected, not hidden.
    let query = format!(
        r#"{{ RenderedRequest(filter: {{ capture_key: {{ _eq: "{}" }} }}, limit: 2) {{ capture_key capture_version request_doc_id request_commit_cid request_id session_id node_did requester_did capture_scope turn_index attempt source request_json provenance_json }} }}"#,
        crate::graphql::escape_graphql_string(&capture_key)
    );
    let response = ReadAccess::Node(node)
        .query(&query, "load_canonical_replay_capture")
        .await?;
    let captures = rows_value(&response, "RenderedRequest")?;
    let capture = match captures.as_slice() {
        [] => return Ok(None),
        [capture] => capture,
        _ => return Ok(None),
    };
    // An unverifiable capture makes only this turn non-replayable; it must not
    // fail every later request in the session.
    match verify_replay_capture(
        node,
        capture,
        &capture_key,
        node_did,
        requester_did,
        session_id,
        request_id,
        request_doc_id,
        request_commits,
        coordinate,
        cache,
    )
    .await
    {
        Ok(verified) => Ok(verified),
        Err(error)
            if error
                .downcast_ref::<crate::rendered_request::CaptureStoreReadError>()
                .is_some() =>
        {
            Err(error)
        }
        Err(error) => {
            warn_unverifiable_replay_capture(&capture_key, &error);
            Ok(None)
        }
    }
}

/// A bad capture is re-read on every request of its session; one warning a
/// minute, with the suppressed count, keeps it visible without flooding.
fn warn_unverifiable_replay_capture(capture_key: &str, error: &anyhow::Error) {
    use crate::log_rate::{CallsiteRateLimiter, Decision, RateLimitConfig};
    static LIMITER: std::sync::LazyLock<std::sync::Mutex<CallsiteRateLimiter<()>>> =
        std::sync::LazyLock::new(|| {
            std::sync::Mutex::new(CallsiteRateLimiter::new(RateLimitConfig {
                max_per_window: 1,
                window: std::time::Duration::from_secs(60),
            }))
        });
    let decision = LIMITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .check((), std::time::Instant::now());
    let suppressed = match decision {
        Decision::Suppress => return,
        Decision::Allow => 0,
        Decision::AllowWithSummary { suppressed } => suppressed,
    };
    tracing::warn!(
        capture_key = %capture_key,
        suppressed,
        error = %format!("{error:#}"),
        "accepted turn capture is unverifiable; its reasoning is not replayed"
    );
}

/// Capture versions whose stored request body can be decoded for reasoning
/// replay. v1 rows keep their own readers and never carry a replayable
/// provenance manifest; later versions decode through the versioned container.
const REPLAY_CAPTURE_VERSIONS: &[u32] = &[2, 3];

#[allow(clippy::too_many_arguments)]
async fn verify_replay_capture(
    node: &EmbeddedNode,
    capture: &serde_json::Value,
    capture_key: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_id: &str,
    request_doc_id: &str,
    request_commits: &BTreeSet<String>,
    coordinate: CanonicalProviderCoordinate,
    cache: &mut crate::rendered_request::CaptureReadCache,
) -> Result<Option<CanonicalReplayCapture>> {
    let CanonicalProviderCoordinate {
        scope: capture_scope,
        turn_index,
        attempt,
    } = coordinate;
    let capture_scope_label = capture_scope.to_string();
    // Rows are immutable, so every container version ever written stays in the
    // store; replay accepts the ones whose request body decodes through the
    // versioned container. A v1 row never carries a replayable provenance
    // manifest, and an unknown version is reported rather than reinterpreted.
    let capture_version = capture["capture_version"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();
    replay_ensure!(
        required_row_str(capture, "capture_key")? == capture_key
            && required_row_str(capture, "request_doc_id")? == request_doc_id
            && required_row_str(capture, "request_id")? == request_id
            && required_row_str(capture, "session_id")? == session_id
            && required_row_str(capture, "node_did")? == node_did
            && required_row_str(capture, "requester_did")? == requester_did.unwrap_or("")
            && required_row_str(capture, "capture_scope")? == capture_scope_label
            && REPLAY_CAPTURE_VERSIONS.contains(&capture_version)
            && capture["turn_index"].as_u64() == Some(u64::from(turn_index))
            && capture["attempt"].as_u64() == Some(u64::from(attempt)),
        "canonical replay capture disagrees with its exact provider close coordinate"
    );
    replay_ensure!(
        request_commits.contains(required_row_str(capture, "request_commit_cid")?),
        "canonical replay capture request CID is not a commit of its physical request"
    );
    let source: gents_protocol::rendered_request::RenderedRequestSource = serde_json::from_value(
        capture
            .get("source")
            .cloned()
            .ok_or_else(|| replay_violation("capture has no source"))?,
    )
    .map_err(|error| {
        replay_violation(format!(
            "canonical replay capture has malformed source: {error}"
        ))
    })?;
    let wire = match source {
        gents_protocol::rendered_request::RenderedRequestSource::ClaudeCliSubscription => {
            gents_loop::claude_messages_body::ReplayWire::ClaudeMessages
        }
        gents_protocol::rendered_request::RenderedRequestSource::OpenAiResponses => {
            gents_loop::claude_messages_body::ReplayWire::Responses
        }
        _ => return Ok(None),
    };
    let manifest = match gents_protocol::rendered_request::ProvenanceManifest::parse(
        required_row_str(capture, "provenance_json")?,
    ) {
        Ok(gents_protocol::rendered_request::ParsedProvenance::Manifest(manifest))
            if manifest.capture_scope == capture_scope_label =>
        {
            manifest
        }
        _ => return Ok(None),
    };
    let Some(issuer) = manifest
        .provider_family
        .as_deref()
        .zip(
            manifest
                .provider_endpoint
                .as_deref()
                .zip(manifest.provider_route_path_sha256.as_deref()),
        )
        .and_then(|(family, (endpoint, path))| {
            gents_loop::rendered_request::transport::replay_issuer_from_capture(
                family, endpoint, path,
            )
        })
    else {
        return Ok(None);
    };
    let body = crate::rendered_request::decode_capture_json_embedded_cached(
        node,
        capture_version,
        required_row_str(capture, "request_json")?,
        crate::rendered_request::CapturePayloadKind::RequestBody,
        cache,
    )
    .await?;
    Ok(Some(CanonicalReplayCapture { issuer, wire, body }))
}

/// The owned loop rebuilds context, prompt and folded messages from admission
/// input. Other publications belonging to that request (notably tool results
/// and background notifications) remain history. This binds PromptAssembly.CurrentInput;
/// request membership alone is not an input classification.
fn is_current_admission_input(
    header: &gents_protocol::output::TranscriptMessage,
    request_doc_id: &str,
) -> bool {
    header.request_doc_id.as_deref() == Some(request_doc_id)
        && matches!(
            header.publication,
            MessagePublication::RequestExecution { .. }
        )
        && (["prompt", "context"].into_iter().any(|key| {
            header.message_key == super::canonical_rows::authored_message_key(request_doc_id, key)
        }) || header
            .message_key
            .starts_with(&super::canonical_rows::authored_message_key(
                request_doc_id,
                &crate::lifecycle::queue::folded_input_key(""),
            )))
}

pub(super) async fn load_sequenced_messages(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    exclude_request_doc_id: Option<&str>,
    replay_profile: Option<crate::provider_input::ProviderInputProfile>,
) -> Result<Vec<SequencedMessage>> {
    let session_scope = session_scope_filter(node_did, session_id, requester_did);
    let mut bounds = Vec::new();
    if let Some(sequence) = after_sequence {
        bounds.push(format!("_gt: {sequence}"));
    }
    if let Some(sequence) = through_sequence {
        bounds.push(format!("_le: {sequence}"));
    }
    let bounds = (!bounds.is_empty())
        .then(|| format!(", sequence: {{ {} }}", bounds.join(", ")))
        .unwrap_or_default();
    let query = format!(
        r#"{{
        AgentMessage(filter: {{ {session_scope}{bounds} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }}
    }}"#
    );
    let response = crate::graphql::graphql_response_with_transaction_retry(
        node,
        &query,
        "load_canonical_history",
    )
    .await?;
    if response.has_errors() {
        anyhow::bail!(
            "loading canonical history for session_id={session_id}: {:?}",
            response.errors
        );
    }
    let data = response
        .data
        .as_ref()
        .context("canonical history query omitted data")?;
    let headers = data
        .get("AgentMessage")
        .and_then(serde_json::Value::as_array)
        .context("canonical history query omitted AgentMessage rows")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let mut cache = ReadCache {
        reuse_node_scans: true,
        ..ReadCache::default()
    };
    let mut sequences = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for row in &headers {
        anyhow::ensure!(
            row.message.session_id == session_id
                && row.message.node_did == node_did
                && row.message.requester_did.as_deref() == requester_did,
            "canonical header crossed requested session scope"
        );
        anyhow::ensure!(
            sequences.insert(row.message.sequence),
            "ambiguous canonical session sequence {}",
            row.message.sequence
        );
        anyhow::ensure!(
            keys.insert(row.message.message_key.clone()),
            "ambiguous canonical session message key {}",
            row.message.message_key
        );
        cache.headers.insert(row.doc_id.clone(), row.clone());
    }
    let mut messages = Vec::new();
    for row in headers.into_iter().filter(|row| {
        exclude_request_doc_id.is_none_or(|id| !is_current_admission_input(&row.message, id))
    }) {
        let sequence = row.message.sequence;
        let reconstructed = reconstruct_scoped_message_with_facts(
            ReadAccess::Node(node),
            &row.doc_id,
            node_did,
            requester_did,
            &mut cache,
        )
        .await
        .with_context(|| {
            format!(
                "reconstructing canonical message {} at sequence {sequence}",
                row.doc_id
            )
        })?;
        let provider_source = if replay_profile.is_some()
            && reconstructed.origin.message.role == MessageRole::Assistant
            && reconstructed.origin.message.outcome == OutputOutcome::Complete
            && matches!(
                reconstructed.origin.message.publication,
                MessagePublication::RequestExecution { .. }
            ) {
            match provider_coordinate_for_candidate(
                &reconstructed,
                gents_protocol::rendered_request::CaptureScopeKind::Inference,
            ) {
                Ok(Some(coordinate)) => Some(gents_loop::claude_messages_body::ReplayTag {
                    request_doc_id: reconstructed
                        .origin
                        .message
                        .request_doc_id
                        .clone()
                        .ok_or_else(|| {
                            replay_violation("provider coordinate requires request document")
                        })?,
                    source: OutputSource::ProviderTurn {
                        scope: coordinate.scope,
                        turn_index: coordinate.turn_index,
                        attempt: coordinate.attempt,
                    },
                }),
                Ok(None) => None,
                Err(error) => {
                    tracing::debug!(header_doc_id = %row.doc_id, error = %error,
                        "canonical assistant has no usable provider replay coordinate");
                    None
                }
            }
        } else {
            None
        };
        messages.push(SequencedMessage {
            provider_source,
            canonical_header_doc_id: Some(reconstructed.origin.doc_id),
            block_indices: match &reconstructed.message {
                gents_protocol::message::Message::Assistant { content, .. } => {
                    (0..content.len()).collect()
                }
                _ => Vec::new(),
            },
            sequence,
            message: reconstructed.message,
        });
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::output::{MessageRole, OutputOutcome, TranscriptMessage};

    #[test]
    fn current_input_selection_matches_lean_owner() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().current_input_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let retained = case
                .headers
                .iter()
                .enumerate()
                .filter_map(|(index, input)| {
                    let publication = match input.kind.as_str() {
                        "prompt" | "context" | "folded" | "assistant" => {
                            MessagePublication::RequestExecution {
                                execution_generation: "observed-generation".into(),
                            }
                        }
                        "tool_result" | "notification" => MessagePublication::ToolDelivery {
                            tool_call_doc_id: format!("tool-{index}"),
                        },
                        other => panic!("unsupported modeled header class {other}"),
                    };
                    let mut row = header(
                        &index.to_string(),
                        "session",
                        Some(&input.request),
                        publication,
                    );
                    let key = match input.kind.as_str() {
                        "folded" => crate::lifecycle::queue::folded_input_key("folded-request"),
                        kind => kind.to_owned(),
                    };
                    row.message.message_key =
                        super::super::canonical_rows::authored_message_key(&input.request, &key);
                    (!is_current_admission_input(&row.message, &case.current_request))
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            assert_eq!(retained, case.retained_indices, "{}", case.name);
        }
    }

    fn header(
        doc_id: &str,
        session_id: &str,
        request_doc_id: Option<&str>,
        publication: MessagePublication,
    ) -> super::super::canonical_rows::TranscriptMessageRow {
        super::super::canonical_rows::TranscriptMessageRow {
            doc_id: doc_id.into(),
            message: TranscriptMessage {
                message_key: format!("key-{doc_id}"),
                session_id: session_id.into(),
                node_did: "did:test:agent".into(),
                requester_did: Some("did:test:requester".into()),
                request_doc_id: request_doc_id.map(str::to_owned),
                publication,
                outcome: OutputOutcome::Complete,
                sequence: 7,
                role: MessageRole::Assistant,
                native_id: Some("native-message".into()),
                blocks: Vec::new(),
                created_at: "2026-09-21T00:00:00Z".into(),
            },
        }
    }

    #[test]
    fn fork_validation_allows_only_identity_session_and_key_to_change() {
        let origin = header(
            "origin",
            "origin-session",
            Some("origin-request"),
            MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            },
        );
        let child = header(
            "child",
            "child-session",
            None,
            MessagePublication::Fork {
                origin_message_doc_id: "origin".into(),
            },
        );
        validate_fork(&child, &origin).expect("valid fork");

        let mut reordered = child.clone();
        reordered.message.sequence += 1;
        assert!(validate_fork(&reordered, &origin).is_err());

        let mut request_bound = child;
        request_bound.message.request_doc_id = Some("child-request".into());
        assert!(validate_fork(&request_bound, &origin).is_err());
    }

    async fn create_doc(
        node: &EmbeddedNode,
        mutation: &str,
        variables: serde_json::Value,
        field: &str,
    ) -> String {
        let response = node
            .execute_request_with_retry(
                defra_node::QueryRequest::new(mutation).with_variables(variables),
                defra_node::ExecuteRetryPolicy::default(),
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        crate::graphql::single_mutation_document(&response, field)
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// `parts` as the ordered segments of one authored text stream; the last
    /// closes the source.
    fn authored_segments(
        request_doc_id: &str,
        key: &str,
        parts: &[&str],
    ) -> Vec<gents_protocol::output::OutputSegment> {
        use gents_protocol::output::*;
        let total = parts.iter().map(|part| part.len() as u64).sum::<u64>();
        parts
            .iter()
            .enumerate()
            .map(|(ordinal, part)| OutputSegment {
                node_did: "did:test:test".into(),
                requester_did: None,
                session_id: "session-scan-gap".into(),
                request_doc_id: request_doc_id.into(),
                source: OutputSource::Authored { key: key.into() },
                writer: OutputWriter::RequestExecution {
                    execution_generation: "observed-generation".into(),
                },
                ordinal: Some(ordinal as u32),
                runs: vec![SegmentRun {
                    stream: 0,
                    bytes: part.len() as u32,
                    declaration: (ordinal == 0).then_some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::Text,
                    }),
                }],
                payload: (*part).into(),
                close: (ordinal + 1 == parts.len()).then(|| SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    segments: parts.len() as u32,
                    stream_bytes: vec![total],
                }),
                created_at: chrono::Utc::now().to_rfc3339(),
            })
            .collect()
    }

    async fn create_authored_header(
        node: &EmbeddedNode,
        request_doc_id: &str,
        key: &str,
        sequence: u32,
        close_doc_id: String,
    ) -> String {
        use gents_protocol::output::*;
        let header = TranscriptMessage {
            message_key: key.into(),
            session_id: "session-scan-gap".into(),
            node_did: "did:test:test".into(),
            requester_did: None,
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence,
            role: MessageRole::User,
            native_id: None,
            blocks: vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        create_doc(
            node,
            super::super::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
            super::super::canonical_rows::transcript_message_create_variables(&header).unwrap(),
            "create_AgentMessage",
        )
        .await
    }

    #[tokio::test]
    async fn reused_scan_observes_payload_segments_replicated_after_their_close() {
        use super::super::canonical_rows::{
            output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        };
        let node = EmbeddedNode::builder().build().await.unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let request = "doc-scan-gap";
        let create_segment = |segment: gents_protocol::output::OutputSegment| {
            let node = &node;
            async move {
                create_doc(
                    node,
                    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                    output_segment_create_variables(&segment).unwrap(),
                    "create_AgentOutputSegment",
                )
                .await
            }
        };
        let [a] = authored_segments(request, "a", &["first"])
            .try_into()
            .unwrap();
        let a_close = create_segment(a).await;
        let a_header = create_authored_header(&node, request, "a", 1, a_close).await;
        // B's close is visible before its payload segment replicates.
        let [b_payload, b_close] = authored_segments(request, "b", &["hel", "lo"])
            .try_into()
            .unwrap();
        let b_close = create_segment(b_close).await;
        let b_header = create_authored_header(&node, request, "b", 2, b_close).await;

        let mut cache = ReadCache {
            reuse_node_scans: true,
            ..ReadCache::default()
        };
        let a = reconstruct_scoped_message(
            ReadAccess::Node(&node),
            &a_header,
            "did:test:test",
            None,
            &mut cache,
        )
        .await
        .unwrap();
        assert_eq!(a.1, gents_protocol::message::Message::user("first"));

        create_segment(b_payload).await;
        let b = reconstruct_scoped_message(
            ReadAccess::Node(&node),
            &b_header,
            "did:test:test",
            None,
            &mut cache,
        )
        .await
        .unwrap();
        assert_eq!(b.1, gents_protocol::message::Message::user("hello"));
    }
}
