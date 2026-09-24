//! Authorized canonical transcript reads.
//!
//! Headers carry no body bytes.  This module is the one session reader that
//! obtains strict header/segment documents and hands the facts to the protocol
//! reconstruction owner.  In particular, a fork header may resolve segments
//! from its origin session, so segment reads are scoped by principal/requester,
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
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use super::query::session_scope_filter;
use super::SequencedMessage;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};

#[derive(Debug, thiserror::Error)]
pub enum CanonicalOutputReadError {
    #[error("canonical header is unresolved or unauthorized: {header_doc_id}")]
    MissingCanonicalHeader { header_doc_id: String },
    #[error("canonical header lookup is ambiguous: {header_doc_id}")]
    AmbiguousCanonicalHeader { header_doc_id: String },
}

/// The physical request and principal scope supplied by the owned restore.
/// Its commit CID is a historical request version, not the latest lease write.
#[derive(Clone, Copy)]
pub(crate) struct CanonicalReplayScope<'a> {
    pub(crate) agent_did: &'a str,
    pub(crate) requester_did: Option<&'a str>,
    pub(crate) session_id: &'a str,
    pub(crate) request_id: &'a str,
    pub(crate) request_doc_id: &'a str,
    pub(crate) request_commit_cid: &'a str,
}

/// One canonical current-request assistant, retaining physical identity even
/// when another row happens to reconstruct to equal native bytes.
pub(crate) struct CanonicalAssistantCandidate {
    pub(crate) header_doc_id: String,
    pub(crate) sequence: u32,
    pub(crate) message: gents_protocol::message::Message,
    pub(crate) origin: gents_loop::claude_messages_body::ReplayOrigin,
    pub(crate) coordinate: CanonicalProviderCoordinate,
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
    message: gents_protocol::message::Message,
    segments: Vec<super::canonical_rows::OutputSegmentRow>,
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
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    let mut cache = ReadCache::default();
    reconstruct_scoped_message(
        ReadAccess::Txn(txn),
        header_doc_id,
        agent_did,
        requester_did,
        &mut cache,
    )
    .await
}

/// Resolve an exact authorized canonical header through either the local or
/// HTTP read adapter. Exports and clients share the same dependency owner as
/// transaction-backed runtime readers; there is no serialized-content fallback.
pub async fn load_canonical_message(
    access: &ConfigAccess,
    header_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    reconstruct_scoped_message(
        ReadAccess::Config(access),
        header_doc_id,
        agent_did,
        requester_did,
        &mut ReadCache::default(),
    )
    .await
}

/// Borrowed-node form of the same exact, scoped reconstruction boundary.
pub async fn load_canonical_message_from_node(
    node: &EmbeddedNode,
    header_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    reconstruct_scoped_message(
        ReadAccess::Node(node),
        header_doc_id,
        agent_did,
        requester_did,
        &mut ReadCache::default(),
    )
    .await
}

/// Reconstruct one exact payload reference inside an authorized physical
/// request. Callers must first obtain the reference from its canonical owner;
/// this reader never searches by logical labels or chooses a latest stream.
pub(crate) async fn load_canonical_payload_from_node(
    node: &EmbeddedNode,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reference: &PayloadRef,
) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
    load_canonical_payload(
        ReadAccess::Node(node),
        request_doc_id,
        agent_did,
        requester_did,
        reference,
        None,
    )
    .await
}

pub(crate) async fn load_canonical_payload_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reference: &PayloadRef,
    expected_source: &gents_protocol::output::OutputSource,
) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
    load_canonical_payload(
        ReadAccess::Txn(txn),
        request_doc_id,
        agent_did,
        requester_did,
        reference,
        Some(expected_source),
    )
    .await
}

async fn load_canonical_payload(
    access: ReadAccess<'_, '_>,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reference: &PayloadRef,
    expected_source: Option<&gents_protocol::output::OutputSource>,
) -> Result<gents_protocol::output::reconstruction::ReconstructedStream> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "canonical payload request id is blank"
    );
    let requester = requester_did
        .map(|did| format!(r#""{}""#, crate::graphql::escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let query = format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        crate::graphql::escape_graphql_string(request_doc_id),
        crate::graphql::escape_graphql_string(agent_did),
    );
    let response = access.query(&query, "load_canonical_payload").await?;
    let rows = rows_value(&response, "AgentOutputSegment")?
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
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
    agent_did: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
) -> Result<Vec<super::canonical_rows::TranscriptMessageRow>> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "request header lookup requires request document id"
    );
    let scope = session_scope_filter(agent_did, session_id, requester_did);
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
                && header.message.agent_did == agent_did
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
    agent_did: &str,
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
    let requester = requester_did
        .map(|did| format!(r#""{}""#, crate::graphql::escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let query = format!(
        r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        crate::graphql::escape_graphql_string(header_doc_id),
        crate::graphql::escape_graphql_string(agent_did)
    );
    let response = access.query(&query, "load_canonical_header").await?;
    let mut headers = rows_value(&response, "AgentMessage")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
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
                    && header.message.agent_did == agent_did
                    && header.message.requester_did.as_deref() == requester_did,
                "canonical header scope mismatch"
            );
            // A pinned physical ID does not make a second header at the same
            // logical coordinate harmless. Include visible key/sequence twins
            // in the shared identity validator before caching the observation.
            let scope = session_scope_filter(agent_did, &header.message.session_id, requester_did);
            let key = crate::graphql::escape_graphql_string(&header.message.message_key);
            let sequence = header.message.sequence;
            let query = format!(
                r#"{{ AgentMessage(filter: {{ {scope},
                _or: [{{ message_key: {{ _eq: "{key}" }} }}, {{ sequence: {{ _eq: {sequence} }} }}]
            }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
            );
            let response = access
                .query(&query, "validate_canonical_header_coordinate")
                .await?;
            let mut observed = rows_value(&response, "AgentMessage")?
                .iter()
                .map(decode_transcript_message_row)
                .collect::<Result<Vec<_>>>()?;
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
                agent_did,
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
    agent_did: &str,
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
            agent_did,
            requester_did,
            cache,
        )
        .await?;
        validate_fork(&child, &origin)?;
        child = origin;
    }
}

async fn load_referenced_segments(
    access: ReadAccess<'_, '_>,
    header: &super::canonical_rows::TranscriptMessageRow,
    agent_did: &str,
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
    let mut closures = Vec::new();
    for close_id in close_ids {
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&close_id),
            crate::graphql::escape_graphql_string(agent_did)
        );
        let response = access.query(&query, "load_output_closure").await?;
        let rows = rows_value(&response, "AgentOutputSegment")?;
        closures.extend(
            rows.iter()
                .map(decode_output_segment_row)
                .collect::<Result<Vec<_>>>()?,
        );
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
        if let Some(rows) = cache.requests.get(&request_doc_id) {
            for row in rows.iter().filter(|row| row.segment.source == source) {
                records.push(row.clone());
            }
            continue;
        }
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&request_doc_id),
            crate::graphql::escape_graphql_string(agent_did)
        );
        let response = access.query(&query, "load_output_source").await?;
        let rows = rows_value(&response, "AgentOutputSegment")?
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?;
        for row in rows.iter().filter(|row| row.segment.source == source) {
            records.push(row.clone());
        }
        // A transaction has a fixed view; a node read may still be receiving
        // additional facts, so never reuse its potentially incomplete scan.
        if matches!(access, ReadAccess::Txn(_)) {
            cache.requests.insert(request_doc_id, rows);
        }
    }
    Ok(records)
}

async fn reconstruct_scoped_message(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    let reconstructed = reconstruct_scoped_message_with_facts(
        access,
        header_doc_id,
        agent_did,
        requester_did,
        cache,
    )
    .await?;
    Ok((reconstructed.header.message, reconstructed.message))
}

async fn reconstruct_scoped_message_with_facts(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<ReconstructedScopedMessage> {
    let header = load_header(access, header_doc_id, agent_did, requester_did, cache).await?;
    let origin = validate_origin_chain(access, &header, agent_did, requester_did, cache).await?;
    let segments =
        load_referenced_segments(access, &header, agent_did, requester_did, cache).await?;
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
        reconstruct_message(&observed, &[], &[], &origin.message)
            .map_err(anyhow::Error::new)
            .with_context(|| format!("reconstructing fork origin {}", origin.doc_id))?;
    }
    let message = reconstruct_message(&observed, &[], &[], &header.message)
        .map_err(anyhow::Error::new)
        .with_context(|| format!("reconstructing exact canonical message {header_doc_id}"))?;
    Ok(ReconstructedScopedMessage {
        header,
        message,
        segments,
    })
}

/// Resolve the physical assistant turns eligible for one restored request.
/// A source boundary limits this read, but does not itself prove that an
/// independently stored checkpoint was causally derived from the whole view.
/// The caller must not select a candidate by comparing native message bytes.
pub(crate) async fn load_current_request_assistant_candidates(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
) -> Result<Vec<CanonicalAssistantCandidate>> {
    crate::provider_context_reduction::validate_source_boundary(
        boundary,
        scope.request_doc_id,
        scope.request_commit_cid,
    )?;
    anyhow::ensure!(
        !scope.agent_did.is_empty()
            && !scope.session_id.is_empty()
            && !scope.request_id.is_empty()
            && !scope.request_doc_id.is_empty()
            && !scope.request_commit_cid.is_empty()
            && scope.requester_did.is_none_or(|did| !did.is_empty()),
        "canonical replay scope is incomplete"
    );
    let request_commits = validate_replay_request(node, scope).await?;
    // None records the empty canonical view at capture time. A later current
    // read cannot turn that historical empty boundary into an unbounded scan.
    let Some(high_water) = boundary.canonical_through.as_ref() else {
        return Ok(Vec::new());
    };
    let mut cache = ReadCache::default();
    let high_water_header = load_header(
        ReadAccess::Node(node),
        &high_water.doc_id,
        scope.agent_did,
        scope.requester_did,
        &mut cache,
    )
    .await?;
    anyhow::ensure!(
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
    .context("canonical replay high-water header has no composite commit")?;
    anyhow::ensure!(
        high_water_commit.cid == high_water.commit_cid,
        "canonical replay high-water CID is not the physical header version"
    );

    let session_filter =
        session_scope_filter(scope.agent_did, scope.session_id, scope.requester_did);
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
        anyhow::ensure!(
            row.message.agent_did == scope.agent_did
                && row.message.requester_did.as_deref() == scope.requester_did
                && row.message.session_id == scope.session_id
                && i64::from(row.message.sequence) <= high_water.sequence,
            "canonical replay header crossed its scoped high-water view"
        );
        anyhow::ensure!(
            ids.insert(row.doc_id.clone())
                && sequences.insert(row.message.sequence)
                && keys.insert(row.message.message_key.clone()),
            "canonical replay header has a physical, sequence, or key twin"
        );
        cache.headers.insert(row.doc_id.clone(), row.clone());
    }
    anyhow::ensure!(
        ids.contains(&high_water.doc_id),
        "canonical replay high-water header is absent from its bounded view"
    );

    let mut candidates = Vec::new();
    for row in headers.into_iter().filter(|row| {
        row.message.request_doc_id.as_deref() == Some(scope.request_doc_id)
            && row.message.role == MessageRole::Assistant
            && matches!(
                &row.message.publication,
                MessagePublication::RequestExecution { .. }
            )
    }) {
        let reconstructed = reconstruct_scoped_message_with_facts(
            ReadAccess::Node(node),
            &row.doc_id,
            scope.agent_did,
            scope.requester_did,
            &mut cache,
        )
        .await?;
        let Some(coordinate) = provider_coordinate_for_candidate(&reconstructed)? else {
            // An authored assistant belongs in permissive history, but is not
            // a provider continuation candidate merely because its role and
            // publication match a provider-owned assistant.
            continue;
        };
        let origin = replay_origin_for_candidate(node, scope, &request_commits, coordinate).await?;
        candidates.push(CanonicalAssistantCandidate {
            header_doc_id: row.doc_id,
            sequence: row.message.sequence,
            message: reconstructed.message,
            origin,
            coordinate,
        });
    }
    Ok(candidates)
}

/// Resolve one carried provider tag against the same physically bounded
/// canonical candidate view used at checkpoint restore. Keep every matching
/// physical header: zero and multiple rows are distinct missing/ambiguous
/// outcomes for the pure replay owner, never an arbitrary winner.
pub(crate) async fn resolve_current_replay_tag(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
    tag: &gents_loop::claude_messages_body::ReplayTag,
) -> Result<Vec<gents_loop::claude_messages_body::ResolvedReplayEvidence>> {
    use gents_loop::claude_messages_body::{reasoning_witness, ResolvedReplayEvidence};

    anyhow::ensure!(
        tag.request_doc_id == scope.request_doc_id,
        "canonical replay tag belongs to another physical request"
    );
    let OutputSource::ProviderTurn {
        scope: tag_scope,
        turn_index,
        attempt,
    } = &tag.source
    else {
        anyhow::bail!("canonical replay tag is not a provider source");
    };
    let candidates = load_current_request_assistant_candidates(node, scope, boundary).await?;
    candidates
        .into_iter()
        .filter(|candidate| {
            candidate.coordinate.scope == *tag_scope
                && candidate.coordinate.turn_index == *turn_index
                && candidate.coordinate.attempt == *attempt
        })
        .map(|candidate| {
            let gents_protocol::message::Message::Assistant { content, .. } = candidate.message
            else {
                anyhow::bail!("canonical replay candidate is not an assistant message");
            };
            Ok(ResolvedReplayEvidence {
                origin: candidate.origin,
                reasoning: reasoning_witness(&content),
            })
        })
        .collect()
}

async fn validate_replay_request(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
) -> Result<BTreeSet<String>> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id session_id agent_did requester_did }} }}"#,
        crate::graphql::escape_graphql_string(scope.request_doc_id)
    );
    let response = ReadAccess::Node(node)
        .query(&query, "load_canonical_replay_request")
        .await?;
    let rows = rows_value(&response, "AgentRequest")?;
    anyhow::ensure!(
        rows.len() == 1,
        "canonical replay request is missing or ambiguous"
    );
    let row = &rows[0];
    anyhow::ensure!(
        required_row_str(row, "_docID")? == scope.request_doc_id
            && required_row_str(row, "request_id")? == scope.request_id
            && required_row_str(row, "session_id")? == scope.session_id
            && required_row_str(row, "agent_did")? == scope.agent_did
            && row.get("requester_did").is_some()
            && row["requester_did"].as_str() == scope.requester_did,
        "canonical replay request crossed its physical principal/session scope"
    );
    let commits = crate::graphql::composite_commits(
        node,
        scope.request_doc_id,
        "canonical replay request commits",
    )
    .await?
    .into_iter()
    .map(|commit| commit.cid)
    .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        commits.contains(scope.request_commit_cid),
        "canonical replay request boundary CID is not a commit of its physical request"
    );
    Ok(commits)
}

fn required_row_str<'a>(row: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("canonical replay row omitted {field}"))
}

fn provider_coordinate_for_candidate(
    reconstructed: &ReconstructedScopedMessage,
) -> Result<Option<CanonicalProviderCoordinate>> {
    let header = &reconstructed.header.message;
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
            close.agent_did == header.agent_did
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
                    capture_scope.kind
                        == gents_protocol::rendered_request::CaptureScopeKind::Inference,
                    "canonical replay assistant references a non-inference provider scope"
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

async fn replay_origin_for_candidate(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    request_commits: &BTreeSet<String>,
    coordinate: CanonicalProviderCoordinate,
) -> Result<gents_loop::claude_messages_body::ReplayOrigin> {
    use gents_loop::claude_messages_body::ReplayOrigin;

    let CanonicalProviderCoordinate {
        scope: capture_scope,
        turn_index,
        attempt,
    } = coordinate;
    let capture_scope_label = capture_scope.to_string();
    let capture_key = gents_loop::rendered_request::capture_key(
        scope.agent_did,
        scope.session_id,
        scope.request_doc_id,
        &capture_scope_label,
        usize::try_from(turn_index)?,
        attempt,
    )?;
    // Query by the existing exact key, but do not filter by the remaining
    // tuple: a contradictory row under that key must be detected, not hidden.
    let query = format!(
        r#"{{ RenderedRequest(filter: {{ capture_key: {{ _eq: "{}" }} }}, limit: 2) {{ capture_key capture_version request_doc_id request_commit_cid request_id session_id agent_did requester_did capture_scope turn_index attempt source }} }}"#,
        crate::graphql::escape_graphql_string(&capture_key)
    );
    let response = ReadAccess::Node(node)
        .query(&query, "load_canonical_replay_capture")
        .await?;
    let captures = rows_value(&response, "RenderedRequest")?;
    let capture = match captures.as_slice() {
        [] => return Ok(ReplayOrigin::Missing),
        [capture] => capture,
        _ => return Ok(ReplayOrigin::Ambiguous),
    };
    anyhow::ensure!(
        required_row_str(capture, "capture_key")? == capture_key
            && required_row_str(capture, "request_doc_id")? == scope.request_doc_id
            && required_row_str(capture, "request_id")? == scope.request_id
            && required_row_str(capture, "session_id")? == scope.session_id
            && required_row_str(capture, "agent_did")? == scope.agent_did
            && required_row_str(capture, "requester_did")? == scope.requester_did.unwrap_or("")
            && required_row_str(capture, "capture_scope")? == capture_scope_label
            && capture["capture_version"].as_u64()
                == Some(u64::from(gents_protocol::rendered_request::CAPTURE_VERSION))
            && capture["turn_index"].as_u64() == Some(u64::from(turn_index))
            && capture["attempt"].as_u64() == Some(u64::from(attempt)),
        "canonical replay capture disagrees with its exact provider close coordinate"
    );
    anyhow::ensure!(
        request_commits.contains(required_row_str(capture, "request_commit_cid")?),
        "canonical replay capture request CID is not a commit of its physical request"
    );
    let source: gents_protocol::rendered_request::RenderedRequestSource = serde_json::from_value(
        capture
            .get("source")
            .cloned()
            .context("capture has no source")?,
    )
    .context("canonical replay capture has malformed source")?;
    Ok(match source {
        gents_protocol::rendered_request::RenderedRequestSource::ClaudeCliSubscription => {
            ReplayOrigin::ClaudeSubscription
        }
        _ => ReplayOrigin::Foreign,
    })
}

/// The owned loop rebuilds context and prompt from admission input. Other
/// publications belonging to that request (notably tool results and background
/// notifications) remain history. This binds PromptAssembly.CurrentInput;
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
        && ["prompt", "context"].into_iter().any(|key| {
            header.message_key == super::canonical_rows::authored_message_key(request_doc_id, key)
        })
}

pub(super) async fn load_sequenced_messages(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    exclude_request_doc_id: Option<&str>,
) -> Result<Vec<SequencedMessage>> {
    let session_scope = session_scope_filter(agent_did, session_id, requester_did);
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
    let mut cache = ReadCache::default();
    let mut sequences = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for row in &headers {
        anyhow::ensure!(
            row.message.session_id == session_id
                && row.message.agent_did == agent_did
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
            agent_did,
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
        let provider_source = if reconstructed.header.message.role == MessageRole::Assistant
            && reconstructed.header.message.outcome == OutputOutcome::Complete
            && matches!(
                reconstructed.header.message.publication,
                MessagePublication::RequestExecution { .. }
            ) {
            provider_coordinate_for_candidate(&reconstructed)?.map(|coordinate| {
                gents_loop::claude_messages_body::ReplayTag {
                    request_doc_id: reconstructed
                        .header
                        .message
                        .request_doc_id
                        .clone()
                        .expect("provider coordinate requires request document"),
                    source: OutputSource::ProviderTurn {
                        scope: coordinate.scope,
                        turn_index: coordinate.turn_index,
                        attempt: coordinate.attempt,
                    },
                }
            })
        } else {
            None
        };
        messages.push(SequencedMessage {
            provider_source,
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
                        "prompt" | "context" | "assistant" => {
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
                    row.message.message_key = super::super::canonical_rows::authored_message_key(
                        &input.request,
                        &input.kind,
                    );
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
                agent_did: "did:test:agent".into(),
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
}
