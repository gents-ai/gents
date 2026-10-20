use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::embedding::vector_literal;
use super::indexer::{CHUNK_VECTOR_FIELD, TRANSCRIPT_CHUNK, TRANSCRIPT_INDEX_STATE};
use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;

/// Index candidates read per requested hit. Hits whose source does not
/// materialize for the caller are dropped after the DiskANN query, so the
/// query over-fetches.
pub(crate) const CANDIDATES_PER_HIT: usize = 4;
pub(crate) const MAX_CANDIDATES: usize = 400;

#[derive(Debug, Clone)]
pub(crate) struct ChunkCandidate {
    pub(crate) node_did: String,
    pub(crate) session_id: String,
    pub(crate) message_doc_id: String,
    pub(crate) sequence: u32,
    pub(crate) ordinal: u32,
    pub(crate) text: String,
    pub(crate) score: f64,
}

/// The DiskANN query for the chunks nearest `vector`. Without a filter it
/// routes through the vector index (one `SIMILARITY` field, a single
/// descending order on its alias, a limit); a session filter keeps the index
/// and widens its candidate pool until the page fills.
pub(crate) fn candidates_query(vector: &[f64], session_id: Option<&str>, limit: usize) -> String {
    let filter = session_id
        .map(|id| {
            format!(
                "filter: {{session_id: {{_eq: \"{}\"}}}}, ",
                escape_graphql_string(id)
            )
        })
        .unwrap_or_default();
    format!(
        "{{{TRANSCRIPT_CHUNK}({filter}order: {{_alias: {{similarity: DESC}}}}, limit: {limit}) {{node_did session_id message_doc_id sequence ordinal text similarity: SIMILARITY({CHUNK_VECTOR_FIELD}: {{vector: {}}})}}}}",
        vector_literal(vector)
    )
}

pub(crate) async fn transcript_candidates(
    txn: &ConfigApplyTxn<'_>,
    vector: &[f64],
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<ChunkCandidate>> {
    let value = txn
        .execute(&candidates_query(vector, session_id, limit))
        .await?;
    value["data"][TRANSCRIPT_CHUNK]
        .as_array()
        .with_context(|| format!("transcript chunk query returned no rows: {value}"))?
        .iter()
        .filter(|row| row["text"].is_string())
        .map(|row| {
            Ok(ChunkCandidate {
                node_did: field(row, "node_did")?,
                session_id: field(row, "session_id")?,
                message_doc_id: field(row, "message_doc_id")?,
                sequence: row["sequence"].as_u64().context("chunk has no sequence")? as u32,
                ordinal: row["ordinal"].as_u64().context("chunk has no ordinal")? as u32,
                text: field(row, "text")?,
                score: row["similarity"]
                    .as_f64()
                    .context("chunk has no similarity")?,
            })
        })
        .collect()
}

/// Index freshness to report beside semantic hits.
pub(crate) async fn index_status(txn: &ConfigApplyTxn<'_>) -> Result<Value> {
    let value = txn
        .execute(&format!(
            "{{pending: COUNT({TRANSCRIPT_INDEX_STATE}: {{filter: {{pending: {{_gt: 0}}}}}}) indexed: COUNT({TRANSCRIPT_INDEX_STATE}: {{}})}}"
        ))
        .await?;
    Ok(json!({
        "indexed_sessions": value["data"]["indexed"],
        "sessions_with_pending_messages": value["data"]["pending"],
    }))
}

fn field(row: &Value, name: &str) -> Result<String> {
    Ok(row[name]
        .as_str()
        .with_context(|| format!("chunk has no {name}"))?
        .to_owned())
}
