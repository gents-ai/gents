use super::rows::dedupe_paths;
use super::*;
use crate::graphql::graphql_response_with_transaction_retry as execute_query_timed;
use rapidhash::RapidHashSet;
use std::collections::BTreeSet;

type Result<T, E = SessionError> = std::result::Result<T, E>;

#[derive(Serialize)]
struct FilePathDelta<'a> {
    version: u32,
    paths: &'a [String],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFilePathDelta {
    version: u32,
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredFilePaths {
    Cumulative(Vec<String>),
    Delta(StoredFilePathDelta),
}

#[derive(Deserialize)]
struct PromptCompactionRow {
    compaction_key: String,
    sequence: u32,
    summary: String,
    messages_compacted: u32,
    #[serde(default)]
    compacted_through_sequence: Option<u32>,
}

#[derive(Clone, Deserialize)]
pub(super) struct CompactionGenerationRow {
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null"
    )]
    compaction_key: String,
    sequence: u32,
    summary: String,
    messages_compacted: u32,
    #[serde(default)]
    compacted_through_sequence: Option<u32>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null"
    )]
    files_read: String,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null"
    )]
    files_modified: String,
    #[serde(default)]
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    #[serde(default)]
    original_tokens: i64,
    #[serde(default)]
    compacted_tokens: i64,
    #[serde(default)]
    created_at: String,
}

pub(super) trait CompactionProjection {
    fn key(&self) -> &str;
    fn sequence(&self) -> u32;
    fn summary(&self) -> &str;
    fn messages_compacted(&self) -> u32;
    fn cursor(&self) -> Option<u32>;
}

impl CompactionProjection for PromptCompactionRow {
    fn key(&self) -> &str {
        &self.compaction_key
    }
    fn sequence(&self) -> u32 {
        self.sequence
    }
    fn summary(&self) -> &str {
        &self.summary
    }
    fn messages_compacted(&self) -> u32 {
        self.messages_compacted
    }
    fn cursor(&self) -> Option<u32> {
        self.compacted_through_sequence
    }
}

impl CompactionProjection for CompactionGenerationRow {
    fn key(&self) -> &str {
        &self.compaction_key
    }
    fn sequence(&self) -> u32 {
        self.sequence
    }
    fn summary(&self) -> &str {
        &self.summary
    }
    fn messages_compacted(&self) -> u32 {
        self.messages_compacted
    }
    fn cursor(&self) -> Option<u32> {
        self.compacted_through_sequence
    }
}

pub fn compaction_key(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    sequence: u32,
) -> String {
    format!(
        "compaction:{}",
        serde_json::to_string(&(node_did, session_id, requester_did, sequence))
            .expect("session scope serializes")
    )
}

pub(super) fn validate_compaction_chain<T>(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    rows: &[T],
) -> Result<(), SessionError>
where
    T: CompactionProjection,
{
    let mut prior_cursor = None;
    for (index, row) in rows.iter().enumerate() {
        let expected_sequence =
            u32::try_from(index + 1).map_err(SessionError::CompactionSequenceOverflow)?;
        let expected_key = compaction_key(node_did, session_id, requester_did, expected_sequence);
        if row.sequence() != expected_sequence || row.key() != expected_key {
            return Err(SessionError::AmbiguousCompactionChain {
                session_id: session_id.to_owned(),
                expected_key,
                found_key: row.key().to_owned(),
                sequence: row.sequence(),
            });
        }
        let cursor = row
            .cursor()
            .ok_or_else(|| SessionError::CompactionMissingCursor {
                session_id: session_id.to_owned(),
                sequence: row.sequence(),
            })?;
        if prior_cursor.is_some_and(|prior| cursor <= prior) {
            return Err(SessionError::CompactionCursorRegression {
                session_id: session_id.to_owned(),
                sequence: row.sequence(),
            });
        }
        prior_cursor = Some(cursor);
    }
    Ok(())
}

fn compaction_generation<T>(rows: &[T]) -> Result<String>
where
    T: CompactionProjection,
{
    let projection = rows
        .iter()
        .map(|row| {
            serde_json::json!([
                row.key(),
                row.sequence(),
                row.summary(),
                row.messages_compacted(),
                row.cursor()
            ])
        })
        .collect::<Vec<_>>();
    Ok(crate::rendered_request::sha256_canonical_json(
        &serde_json::json!(projection),
    )?)
}

pub(crate) async fn load_prompt_compaction_state(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
) -> Result<PromptCompactionState> {
    let scope = super::query::session_scope_filter(node_did, session_id, requester_did);
    // A background request is claimed against an immutable transcript high
    // water mark. A compaction produced later may only shape that request when
    // its cumulative canonical cursor is itself inside the claimed snapshot.
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ {scope} }},
                order: {{ sequence: ASC }}
            ) {{
                compaction_key
                sequence
                summary
                messages_compacted
                compacted_through_sequence
            }}
        }}"#
    );
    let resp = execute_query_timed(node, &query, "load_prompt_compaction_state").await?;
    if resp.has_errors() {
        return Err(SessionError::LoadingPromptCompactionStateSessionId {
            session_id: session_id.to_string(),
            errors: format!("{:?}", resp.errors),
        });
    }
    let rows: Vec<PromptCompactionRow> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };
    validate_compaction_chain(node_did, session_id, requester_did, &rows)?;
    let active_len = through_sequence.map_or(rows.len(), |cutoff| {
        rows.iter()
            .rposition(|row| {
                row.compacted_through_sequence
                    .is_some_and(|cursor| cursor <= cutoff)
            })
            .map_or(0, |index| index + 1)
    });
    let active_rows = &rows[..active_len];
    let total_messages_compacted = active_rows.iter().try_fold(0usize, |total, row| {
        total
            .checked_add(row.messages_compacted as usize)
            .ok_or(SessionError::CompactionEntryMessagesCompactedTotalOverflowedUsize)
    })?;
    let state = PromptCompactionState {
        summaries: active_rows.iter().map(|row| row.summary.clone()).collect(),
        total_messages_compacted,
        compacted_through_sequence: active_rows
            .last()
            .and_then(|row| row.compacted_through_sequence),
        generation: compaction_generation(active_rows)?,
        is_latest_generation: active_len == rows.len(),
    };
    tracing::Span::current().record("compaction_entry_count", rows.len() as i64);
    tracing::Span::current().record(
        "compacted_message_count",
        state.total_messages_compacted as i64,
    );
    tracing::Span::current().record("summary_count", state.summaries.len() as i64);
    Ok(state)
}

pub async fn load_compaction_entries(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<CompactionEntry>> {
    let scope = super::query::session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ {scope} }},
                order: {{ sequence: ASC }}
            ) {{
                compaction_key
                session_id
                node_did
                requester_did
                sequence
                summary
                files_read
                files_modified
                messages_compacted
                compacted_through_sequence
                original_tokens
                compacted_tokens
                created_at
            }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "load_compaction_entries").await?;
    if resp.has_errors() {
        return Err(SessionError::LoadingCompactionEntriesSessionId {
            session_id: session_id.to_string(),
            errors: format!("{:?}", resp.errors),
        });
    }

    let rows: Vec<CompactionGenerationRow> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    validate_file_path_projection_chain(node_did, session_id, requester_did, &rows)?;

    let mut files_read = FilePathAccumulator::default();
    let mut files_modified = FilePathAccumulator::default();
    let entries = rows
        .into_iter()
        .map(|row| {
            files_read.push(&row.files_read)?;
            files_modified.push(&row.files_modified)?;
            let original_tokens = usize::try_from(row.original_tokens)
                .map_err(SessionError::CompactionOriginalTokensMustNonNegative)?;
            let compacted_tokens = usize::try_from(row.compacted_tokens)
                .map_err(SessionError::CompactionCompactedTokensMustNonNegative)?;
            Ok(CompactionEntry {
                session_id: session_id.to_owned(),
                sequence: row.sequence,
                summary: row.summary,
                files_read: files_read.snapshot(),
                files_modified: files_modified.snapshot(),
                messages_compacted: row.messages_compacted,
                compacted_through_sequence: row.compacted_through_sequence,
                original_tokens,
                compacted_tokens,
                created_at: row.created_at,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let compacted_message_count = entries
        .iter()
        .map(|entry| entry.messages_compacted as i64)
        .sum::<i64>();
    tracing::Span::current().record("compaction_entry_count", entries.len() as i64);
    tracing::Span::current().record("compacted_message_count", compacted_message_count);
    Ok(entries)
}

pub(crate) struct NewExactSessionCompaction<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) node_did: &'a str,
    pub(crate) requester_did: Option<&'a str>,
    pub(crate) request_id: &'a str,
    pub(crate) request_doc_id: &'a str,
    pub(crate) files_read: &'a [String],
    pub(crate) files_modified: &'a [String],
    pub(crate) compacted_through_sequence: u32,
    pub(crate) original_tokens: usize,
    pub(crate) compacted_tokens: usize,
    pub(crate) expected_generation: &'a str,
}

/// Commit the shared exact reduction as one session-prefix fact. The summary
/// and compacted count are derived from the artifact, so callers cannot pair a
/// cursor with a different split or checkpoint.
pub(crate) async fn save_exact_compaction_entry(
    node: &std::sync::Arc<EmbeddedNode>,
    input: NewExactSessionCompaction<'_>,
    reduction: crate::compaction::ExactReduction<'_>,
) -> Result<CompactionEntry> {
    save_compaction_entry_with_requester_did(
        node,
        input.session_id,
        input.node_did,
        input.requester_did,
        input.request_id,
        input.request_doc_id,
        reduction.checkpoint,
        input.files_read,
        input.files_modified,
        reduction.messages_compacted()?,
        input.compacted_through_sequence,
        input.original_tokens,
        input.compacted_tokens,
        input.expected_generation,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) async fn save_compaction_entry(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    request_id: &str,
    request_doc_id: &str,
    summary: &str,
    files_read: &[String],
    files_modified: &[String],
    messages_compacted: u32,
    compacted_through_sequence: u32,
    original_tokens: usize,
    compacted_tokens: usize,
) -> Result<CompactionEntry> {
    let state = load_prompt_compaction_state(node, session_id, node_did, None, None).await?;
    save_compaction_entry_with_requester_did(
        node,
        session_id,
        node_did,
        None,
        request_id,
        request_doc_id,
        summary,
        files_read,
        files_modified,
        messages_compacted,
        compacted_through_sequence,
        original_tokens,
        compacted_tokens,
        &state.generation,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_compaction_entry_with_requester_did(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    request_id: &str,
    request_doc_id: &str,
    summary: &str,
    files_read: &[String],
    files_modified: &[String],
    messages_compacted: u32,
    compacted_through_sequence: u32,
    original_tokens: usize,
    compacted_tokens: usize,
    expected_generation: &str,
) -> Result<CompactionEntry> {
    let compacted_through_sequence = Some(compacted_through_sequence);
    let summary = summary.trim().to_string();
    let escaped_session_id = escape_graphql_string(session_id);
    let scope = super::query::session_scope_filter(node_did, session_id, requester_did);
    let escaped_node_did = escape_graphql_string(node_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_request_doc_id = escape_graphql_string(request_doc_id);
    let requester_did_field = super::requester_did_create_field(requester_did);
    let summary = &summary;
    let scope = &scope;
    let escaped_session_id = &escaped_session_id;
    let escaped_node_did = &escaped_node_did;
    let escaped_request_id = &escaped_request_id;
    let escaped_request_doc_id = &escaped_request_doc_id;
    let requester_did_field = &requester_did_field;
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node,
        None,
        crate::config_client::IdempotentTransactionRetry::Standard,
        "session.save_compaction_entry",
        move |txn| {
            Box::pin(async move {
                let query = format!(
                    r#"{{
                    CompactionEntry(
                        filter: {{ {scope} }},
                        order: {{ sequence: ASC }}
                    ) {{
                        compaction_key sequence summary messages_compacted
                        compacted_through_sequence files_read files_modified
                        node_did requester_did request_id request_doc_id
                        original_tokens compacted_tokens created_at
                    }}
                }}"#
                );
                let value = txn.execute(&query).await?;
                let rows: Vec<CompactionGenerationRow> = serde_json::from_value(
                    value
                        .get("data")
                        .and_then(|data| data.get("CompactionEntry"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([])),
                )?;
                validate_compaction_chain(node_did, session_id, requester_did, &rows)?;
                let actual_generation = compaction_generation(&rows)?;
                if actual_generation != expected_generation {
                    if let Some(entry) = reconcile_exact_redelivery(
                        &rows,
                        expected_generation,
                        session_id,
                        node_did,
                        requester_did,
                        request_id,
                        request_doc_id,
                        summary,
                        files_read,
                        files_modified,
                        messages_compacted,
                        compacted_through_sequence,
                        original_tokens,
                        compacted_tokens,
                    )? {
                        return Ok(entry);
                    }
                }
                if actual_generation != expected_generation {
                    return Err(SessionError::StaleCompactionGenerationSession {
                        session_id: session_id.to_string(),
                    });
                }
                if let Some(cursor) = compacted_through_sequence {
                    let prior_cursor = rows
                        .iter()
                        .rev()
                        .find_map(|row| row.compacted_through_sequence);
                    if !prior_cursor.is_none_or(|prior| cursor > prior) {
                        return Err(SessionError::CompactionCursorRegressionSession {
                            session_id: session_id.to_string(),
                        });
                    }
                }

                let mut cumulative_files_read =
                    cumulative_file_paths(&rows, |row| &row.files_read)?;
                let file_read_delta = new_file_paths(&cumulative_files_read, files_read);
                cumulative_files_read.extend(files_read.iter().cloned());
                dedupe_paths(&mut cumulative_files_read);
                let mut cumulative_files_modified =
                    cumulative_file_paths(&rows, |row| &row.files_modified)?;
                let file_modified_delta =
                    new_file_paths(&cumulative_files_modified, files_modified);
                cumulative_files_modified.extend(files_modified.iter().cloned());
                dedupe_paths(&mut cumulative_files_modified);

                let sequence = u32::try_from(rows.len() + 1)
                    .map_err(SessionError::CompactionSequenceOverflow)?;
                let compaction_key = compaction_key(node_did, session_id, requester_did, sequence);
                let created_at =
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let cursor_field = compacted_through_sequence
                    .map(|cursor| cursor.to_string())
                    .unwrap_or_else(|| "null".to_string());
                let mutation = format!(
                    r#"mutation {{
                    create_CompactionEntry(input: {{
                        compaction_key: "{compaction_key}"
                        session_id: "{escaped_session_id}"
                        node_did: "{escaped_node_did}"
                        {requester_did_field}
                        request_id: "{escaped_request_id}"
                        request_doc_id: "{escaped_request_doc_id}"
                        sequence: {sequence}
                        summary: "{summary}"
                        files_read: "{files_read_json}"
                        files_modified: "{files_modified_json}"
                        messages_compacted: {messages_compacted}
                        compacted_through_sequence: {cursor_field}
                        original_tokens: {original_tokens}
                        compacted_tokens: {compacted_tokens}
                        created_at: "{created_at}"
                    }}) {{ _docID }}
                }}"#,
                    compaction_key = escape_graphql_string(&compaction_key),
                    summary = escape_graphql_string(summary),
                    files_read_json =
                        escape_graphql_string(&encode_file_path_delta(&file_read_delta)?),
                    files_modified_json =
                        escape_graphql_string(&encode_file_path_delta(&file_modified_delta)?),
                    created_at = escape_graphql_string(&created_at),
                );
                txn.execute(&mutation).await?;
                Ok::<_, SessionError>(CompactionEntry {
                    session_id: session_id.to_string(),
                    sequence,
                    summary: summary.clone(),
                    files_read: cumulative_files_read,
                    files_modified: cumulative_files_modified,
                    messages_compacted,
                    compacted_through_sequence,
                    original_tokens,
                    compacted_tokens,
                    created_at,
                })
            })
        },
    )
    .await
}

fn encode_file_path_delta(paths: &[String]) -> Result<String, SessionError> {
    serde_json::to_string(&FilePathDelta { version: 1, paths }).map_err(SessionError::FilePathJson)
}

fn new_file_paths(cumulative: &[String], current: &[String]) -> Vec<String> {
    let mut seen = cumulative
        .iter()
        .map(String::as_str)
        .collect::<RapidHashSet<_>>();
    let mut delta = current
        .iter()
        .filter(|path| seen.insert(path.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    dedupe_paths(&mut delta);
    delta
}

fn decode_file_paths(encoded: &str) -> Result<StoredFilePaths, SessionError> {
    if encoded.trim().is_empty() {
        return Ok(StoredFilePaths::Cumulative(Vec::new()));
    }
    serde_json::from_str::<StoredFilePaths>(encoded).map_err(SessionError::FilePathJson)
}

#[derive(Default)]
struct FilePathAccumulator {
    cumulative: Vec<String>,
    deltas: Option<BTreeSet<String>>,
}

impl FilePathAccumulator {
    fn push(&mut self, encoded: &str) -> Result<(), SessionError> {
        match decode_file_paths(encoded)? {
            StoredFilePaths::Cumulative(paths) => {
                self.cumulative = paths;
                self.deltas = None;
            }
            StoredFilePaths::Delta(delta) => {
                if delta.version != 1 {
                    return Err(SessionError::UnsupportedFilePathVersion);
                }
                let paths = self.deltas.get_or_insert_with(|| {
                    std::mem::take(&mut self.cumulative).into_iter().collect()
                });
                paths.extend(delta.paths);
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> Vec<String> {
        self.deltas.as_ref().map_or_else(
            || self.cumulative.clone(),
            |paths| paths.iter().cloned().collect(),
        )
    }

    fn finish(self) -> Vec<String> {
        self.deltas
            .map_or(self.cumulative, |paths| paths.into_iter().collect())
    }
}

fn cumulative_file_paths(
    rows: &[CompactionGenerationRow],
    paths: impl for<'a> Fn(&'a CompactionGenerationRow) -> &'a str,
) -> Result<Vec<String>, SessionError> {
    let mut accumulator = FilePathAccumulator::default();
    for row in rows {
        accumulator.push(paths(row))?;
    }
    Ok(accumulator.finish())
}

fn is_cumulative_file_paths(encoded: &str) -> bool {
    encoded.trim().is_empty() || encoded.trim_start().starts_with('[')
}

fn validate_file_path_projection_chain(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    rows: &[CompactionGenerationRow],
) -> Result<(), SessionError> {
    if rows.iter().any(|row| {
        !is_cumulative_file_paths(&row.files_read) || !is_cumulative_file_paths(&row.files_modified)
    }) {
        validate_compaction_chain(node_did, session_id, requester_did, rows)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn reconcile_exact_redelivery(
    rows: &[CompactionGenerationRow],
    expected_generation: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    request_id: &str,
    request_doc_id: &str,
    summary: &str,
    files_read: &[String],
    files_modified: &[String],
    messages_compacted: u32,
    compacted_through_sequence: Option<u32>,
    original_tokens: usize,
    compacted_tokens: usize,
) -> Result<Option<CompactionEntry>> {
    let Some(predecessor_len) = (0..rows.len()).find(|length| {
        compaction_generation(&rows[..*length]).ok().as_deref() == Some(expected_generation)
    }) else {
        return Ok(None);
    };
    let predecessor = &rows[..predecessor_len];
    let persisted = &rows[predecessor_len];

    let mut expected_files_read = cumulative_file_paths(predecessor, |row| &row.files_read)?;
    expected_files_read.extend(files_read.iter().cloned());
    dedupe_paths(&mut expected_files_read);
    let mut expected_files_modified =
        cumulative_file_paths(predecessor, |row| &row.files_modified)?;
    expected_files_modified.extend(files_modified.iter().cloned());
    dedupe_paths(&mut expected_files_modified);

    let persisted_files_read =
        cumulative_file_paths(&rows[..=predecessor_len], |row| &row.files_read)?;
    let persisted_files_modified =
        cumulative_file_paths(&rows[..=predecessor_len], |row| &row.files_modified)?;
    let requester_matches = persisted.requester_did.as_deref() == requester_did;
    let matches = persisted.node_did == node_did
        && requester_matches
        && persisted.request_id.as_deref() == Some(request_id)
        && persisted.request_doc_id.as_deref() == Some(request_doc_id)
        && persisted.summary == summary
        && persisted.messages_compacted == messages_compacted
        && persisted.compacted_through_sequence == compacted_through_sequence
        && usize::try_from(persisted.original_tokens).ok() == Some(original_tokens)
        && usize::try_from(persisted.compacted_tokens).ok() == Some(compacted_tokens)
        && persisted_files_read == expected_files_read
        && persisted_files_modified == expected_files_modified;
    if !matches {
        return Ok(None);
    }

    Ok(Some(CompactionEntry {
        session_id: session_id.to_string(),
        sequence: persisted.sequence,
        summary: persisted.summary.clone(),
        files_read: persisted_files_read,
        files_modified: persisted_files_modified,
        messages_compacted: persisted.messages_compacted,
        compacted_through_sequence: persisted.compacted_through_sequence,
        original_tokens,
        compacted_tokens,
        created_at: persisted.created_at.clone(),
    }))
}

#[cfg(test)]
mod file_path_tests {
    use super::*;

    fn compaction_row(
        sequence: u32,
        files_read: &str,
        files_modified: &str,
    ) -> CompactionGenerationRow {
        CompactionGenerationRow {
            compaction_key: compaction_key("did:test:files", "session", None, sequence),
            sequence,
            summary: format!("summary-{sequence}"),
            messages_compacted: 1,
            compacted_through_sequence: Some(sequence * 10),
            files_read: files_read.to_owned(),
            files_modified: files_modified.to_owned(),
            node_did: "did:test:files".to_owned(),
            requester_did: None,
            request_id: Some(format!("request-{sequence}")),
            request_doc_id: Some(format!("request-doc-{sequence}")),
            original_tokens: 10,
            compacted_tokens: 5,
            created_at: format!("2026-10-15T00:00:{sequence:02}Z"),
        }
    }

    #[test]
    fn cumulative_arrays_and_versioned_deltas_keep_legacy_views() {
        let mut paths = FilePathAccumulator::default();
        paths.push(r#"["/b.rs","/a.rs"]"#).unwrap();
        assert_eq!(paths.snapshot(), vec!["/b.rs", "/a.rs"]);
        paths.push(r#"["/b.rs","/a.rs","/c.rs"]"#).unwrap();
        assert_eq!(paths.snapshot(), vec!["/b.rs", "/a.rs", "/c.rs"]);
        paths
            .push(r#"{"version":1,"paths":["/d.rs","/a.rs"]}"#)
            .unwrap();
        assert_eq!(paths.snapshot(), vec!["/a.rs", "/b.rs", "/c.rs", "/d.rs"]);

        paths.push(r#"["/historical.rs"]"#).unwrap();
        assert_eq!(paths.snapshot(), vec!["/historical.rs"]);
    }

    #[test]
    fn file_path_deltas_reject_unknown_versions_and_shapes() {
        let mut paths = FilePathAccumulator::default();
        assert!(paths.push(r#"{"version":2,"paths":[]}"#).is_err());
        assert!(paths
            .push(r#"{"version":1,"paths":[],"extra":true}"#)
            .is_err());
        assert!(paths.push(r#"{"version":1}"#).is_err());
    }

    #[test]
    fn delta_projection_rejects_missing_prefix_and_interior_chain_rows() {
        let missing_prefix = [compaction_row(
            2,
            r#"{"version":1,"paths":["/b.rs"]}"#,
            r#"{"version":1,"paths":[]}"#,
        )];
        assert!(validate_file_path_projection_chain(
            "did:test:files",
            "session",
            None,
            &missing_prefix
        )
        .is_err());

        let interior_gap = [
            compaction_row(1, r#"["/a.rs"]"#, "[]"),
            compaction_row(
                3,
                r#"{"version":1,"paths":["/c.rs"]}"#,
                r#"{"version":1,"paths":[]}"#,
            ),
        ];
        assert!(validate_file_path_projection_chain(
            "did:test:files",
            "session",
            None,
            &interior_gap
        )
        .is_err());
    }

    #[test]
    fn delta_projection_accepts_complete_mixed_legacy_chain() {
        let complete = [
            compaction_row(1, r#"["/a.rs"]"#, r#"["/x.rs"]"#),
            compaction_row(
                2,
                r#"{"version":1,"paths":["/b.rs"]}"#,
                r#"{"version":1,"paths":[]}"#,
            ),
        ];
        validate_file_path_projection_chain("did:test:files", "session", None, &complete).unwrap();
        assert_eq!(
            cumulative_file_paths(&complete, |row| &row.files_read).unwrap(),
            vec!["/a.rs", "/b.rs"]
        );
        assert_eq!(
            cumulative_file_paths(&complete, |row| &row.files_modified).unwrap(),
            vec!["/x.rs"]
        );
    }
}
