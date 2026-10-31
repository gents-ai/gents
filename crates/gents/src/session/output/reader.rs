use anyhow::Result;
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{OutputSource, PayloadRef};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::super::canonical_rows::{
    decode_output_segment_row, decode_scoped_request_output_segments,
    decode_transcript_message_row, AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS,
};
use super::super::query::session_scope_filter;
use super::access::{rows_value, ReadAccess, ReadCache};
use super::reconstruct::{reconstruct_scoped_message, source_rows};
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};

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
        headers: &[super::super::canonical_rows::TranscriptMessageRow],
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
    ) -> Result<Vec<super::super::canonical_rows::OutputSegmentRow>> {
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
            .map(|did| format!(r#""{}""#, crate::store::graphql::escape_graphql_string(did)))
            .unwrap_or_else(|| "null".into());
        let scope = format!(
            r#"node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }}"#,
            crate::store::graphql::escape_graphql_string(self.node_did)
        );
        let query = format!(
            r#"{{
            created_output: AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }}, {scope} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
            created_header: AgentMessage(filter: {{ _docID: {{ _eq: "{}" }}, {scope} }}) {{ {AGENT_MESSAGE_FIELDS} }}
        }}"#,
            crate::store::graphql::escape_graphql_string(output_doc_id),
            crate::store::graphql::escape_graphql_string(header_doc_id)
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
    let values = access
        .request_segment_values(request_doc_id, "load_canonical_payload")
        .await?;
    let rows = decode_scoped_request_output_segments(&values, node_did, None, requester_did)?;
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
) -> Result<Vec<super::super::canonical_rows::TranscriptMessageRow>> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "request header lookup requires request document id"
    );
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{ AgentMessage(filter: {{ {scope}, request_doc_id: {{ _eq: "{}" }} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        crate::store::graphql::escape_graphql_string(request_doc_id)
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
) -> Result<Vec<super::super::canonical_rows::OutputSegmentRow>> {
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
