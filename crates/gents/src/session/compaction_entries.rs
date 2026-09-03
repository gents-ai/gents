use super::retry::execute_query_timed;
use super::rows::{dedupe_paths, CompactionEntryRow};
use super::*;
use anyhow::Context;

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
struct CompactionGenerationRow {
    compaction_key: String,
    sequence: u32,
    summary: String,
    messages_compacted: u32,
    #[serde(default)]
    compacted_through_sequence: Option<u32>,
    #[serde(default)]
    files_read: String,
    #[serde(default)]
    files_modified: String,
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    request_doc_id: String,
    #[serde(default)]
    original_tokens: i64,
    #[serde(default)]
    compacted_tokens: i64,
    #[serde(default)]
    created_at: String,
}

trait CompactionProjection {
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

fn validate_compaction_chain<T: CompactionProjection>(session_id: &str, rows: &[T]) -> Result<()> {
    let mut prior_cursor = None;
    for (index, row) in rows.iter().enumerate() {
        let expected_sequence = u32::try_from(index + 1).context("compaction sequence overflow")?;
        let expected_key = format!("{session_id}:{expected_sequence}");
        anyhow::ensure!(
            row.sequence() == expected_sequence && row.key() == expected_key,
            "ambiguous compaction chain for session {session_id}: expected {expected_key}, found {} at sequence {}",
            row.key(),
            row.sequence()
        );
        let cursor = row.cursor().with_context(|| {
            format!(
                "compaction entry for session {session_id} at sequence {} has no canonical cursor",
                row.sequence()
            )
        })?;
        anyhow::ensure!(
            prior_cursor.is_none_or(|prior| cursor > prior),
            "compaction cursor regression for session {session_id} at sequence {}",
            row.sequence()
        );
        prior_cursor = Some(cursor);
    }
    Ok(())
}

fn compaction_generation<T: CompactionProjection>(rows: &[T]) -> Result<String> {
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
    crate::rendered_request::sha256_canonical_json(&serde_json::json!(projection))
}

/// Load only the compaction projection consumed while assembling a prompt.
///
/// Every persisted entry must carry a canonical cursor. Rows without one are
/// rejected rather than projected into provider history.
pub(crate) async fn load_prompt_compaction_state(
    node: &EmbeddedNode,
    session_id: &str,
    through_sequence: Option<u32>,
) -> Result<PromptCompactionState> {
    let escaped_session_id = escape_graphql_string(session_id);
    // A background request is claimed against an immutable transcript high
    // water mark. A compaction produced later may only shape that request when
    // its cumulative canonical cursor is itself inside the claimed snapshot.
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
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
    let resp = execute_query_timed(node, &query, "load_prompt_compaction_state").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading prompt compaction state for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }
    let rows: Vec<PromptCompactionRow> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };
    validate_compaction_chain(session_id, &rows)?;
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
            .context("CompactionEntry messages_compacted total overflowed usize")
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
) -> Result<Vec<CompactionEntry>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                session_id
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

    let resp = execute_query_timed(node, &query, "load_compaction_entries").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading compaction entries for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let rows: Vec<CompactionEntryRow> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    let entries = rows
        .into_iter()
        .map(CompactionEntry::try_from)
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
    pub(crate) agent_did: &'a str,
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
    node: &EmbeddedNode,
    input: NewExactSessionCompaction<'_>,
    reduction: crate::compaction::ExactReduction<'_>,
) -> Result<CompactionEntry> {
    save_compaction_entry_with_requester_did(
        node,
        input.session_id,
        input.agent_did,
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
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
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
    let state = load_prompt_compaction_state(node, session_id, None).await?;
    save_compaction_entry_with_requester_did(
        node,
        session_id,
        agent_did,
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
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
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
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_request_doc_id = escape_graphql_string(request_doc_id);
    let requester_did_field = super::requester_did_create_field(requester_did);
    for retry_index in 0..=crate::graphql::DEFRA_DB_CONFLICT_MAX_RETRIES {
        let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
        let attempt = async {
            let query = format!(
                r#"{{
                    CompactionEntry(
                        filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                        order: {{ sequence: ASC }}
                    ) {{
                        compaction_key sequence summary messages_compacted
                        compacted_through_sequence files_read files_modified
                        agent_did requester_did request_id request_doc_id
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
            validate_compaction_chain(session_id, &rows)?;
            let actual_generation = compaction_generation(&rows)?;
            if actual_generation != expected_generation {
                if let Some(entry) = reconcile_exact_redelivery(
                    &rows,
                    expected_generation,
                    session_id,
                    agent_did,
                    requester_did,
                    request_id,
                    request_doc_id,
                    &summary,
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
            anyhow::ensure!(
                actual_generation == expected_generation,
                "stale compaction generation for session {session_id}"
            );
            if let Some(cursor) = compacted_through_sequence {
                let prior_cursor = rows
                    .iter()
                    .rev()
                    .find_map(|row| row.compacted_through_sequence);
                anyhow::ensure!(
                    prior_cursor.is_none_or(|prior| cursor > prior),
                    "compaction cursor regression for session {session_id}"
                );
            }

            let mut cumulative_files_read = rows
                .last()
                .map(|entry| decode_paths(&entry.files_read))
                .transpose()?
                .unwrap_or_default();
            cumulative_files_read.extend(files_read.iter().cloned());
            dedupe_paths(&mut cumulative_files_read);
            let mut cumulative_files_modified = rows
                .last()
                .map(|entry| decode_paths(&entry.files_modified))
                .transpose()?
                .unwrap_or_default();
            cumulative_files_modified.extend(files_modified.iter().cloned());
            dedupe_paths(&mut cumulative_files_modified);

            let sequence = u32::try_from(rows.len() + 1).context("compaction sequence overflow")?;
            let compaction_key = format!("{session_id}:{sequence}");
            // DefraDB canonicalizes fractional seconds with Go's RFC3339Nano
            // formatter, which trims trailing zeros.  Emit whole seconds so the
            // value returned from this create is byte-identical to the value
            // loaded during an exact redelivery.
            let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let cursor_field = compacted_through_sequence
                .map(|cursor| cursor.to_string())
                .unwrap_or_else(|| "null".to_string());
            let mutation = format!(
                r#"mutation {{
                    create_CompactionEntry(input: {{
                        compaction_key: "{compaction_key}"
                        session_id: "{escaped_session_id}"
                        agent_did: "{escaped_agent_did}"
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
                summary = escape_graphql_string(&summary),
                files_read_json =
                    escape_graphql_string(&serde_json::to_string(&cumulative_files_read)?),
                files_modified_json =
                    escape_graphql_string(&serde_json::to_string(&cumulative_files_modified)?),
                created_at = escape_graphql_string(&created_at),
            );
            txn.execute(&mutation).await?;
            Ok(CompactionEntry {
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
        }
        .await;

        match attempt {
            Ok(entry) => match txn.commit().await {
                Ok(()) => return Ok(entry),
                Err(error)
                    if retryable_compaction_transaction(&error)
                        && retry_index < crate::graphql::DEFRA_DB_CONFLICT_MAX_RETRIES =>
                {
                    tokio::time::sleep(crate::graphql::defradb_conflict_retry_backoff(retry_index))
                        .await;
                }
                Err(error) => return Err(error),
            },
            Err(error) => {
                if let Err(discard_error) = txn.discard().await {
                    tracing::warn!(error = %discard_error, "discarding failed compaction transaction also failed");
                }
                if retryable_compaction_transaction(&error)
                    && retry_index < crate::graphql::DEFRA_DB_CONFLICT_MAX_RETRIES
                {
                    tokio::time::sleep(crate::graphql::defradb_conflict_retry_backoff(retry_index))
                        .await;
                    continue;
                }
                return Err(error);
            }
        }
    }
    unreachable!("bounded compaction transaction retry loop returns")
}

fn retryable_compaction_transaction(error: &anyhow::Error) -> bool {
    let diagnostic = format!("{error:#}").to_ascii_lowercase();
    crate::graphql::is_defradb_transaction_conflict_text(&diagnostic)
        || diagnostic.contains("unique")
        || diagnostic.contains("duplicate")
        || diagnostic.contains("already exists")
}

fn decode_paths(value: &str) -> Result<Vec<String>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(value).map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn reconcile_exact_redelivery(
    rows: &[CompactionGenerationRow],
    expected_generation: &str,
    session_id: &str,
    agent_did: &str,
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

    let mut expected_files_read = predecessor
        .last()
        .map(|row| decode_paths(&row.files_read))
        .transpose()?
        .unwrap_or_default();
    expected_files_read.extend(files_read.iter().cloned());
    dedupe_paths(&mut expected_files_read);
    let mut expected_files_modified = predecessor
        .last()
        .map(|row| decode_paths(&row.files_modified))
        .transpose()?
        .unwrap_or_default();
    expected_files_modified.extend(files_modified.iter().cloned());
    dedupe_paths(&mut expected_files_modified);

    let persisted_files_read = decode_paths(&persisted.files_read)?;
    let persisted_files_modified = decode_paths(&persisted.files_modified)?;
    let requester_matches = persisted.requester_did.as_deref().and_then(|did| {
        let did = did.trim();
        (!did.is_empty()).then_some(did)
    }) == requester_did.map(str::trim).filter(|did| !did.is_empty());
    let matches = persisted.agent_did == agent_did
        && requester_matches
        && persisted.request_id == request_id
        && persisted.request_doc_id == request_doc_id
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
