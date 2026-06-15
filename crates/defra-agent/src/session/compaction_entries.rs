use super::retry::{execute_mutation_with_retry, execute_query_timed, retry_operation};
use super::rows::{dedupe_paths, CompactionEntryRow};
use super::*;

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

#[allow(clippy::too_many_arguments)]
pub async fn save_compaction_entry(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    summary: &str,
    files_read: &[String],
    files_modified: &[String],
    messages_compacted: u32,
    original_tokens: usize,
    compacted_tokens: usize,
) -> Result<CompactionEntry> {
    let previous_entries = retry_operation("load_compaction_entries", || {
        load_compaction_entries(node, session_id)
    })
    .await?;
    let previous = previous_entries.last();

    let mut cumulative_files_read = previous
        .map(|entry| entry.files_read.clone())
        .unwrap_or_default();
    cumulative_files_read.extend(files_read.iter().cloned());
    dedupe_paths(&mut cumulative_files_read);

    let mut cumulative_files_modified = previous
        .map(|entry| entry.files_modified.clone())
        .unwrap_or_default();
    cumulative_files_modified.extend(files_modified.iter().cloned());
    dedupe_paths(&mut cumulative_files_modified);

    let sequence = previous.map_or(1, |entry| entry.sequence + 1);
    let created_at = chrono::Utc::now().to_rfc3339();
    let summary = summary.trim().to_string();
    let files_read_json = escape_graphql_string(&serde_json::to_string(&cumulative_files_read)?);
    let files_modified_json =
        escape_graphql_string(&serde_json::to_string(&cumulative_files_modified)?);
    let escaped_summary = escape_graphql_string(&summary);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);

    // `agent_did` is the immutable scope key: written only in the `add` branch
    // (create), never rewritten on update.
    let compaction_key = format!("{escaped_session_id}:{sequence}");
    let mutation = format!(
        r#"mutation {{
            upsert_CompactionEntry(
                filter: {{ compaction_key: {{ _eq: "{compaction_key}" }} }},
                add: {{
                    compaction_key: "{compaction_key}",
                    session_id: "{escaped_session_id}",
                    agent_did: "{escaped_agent_did}",
                    sequence: {sequence},
                    summary: "{escaped_summary}",
                    files_read: "{files_read_json}",
                    files_modified: "{files_modified_json}",
                    messages_compacted: {messages_compacted},
                    original_tokens: {original_tokens},
                    compacted_tokens: {compacted_tokens},
                    created_at: "{created_at}"
                }},
                update: {{
                    summary: "{escaped_summary}",
                    files_read: "{files_read_json}",
                    files_modified: "{files_modified_json}",
                    messages_compacted: {messages_compacted},
                    original_tokens: {original_tokens},
                    compacted_tokens: {compacted_tokens}
                }}
            ) {{ _docID }}
        }}"#
    );

    execute_mutation_with_retry(node, &mutation, "save_compaction_entry").await?;

    Ok(CompactionEntry {
        session_id: session_id.to_string(),
        sequence,
        summary,
        files_read: cumulative_files_read,
        files_modified: cumulative_files_modified,
        messages_compacted,
        original_tokens,
        compacted_tokens,
        created_at,
    })
}
