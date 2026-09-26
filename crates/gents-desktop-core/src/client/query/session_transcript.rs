use super::*;
use gents::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, OutputSegmentRow,
    TranscriptMessageRow, AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS,
};
use gents_protocol::output::MessagePublication;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A page can contain at most 80 headers.  Dependency reads are additionally
/// capped so a malformed header cannot turn an interactive page render into an
/// unbounded request scan.  Hitting a cap is an honest query failure, never a
/// partial transcript or a fallback to a parent-session scan.
const MAX_FORK_ORIGIN_DEPTH: usize = 32;
const MAX_CANONICAL_DEPENDENCY_ROWS: usize = 2_048;

#[derive(Clone, Copy)]
enum TranscriptAccess<'a> {
    Local(&'a EmbeddedNode),
    Config(&'a gents::config_client::ConfigAccess),
}

impl TranscriptAccess<'_> {
    async fn execute(self, query: &str, operation: &str) -> Result<Value> {
        match self {
            Self::Local(node) => execute_local_graphql_query(node, query, operation).await,
            Self::Config(access) => execute_access_graphql_query(access, query, operation).await,
        }
    }
}

fn scope_filter(agent_did: &str, requester_did: Option<&str>) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let requester = requester_did
        .map(escape_graphql_string)
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".to_string());
    format!("agent_did: {{ _eq: \"{agent_did}\" }}, requester_did: {{ _eq: {requester} }}")
}

fn insert_exact_header(
    headers: &mut BTreeMap<String, TranscriptMessageRow>,
    row: TranscriptMessageRow,
) -> Result<()> {
    if let Some(existing) = headers.get(&row.doc_id) {
        anyhow::ensure!(
            existing.message == row.message,
            "conflicting canonical header facts share physical identity {}",
            row.doc_id
        );
        return Ok(());
    }
    headers.insert(row.doc_id.clone(), row);
    Ok(())
}

fn validate_known_header_identities(
    headers: &BTreeMap<String, TranscriptMessageRow>,
) -> Result<()> {
    let observed = headers
        .values()
        .map(|row| gents_protocol::output::origin::ObservedMessage {
            doc_id: &row.doc_id,
            message: &row.message,
        })
        .collect::<Vec<_>>();
    for row in headers.values() {
        gents_protocol::output::origin::lookup_message(
            &observed,
            &[],
            &row.doc_id,
            &row.message.agent_did,
            row.message.requester_did.as_deref(),
        )
        .map_err(anyhow::Error::new)?;
    }
    Ok(())
}

fn insert_exact_segment(
    records: &mut BTreeMap<String, OutputSegmentRow>,
    row: OutputSegmentRow,
) -> Result<()> {
    if let Some(existing) = records.get(&row.doc_id) {
        anyhow::ensure!(
            existing.segment == row.segment,
            "conflicting canonical output facts share physical identity {}",
            row.doc_id
        );
        return Ok(());
    }
    records.insert(row.doc_id.clone(), row);
    Ok(())
}

/// Resolve only immutable facts named by the current page.  A fork origin is
/// addressed by its physical document id and can therefore live in another
/// session; we never substitute a latest/parent-session header.  Output
/// records are similarly read only for a referenced closure's request/source
/// scope, not for every segment in the selected session.
async fn load_page_canonical_dependencies(
    access: TranscriptAccess<'_>,
    page_headers: &[TranscriptMessageRow],
) -> Result<(CanonicalTranscriptDependencies, u64, usize)> {
    let mut dependencies = CanonicalTranscriptDependencies::default();
    let mut known_headers = BTreeMap::new();
    for header in page_headers.iter().cloned() {
        insert_exact_header(&mut known_headers, header)?;
    }
    validate_known_header_identities(&known_headers)?;
    let mut pending_origins = page_headers
        .iter()
        .filter_map(|header| match &header.message.publication {
            MessagePublication::Fork {
                origin_message_doc_id,
            } => Some((
                origin_message_doc_id.clone(),
                header.message.agent_did.clone(),
                header.message.requester_did.clone(),
                1_usize,
            )),
            _ => None,
        })
        .collect::<VecDeque<_>>();
    let mut queried_headers = BTreeSet::new();
    let mut query_count = 0_u64;
    let mut queried_rows = 0_usize;

    while let Some((origin_id, agent_did, requester_did, depth)) = pending_origins.pop_front() {
        if known_headers.contains_key(&origin_id) || !queried_headers.insert(origin_id.clone()) {
            continue;
        }
        if depth > MAX_FORK_ORIGIN_DEPTH {
            bail!(
                "canonical fork origin chain exceeds bounded depth {MAX_FORK_ORIGIN_DEPTH} at {origin_id}"
            );
        }
        let origin_id_escaped = escape_graphql_string(&origin_id);
        let query = format!(
            r#"query DesktopExactForkOrigin {{
  AgentMessage(filter: {{ _docID: {{ _eq: "{origin_id_escaped}" }}, {} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }}
}}"#,
            scope_filter(&agent_did, requester_did.as_deref()),
        );
        let data = access
            .execute(&query, "exact canonical fork origin")
            .await?;
        query_count += 1;
        let rows = parse_canonical_rows(&data, AGENT_MESSAGE_NAME, decode_transcript_message_row)?;
        queried_rows = queried_rows.saturating_add(rows.len());
        match rows.len() {
            0 => {
                // ACP-filtered absence is deliberately only absence.  The
                // caller has not received evidence that this identity was
                // denied, so reconstruction must remain Loading.
            }
            1 => {
                let origin = rows.into_iter().next().expect("one row checked");
                if origin.doc_id != origin_id
                    || origin.message.agent_did != agent_did
                    || origin.message.requester_did != requester_did
                {
                    bail!("exact canonical fork origin crossed its authorized scope");
                }
                if let MessagePublication::Fork {
                    origin_message_doc_id,
                } = &origin.message.publication
                {
                    pending_origins.push_back((
                        origin_message_doc_id.clone(),
                        origin.message.agent_did.clone(),
                        origin.message.requester_did.clone(),
                        depth.saturating_add(1),
                    ));
                }
                insert_exact_header(&mut known_headers, origin.clone())?;
                validate_known_header_identities(&known_headers)?;
                dependencies.origin_headers.push(origin);
            }
            _ => bail!("exact canonical fork origin has conflicting physical rows: {origin_id}"),
        }
    }

    let close_scopes = known_headers
        .values()
        .flat_map(|header| {
            header
                .message
                .payload_references()
                .into_iter()
                .map(move |reference| {
                    (
                        reference.close_doc_id.clone(),
                        header.message.agent_did.clone(),
                        header.message.requester_did.clone(),
                    )
                })
        })
        .collect::<BTreeSet<_>>();
    if close_scopes.len() > MAX_CANONICAL_DEPENDENCY_ROWS {
        bail!(
            "canonical transcript page names more than {MAX_CANONICAL_DEPENDENCY_ROWS} output closures"
        );
    }

    let mut closures = Vec::new();
    for (close_id, agent_did, requester_did) in close_scopes {
        let close_id_escaped = escape_graphql_string(&close_id);
        let query = format!(
            r#"query DesktopExactOutputClosure {{
  AgentOutputSegment(filter: {{ _docID: {{ _eq: "{close_id_escaped}" }}, {} }}, limit: 2) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
}}"#,
            scope_filter(&agent_did, requester_did.as_deref()),
        );
        let data = access
            .execute(&query, "exact canonical output closure")
            .await?;
        query_count += 1;
        let rows =
            parse_canonical_rows(&data, AGENT_OUTPUT_SEGMENT_NAME, decode_output_segment_row)?;
        queried_rows = queried_rows.saturating_add(rows.len());
        match rows.len() {
            0 => {}
            1 => {
                let closure = rows.into_iter().next().expect("one row checked");
                if closure.doc_id != close_id
                    || closure.segment.agent_did != agent_did
                    || closure.segment.requester_did != requester_did
                {
                    bail!("exact canonical output closure crossed its authorized scope");
                }
                closures.push(closure);
            }
            _ => bail!("exact canonical output closure has conflicting physical rows: {close_id}"),
        }
    }

    // The schema intentionally indexes request identity, rather than the JSON
    // source value.  Read that bounded request slice, then retain exactly the
    // source named by the closure.  The hard limit makes a malformed/high-volume
    // request fail visibly instead of silently dropping a segment.
    let mut sources_by_request = BTreeMap::new();
    for closure in &closures {
        sources_by_request
            .entry((
                closure.segment.request_doc_id.clone(),
                closure.segment.agent_did.clone(),
                closure.segment.requester_did.clone(),
            ))
            .or_insert_with(BTreeSet::new)
            .insert(serde_json::to_string(&closure.segment.source)?);
    }
    let mut records = BTreeMap::<String, OutputSegmentRow>::new();
    for closure in closures {
        insert_exact_segment(&mut records, closure)?;
    }
    for (request_scope, expected_sources) in sources_by_request {
        let request_id = escape_graphql_string(&request_scope.0);
        let query = format!(
            r#"query DesktopReferencedOutputSource {{
  AgentOutputSegment(
    filter: {{ request_doc_id: {{ _eq: "{request_id}" }}, {} }},
    order: [{{ ordinal: ASC }}, {{ _docID: ASC }}],
    limit: {}
  ) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
}}"#,
            scope_filter(&request_scope.1, request_scope.2.as_deref()),
            MAX_CANONICAL_DEPENDENCY_ROWS.saturating_add(1),
        );
        let data = access
            .execute(&query, "referenced canonical output source")
            .await?;
        query_count += 1;
        let rows =
            parse_canonical_rows(&data, AGENT_OUTPUT_SEGMENT_NAME, decode_output_segment_row)?;
        queried_rows = queried_rows.saturating_add(rows.len());
        if rows.len() > MAX_CANONICAL_DEPENDENCY_ROWS {
            bail!(
                "canonical output request {} exceeds bounded dependency read of {MAX_CANONICAL_DEPENDENCY_ROWS} rows",
                request_scope.0
            );
        }
        for row in rows {
            if expected_sources.contains(&serde_json::to_string(&row.segment.source)?) {
                insert_exact_segment(&mut records, row)?;
            }
        }
    }
    dependencies.output_segments = records.into_values().collect();
    Ok((dependencies, query_count, queried_rows))
}

pub(super) fn tool_group_cursor_sequence(cursor: &str) -> Option<i64> {
    cursor
        .strip_prefix("tools-")
        .and_then(|value| value.parse::<i64>().ok())
}

async fn resolve_transcript_cursor_sequence(
    access: TranscriptAccess<'_>,
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
            .unwrap_or_else(|| ", requester_did: { _eq: null }".to_string());
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
        let data = access.execute(&query, "session tool cursor").await?;
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
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_string());
    let query = format!(
        r#"query {{
  AgentMessage(
    filter: {{
      session_id: {{ _eq: "{session_id}" }},
      message_key: {{ _eq: "{message_key}" }}{agent_filter}{requester_filter}
    }},
    limit: 1
  ) {{ sequence }}
}}"#
    );
    let data = access.execute(&query, "session message cursor").await?;
    let rows: Vec<TranscriptCursorRow> = parse_query_rows(&data, AGENT_MESSAGE_NAME)?;
    rows.first()
        .and_then(|row| row.sequence)
        .ok_or_else(|| anyhow!("session transcript cursor is no longer present: {cursor}"))
}

/// The exact requester scope under which one session's transcript is read.
///
/// Transcript rows carry their session's requester scope, and a desktop
/// reading an enrolled agent defaults to its own principal. A subagent the
/// agent spawns for itself is admitted as a `LocalChild`, whose requester is
/// the agent's own DID, so under the principal its session reads as empty.
/// Only the agent's operator (its operator GraphQL endpoint, which already
/// carries full read authority over that runtime) reads such a session under
/// the session's own scope. Every other session keeps the principal scope, so
/// sessions requested by other principals are never widened into view.
pub fn session_transcript_requester_scope(
    session: Option<&AgentSession>,
    agent_did: Option<&str>,
    principal_scope: Option<&str>,
    operator: bool,
) -> Option<String> {
    let agent_own_scope = match (session, agent_did) {
        (Some(session), Some(agent_did)) => {
            session.agent_did == agent_did && session.requester_did.as_deref() == Some(agent_did)
        }
        _ => false,
    };
    if operator && agent_own_scope {
        return agent_did.map(str::to_owned);
    }
    principal_scope.map(str::to_owned)
}

/// Why no scope this client can present reads one session's transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTranscriptDenial {
    /// The session records its own agent as requester, as work the agent
    /// admitted for itself does, and this client holds no operator authority
    /// over that runtime.
    AgentOwnedWithoutOperatorAccess,
    /// The session records a requester scope other than the one this client
    /// can present.
    RequesterScopeMismatch,
}

/// Whether the scope [`session_transcript_requester_scope`] chose can return
/// this session's rows at all, and if not, why.
///
/// Transcript rows carry their session's own requester scope, so reading under
/// any other scope is empty because the scopes differ and not because the
/// session has no activity. `None` means the chosen scope is the session's own,
/// so an empty read there is an empty session. A denial is stated only from the
/// session document itself, never from the size of a read result.
pub fn session_transcript_denial(
    session: Option<&AgentSession>,
    agent_did: Option<&str>,
    principal_scope: Option<&str>,
    operator: bool,
) -> Option<SessionTranscriptDenial> {
    let session = session?;
    let scope =
        session_transcript_requester_scope(Some(session), agent_did, principal_scope, operator);
    if session.requester_did.as_deref() == scope.as_deref() {
        return None;
    }
    Some(
        if agent_did.is_some() && session.requester_did.as_deref() == agent_did {
            SessionTranscriptDenial::AgentOwnedWithoutOperatorAccess
        } else {
            SessionTranscriptDenial::RequesterScopeMismatch
        },
    )
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
    load_session_transcript_page_with_access(
        TranscriptAccess::Local(node),
        session_id,
        agent_did,
        requester_did,
        before_item_key,
        requested_limit,
    )
    .await
}

pub async fn load_session_transcript_page_on(
    access: &gents::config_client::ConfigAccess,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
    before_item_key: Option<&str>,
    requested_limit: Option<usize>,
) -> Result<SessionTranscriptQueryPage> {
    load_session_transcript_page_with_access(
        TranscriptAccess::Config(access),
        session_id,
        agent_did,
        requester_did,
        before_item_key,
        requested_limit,
    )
    .await
}

async fn load_session_transcript_page_with_access(
    access: TranscriptAccess<'_>,
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
            resolve_transcript_cursor_sequence(
                access,
                session_id,
                agent_did,
                requester_did,
                cursor,
            )
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
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_string());
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
    let transcript_data = access
        .execute(&transcript_query, "session transcript page")
        .await?;
    let queried_messages = parse_canonical_rows(
        &transcript_data,
        AGENT_MESSAGE_NAME,
        decode_transcript_message_row,
    )?;
    let queried_tool_calls: Vec<AgentToolCallRow> =
        parse_query_rows(&transcript_data, AGENT_TOOL_CALL_NAME)?;
    let messages_exhausted = queried_messages.len() < message_query_limit;
    let mut messages = queried_messages.clone();
    let deferred_message_sequence = (!messages_exhausted)
        .then(|| queried_messages.last().map(|row| row.message.sequence))
        .flatten();
    if let Some(sequence) = deferred_message_sequence {
        messages.retain(|row| row.message.sequence != sequence);
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
            .is_some_and(|sequence| sequence <= i64::from(boundary))
    });
    let tools_exhausted =
        queried_tool_calls.len() < tool_call_query_limit || crossed_message_boundary;
    let mut tool_calls = queried_tool_calls
        .iter()
        .filter(|row| {
            deferred_message_sequence.is_none_or(|boundary| {
                row.message_sequence
                    .is_some_and(|sequence| sequence > i64::from(boundary))
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
        messages.retain(|row| i64::from(row.message.sequence) > deferred_sequence);
        if tool_calls.is_empty() {
            bail!(
                "session transcript has more than {} tool calls at sequence {deferred_sequence}; the sequence-atomic tool budget cannot represent it losslessly",
                SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET
            );
        }
    }
    let (canonical_dependencies, dependency_query_count, dependency_rows) =
        load_page_canonical_dependencies(access, &messages).await?;
    let source_exhausted = messages_exhausted && tools_exhausted;
    let queried_rows = queried_messages
        .len()
        .saturating_add(queried_tool_calls.len())
        .saturating_add(dependency_rows);
    let query_count = 1 + u64::from(before_item_key.is_some()) + dependency_query_count;
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
            transcript_messages: messages,
            output_segments: canonical_dependencies.output_segments.clone(),
            tool_calls,
            ..ClientStoreRows::default()
        }),
        canonical_dependencies,
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
    load_session_context_store_with_access(
        TranscriptAccess::Local(node),
        session_id,
        agent_did,
        requester_did,
    )
    .await
}

pub async fn load_session_context_store_on(
    access: &gents::config_client::ConfigAccess,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
) -> Result<ClientStore> {
    load_session_context_store_with_access(
        TranscriptAccess::Config(access),
        session_id,
        agent_did,
        requester_did,
    )
    .await
}

async fn load_session_context_store_with_access(
    access: TranscriptAccess<'_>,
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
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_string());
    let query = format!(
        r#"query DesktopSessionContext {{
  AgentMessage(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter} }},
    order: [{{ sequence: ASC }}, {{ message_key: ASC }}]
  ) {{ {AGENT_MESSAGE_FIELDS} }}
  AgentOutputSegment(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter} }}
  ) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
  CompactionEntry(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter} }},
    order: [{{ sequence: ASC }}, {{ compaction_key: ASC }}]
  ) {{ {COMPACTION_ENTRY_FIELDS} }}
}}"#
    );
    let started = std::time::Instant::now();
    let data = access.execute(&query, "session context").await?;
    let messages = parse_canonical_rows(&data, AGENT_MESSAGE_NAME, decode_transcript_message_row)?;
    let output_segments =
        parse_canonical_rows(&data, AGENT_OUTPUT_SEGMENT_NAME, decode_output_segment_row)?;
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
        transcript_messages: messages,
        output_segments,
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
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_string());
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
  AgentOutputSegment(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter} }}
  ) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
}}"#
    );
    let started = std::time::Instant::now();
    let data = execute_local_graphql_query(node, &query, "session diagnostics").await?;
    let messages = parse_canonical_rows(&data, AGENT_MESSAGE_NAME, decode_transcript_message_row)?;
    let tool_calls: Vec<AgentToolCallRow> = parse_query_rows(&data, AGENT_TOOL_CALL_NAME)?;
    let output_segments =
        parse_canonical_rows(&data, AGENT_OUTPUT_SEGMENT_NAME, decode_output_segment_row)?;
    tracing::debug!(
        target: "gents_desktop_core::query",
        session_id,
        message_rows = messages.len(),
        tool_call_rows = tool_calls.len(),
        output_segment_rows = output_segments.len(),
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "loaded exact on-demand DefraDB session diagnostics"
    );
    Ok(ClientStore::from_rows(ClientStoreRows {
        transcript_messages: messages,
        tool_calls,
        output_segments,
        ..ClientStoreRows::default()
    }))
}

#[cfg(test)]
mod dependency_identity_tests {
    use super::*;
    use gents_protocol::output::{
        MessagePublication, MessageRole, OutputOutcome, TranscriptMessage,
    };

    fn header(doc_id: &str, message_key: &str, sequence: u32) -> TranscriptMessageRow {
        TranscriptMessageRow {
            doc_id: doc_id.to_string(),
            message: TranscriptMessage {
                message_key: message_key.to_string(),
                session_id: "session".to_string(),
                agent_did: "agent".to_string(),
                requester_did: None,
                request_doc_id: Some("request".to_string()),
                publication: MessagePublication::RequestExecution {
                    execution_generation: "generation".to_string(),
                },
                outcome: OutputOutcome::Complete,
                sequence,
                role: MessageRole::Assistant,
                native_id: None,
                blocks: Vec::new(),
                created_at: "2026-09-01T00:00:00Z".to_string(),
            },
        }
    }

    #[test]
    fn exact_dependency_headers_reject_physical_twins_and_delegate_logical_twins() {
        let first = header("physical-a", "message-a", 1);
        let mut headers = BTreeMap::new();
        insert_exact_header(&mut headers, first.clone()).expect("first fact");

        let mut physical_conflict = first.clone();
        physical_conflict.message.native_id = Some("different".to_string());
        assert!(insert_exact_header(&mut headers, physical_conflict).is_err());

        let same_key = header("physical-b", "message-a", 2);
        insert_exact_header(&mut headers, same_key).expect("retain keyed twin observation");
        assert!(validate_known_header_identities(&headers).is_err());

        let mut sequence_headers = BTreeMap::new();
        insert_exact_header(&mut sequence_headers, first).expect("first fact");
        let same_sequence = header("physical-c", "message-c", 1);
        insert_exact_header(&mut sequence_headers, same_sequence)
            .expect("retain sequence twin observation");
        assert!(validate_known_header_identities(&sequence_headers).is_err());
    }

    #[test]
    fn exact_dependency_segments_reject_conflicting_physical_identity() {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter, SourceClose};

        let segment = OutputSegmentRow {
            doc_id: "segment".to_string(),
            segment: OutputSegment {
                agent_did: "agent".to_string(),
                requester_did: None,
                session_id: "session".to_string(),
                request_doc_id: "request".to_string(),
                source: OutputSource::Authored {
                    key: "prompt".to_string(),
                },
                writer: OutputWriter::RequestExecution {
                    execution_generation: "generation".to_string(),
                },
                ordinal: None,
                runs: Vec::new(),
                payload: String::new(),
                close: Some(SourceClose::Retracted),
                created_at: "2026-09-01T00:00:00Z".to_string(),
            },
        };
        let mut rows = BTreeMap::new();
        insert_exact_segment(&mut rows, segment.clone()).expect("first fact");
        let mut conflict = segment;
        conflict.segment.created_at = "2026-09-01T00:00:01Z".to_string();
        assert!(insert_exact_segment(&mut rows, conflict).is_err());
    }
}
