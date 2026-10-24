//! Authorized canonical transcript reads.
//!
//! Headers carry no body bytes.  This module is the one session reader that
//! obtains strict header/segment documents and hands the facts to the protocol
//! reconstruction owner.  In particular, a fork header may resolve segments
//! from its origin session, so segment reads are scoped by node/requester,
//! not by the child session label.

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::error_chain::ErrorChain;
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{
    MessagePublication, MessageRole, OutputOutcome, OutputSource, OutputWriter, PayloadRef,
    SourceClose,
};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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
    #[error("canonical output query omitted {collection} rows")]
    RowsOmitted { collection: String },
}

#[derive(Debug, thiserror::Error)]
enum ReplayRequestFactsError {
    #[error(transparent)]
    Violation(#[from] gents_loop::loop_stream::ReplayEvidenceViolation),
    #[error("canonical replay request batch omitted {request_doc_id}")]
    Omitted { request_doc_id: String },
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
    requests: BTreeMap<String, Arc<RequestOutputRows>>,
    sources: RapidHashMap<(String, OutputSource), Vec<super::canonical_rows::OutputSegmentRow>>,
    source_reads: bool,
    /// Rows the caller read by an exact scoped query in the same transaction;
    /// each still passes coordinate validation before it enters `headers`.
    observed: BTreeMap<String, super::canonical_rows::TranscriptMessageRow>,
    sessions: Option<BTreeMap<String, SessionHeaderRows>>,
    /// One bounded bulk pass reconstructs every message of a request
    /// against the same scan. A reused scan that lacks a referenced close is
    /// dropped and re-read, so replication filling that gap is still observed.
    reuse_node_scans: bool,
}

/// Indexes retain every occurrence, including contradictory observations of
/// one physical document. They must not turn canonical integrity checks into
/// last-writer-wins selection.
#[derive(Clone)]
struct RequestOutputRows {
    rows: Vec<super::canonical_rows::OutputSegmentRow>,
    by_doc_id: RapidHashMap<String, Vec<usize>>,
    by_source: RapidHashMap<OutputSource, Vec<usize>>,
}

/// Unrelated malformed headers remain undecoded. Coordinate matches retain
/// their original order and every physical occurrence, even identical IDs.
struct SessionHeaderRows {
    rows: Vec<serde_json::Value>,
    by_key: RapidHashMap<String, Vec<usize>>,
    by_sequence: RapidHashMap<u64, Vec<usize>>,
}

impl From<Vec<serde_json::Value>> for SessionHeaderRows {
    fn from(rows: Vec<serde_json::Value>) -> Self {
        let mut by_key: RapidHashMap<String, Vec<usize>> = RapidHashMap::with_capacity(rows.len());
        let mut by_sequence: RapidHashMap<u64, Vec<usize>> =
            RapidHashMap::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            if let Some(key) = row.get("message_key").and_then(serde_json::Value::as_str) {
                by_key.entry(key.to_owned()).or_default().push(index);
            }
            if let Some(sequence) = row.get("sequence").and_then(serde_json::Value::as_u64) {
                by_sequence.entry(sequence).or_default().push(index);
            }
        }
        Self {
            rows,
            by_key,
            by_sequence,
        }
    }
}

impl SessionHeaderRows {
    fn coordinate_matches(
        &self,
        key: &str,
        sequence: u32,
    ) -> impl Iterator<Item = &serde_json::Value> {
        let mut indices = self
            .by_key
            .get(key)
            .into_iter()
            .flatten()
            .chain(
                self.by_sequence
                    .get(&u64::from(sequence))
                    .into_iter()
                    .flatten(),
            )
            .copied()
            .collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        indices.into_iter().map(|index| &self.rows[index])
    }
}

impl From<Vec<super::canonical_rows::OutputSegmentRow>> for RequestOutputRows {
    fn from(rows: Vec<super::canonical_rows::OutputSegmentRow>) -> Self {
        let mut by_doc_id: RapidHashMap<String, Vec<usize>> =
            RapidHashMap::with_capacity(rows.len());
        let mut by_source: RapidHashMap<OutputSource, Vec<usize>> = RapidHashMap::new();
        for (index, row) in rows.iter().enumerate() {
            by_doc_id.entry(row.doc_id.clone()).or_default().push(index);
            by_source
                .entry(row.segment.source.clone())
                .or_default()
                .push(index);
        }
        Self {
            rows,
            by_doc_id,
            by_source,
        }
    }
}

impl RequestOutputRows {
    fn push(&mut self, row: super::canonical_rows::OutputSegmentRow) {
        let index = self.rows.len();
        self.by_doc_id
            .entry(row.doc_id.clone())
            .or_default()
            .push(index);
        self.by_source
            .entry(row.segment.source.clone())
            .or_default()
            .push(index);
        self.rows.push(row);
    }

    fn with_doc_id(
        &self,
        doc_id: &str,
    ) -> impl Iterator<Item = &super::canonical_rows::OutputSegmentRow> {
        self.by_doc_id
            .get(doc_id)
            .into_iter()
            .flatten()
            .map(|&index| &self.rows[index])
    }

    fn for_source(
        &self,
        source: &OutputSource,
    ) -> impl Iterator<Item = &super::canonical_rows::OutputSegmentRow> {
        self.by_source
            .get(source)
            .into_iter()
            .flatten()
            .map(|&index| &self.rows[index])
    }
}

impl ReadAccess<'_, '_> {
    async fn query(self, document: &str, operation: &str) -> Result<serde_json::Value> {
        let response = match self {
            Self::Node(node) => {
                crate::graphql::graphql_response_with_transaction_retry(node, document, operation)
                    .await?
            }
            Self::Txn(txn) => return Ok(txn.execute(document).await?),
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
        Ok(crate::graphql::into_data_envelope(response.data))
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
        &mut ReadCache {
            source_reads: true,
            ..ReadCache::default()
        },
    )
    .await
}

/// Its cache is a transaction snapshot. A publication that appends exactly
/// one segment and header may consume it through `load_created_message`,
/// which reads the two new rows under ACP and invalidates coordinate checks.
/// Every other canonical mutation requires a new reader. No facts survive a retry.
pub(crate) struct TxnCanonicalReader<'a, 'txn> {
    txn: &'a ConfigApplyTxn<'txn>,
    node_did: &'a str,
    requester_did: Option<&'a str>,
    cache: ReadCache,
}

impl<'a, 'txn> TxnCanonicalReader<'a, 'txn> {
    pub(crate) fn new_single(
        txn: &'a ConfigApplyTxn<'txn>,
        node_did: &'a str,
        requester_did: Option<&'a str>,
    ) -> Self {
        Self {
            txn,
            node_did,
            requester_did,
            cache: ReadCache {
                source_reads: true,
                ..ReadCache::default()
            },
        }
    }

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

    pub(crate) fn clear_output_cache(&mut self) {
        self.cache.requests.clear();
        self.cache.sources.clear();
    }

    pub(crate) async fn load_source(
        &mut self,
        request_doc_id: &str,
        session_id: &str,
        source: &OutputSource,
    ) -> Result<Vec<super::canonical_rows::OutputSegmentRow>> {
        let rows = source_rows(
            ReadAccess::Txn(self.txn),
            request_doc_id,
            source,
            self.node_did,
            self.requester_did,
        )
        .await?;
        self.cache
            .sources
            .insert((request_doc_id.to_owned(), source.clone()), rows.clone());
        Ok(rows
            .into_iter()
            .filter(|row| row.segment.session_id == session_id)
            .collect())
    }

    /// DefraDB transaction reads are snapshots with read-your-writes. Only
    /// these two creates may have changed canonical output since this reader's
    /// last use. Read both exact rows through ACP before extending the complete
    /// request snapshot; mutation receipts and prepared inputs are not read
    /// authorization. Consume the reader so the extended snapshot cannot escape
    /// this publication.
    pub(crate) async fn load_created_message(
        mut self,
        header_doc_id: &str,
        output_doc_id: &str,
    ) -> Result<(
        gents_protocol::output::TranscriptMessage,
        gents_protocol::message::Message,
    )> {
        let requester = self
            .requester_did
            .map(|did| format!(r#""{}""#, crate::graphql::escape_graphql_string(did)))
            .unwrap_or_else(|| "null".into());
        let scope = format!(
            r#"node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }}"#,
            crate::graphql::escape_graphql_string(self.node_did)
        );
        let query = format!(
            r#"{{
            created_output: AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }}, {scope} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
            created_header: AgentMessage(filter: {{ _docID: {{ _eq: "{}" }}, {scope} }}) {{ {AGENT_MESSAGE_FIELDS} }}
        }}"#,
            crate::graphql::escape_graphql_string(output_doc_id),
            crate::graphql::escape_graphql_string(header_doc_id)
        );
        let response = ReadAccess::Txn(self.txn)
            .query(&query, "load_created_canonical_message")
            .await?;
        let output = rows_value(&response, "created_output")?;
        anyhow::ensure!(
            output.len() == 1,
            "created canonical output is not uniquely readable"
        );
        let output = decode_output_segment_row(&output[0])?;
        let headers = rows_value(&response, "created_header")?;
        anyhow::ensure!(
            headers.len() == 1,
            "created canonical header is not uniquely readable"
        );
        let header = decode_transcript_message_row(&headers[0])?;
        anyhow::ensure!(
            output.doc_id == output_doc_id
                && output.segment.node_did == self.node_did
                && output.segment.requester_did.as_deref() == self.requester_did
                && header.doc_id == header_doc_id
                && header.message.node_did == self.node_did
                && header.message.requester_did.as_deref() == self.requester_did,
            "created canonical rows crossed their authorized scope"
        );
        if let Some(rows) = self.cache.sources.get_mut(&(
            output.segment.request_doc_id.clone(),
            output.segment.source.clone(),
        )) {
            rows.push(output.clone());
        }
        if let Some(rows) = self.cache.requests.get_mut(&output.segment.request_doc_id) {
            Arc::make_mut(rows).push(output);
        }
        self.cache.headers.clear();
        self.cache.observed.clear();
        if let Some(sessions) = self.cache.sessions.as_mut() {
            sessions.clear();
        }
        self.observe_headers(std::slice::from_ref(&header));
        self.load_message(header_doc_id).await
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
        &mut ReadCache {
            source_reads: true,
            ..ReadCache::default()
        },
    )
    .await
}

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
        &mut ReadCache {
            source_reads: true,
            ..ReadCache::default()
        },
    )
    .await
}

#[cfg(test)]
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
        .collect::<Result<Vec<_>, _>>()?;
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
) -> Result<&'a Vec<serde_json::Value>, CanonicalOutputReadError> {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CanonicalOutputReadError::RowsOmitted {
            collection: collection.to_owned(),
        })
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
            .collect::<Result<Vec<_>, _>>()?
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
        sessions.insert(session_id.clone(), SessionHeaderRows::from(rows));
    }
    Ok(sessions[session_id]
        .coordinate_matches(key, sequence)
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>, _>>()?)
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

async fn request_rows(
    access: ReadAccess<'_, '_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &ReadCache,
    local: &mut BTreeMap<String, Arc<RequestOutputRows>>,
) -> Result<Arc<RequestOutputRows>> {
    if let Some(rows) = cache
        .requests
        .get(request_doc_id)
        .or_else(|| local.get(request_doc_id))
    {
        return Ok(Arc::clone(rows));
    }
    #[cfg(test)]
    record_request_output_scan();
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
    let rows = Arc::new(RequestOutputRows::from(rows));
    local.insert(request_doc_id.to_owned(), Arc::clone(&rows));
    Ok(rows)
}

async fn source_rows(
    access: ReadAccess<'_, '_>,
    request_doc_id: &str,
    source: &OutputSource,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<super::canonical_rows::OutputSegmentRow>> {
    #[cfg(test)]
    let _ = SOURCE_OUTPUT_QUERIES
        .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let query = super::canonical_rows::source_output_segments_query(request_doc_id, source)?;
    let response = access.query(&query, "load_output_source").await?;
    Ok(decode_scoped_request_output_segments(
        rows_value(&response, "AgentOutputSegment")?,
        node_did,
        None,
        requester_did,
    )?
    .into_iter()
    .filter(|row| row.segment.source == *source)
    .collect())
}

/// One source's segments in a write transaction, read through the
/// `(request_doc_id, source)` index. Scope fields beside `request_doc_id` in
/// the filter let the planner pick a scope field's index instead, which scans
/// every segment the node owns, so scope is checked on the decoded rows.
pub(crate) async fn load_source_rows_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_doc_id: &str,
    source: &OutputSource,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<super::canonical_rows::OutputSegmentRow>> {
    Ok(source_rows(
        ReadAccess::Txn(txn),
        request_doc_id,
        source,
        node_did,
        requester_did,
    )
    .await?
    .into_iter()
    .filter(|row| row.segment.session_id == session_id)
    .collect())
}

/// Load output segments for the pass's referenced request documents
/// through the indexed request field, in chunks. The per-pass cache still
/// owns freshness: if reconstruction detects an incomplete reused scan, its
/// existing retry evicts and rereads that request alone.
async fn preload_request_output_segments(
    node: &EmbeddedNode,
    request_doc_ids: &[String],
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<()> {
    let distinct = request_doc_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    for chunk in distinct.chunks(crate::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH) {
        let filter = chunk
            .iter()
            .map(|request_doc_id| {
                format!(
                    r#""{}""#,
                    crate::graphql::escape_graphql_string(request_doc_id)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _in: [{filter}] }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
        );
        #[cfg(test)]
        record_request_output_scan();
        let response = ReadAccess::Node(node)
            .query(&query, "preload_canonical_replay_output_segments")
            .await?;
        let rows = decode_scoped_request_output_segments(
            rows_value(&response, "AgentOutputSegment")?,
            node_did,
            None,
            requester_did,
        )?;
        let allowed = chunk.iter().cloned().collect::<BTreeSet<_>>();
        let mut grouped: BTreeMap<String, Vec<super::canonical_rows::OutputSegmentRow>> =
            BTreeMap::new();
        for row in rows {
            anyhow::ensure!(
                allowed.contains(&row.segment.request_doc_id),
                "canonical replay output batch returned an unrequested physical request"
            );
            grouped
                .entry(row.segment.request_doc_id.clone())
                .or_default()
                .push(row);
        }
        for request_doc_id in chunk {
            cache.requests.insert(
                request_doc_id.clone(),
                Arc::new(RequestOutputRows::from(
                    grouped.remove(request_doc_id).unwrap_or_default(),
                )),
            );
        }
    }
    Ok(())
}

async fn load_replay_request_facts_batch(
    node: &EmbeddedNode,
    request_doc_ids: &[String],
) -> Result<BTreeMap<String, ReplayRequestFacts>> {
    let distinct = request_doc_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut facts = BTreeMap::new();
    for chunk in distinct.chunks(crate::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH) {
        let ids = chunk
            .iter()
            .map(|request_doc_id| {
                format!(
                    r#""{}""#,
                    crate::graphql::escape_graphql_string(request_doc_id)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut query = format!(
            r#"{{ requests: AgentRequest(docIDs: [{ids}]) {{ _docID request_id purpose session_id node_did requester_did }}"#
        );
        for (index, request_doc_id) in chunk.iter().enumerate() {
            let escaped = crate::graphql::escape_graphql_string(request_doc_id);
            query.push_str(&format!(
                r#" commit{index}: _commits(docID: "{escaped}") {{ cid height fieldName signature {{ identity type }} }}"#
            ));
        }
        query.push_str(" }");
        let response = ReadAccess::Node(node)
            .query(&query, "load_canonical_replay_request_facts")
            .await?;
        let request_rows = rows_value(&response, "requests")?;
        let allowed = chunk.iter().cloned().collect::<BTreeSet<_>>();
        let mut grouped = chunk
            .iter()
            .map(|request_doc_id| (request_doc_id.clone(), Vec::new()))
            .collect::<BTreeMap<_, Vec<_>>>();
        for row in request_rows {
            let request_doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .with_context(|| "canonical replay request batch returned a row without _docID")?;
            anyhow::ensure!(
                allowed.contains(request_doc_id),
                "canonical replay request batch returned an unrequested physical request"
            );
            grouped
                .get_mut(request_doc_id)
                .expect("requested request IDs were initialized")
                .push(row.clone());
        }
        for (index, request_doc_id) in chunk.iter().enumerate() {
            let commit_rows = response
                .pointer(&format!("/data/commit{index}"))
                .and_then(serde_json::Value::as_array)
                .with_context(|| {
                    format!("canonical replay commit batch omitted {request_doc_id}")
                })?;
            let commits = crate::graphql::decode_composite_commits(commit_rows)?;
            let rows = grouped
                .remove(request_doc_id)
                .expect("requested request IDs were initialized");
            facts.insert(request_doc_id.clone(), ReplayRequestFacts { rows, commits });
        }
    }
    Ok(facts)
}

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
        Some(request) if !cache.source_reads && !close_ids.is_empty() => {
            Some(request_rows(access, request, node_did, requester_did, cache, &mut local).await?)
        }
        _ => None,
    };
    let mut closures = Vec::new();
    for close_id in close_ids {
        let found = hinted
            .iter()
            .flat_map(|rows| rows.with_doc_id(&close_id))
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
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    for closure in &closures {
        if cache
            .requests
            .get(&closure.segment.request_doc_id)
            .is_some_and(|rows| rows.with_doc_id(&closure.doc_id).next().is_none())
        {
            cache.requests.remove(&closure.segment.request_doc_id);
        }
    }
    let mut sources = Vec::new();
    let mut seen = RapidHashSet::new();
    for closure in &closures {
        if seen.insert((&closure.segment.request_doc_id, &closure.segment.source)) {
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
        if cache.source_reads {
            if let Some(rows) = cache.sources.get(&(request_doc_id.clone(), source.clone())) {
                records.extend(rows.iter().cloned());
            } else {
                records.extend(
                    source_rows(access, &request_doc_id, &source, node_did, requester_did).await?,
                );
            }
            continue;
        }
        let rows = request_rows(
            access,
            &request_doc_id,
            node_did,
            requester_did,
            cache,
            &mut local,
        )
        .await?;
        records.extend(rows.for_source(&source).cloned());
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
#[cfg(test)]
pub(crate) async fn load_canonical_assistant_candidates(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
) -> Result<Vec<CanonicalAssistantCandidate>> {
    load_canonical_assistant_candidates_with(node, node, scope, boundary, None).await
}

/// Physical-request reads for replay. Implementations read a bounded batch so
/// replay does not issue one request and commit query per historical turn. A
/// failed store read is an error, never empty rows, so it cannot pass for a
/// missing request.
#[async_trait::async_trait]
pub(crate) trait ReplayRequestReader: Sync {
    async fn request_facts(
        &self,
        request_doc_ids: &[String],
    ) -> Result<BTreeMap<String, ReplayRequestFacts>>;
}

#[derive(Clone)]
pub(crate) struct ReplayRequestFacts {
    pub(super) rows: Vec<serde_json::Value>,
    commits: Vec<crate::graphql::CompositeCommit>,
}

#[async_trait::async_trait]
impl ReplayRequestReader for EmbeddedNode {
    async fn request_facts(
        &self,
        request_doc_ids: &[String],
    ) -> Result<BTreeMap<String, ReplayRequestFacts>> {
        load_replay_request_facts_batch(self, request_doc_ids).await
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
        .collect::<Result<Vec<_>, _>>()?;
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

    let replay_output_requests = headers
        .iter()
        .filter(|row| {
            row.message.role == MessageRole::Assistant
                && !row.message.payload_references().is_empty()
        })
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<Vec<_>>();
    preload_request_output_segments(
        node,
        &replay_output_requests,
        scope.node_did,
        scope.requester_did,
        &mut cache,
    )
    .await?;

    let mut pending = Vec::new();
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
        pending.push(PendingReplayCandidate {
            capture_key: replay_capture_key(
                &origin_header.node_did,
                &origin_header.session_id,
                request_doc_id,
                coordinate,
            )?,
            header_doc_id: reconstructed.origin.doc_id,
            request_doc_id: request_doc_id.to_owned(),
            node_did: origin_header.node_did.clone(),
            requester_did: origin_header.requester_did.clone(),
            session_id: origin_header.session_id.clone(),
            coordinate,
            #[cfg(test)]
            sequence: row.message.sequence,
            message: reconstructed.message,
        });
    }
    let missing_request_ids = pending
        .iter()
        .filter(|candidate| {
            !request_facts.contains_key(&(
                candidate.request_doc_id.clone(),
                candidate.node_did.clone(),
                candidate.requester_did.clone(),
                candidate.session_id.clone(),
            ))
        })
        .map(|candidate| candidate.request_doc_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let loaded_request_facts = requests.request_facts(&missing_request_ids).await?;
    let mut selected = Vec::new();
    for candidate in pending {
        let request_scope = (
            candidate.request_doc_id.clone(),
            candidate.node_did.clone(),
            candidate.requester_did.clone(),
            candidate.session_id.clone(),
        );
        let loaded = match request_facts.get(&request_scope) {
            Some(facts) => Ok(facts.clone()),
            None => match loaded_request_facts.get(&candidate.request_doc_id) {
                Some(facts) => validate_replay_request_facts(
                    &candidate.request_doc_id,
                    &candidate.node_did,
                    candidate.requester_did.as_deref(),
                    &candidate.session_id,
                    facts,
                )
                .map_err(ReplayRequestFactsError::Violation),
                None => Err(ReplayRequestFactsError::Omitted {
                    request_doc_id: candidate.request_doc_id.clone(),
                }),
            },
        };
        let (request_id, request_commits) = match loaded {
            Ok(request) => request,
            // A historical request that is verifiably missing or out of scope
            // makes only its turn non-replayable; the current request is
            // validated above. A failed store read says nothing about the
            // request and must not silently strip reasoning, so it propagates.
            Err(ReplayRequestFactsError::Violation(violation))
                if candidate.request_doc_id != scope.request_doc_id =>
            {
                tracing::warn!(
                    header_doc_id = %candidate.header_doc_id,
                    error = %ErrorChain(&violation),
                    "accepted turn request is unverifiable; its reasoning is not replayed"
                );
                continue;
            }
            Err(ReplayRequestFactsError::Violation(violation)) => {
                return Err(anyhow::Error::from(violation));
            }
            Err(error @ ReplayRequestFactsError::Omitted { .. }) => return Err(error.into()),
        };
        request_facts
            .entry(request_scope)
            .or_insert_with(|| (request_id.clone(), request_commits.clone()));
        selected.push(SelectedReplayCandidate {
            capture_key: candidate.capture_key,
            header_doc_id: candidate.header_doc_id,
            request_doc_id: candidate.request_doc_id,
            node_did: candidate.node_did,
            requester_did: candidate.requester_did,
            session_id: candidate.session_id,
            request_id,
            request_commits,
            coordinate: candidate.coordinate,
            #[cfg(test)]
            sequence: candidate.sequence,
            message: candidate.message,
        });
    }
    let capture_rows = load_replay_capture_rows(node, &selected).await?;
    preload_replay_capture_blocks(node, &capture_rows, &mut capture_cache).await?;

    let mut candidates = Vec::new();
    for item in selected {
        let capture = replay_capture_from_row(
            node,
            &capture_rows,
            &item.capture_key,
            &item.node_did,
            item.requester_did.as_deref(),
            &item.session_id,
            &item.request_id,
            &item.request_doc_id,
            &item.request_commits,
            item.coordinate,
            &mut capture_cache,
        )
        .await?;
        candidates.push(CanonicalAssistantCandidate {
            header_doc_id: item.header_doc_id,
            request_doc_id: item.request_doc_id,
            #[cfg(test)]
            sequence: item.sequence,
            message: item.message,
            coordinate: item.coordinate,
            capture,
        });
    }
    Ok(candidates)
}

struct PendingReplayCandidate {
    capture_key: String,
    header_doc_id: String,
    request_doc_id: String,
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    coordinate: CanonicalProviderCoordinate,
    #[cfg(test)]
    sequence: u32,
    message: gents_protocol::message::Message,
}

/// One assistant of the bounded canonical view that passed every check a
/// capture read does not participate in: bounded-header, coordinate, tag
/// selection and physical-request validation.
struct SelectedReplayCandidate {
    capture_key: String,
    header_doc_id: String,
    request_doc_id: String,
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    request_id: String,
    request_commits: BTreeSet<String>,
    coordinate: CanonicalProviderCoordinate,
    #[cfg(test)]
    sequence: u32,
    message: gents_protocol::message::Message,
}

fn replay_capture_key(
    node_did: &str,
    session_id: &str,
    request_doc_id: &str,
    coordinate: CanonicalProviderCoordinate,
) -> Result<String> {
    let CanonicalProviderCoordinate {
        scope: capture_scope,
        turn_index,
        attempt,
    } = coordinate;
    Ok(gents_loop::rendered_request::capture_key(
        node_did,
        session_id,
        request_doc_id,
        &capture_scope.to_string(),
        usize::try_from(turn_index)?,
        attempt,
    )?)
}

/// Batched `capture_key` read for the pass's selected coordinates: one chunked
/// `_in` statement instead of one per candidate, with rows grouped per key so
/// a key with zero or multiple rows still leaves its turn without evidence,
/// never an error. Rows are queried by the exact key only: a contradictory row
/// under a key must be detected, not filtered away.
async fn load_replay_capture_rows(
    node: &EmbeddedNode,
    selected: &[SelectedReplayCandidate],
) -> Result<BTreeMap<String, Vec<serde_json::Value>>> {
    let mut keys = Vec::new();
    for item in selected {
        if !keys.contains(&item.capture_key) {
            keys.push(item.capture_key.clone());
        }
    }
    let mut rows: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for chunk in keys.chunks(crate::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH) {
        let filter = chunk
            .iter()
            .map(|key| format!("\"{}\"", crate::graphql::escape_graphql_string(key)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"{{ RenderedRequest(filter: {{ capture_key: {{ _in: [{filter}] }} }}) {{ capture_key capture_version request_doc_id request_commit_cid request_id session_id node_did requester_did capture_scope turn_index attempt source request_json provenance_json }} }}"#
        );
        #[cfg(test)]
        let _ = CAPTURE_ROW_QUERIES
            .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let response = ReadAccess::Node(node)
            .query(&query, "load_canonical_replay_captures")
            .await?;
        for row in rows_value(&response, "RenderedRequest")? {
            // An `_in` on capture_key only returns rows carrying that field;
            // a row without one is attributable to no requested key.
            let Some(key) = row.get("capture_key").and_then(serde_json::Value::as_str) else {
                continue;
            };
            rows.entry(key.to_owned()).or_default().push(row.clone());
        }
    }
    Ok(rows)
}

/// Read every manifest block the pass's unambiguous captures name in one
/// shared batched pass, so per-candidate verification resolves from positive
/// cache observations only. A capture whose record does not decode locally is
/// left to its verifier, which reports it per capture exactly as an unbatched
/// read would; the shared block read isolates a verification failure per
/// manifest the same way.
async fn preload_replay_capture_blocks(
    node: &EmbeddedNode,
    capture_rows: &BTreeMap<String, Vec<serde_json::Value>>,
    cache: &mut crate::rendered_request::CaptureReadCache,
) -> Result<()> {
    let mut manifests = Vec::new();
    let mut trees = Vec::new();
    for rows in capture_rows.values() {
        let [capture] = rows.as_slice() else {
            continue;
        };
        let Some(version) = capture["capture_version"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        let Some(stored) = capture
            .get("request_json")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        match crate::rendered_request::encoding::decode_capture_record(
            version,
            stored,
            crate::rendered_request::CapturePayloadKind::RequestBody,
        ) {
            Ok(crate::rendered_request::encoding::DecodedRecord::Manifest { blocks }) => {
                manifests.push(blocks)
            }
            Ok(crate::rendered_request::encoding::DecodedRecord::Tree(tree)) => trees.push(tree),
            _ => {}
        }
    }
    crate::rendered_request::preload_manifest_blocks(node, &manifests, cache).await?;
    Ok(crate::rendered_request::preload_manifest_trees(node, &trees, cache).await?)
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

/// Separates a canonical replay violation from storage failures in
/// `resolve_canonical_replay_tags`'s anyhow result; the violation may sit under
/// added context.
pub(crate) fn replay_resolution_error(
    error: anyhow::Error,
) -> gents_loop::loop_stream::ReplayEvidenceResolveError {
    use gents_loop::loop_stream::{ReplayEvidenceResolveError, ReplayEvidenceViolation};
    match error.downcast::<ReplayEvidenceViolation>() {
        Ok(violation) => ReplayEvidenceResolveError::Violation(violation),
        Err(error) => ReplayEvidenceResolveError::Storage {
            source: error.into(),
        },
    }
}

#[cfg(test)]
#[test]
fn replay_violation_is_classified_through_context() {
    use gents_loop::loop_stream::{ReplayEvidenceResolveError, ReplayEvidenceViolation};
    let error = anyhow::Error::new(ReplayEvidenceViolation("invalid canonical join".into()))
        .context("resolving required replay");
    assert!(matches!(
        replay_resolution_error(error),
        ReplayEvidenceResolveError::Violation(_)
    ));
    let error = anyhow::anyhow!("canonical replay tag belongs to another physical request");
    assert!(matches!(
        replay_resolution_error(error),
        ReplayEvidenceResolveError::Storage { .. }
    ));
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
    let request_doc_ids = [request_doc_id.to_owned()];
    let mut facts = requests.request_facts(&request_doc_ids).await?;
    let facts = facts
        .remove(request_doc_id)
        .with_context(|| format!("canonical replay request batch omitted {request_doc_id}"))?;
    validate_replay_request_facts(request_doc_id, node_did, requester_did, session_id, &facts)
        .map_err(anyhow::Error::from)
}

fn validate_replay_request_facts(
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    facts: &ReplayRequestFacts,
) -> std::result::Result<(String, BTreeSet<String>), gents_loop::loop_stream::ReplayEvidenceViolation>
{
    let rows = &facts.rows;
    if rows.len() != 1 {
        return Err(gents_loop::loop_stream::ReplayEvidenceViolation(
            "canonical replay request is missing or ambiguous".to_owned(),
        ));
    }
    let row = &rows[0];
    let scoped = required_row_str(row, "_docID")? == request_doc_id
        && required_row_str(row, "purpose")? == "normal"
        && required_row_str(row, "session_id")? == session_id
        && required_row_str(row, "node_did")? == node_did
        && row.get("requester_did").is_some()
        && row["requester_did"].as_str() == requester_did;
    if !scoped {
        return Err(gents_loop::loop_stream::ReplayEvidenceViolation(
            "canonical replay request crossed its physical node/session scope".to_owned(),
        ));
    }
    let commits = facts
        .commits
        .iter()
        .map(|commit| commit.cid.clone())
        .collect::<BTreeSet<_>>();
    Ok((required_row_str(row, "request_id")?.to_owned(), commits))
}

fn required_row_str<'a>(
    row: &'a serde_json::Value,
    field: &str,
) -> std::result::Result<&'a str, gents_loop::loop_stream::ReplayEvidenceViolation> {
    row.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            gents_loop::loop_stream::ReplayEvidenceViolation(format!(
                "canonical replay row omitted {field}"
            ))
        })
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

/// Verify the one capture row the pass's batched read returned under
/// `capture_key`. Zero rows (no capture) and multiple rows (an ambiguous key)
/// both leave the turn without evidence, never an error.
#[allow(clippy::too_many_arguments)]
async fn replay_capture_from_row(
    node: &EmbeddedNode,
    capture_rows: &BTreeMap<String, Vec<serde_json::Value>>,
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
    let Some([capture]) = capture_rows.get(capture_key).map(Vec::as_slice) else {
        return Ok(None);
    };
    // An unverifiable capture makes only this turn non-replayable; it must not
    // fail every later request in the session.
    match verify_replay_capture(
        node,
        capture,
        capture_key,
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
        Err(error) if crate::rendered_request::is_capture_store_read(error.as_ref()) => Err(error),
        Err(error) => {
            warn_unverifiable_replay_capture(capture_key, &error);
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
        error = %ErrorChain(error.as_ref()),
        "accepted turn capture is unverifiable; its reasoning is not replayed"
    );
}

/// Capture versions whose stored request body can be decoded for reasoning
/// replay. v1 rows keep their own readers and never carry a replayable
/// provenance manifest; later versions decode through the versioned container.
const REPLAY_CAPTURE_VERSIONS: &[u32] = &[2, 3, 4];

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

#[expect(
    clippy::too_many_arguments,
    reason = "independent transcript range filters"
)]
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
    let bounds = if !bounds.is_empty() {
        format!(", sequence: {{ {} }}", bounds.join(", "))
    } else {
        Default::default()
    };
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
        .collect::<Result<Vec<_>, _>>()?;
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
    let output_requests = headers
        .iter()
        .filter(|row| {
            exclude_request_doc_id.is_none_or(|id| !is_current_admission_input(&row.message, id))
                && !row.message.payload_references().is_empty()
        })
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<Vec<_>>();
    preload_request_output_segments(node, &output_requests, node_did, requester_did, &mut cache)
        .await?;
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
    fn session_header_index_preserves_coordinate_observations() {
        let rows = vec![
            serde_json::json!({"_docID": "duplicate", "message_key": "key", "sequence": 1}),
            serde_json::json!({"_docID": "duplicate", "message_key": "key", "sequence": 2}),
            serde_json::json!({"_docID": "sequence-twin", "message_key": "other", "sequence": 1}),
            serde_json::json!({"message_key": false, "sequence": -1, "blocks": "malformed"}),
            serde_json::json!({"message_key": "key", "sequence": "1", "blocks": "malformed"}),
            serde_json::json!({"message_key": 1, "sequence": 1, "blocks": "malformed"}),
        ];
        let indexed = SessionHeaderRows::from(rows.clone());
        for (key, sequence, expected) in [
            ("key", 1, vec![0, 1, 2, 4, 5]),
            ("other", 2, vec![1, 2]),
            ("missing", 3, vec![]),
        ] {
            assert_eq!(
                indexed
                    .coordinate_matches(key, sequence)
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|&index| &rows[index])
                    .collect::<Vec<_>>()
            );
        }
    }

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

    async fn create_doc(
        node: &EmbeddedNode,
        mutation: &str,
        variables: serde_json::Value,
        field: &str,
    ) -> String {
        let response = node
            .execute_request(defra_node::QueryRequest::new(mutation).with_variables(variables))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        crate::graphql::single_mutation_document(&response, field)
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    #[ignore = "manual canonical session-coordinate benchmark"]
    fn session_coordinate_lookup_size_benchmark() {
        let mut measurements = Vec::new();
        for count in [1_usize, 500, 10_000] {
            let rows = (0 .. count) . map (| sequence | { serde_json :: json ! ({ "_docID" : format ! ("header-{sequence}") , "message_key" : format ! ("key-{sequence}") , "sequence" : sequence , }) }) . collect :: < Vec < _ > > () ;
            let coordinates = (0..count)
                .map(|sequence| (format!("key-{sequence}"), u32::try_from(sequence).unwrap()))
                .collect::<Vec<_>>();
            let mut baseline = Vec::new();
            let mut indexed = Vec::new();
            let mut construction = Vec::new();
            for iteration in 0..6 {
                let start = std::time::Instant::now();
                let cache = SessionHeaderRows::from(rows.clone());
                let build_ms = start.elapsed().as_secs_f64() * 1000.0;
                let scan = || {
                    let start = std::time::Instant::now();
                    for (key, sequence) in &coordinates {
                        let matches = rows
                            .iter()
                            .filter(|row| {
                                row.get("message_key").and_then(serde_json::Value::as_str)
                                    == Some(key.as_str())
                                    || row.get("sequence").and_then(serde_json::Value::as_u64)
                                        == Some(u64::from(*sequence))
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(std::hint::black_box(matches).len(), 1);
                    }
                    start.elapsed().as_secs_f64() * 1000.0
                };
                let lookup = || {
                    let start = std::time::Instant::now();
                    for (key, sequence) in &coordinates {
                        let matches = cache.coordinate_matches(key, *sequence).collect::<Vec<_>>();
                        assert_eq!(std::hint::black_box(matches).len(), 1);
                    }
                    start.elapsed().as_secs_f64() * 1000.0
                };
                let (scan_ms, lookup_ms) = if iteration % 2 == 0 {
                    (scan(), lookup())
                } else {
                    let lookup_ms = lookup();
                    (scan(), lookup_ms)
                };
                if iteration > 0 {
                    baseline.push(scan_ms);
                    indexed.push(lookup_ms);
                    construction.push(build_ms);
                }
            }
            for values in [&mut baseline, &mut indexed, &mut construction] {
                values.sort_by(f64::total_cmp);
            }
            measurements . push (serde_json :: json ! ({ "headers" : count , "lookups" : count , "baseline_median_ms" : baseline [2] , "indexed_median_ms" : indexed [2] , "construction_with_row_clone_median_ms" : construction [2] , })) ;
        }
        let result = serde_json :: json ! ({ "measurements" : measurements , "includes_database_reads" : false });
        if let Some(path) = std::env::var_os("GENTS_COORDINATE_BENCHMARK_OUTPUT") {
            std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
        }
        tracing :: info ! (% result , "session coordinate lookup benchmark");
    }

    #[tokio::test]
    #[ignore = "manual canonical request-size benchmark"]
    async fn cached_output_reconstruction_request_size_benchmark() {
        use gents_protocol::output::{MessageBlock, PayloadPresentation, PresentedPayload};
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let node = EmbeddedNode::builder().build().await.unwrap();
        let request = "request-size-benchmark";
        let payload = "x".repeat(64 * 1024);
        for sources in [1, 32, 128] {
            let mut rows = Vec::new();
            let mut headers = Vec::new();
            for index in 0..sources {
                let key = format!("source-{index}");
                let doc_id = format!("close-{index}");
                let segment = authored_segments(request, &key, &[&payload]).pop().unwrap();
                let mut message = header(
                    &format!("header-{index}"),
                    &segment.session_id,
                    Some(request),
                    MessagePublication::RequestExecution {
                        execution_generation: "observed-generation".into(),
                    },
                );
                message.message.node_did = segment.node_did.clone();
                message.message.requester_did = None;
                message.message.role = MessageRole::User;
                message.message.native_id = None;
                message.message.sequence = index;
                message.message.blocks = vec![MessageBlock::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id: doc_id.clone(),
                            stream: 0,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                }];
                rows.push(super::super::canonical_rows::OutputSegmentRow { doc_id, segment });
                headers.push(message);
            }
            let mut cache = ReadCache::default();
            cache.requests.insert(request.into(), Arc::new(rows.into()));
            let expected = gents_protocol::message::Message::user(&payload);
            let mut samples = Vec::new();
            for pass in 0..6 {
                let started = std::time::Instant::now();
                for message in &headers {
                    let selected = load_referenced_segments(
                        ReadAccess::Node(&node),
                        message,
                        Some(request),
                        "did:test:test",
                        None,
                        &mut cache,
                    )
                    .await
                    .unwrap();
                    let observed = selected
                        .iter()
                        .map(|row| ObservedSegment {
                            doc_id: &row.doc_id,
                            segment: &row.segment,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        reconstruct_message(&observed, &[], &[], &message.message).unwrap(),
                        expected
                    );
                }
                if pass > 0 {
                    samples.push(started.elapsed().as_secs_f64() * 1_000.0);
                }
            }
            samples.sort_by(f64::total_cmp);
            tracing::info!(
                sources,
                payload_bytes = payload.len(),
                median_ms = samples[2],
                min_ms = samples[0],
                max_ms = samples[4],
                "cached canonical reconstruction benchmark"
            );
        }
        node.shutdown().await;
    }

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
        let node = crate::test_support::node_builder().build().await.unwrap();
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
