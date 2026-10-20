//! Background owner of the `TranscriptChunk` projection. It runs off the
//! inference path: requests and messages publish exactly as before, and this
//! task embeds their settled text afterwards through DefraDB's `@embedding`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use identity::Did;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::chunk::chunk_text;
use super::plan::{pending, step, IndexMessage, SessionIndex};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::session::canonical_rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS};
use crate::session::TxnCanonicalReader;

pub const TRANSCRIPT_CHUNK: &str = "TranscriptChunk";
pub const TRANSCRIPT_INDEX_STATE: &str = "TranscriptIndexState";
pub const CHUNK_VECTOR_FIELD: &str = "embedding";

/// Messages one pass reads per session; longer backlogs continue next pass.
const MESSAGE_WINDOW: usize = if cfg!(test) { 3 } else { 100 };
/// Chunks per create mutation. DefraDB asks the provider for a mutation's
/// embeddings in one batched request.
const CHUNKS_PER_WRITE: usize = 64;
const SESSION_PAGE: usize = 100;
const DIRTY_DEBOUNCE: Duration = Duration::from_secs(2);
const FULL_PASS_INTERVAL: Duration = Duration::from_secs(30 * 60);
const RETRY_INITIAL: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionKey {
    pub node_did: String,
    pub session_id: String,
}

#[derive(Debug, Clone)]
struct SessionRow {
    key: SessionKey,
    requester_did: Option<String>,
}

/// What one pass over a session did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPass {
    pub indexed_messages: usize,
    pub deleted_messages: usize,
    pub pending: usize,
    /// More messages remain beyond this pass's window.
    pub more: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FullPass {
    pub sessions: usize,
    pub indexed_messages: usize,
    pub deleted_messages: usize,
    pub removed_sessions: usize,
}

/// `reader` is the runtime's node DID: the projection covers what that DID
/// can read through DefraDB ACP.
pub async fn run_transcript_indexer(
    node: Arc<EmbeddedNode>,
    reader: Option<Did>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut changes = crate::subscribe_collections(
        &node,
        [
            gents_protocol::schemas::AGENT_REQUEST_NAME,
            gents_protocol::schemas::AGENT_MESSAGE_NAME,
        ],
    )?;
    let request_collection = crate::trigger_engine::subscription_source::collection_ids(
        &node,
        [gents_protocol::schemas::AGENT_REQUEST_NAME],
    )?
    .remove(0);
    let mut dirty: BTreeSet<String> = BTreeSet::new();
    let mut dirty_requests: BTreeSet<String> = BTreeSet::new();
    let mut retry = RETRY_INITIAL;
    let mut full_pass_due = tokio::time::Instant::now();
    loop {
        if tokio::time::Instant::now() >= full_pass_due {
            match index_all_sessions(&node, &reader, &cancel).await {
                Ok(pass) => {
                    retry = RETRY_INITIAL;
                    full_pass_due = tokio::time::Instant::now() + FULL_PASS_INTERVAL;
                    tracing::info!(
                        sessions = pass.sessions,
                        indexed_messages = pass.indexed_messages,
                        deleted_messages = pass.deleted_messages,
                        removed_sessions = pass.removed_sessions,
                        "transcript index pass complete"
                    );
                }
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), retry_in = ?retry, "transcript index pass failed");
                    full_pass_due = tokio::time::Instant::now() + retry;
                    retry = (retry * 2).min(RETRY_MAX);
                }
            }
        }
        let debounce = tokio::time::sleep(if dirty.is_empty() && dirty_requests.is_empty() {
            full_pass_due.saturating_duration_since(tokio::time::Instant::now())
        } else {
            DIRTY_DEBOUNCE
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            batch = changes.recv() => {
                let batch = batch.context("transcript index subscription closed")?;
                if batch.resync_required {
                    full_pass_due = tokio::time::Instant::now();
                }
                for change in batch.changes {
                    if change.collection_id == request_collection {
                        dirty_requests.insert(change.doc_id);
                    } else {
                        dirty.insert(change.doc_id);
                    }
                }
            }
            _ = debounce => {
                if dirty.is_empty() && dirty_requests.is_empty() {
                    continue;
                }
                let messages = std::mem::take(&mut dirty);
                let requests = std::mem::take(&mut dirty_requests);
                if let Err(error) = index_changed(&node, &reader, &messages, &requests, &cancel).await {
                    tracing::warn!(error = %format!("{error:#}"), "transcript index update failed; the next pass retries");
                    full_pass_due = full_pass_due.min(tokio::time::Instant::now() + retry);
                    retry = (retry * 2).min(RETRY_MAX);
                }
            }
        }
    }
}

async fn index_changed(
    node: &Arc<EmbeddedNode>,
    reader: &Option<Did>,
    message_doc_ids: &BTreeSet<String>,
    request_doc_ids: &BTreeSet<String>,
    cancel: &CancellationToken,
) -> Result<()> {
    let sessions = ConfigAccess::transact_local_readonly(
        node,
        reader.clone(),
        "retrieval.index.changes",
        |txn| {
            Box::pin(async move {
                let mut sessions = BTreeSet::new();
                for (collection, ids) in [
                    (gents_protocol::schemas::AGENT_MESSAGE_NAME, message_doc_ids),
                    (gents_protocol::schemas::AGENT_REQUEST_NAME, request_doc_ids),
                ] {
                    if ids.is_empty() {
                        continue;
                    }
                    let value = txn
                        .execute(&format!(
                        "{{{collection}(filter: {{_docID: {{_in: {}}}}}) {{node_did session_id}}}}",
                        string_list(ids)
                    ))
                        .await?;
                    for row in rows(&value, collection)? {
                        if let (Some(node_did), Some(session_id)) =
                            (row["node_did"].as_str(), row["session_id"].as_str())
                        {
                            sessions.insert(SessionKey {
                                node_did: node_did.to_owned(),
                                session_id: session_id.to_owned(),
                            });
                        }
                    }
                }
                Ok(sessions)
            })
        },
    )
    .await?;
    for key in sessions {
        if cancel.is_cancelled() {
            return Ok(());
        }
        index_session_to_end(node, reader, &key, cancel).await?;
    }
    Ok(())
}

/// Indexes every session (newest first) and removes the projection of
/// sessions that no longer exist. Each session resumes from its durable
/// watermark, so an interrupted backfill continues where it stopped.
pub async fn index_all_sessions(
    node: &Arc<EmbeddedNode>,
    reader: &Option<Did>,
    cancel: &CancellationToken,
) -> Result<FullPass> {
    let mut pass = FullPass::default();
    let mut after: Option<(String, String)> = None;
    loop {
        let page = session_page(node, reader, after.as_ref()).await?;
        let exhausted = page.len() < SESSION_PAGE;
        for (created_at, session) in page {
            if cancel.is_cancelled() {
                return Ok(pass);
            }
            let result = index_session_to_end(node, reader, &session.key, cancel).await?;
            pass.sessions += 1;
            pass.indexed_messages += result.indexed_messages;
            pass.deleted_messages += result.deleted_messages;
            after = Some((created_at, session.key.session_id.clone()));
        }
        if exhausted {
            break;
        }
    }
    pass.removed_sessions = remove_orphaned_sessions(node, reader).await?;
    Ok(pass)
}

async fn index_session_to_end(
    node: &Arc<EmbeddedNode>,
    reader: &Option<Did>,
    key: &SessionKey,
    cancel: &CancellationToken,
) -> Result<SessionPass> {
    let mut total = SessionPass::default();
    loop {
        let pass = index_session(node, reader, key).await?;
        total.indexed_messages += pass.indexed_messages;
        total.deleted_messages += pass.deleted_messages;
        total.pending = pass.pending;
        if !pass.more || cancel.is_cancelled() {
            return Ok(total);
        }
    }
}

struct SessionWork {
    session: Option<SessionRow>,
    window: SessionIndex,
    window_end: Option<u32>,
    messages: Vec<IndexMessage>,
    chunks: BTreeMap<u32, MessageChunks>,
}

struct MessageChunks {
    message_doc_id: String,
    chunks: Vec<String>,
}

/// One indexer `step` over the session's window `[watermark, window_end]`.
/// Chunk writes and deletes commit before the watermark
/// (`crash_replay_converges`).
pub async fn index_session(
    node: &Arc<EmbeddedNode>,
    reader: &Option<Did>,
    key: &SessionKey,
) -> Result<SessionPass> {
    let work = read_session_work(node, reader, key).await?;
    let Some(session) = work.session else {
        delete_session_projection(node, key).await?;
        return Ok(SessionPass::default());
    };
    let result = step(&work.messages, &work.window);
    let more = work
        .window_end
        .is_some_and(|end| result.next.watermark > end);
    let pending_after = pending(&work.messages, &result.next);

    if !result.delete.is_empty() {
        ConfigAccess::write_local(
            node,
            "retrieval.index.delete_chunks",
            &format!(
                "mutation {{delete_{TRANSCRIPT_CHUNK}(filter: {{{}, sequence: {{_in: [{}]}}}}) {{_docID}}}}",
                session_scope(key),
                join_numbers(&result.delete)
            ),
        )
        .await?;
    }
    let inputs: Vec<String> = result
        .index
        .iter()
        .map(|sequence| chunk_inputs(&session, *sequence, &work.chunks[sequence]))
        .collect();
    let mut batch: Vec<String> = Vec::new();
    let mut batch_chunks = 0;
    for (message_inputs, sequence) in inputs.into_iter().zip(&result.index) {
        let count = work.chunks[sequence].chunks.len().max(1);
        if !batch.is_empty() && batch_chunks + count > CHUNKS_PER_WRITE {
            write_chunks(node, &batch).await?;
            batch.clear();
            batch_chunks = 0;
        }
        batch.push(message_inputs);
        batch_chunks += count;
    }
    if !batch.is_empty() {
        write_chunks(node, &batch).await?;
    }
    write_state(node, key, result.next.watermark, pending_after).await?;
    Ok(SessionPass {
        indexed_messages: result.index.len(),
        deleted_messages: result.delete.len(),
        pending: pending_after,
        more,
    })
}

async fn read_session_work(
    node: &Arc<EmbeddedNode>,
    reader: &Option<Did>,
    key: &SessionKey,
) -> Result<SessionWork> {
    ConfigAccess::transact_local_readonly(node, reader.clone(), "retrieval.index.read", |txn| {
        Box::pin(async move {
            let session = rows(
                &txn.execute(&format!(
                    "{{AgentSession(filter: {{node_did: {{_eq: {}}}, session_id: {{_eq: {}}}}}, limit: 1) {{node_did session_id requester_did}}}}",
                    quoted(&key.node_did),
                    quoted(&key.session_id)
                ))
                .await?,
                "AgentSession",
            )?
            .first()
            .map(|row| SessionRow {
                key: key.clone(),
                requester_did: row["requester_did"].as_str().map(str::to_owned),
            });
            let state = txn
                .execute(&format!(
                    "{{{TRANSCRIPT_INDEX_STATE}(filter: {{{}}}, limit: 1) {{watermark}}}}",
                    session_scope(key)
                ))
                .await?;
            let watermark = rows(&state, TRANSCRIPT_INDEX_STATE)?
                .first()
                .and_then(|row| row["watermark"].as_u64())
                .map_or(super::plan::FIRST_SEQUENCE, |w| w as u32);
            let Some(session) = session else {
                return Ok(SessionWork {
                    session: None,
                    window: SessionIndex::initial(),
                    window_end: None,
                    messages: Vec::new(),
                    chunks: BTreeMap::new(),
                });
            };
            let requester = session.requester_did.as_deref();
            // The window is a sequence range with no limit: the pinned planner
            // can bound an ordered index scan before its residual filters, so a
            // limited `sequence >= watermark` read returns short windows.
            let window_end = watermark + MESSAGE_WINDOW as u32 - 1;
            let headers = rows(
                &txn.execute(&format!(
                    "{{AgentMessage(filter: {{node_did: {{_eq: {}}}, requester_did: {{_eq: {}}}, session_id: {{_eq: {}}}, sequence: {{_ge: {watermark}}}, _and: [{{sequence: {{_le: {window_end}}}}}]}}, order: {{sequence: ASC}}) {{{AGENT_MESSAGE_FIELDS}}}}}",
                    quoted(&key.node_did),
                    requester.map(quoted).unwrap_or_else(|| "null".into()),
                    quoted(&key.session_id),
                ))
                .await?,
                "AgentMessage",
            )?
            .iter()
            .map(decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?;
            let window_end = Some(window_end);
            let settled_requests = terminal_requests(txn, &headers).await?;
            let messages: Vec<IndexMessage> = headers
                .iter()
                .map(|header| IndexMessage {
                    sequence: header.message.sequence,
                    settled: header
                        .message
                        .request_doc_id
                        .as_ref()
                        .is_none_or(|request| settled_requests.contains(request)),
                })
                .collect();
            let bound = window_end
                .map(|end| format!(", _and: [{{sequence: {{_le: {end}}}}}]"))
                .unwrap_or_default();
            let indexed: BTreeSet<u32> = rows(
                &txn.execute(&format!(
                    "{{{TRANSCRIPT_CHUNK}(filter: {{{}, sequence: {{_ge: {watermark}}}{bound}}}) {{sequence}}}}",
                    session_scope(key)
                ))
                .await?,
                TRANSCRIPT_CHUNK,
            )?
            .iter()
            .filter_map(|row| row["sequence"].as_u64().map(|s| s as u32))
            .collect();
            let window = SessionIndex { indexed, watermark };
            let planned = step(&messages, &window);
            let mut reader = TxnCanonicalReader::new(txn, &key.node_did, requester);
            reader.observe_headers(&headers);
            let mut chunks = BTreeMap::new();
            for header in &headers {
                if !planned.index.contains(&header.message.sequence) {
                    continue;
                }
                let (_, message) = reader.load_message(&header.doc_id).await?;
                let text = gents_protocol::transcript::present_message(&message).body_markdown;
                chunks.insert(
                    header.message.sequence,
                    MessageChunks {
                        message_doc_id: header.doc_id.clone(),
                        chunks: chunk_text(&text),
                    },
                );
            }
            Ok(SessionWork {
                session: Some(session),
                window,
                window_end,
                messages,
                chunks,
            })
        })
    })
    .await
}

async fn terminal_requests(
    txn: &ConfigApplyTxn<'_>,
    headers: &[crate::session::canonical_rows::TranscriptMessageRow],
) -> Result<BTreeSet<String>> {
    let requests: BTreeSet<&str> = headers
        .iter()
        .filter_map(|header| header.message.request_doc_id.as_deref())
        .collect();
    if requests.is_empty() {
        return Ok(BTreeSet::new());
    }
    let value = txn
        .execute(&format!(
            "{{AgentRequest(filter: {{_docID: {{_in: {}}}}}) {{_docID lifecycle_state}}}}",
            string_list(requests)
        ))
        .await?;
    Ok(rows(&value, "AgentRequest")?
        .iter()
        .filter(|row| RequestLifecycleState::is_terminal_str(row["lifecycle_state"].as_str()))
        .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
        .collect())
}

/// Create inputs for one message's chunks. A message with no text gets one
/// marker row passing `embedding: null`, so DefraDB embeds nothing and the
/// message still counts as indexed.
fn chunk_inputs(session: &SessionRow, sequence: u32, message: &MessageChunks) -> String {
    let requester = session
        .requester_did
        .as_deref()
        .map(quoted)
        .unwrap_or_else(|| "null".into());
    let row = |ordinal: usize, text: Option<&str>| {
        format!(
            "{{chunk_id: {}, node_did: {}, requester_did: {requester}, session_id: {}, message_doc_id: {}, sequence: {sequence}, ordinal: {ordinal}, text: {}{}}}",
            quoted(&format!("{}:{ordinal}", message.message_doc_id)),
            quoted(&session.key.node_did),
            quoted(&session.key.session_id),
            quoted(&message.message_doc_id),
            text.map(quoted).unwrap_or_else(|| "null".into()),
            if text.is_none() { ", embedding: null" } else { "" }
        )
    };
    if message.chunks.is_empty() {
        return row(0, None);
    }
    message
        .chunks
        .iter()
        .enumerate()
        .map(|(ordinal, text)| row(ordinal, Some(text)))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn write_chunks(node: &Arc<EmbeddedNode>, inputs: &[String]) -> Result<()> {
    ConfigAccess::write_local(
        node,
        "retrieval.index.create_chunks",
        &format!(
            "mutation {{create_{TRANSCRIPT_CHUNK}(input: [{}]) {{_docID}}}}",
            inputs.join(", ")
        ),
    )
    .await
    .context("writing transcript chunks; their embeddings come from the TranscriptChunk @embedding endpoint")?;
    Ok(())
}

async fn write_state(
    node: &Arc<EmbeddedNode>,
    key: &SessionKey,
    watermark: u32,
    pending: usize,
) -> Result<()> {
    let now = quoted(&chrono::Utc::now().to_rfc3339());
    ConfigAccess::write_local(
        node,
        "retrieval.index.write_state",
        &format!(
            "mutation {{upsert_{TRANSCRIPT_INDEX_STATE}(filter: {{{}}}, add: {{node_did: {}, session_id: {}, watermark: {watermark}, pending: {pending}, updated_at: {now}}}, update: {{watermark: {watermark}, pending: {pending}, updated_at: {now}}}) {{_docID}}}}",
            session_scope(key),
            quoted(&key.node_did),
            quoted(&key.session_id)
        ),
    )
    .await?;
    Ok(())
}

async fn delete_session_projection(node: &Arc<EmbeddedNode>, key: &SessionKey) -> Result<()> {
    for collection in [TRANSCRIPT_CHUNK, TRANSCRIPT_INDEX_STATE] {
        ConfigAccess::write_local(
            node,
            "retrieval.index.delete_session",
            &format!(
                "mutation {{delete_{collection}(filter: {{{}}}) {{_docID}}}}",
                session_scope(key)
            ),
        )
        .await?;
    }
    Ok(())
}

async fn remove_orphaned_sessions(node: &Arc<EmbeddedNode>, reader: &Option<Did>) -> Result<usize> {
    let orphans = ConfigAccess::transact_local_readonly(
        node,
        reader.clone(),
        "retrieval.index.orphans",
        |txn| {
            Box::pin(async move {
                let states = txn
                    .execute(&format!(
                        "{{{TRANSCRIPT_INDEX_STATE} {{node_did session_id}}}}"
                    ))
                    .await?;
                let mut orphans = Vec::new();
                for state in rows(&states, TRANSCRIPT_INDEX_STATE)? {
                    let (Some(node_did), Some(session_id)) =
                        (state["node_did"].as_str(), state["session_id"].as_str())
                    else {
                        continue;
                    };
                    let key = SessionKey {
                        node_did: node_did.to_owned(),
                        session_id: session_id.to_owned(),
                    };
                    let found = txn
                        .execute(&format!(
                            "{{AgentSession(filter: {{{}}}, limit: 1) {{_docID}}}}",
                            session_scope(&key)
                        ))
                        .await?;
                    if rows(&found, "AgentSession")?.is_empty() {
                        orphans.push(key);
                    }
                }
                Ok(orphans)
            })
        },
    )
    .await?;
    for key in &orphans {
        delete_session_projection(node, key).await?;
    }
    Ok(orphans.len())
}

async fn session_page(
    node: &Arc<EmbeddedNode>,
    reader: &Option<Did>,
    after: Option<&(String, String)>,
) -> Result<Vec<(String, SessionRow)>> {
    let filter = after
        .map(|(created_at, session_id)| {
            format!(
                "(filter: {{_or: [{{created_at: {{_lt: {created}}}}}, {{created_at: {{_eq: {created}}}, session_id: {{_lt: {id}}}}}]}}, ",
                created = quoted(created_at),
                id = quoted(session_id)
            )
        })
        .unwrap_or_else(|| "(".into());
    ConfigAccess::transact_local_readonly(node, reader.clone(), "retrieval.index.sessions", |txn| {
        let filter = filter.clone();
        Box::pin(async move {
            let value = txn
                .execute(&format!(
                    "{{AgentSession{filter}order: [{{created_at: DESC}}, {{session_id: DESC}}], limit: {SESSION_PAGE}) {{node_did session_id requester_did created_at}}}}"
                ))
                .await?;
            rows(&value, "AgentSession")?
                .iter()
                .map(|row| {
                    Ok((
                        row["created_at"].as_str().context("session has no creation time")?.to_owned(),
                        SessionRow {
                            key: SessionKey {
                                node_did: row["node_did"].as_str().context("session has no node DID")?.to_owned(),
                                session_id: row["session_id"].as_str().context("session has no ID")?.to_owned(),
                            },
                            requester_did: row["requester_did"].as_str().map(str::to_owned),
                        },
                    ))
                })
                .collect()
        })
    })
    .await
}

fn session_scope(key: &SessionKey) -> String {
    format!(
        "node_did: {{_eq: {}}}, session_id: {{_eq: {}}}",
        quoted(&key.node_did),
        quoted(&key.session_id)
    )
}

fn rows<'a>(value: &'a Value, collection: &str) -> Result<&'a Vec<Value>> {
    value["data"][collection]
        .as_array()
        .with_context(|| format!("{collection} query returned no rows: {value}"))
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", escape_graphql_string(value))
}

fn string_list<S: AsRef<str>>(values: impl IntoIterator<Item = S>) -> String {
    format!(
        "[{}]",
        values
            .into_iter()
            .map(|value| quoted(value.as_ref()))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn join_numbers(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
