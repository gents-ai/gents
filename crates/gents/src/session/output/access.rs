use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::OutputSource;
use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::super::canonical_rows::{request_output_segments_query, AGENT_OUTPUT_SEGMENT_FIELDS};
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};

#[derive(Clone, Copy)]
pub(super) enum ReadAccess<'a, 'txn> {
    Node(&'a EmbeddedNode),
    Txn(&'a ConfigApplyTxn<'txn>),
    Config(&'a ConfigAccess),
}

/// One scoped read only, never a replacement for an authoritative transaction.
/// Cache positive immutable facts, not missing dependencies: a later node query
/// can observe replication filling a gap. The caller fixes agent/requester scope.
#[derive(Default)]
pub(super) struct ReadCache {
    pub(super) headers: BTreeMap<String, super::super::canonical_rows::TranscriptMessageRow>,
    pub(super) requests: BTreeMap<String, Arc<RequestOutputRows>>,
    pub(super) sources:
        RapidHashMap<(String, OutputSource), Vec<super::super::canonical_rows::OutputSegmentRow>>,
    pub(super) source_reads: bool,
    /// Rows the caller read by an exact scoped query in the same transaction;
    /// each still passes coordinate validation before it enters `headers`.
    pub(super) observed: BTreeMap<String, super::super::canonical_rows::TranscriptMessageRow>,
    pub(super) sessions: Option<BTreeMap<String, SessionHeaderRows>>,
    /// One bounded bulk pass reconstructs every message of a request
    /// against the same scan. A reused scan that lacks a referenced close is
    /// dropped and re-read, so replication filling that gap is still observed.
    pub(super) reuse_node_scans: bool,
}

/// Indexes retain every occurrence, including contradictory observations of
/// one physical document. They must not turn canonical integrity checks into
/// last-writer-wins selection.
#[derive(Clone)]
pub(super) struct RequestOutputRows {
    rows: Vec<super::super::canonical_rows::OutputSegmentRow>,
    by_doc_id: RapidHashMap<String, Vec<usize>>,
    by_source: RapidHashMap<OutputSource, Vec<usize>>,
}

/// Unrelated malformed headers remain undecoded. Coordinate matches retain
/// their original order and every physical occurrence, even identical IDs.
pub(super) struct SessionHeaderRows {
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
    pub(super) fn coordinate_matches(
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

impl From<Vec<super::super::canonical_rows::OutputSegmentRow>> for RequestOutputRows {
    fn from(rows: Vec<super::super::canonical_rows::OutputSegmentRow>) -> Self {
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
    pub(super) fn push(&mut self, row: super::super::canonical_rows::OutputSegmentRow) {
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

    pub(super) fn with_doc_id(
        &self,
        doc_id: &str,
    ) -> impl Iterator<Item = &super::super::canonical_rows::OutputSegmentRow> {
        self.by_doc_id
            .get(doc_id)
            .into_iter()
            .flatten()
            .map(|&index| &self.rows[index])
    }

    pub(super) fn for_source(
        &self,
        source: &OutputSource,
    ) -> impl Iterator<Item = &super::super::canonical_rows::OutputSegmentRow> {
        self.by_source
            .get(source)
            .into_iter()
            .flatten()
            .map(|&index| &self.rows[index])
    }
}

impl ReadAccess<'_, '_> {
    pub(super) async fn request_segment_values(
        self,
        request_doc_id: &str,
        operation: &str,
    ) -> Result<Vec<serde_json::Value>> {
        if let Self::Txn(txn) = self {
            return Ok(txn
                .cursor_rows(
                    "AgentOutputSegment",
                    AGENT_OUTPUT_SEGMENT_FIELDS,
                    &super::super::canonical_rows::request_output_segments_filter(request_doc_id),
                )
                .await?);
        }
        let response = self
            .query(&request_output_segments_query(request_doc_id), operation)
            .await?;
        Ok(rows_value(&response, "AgentOutputSegment")?.to_vec())
    }

    pub(super) async fn query(self, document: &str, operation: &str) -> Result<serde_json::Value> {
        let response =
            match self {
                Self::Node(node) => {
                    crate::store::graphql::graphql_response_with_transaction_retry(
                        node, document, operation,
                    )
                    .await?
                }
                Self::Txn(txn) => return Ok(txn.execute(document).await?),
                Self::Config(access) => {
                    let value = access.execute(document).await?;
                    anyhow::ensure!(
                        value.get("errors").is_none_or(|errors| errors.is_null()
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
        Ok(crate::store::graphql::into_data_envelope(response.data))
    }
}

pub(super) fn rows_value<'a>(
    response: &'a serde_json::Value,
    collection: &str,
) -> Result<&'a Vec<serde_json::Value>> {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(serde_json::Value::as_array)
        .with_context(|| format!("canonical output query omitted {collection} rows"))
}
