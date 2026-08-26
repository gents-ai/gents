use super::*;

pub(super) fn tool_group_cursor_sequence(cursor: &str) -> Option<i64> {
    cursor
        .strip_prefix("tools-")
        .and_then(|value| value.parse::<i64>().ok())
}

async fn resolve_transcript_cursor_sequence(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
    cursor: &str,
) -> Result<i64> {
    if let Some(sequence) = tool_group_cursor_sequence(cursor) {
        let session_id = escape_graphql_string(session_id);
        let agent_filter = agent_did
            .map(escape_graphql_string)
            .map(|agent_did| format!(", agent_did: {{ _eq: \"{agent_did}\" }}"))
            .unwrap_or_default();
        let requester_filter = requester_did
            .map(escape_graphql_string)
            .map(|requester_did| format!(", requester_did: {{ _eq: \"{requester_did}\" }}"))
            .unwrap_or_default();
        let query = format!(
            r#"query DesktopSessionToolCursor {{
  AgentMessage(
    filter: {{ session_id: {{ _eq: "{session_id}" }}, sequence: {{ _eq: {sequence} }}{agent_filter}{requester_filter} }},
    limit: 1
  ) {{ sequence }}
  AgentToolCall(
    filter: {{ session_id: {{ _eq: "{session_id}" }}, message_sequence: {{ _eq: {sequence} }}{agent_filter}{requester_filter} }},
    limit: 1
  ) {{ sequence: message_sequence }}
}}"#
        );
        let data = execute_local_graphql_query(node, &query, "session tool cursor").await?;
        let rows: Vec<TranscriptCursorRow> = parse_query_rows(&data, AGENT_MESSAGE_NAME)?;
        let tool_rows: Vec<TranscriptCursorRow> = parse_query_rows(&data, AGENT_TOOL_CALL_NAME)?;
        if rows
            .iter()
            .chain(&tool_rows)
            .any(|row| row.sequence == Some(sequence))
        {
            return Ok(sequence);
        }
        bail!("session transcript cursor is no longer present: {cursor}");
    }
    let session_id = escape_graphql_string(session_id);
    let message_key = escape_graphql_string(cursor);
    let agent_filter = agent_did
        .map(escape_graphql_string)
        .map(|agent_did| format!(", agent_did: {{ _eq: \"{agent_did}\" }}"))
        .unwrap_or_default();
    let requester_filter = requester_did
        .map(escape_graphql_string)
        .map(|requester_did| format!(", requester_did: {{ _eq: \"{requester_did}\" }}"))
        .unwrap_or_default();
    let rows: Vec<TranscriptCursorRow> = load_rows(
        node,
        AGENT_MESSAGE_NAME,
        &format!(
            r#"query {{
  AgentMessage(
    filter: {{
      session_id: {{ _eq: "{session_id}" }},
      message_key: {{ _eq: "{message_key}" }}{agent_filter}{requester_filter}
    }},
    limit: 1
  ) {{ sequence }}
}}"#
        ),
    )
    .await?;
    rows.first()
        .and_then(|row| row.sequence)
        .ok_or_else(|| anyhow!("session transcript cursor is no longer present: {cursor}"))
}

/// Query a bounded transcript window directly from DefraDB. The cursor is a
/// bridge item key, but is resolved to the durable sequence space before the
/// page query so inserts at the tip cannot shift an older page. Messages and
/// tool groups are independently overscanned because one sequence may produce
/// both timeline items; the bridge performs the final visible-item limit.
pub async fn load_session_transcript_page(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
    before_item_key: Option<&str>,
    requested_limit: Option<usize>,
) -> Result<SessionTranscriptQueryPage> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        bail!("session transcript query requires a session id");
    }
    let limit = requested_limit
        .unwrap_or(DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE)
        .clamp(1, MAX_SESSION_TRANSCRIPT_PAGE_SIZE);
    let message_query_limit = limit.saturating_add(1);
    let tool_call_query_limit = SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET.saturating_add(1);
    let before_sequence = match before_item_key {
        Some(cursor) => Some(
            resolve_transcript_cursor_sequence(node, session_id, agent_did, requester_did, cursor)
                .await?,
        ),
        None => None,
    };
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_filter = agent_did
        .map(escape_graphql_string)
        .map(|agent_did| format!(", agent_did: {{ _eq: \"{agent_did}\" }}"))
        .unwrap_or_default();
    let requester_filter = requester_did
        .map(escape_graphql_string)
        .map(|requester_did| format!(", requester_did: {{ _eq: \"{requester_did}\" }}"))
        .unwrap_or_default();
    let message_sequence_filter = before_sequence
        .map(|sequence| format!(", sequence: {{ _lt: {sequence} }}"))
        .unwrap_or_default();
    let tool_before_sequence_filter = before_sequence
        .map(|sequence| format!(", message_sequence: {{ _lt: {sequence} }}"))
        .unwrap_or_default();
    let transcript_query = format!(
        r#"query DesktopSessionTranscriptPage {{
  AgentMessage(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter}{message_sequence_filter} }},
    order: [{{ sequence: DESC }}, {{ message_key: DESC }}],
    limit: {message_query_limit}
  ) {{ {AGENT_MESSAGE_FIELDS} }}
  AgentToolCall(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter}{tool_before_sequence_filter} }},
    order: [{{ message_sequence: DESC }}, {{ tool_call_key: DESC }}],
    limit: {tool_call_query_limit}
  ) {{ {AGENT_TOOL_CALL_FIELDS} }}
}}"#
    );
    let started = std::time::Instant::now();
    // Messages and tool calls must come from one DefraDB query evaluation. A
    // pair of sequential reads can observe a new tool group without the
    // message window that owns it when the live tip advances between reads.
    let transcript_data =
        execute_local_graphql_query(node, &transcript_query, "session transcript page").await?;
    let queried_messages: Vec<AgentMessageRow> =
        parse_query_rows(&transcript_data, AGENT_MESSAGE_NAME)?;
    let queried_tool_calls: Vec<AgentToolCallRow> =
        parse_query_rows(&transcript_data, AGENT_TOOL_CALL_NAME)?;
    if queried_messages.iter().any(|row| row.sequence.is_none()) {
        bail!(
            "session transcript contains legacy messages without a sequence; bounded pagination cannot represent that schema state losslessly"
        );
    }
    let messages_exhausted = queried_messages.len() < message_query_limit;
    let mut messages = queried_messages.clone();
    let deferred_message_sequence = (!messages_exhausted)
        .then(|| queried_messages.last().and_then(|row| row.sequence))
        .flatten();
    if let Some(sequence) = deferred_message_sequence {
        messages.retain(|row| row.sequence != Some(sequence));
        if messages.is_empty() {
            bail!(
                "session transcript has more than {limit} messages at sequence {sequence}; the sequence-atomic page budget cannot represent it losslessly"
            );
        }
    }

    if queried_tool_calls
        .iter()
        .any(|row| row.message_sequence.is_none())
    {
        bail!(
            "session transcript contains legacy tool calls without a message sequence; bounded pagination cannot represent that schema state losslessly"
        );
    }
    // The message lookahead boundary is known only after parsing the atomic
    // response. DESC ordering proves the relevant tool window is complete if
    // the query either exhausted all tools or crossed that lower boundary.
    let crossed_message_boundary = deferred_message_sequence.is_some_and(|boundary| {
        queried_tool_calls
            .last()
            .and_then(|row| row.message_sequence)
            .is_some_and(|sequence| sequence <= boundary)
    });
    let tools_exhausted =
        queried_tool_calls.len() < tool_call_query_limit || crossed_message_boundary;
    let mut tool_calls = queried_tool_calls
        .iter()
        .filter(|row| {
            deferred_message_sequence.is_none_or(|boundary| {
                row.message_sequence
                    .is_some_and(|sequence| sequence > boundary)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if !tools_exhausted {
        let deferred_sequence = queried_tool_calls
            .last()
            .and_then(|row| row.message_sequence)
            .expect("null tool sequences rejected above");
        tool_calls.retain(|row| row.message_sequence != Some(deferred_sequence));
        // The overscan row proves every lower sequence is outside the complete
        // tool window too. Defer their messages with the boundary group so the
        // bridge never renders a message whose tools were truncated.
        messages.retain(|row| {
            row.sequence
                .is_some_and(|sequence| sequence > deferred_sequence)
        });
        if tool_calls.is_empty() {
            bail!(
                "session transcript has more than {} tool calls at sequence {deferred_sequence}; the sequence-atomic tool budget cannot represent it losslessly",
                SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET
            );
        }
    }
    let source_exhausted = messages_exhausted && tools_exhausted;
    let queried_rows = queried_messages
        .len()
        .saturating_add(queried_tool_calls.len());
    let query_count = 1 + u64::from(before_item_key.is_some());
    tracing::debug!(
        target: "gents_desktop_core::query",
        session_id,
        before_sequence,
        requested_limit = limit,
        message_query_limit,
        tool_call_query_limit,
        query_count,
        queried_rows,
        source_exhausted,
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "loaded bounded DefraDB transcript page"
    );
    Ok(SessionTranscriptQueryPage {
        store: ClientStore::from_rows(ClientStoreRows {
            messages,
            tool_calls,
            ..ClientStoreRows::default()
        }),
        query_count,
        queried_rows,
        message_query_limit,
        tool_call_query_limit,
        source_exhausted,
        has_newer: before_sequence.is_some(),
    })
}

/// Load the durable rows needed to calculate the session context meter.
///
/// Unlike an observer snapshot, this store is intentionally short-lived. It
/// is read directly from DefraDB for the selected session, consumed while the
/// bridge builds one tip projection, and then dropped. Transcript content must
/// never be merged into the process-wide observed store.
pub async fn load_session_context_store(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
) -> Result<ClientStore> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        bail!("session context query requires a session id");
    }
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_filter = agent_did
        .map(escape_graphql_string)
        .map(|agent_did| format!(", agent_did: {{ _eq: \"{agent_did}\" }}"))
        .unwrap_or_default();
    let requester_filter = requester_did
        .map(escape_graphql_string)
        .map(|requester_did| format!(", requester_did: {{ _eq: \"{requester_did}\" }}"))
        .unwrap_or_default();
    let query = format!(
        r#"query DesktopSessionContext {{
  AgentMessage(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter} }},
    order: [{{ sequence: ASC }}, {{ message_key: ASC }}]
  ) {{ {AGENT_MESSAGE_FIELDS} }}
  CompactionEntry(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter} }},
    order: [{{ sequence: ASC }}, {{ compaction_key: ASC }}]
  ) {{ {COMPACTION_ENTRY_FIELDS} }}
}}"#
    );
    let started = std::time::Instant::now();
    let data = execute_local_graphql_query(node, &query, "session context").await?;
    let messages: Vec<AgentMessageRow> = parse_query_rows(&data, AGENT_MESSAGE_NAME)?;
    let compaction_entries: Vec<CompactionEntryRow> =
        parse_query_rows(&data, COMPACTION_ENTRY_NAME)?;
    tracing::debug!(
        target: "gents_desktop_core::query",
        session_id,
        message_rows = messages.len(),
        compaction_rows = compaction_entries.len(),
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "loaded ephemeral DefraDB session context rows"
    );
    Ok(ClientStore::from_rows(ClientStoreRows {
        messages,
        compaction_entries,
        ..ClientStoreRows::default()
    }))
}

/// Load an exact transcript only for explicit diagnostic evidence.
///
/// This is intentionally separate from the interactive projection: it is
/// unbounded, may be expensive, and must never feed React or the observer.
pub async fn load_session_diagnostics_store(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
) -> Result<ClientStore> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        bail!("session diagnostics query requires a session id");
    }
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_filter = agent_did
        .map(escape_graphql_string)
        .map(|agent_did| format!(", agent_did: {{ _eq: \"{agent_did}\" }}"))
        .unwrap_or_default();
    let requester_filter = requester_did
        .map(escape_graphql_string)
        .map(|requester_did| format!(", requester_did: {{ _eq: \"{requester_did}\" }}"))
        .unwrap_or_default();
    let query = format!(
        r#"query DesktopSessionDiagnostics {{
  AgentMessage(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter} }},
    order: [{{ sequence: ASC }}, {{ message_key: ASC }}]
  ) {{ {AGENT_MESSAGE_FIELDS} }}
  AgentToolCall(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter} }},
    order: [{{ message_sequence: ASC }}, {{ tool_call_key: ASC }}]
  ) {{ {AGENT_TOOL_CALL_FIELDS} }}
  AgentToolResult(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter} }},
    order: [{{ created_at: ASC }}]
  ) {{ {AGENT_TOOL_RESULT_FIELDS} }}
}}"#
    );
    let started = std::time::Instant::now();
    let data = execute_local_graphql_query(node, &query, "session diagnostics").await?;
    let messages: Vec<AgentMessageRow> = parse_query_rows(&data, AGENT_MESSAGE_NAME)?;
    let tool_calls: Vec<AgentToolCallRow> = parse_query_rows(&data, AGENT_TOOL_CALL_NAME)?;
    let tool_results: Vec<AgentToolResultRow> = parse_query_rows(&data, AGENT_TOOL_RESULT_NAME)?;
    tracing::debug!(
        target: "gents_desktop_core::query",
        session_id,
        message_rows = messages.len(),
        tool_call_rows = tool_calls.len(),
        tool_result_rows = tool_results.len(),
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "loaded exact on-demand DefraDB session diagnostics"
    );
    Ok(ClientStore::from_rows(ClientStoreRows {
        messages,
        tool_calls,
        tool_results,
        ..ClientStoreRows::default()
    }))
}
