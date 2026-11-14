use super::*;
use gents_protocol::document_query::Direction;
use gents_protocol::output::rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_protocol::output::MessagePublication;
use gents_session::views::requests::RequestRows;
use gents_session::views::transcript::{
    CompactionRows, MessageRows, MessageSequences, Scope, SegmentRows, SequenceRange, ToolCallRows,
    ToolSequences,
};
use gents_store::config_client::ConfigAccess;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Bound traversal of fork origins supplied by persisted session headers.
const MAX_FORK_ORIGIN_DEPTH: usize = 32;
/// A page fails rather than projecting incomplete canonical dependencies when
/// their bounded read would exceed this budget.
const MAX_CANONICAL_DEPENDENCY_ROWS: usize = 2_048;

fn scoped_session(
    session_id: &str,
    node_did: Option<&str>,
    requester_did: Option<&str>,
) -> Result<Scope> {
    Ok(Scope::Session(gents_protocol::session::SessionScope {
        node_did: node_did
            .context("session read requires a node DID")?
            .to_owned(),
        session_id: session_id.to_owned(),
        requester_did: requester_did.map(str::to_owned),
    }))
}

/// Request documents by physical identity, in batches, returning the rows
/// found and how many distinct identities were asked for. A caller that
/// needs every one compares the two; a transcript read does not, since a
/// request can be unreadable or not yet replicated while its messages are.
pub(super) async fn read_request_documents(
    access: &ConfigAccess,
    node_did: &str,
    doc_ids: &[String],
    operation: &str,
) -> Result<(Vec<AgentRequestRow>, usize)> {
    const BATCH_SIZE: usize = 128;
    let mut expected =
        rapidhash::RapidHashSet::with_capacity_and_hasher(doc_ids.len(), Default::default());
    let unique_doc_ids = doc_ids
        .iter()
        .filter(|doc_id| expected.insert((*doc_id).clone()))
        .cloned()
        .collect::<Vec<_>>();
    let mut rows = Vec::with_capacity(unique_doc_ids.len());
    let mut observed =
        rapidhash::RapidHashSet::with_capacity_and_hasher(unique_doc_ids.len(), Default::default());
    for batch in unique_doc_ids.chunks(BATCH_SIZE) {
        let view = RequestRows::new(Scope::Node(node_did.to_owned())).doc_ids(batch.to_vec());
        let batch_rows = access.read_view(&view).await?;
        for row in &batch_rows {
            let doc_id = row
                .doc_id
                .as_ref()
                .with_context(|| format!("{operation} lacks physical identity"))?;
            anyhow::ensure!(
                batch.contains(doc_id),
                "{operation} returned an unrelated document"
            );
            anyhow::ensure!(
                observed.insert(doc_id.to_owned()),
                "{operation} duplicated a document"
            );
        }
        rows.extend(batch_rows);
    }
    Ok((rows, unique_doc_ids.len()))
}

async fn read_legacy_question_sources<'a>(
    access: &ConfigAccess,
    requests: impl Iterator<Item = &'a AgentRequestRow>,
) -> Result<(Vec<MailboxItemRow>, u64)> {
    let mut sources = BTreeMap::<String, BTreeSet<String>>::new();
    for request in requests {
        if !matches!(
            gents_session::request::lifecycle::request_origin(request),
            gents_session::request::lifecycle::RequestOrigin::Person
        ) || request
            .input
            .as_ref()
            .is_some_and(|input| input.question_reply.is_some())
        {
            continue;
        }
        if let (Some(node), Some(doc)) = (
            request.node_did.as_ref(),
            request
                .caused_by_source_doc_id
                .as_ref()
                .filter(|id| !id.is_empty()),
        ) {
            sources.entry(node.clone()).or_default().insert(doc.clone());
        }
    }
    let mut rows = Vec::new();
    let mut count = 0;
    for (node, ids) in sources {
        let ids = ids.into_iter().collect::<Vec<_>>();
        for batch in ids.chunks(128) {
            let view = gents_automation::views::MailboxRows::new(&node).doc_ids(batch.to_vec());
            count += 1;
            match tokio::time::timeout(std::time::Duration::from_secs(1), access.read_view(&view))
                .await
            {
                Ok(Ok(found)) => rows.extend(found),
                Ok(Err(error)) if error.permission_denied() => return Err(error.into()),
                Ok(Err(error)) => tracing::debug!(%error,"question reply sources unavailable"),
                Err(_) => tracing::debug!("question reply sources timed out"),
            }
        }
    }
    Ok((rows, count))
}

#[cfg(test)]
mod question_source_tests {
    use super::*;
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::NodeIdentity;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn hidden_question_source_is_empty_but_explicit_denial_is_returned() {
        let request = AgentRequestRow {
            request_id: "request".into(),
            caused_by_source_doc_id: Some("question".into()),
            node_did: Some("node".into()),
            ..Default::default()
        };
        for denied in [false, true] {
            let server = MockServer::start().await;
            let body = if denied {
                serde_json::json!({"errors":[{"message":"opaque", "extensions":{"code":"PERMISSION_DENIED"}}]})
            } else {
                serde_json::json!({"data":{"MailboxItem":[]}})
            };
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            let identity = tokio::task::spawn_blocking(|| {
                gents_store::test_support::test_identity("question-reader")
            })
            .await
            .unwrap();
            let access = ConfigAccess::Graphql(
                gents_store::config_client::GraphqlEndpoint::with_authenticated_delegation(
                    format!("{}/api/v0/graphql", server.uri()),
                    "Bearer fixture",
                    identity.did().parse().unwrap(),
                )
                .unwrap(),
            );
            let result = read_legacy_question_sources(&access, [&request].into_iter()).await;
            if denied {
                assert!(result.unwrap_err().permission_denied());
            } else {
                assert!(result.unwrap().0.is_empty());
            }
        }
    }
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
            &row.message.node_did,
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

/// Loads the copies each page fork header resolves within its own session
/// (`gents_protocol::output::reconstruction::fork_copy_observations`) and
/// returns the headers they cover; those need nothing from their origin.
async fn load_fork_copies(
    access: &ConfigAccess,
    page_headers: &[TranscriptMessageRow],
    records: &mut BTreeMap<String, OutputSegmentRow>,
    query_count: &mut u64,
    queried_rows: &mut usize,
) -> Result<BTreeSet<String>> {
    let mut scopes = BTreeMap::<(String, String, Option<String>), BTreeSet<String>>::new();
    for header in page_headers {
        if !matches!(header.message.publication, MessagePublication::Fork { .. }) {
            continue;
        }
        scopes
            .entry((
                header.message.session_id.clone(),
                header.message.node_did.clone(),
                header.message.requester_did.clone(),
            ))
            .or_default()
            .extend(
                header
                    .message
                    .payload_references()
                    .into_iter()
                    .map(|reference| reference.close_doc_id.clone()),
            );
    }
    let mut copies = Vec::new();
    for ((session_id, node_did, requester_did), logicals) in &scopes {
        if logicals.is_empty() {
            continue;
        }
        if logicals.len() > MAX_CANONICAL_DEPENDENCY_ROWS {
            bail!(
                "canonical transcript page names more than {MAX_CANONICAL_DEPENDENCY_ROWS} fork copies"
            );
        }
        let mut view = SegmentRows::new(scoped_session(
            session_id,
            Some(node_did),
            requester_did.as_deref(),
        )?);
        view.request_doc_ids = Some(logicals.iter().cloned().collect());
        view.limit = Some((MAX_CANONICAL_DEPENDENCY_ROWS + 1) as u64);
        let rows = access
            .read_view(&view)
            .await?
            .into_iter()
            .map(|row| row.into_canonical())
            .collect::<Vec<_>>();
        *query_count += 1;
        *queried_rows = queried_rows.saturating_add(rows.len());
        if rows.len() > MAX_CANONICAL_DEPENDENCY_ROWS {
            bail!(
                "canonical fork copies exceed bounded dependency read of {MAX_CANONICAL_DEPENDENCY_ROWS} rows"
            );
        }
        for row in rows {
            anyhow::ensure!(
                row.segment.session_id == *session_id
                    && row.segment.node_did == *node_did
                    && row.segment.requester_did == *requester_did
                    && logicals.contains(&row.segment.request_doc_id),
                "canonical fork copy crossed its session scope"
            );
            copies.push(row);
        }
    }
    let observed = copies
        .iter()
        .map(
            |row| gents_protocol::output::reconstruction::ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            },
        )
        .collect::<Vec<_>>();
    let copied = page_headers
        .iter()
        .filter(|header| {
            gents_protocol::output::reconstruction::fork_copy_observations(
                &header.message,
                &observed,
            )
            .is_some()
        })
        .map(|header| header.doc_id.clone())
        .collect();
    for row in copies {
        insert_exact_segment(records, row)?;
    }
    Ok(copied)
}

async fn load_page_canonical_dependencies(
    access: &ConfigAccess,
    page_headers: &[TranscriptMessageRow],
    page_tools: &[AgentToolCallRow],
) -> Result<(CanonicalTranscriptDependencies, u64, usize)> {
    let mut dependencies = CanonicalTranscriptDependencies::default();
    let mut known_headers = BTreeMap::new();
    for header in page_headers.iter().cloned() {
        insert_exact_header(&mut known_headers, header)?;
    }
    validate_known_header_identities(&known_headers)?;
    let mut query_count = 0_u64;
    let mut queried_rows = 0_usize;
    let mut records = BTreeMap::<String, OutputSegmentRow>::new();
    let copied = load_fork_copies(
        access,
        page_headers,
        &mut records,
        &mut query_count,
        &mut queried_rows,
    )
    .await?;
    // A copied fork reconstructs from its own documents (Lean
    // `SessionFork.fork_reader_needs_only_child_grant`); its origin chain is
    // provenance only (`retentionDependencies`). It is still followed where
    // the reader may see it, for the input attribution a copy keeps, but its
    // headers never become content to resolve.
    let mut pending_origins = page_headers
        .iter()
        .filter_map(|header| match &header.message.publication {
            MessagePublication::Fork {
                origin_message_doc_id,
            } => Some((
                origin_message_doc_id.clone(),
                header.message.node_did.clone(),
                header.message.requester_did.clone(),
                1_usize,
                copied.contains(&header.doc_id),
            )),
            _ => None,
        })
        .collect::<VecDeque<_>>();
    let mut queried_headers = BTreeSet::new();

    while let Some((origin_id, node_did, requester_did, depth, provenance)) =
        pending_origins.pop_front()
    {
        if known_headers.contains_key(&origin_id) || !queried_headers.insert(origin_id.clone()) {
            continue;
        }
        if depth > MAX_FORK_ORIGIN_DEPTH {
            bail!(
                "canonical fork origin chain exceeds bounded depth {MAX_FORK_ORIGIN_DEPTH} at {origin_id}"
            );
        }
        let mut view = MessageRows::new(Scope::Origin {
            node_did: node_did.clone(),
            requester_did: requester_did.clone(),
        });
        view.document_ids = Some(vec![origin_id.clone()]);
        view.limit = Some(2);
        let rows = access
            .read_view(&view)
            .await?
            .into_iter()
            .map(|row| row.into_canonical())
            .collect::<Vec<_>>();
        query_count += 1;
        queried_rows = queried_rows.saturating_add(rows.len());
        match rows.len() {
            0 => {}
            1 => {
                let origin = rows.into_iter().next().expect("one row checked");
                if origin.doc_id != origin_id
                    || origin.message.node_did != node_did
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
                        origin.message.node_did.clone(),
                        origin.message.requester_did.clone(),
                        depth.saturating_add(1),
                        provenance,
                    ));
                }
                if !provenance {
                    insert_exact_header(&mut known_headers, origin.clone())?;
                    validate_known_header_identities(&known_headers)?;
                }
                dependencies.origin_headers.push(origin);
            }
            _ => bail!("exact canonical fork origin has conflicting physical rows: {origin_id}"),
        }
    }

    let mut close_scopes = known_headers
        .values()
        .filter(|header| !copied.contains(&header.doc_id))
        .flat_map(|header| {
            header
                .message
                .payload_references()
                .into_iter()
                .map(move |reference| {
                    (
                        reference.close_doc_id.clone(),
                        header.message.node_did.clone(),
                        header.message.requester_did.clone(),
                    )
                })
        })
        .collect::<BTreeSet<_>>();
    for tool in page_tools {
        if let (Some(payload), Some(node_did)) = (&tool.terminal_output, &tool.node_did) {
            close_scopes.insert((
                payload.output.close_doc_id.clone(),
                node_did.clone(),
                tool.requester_did.clone(),
            ));
        }
    }
    if close_scopes.len() > MAX_CANONICAL_DEPENDENCY_ROWS {
        bail!(
            "canonical transcript page names more than {MAX_CANONICAL_DEPENDENCY_ROWS} output closures"
        );
    }

    let mut closures = Vec::new();
    let close_scopes = close_scopes.into_iter().collect::<Vec<_>>();
    for batch in close_scopes.chunks(32) {
        let views = batch
            .iter()
            .map(|(close_id, node, requester)| {
                let mut view = SegmentRows::new(Scope::Origin {
                    node_did: node.clone(),
                    requester_did: requester.clone(),
                });
                view.document_ids = Some(vec![close_id.clone()]);
                view.limit = Some(2);
                view
            })
            .collect::<Vec<_>>();
        let result = access.read_views(&views).await?;
        query_count += 1;
        for ((close_id, node_did, requester_did), rows) in batch.iter().zip(result) {
            let rows = rows
                .into_iter()
                .map(|row| row.into_canonical())
                .collect::<Vec<_>>();
            queried_rows = queried_rows.saturating_add(rows.len());
            match rows.len() {
                0 => {}
                1 => {
                    let closure = rows.into_iter().next().expect("one row checked");
                    if closure.doc_id != *close_id
                        || closure.segment.node_did != *node_did
                        || closure.segment.requester_did != *requester_did
                    {
                        bail!("exact canonical output closure crossed its authorized scope");
                    }
                    closures.push(closure);
                }
                _ => bail!(
                    "exact canonical output closure has conflicting physical rows: {close_id}"
                ),
            }
        }
    }

    let mut sources_by_request = BTreeMap::new();
    for tool in page_tools.iter().filter(|tool| {
        tool.lifecycle_state.as_deref() == Some("running")
            || (tool.spawned_by_tool_call_doc_id.is_some()
                && gents_protocol::wait_status::WaitLifecycle::from_persisted(
                    tool.lifecycle_state.as_deref(),
                )
                .is_terminal())
    }) {
        let (Some(tool_doc_id), Some(request_doc_id), Some(node_did)) = (
            tool.doc_id.as_deref(),
            tool.request_doc_id.as_deref(),
            tool.node_did.as_deref(),
        ) else {
            continue;
        };
        sources_by_request
            .entry((
                request_doc_id.to_owned(),
                node_did.to_owned(),
                tool.requester_did.clone(),
            ))
            .or_insert_with(BTreeSet::new)
            .insert(serde_json::to_string(
                &gents_protocol::output::OutputSource::ToolCall {
                    tool_call_doc_id: tool_doc_id.to_owned(),
                },
            )?);
    }
    for closure in &closures {
        sources_by_request
            .entry((
                closure.segment.request_doc_id.clone(),
                closure.segment.node_did.clone(),
                closure.segment.requester_did.clone(),
            ))
            .or_insert_with(BTreeSet::new)
            .insert(serde_json::to_string(&closure.segment.source)?);
    }
    for closure in closures {
        insert_exact_segment(&mut records, closure)?;
    }
    let sources_by_request = sources_by_request.into_iter().collect::<Vec<_>>();
    for batch in sources_by_request.chunks(8) {
        let views = batch
            .iter()
            .map(|(request_scope, sources)| -> Result<_> {
                let mut view = SegmentRows::new(Scope::Origin {
                    node_did: request_scope.1.clone(),
                    requester_did: request_scope.2.clone(),
                });
                view.request_doc_ids = Some(vec![request_scope.0.clone()]);
                view.sources = sources
                    .iter()
                    .map(|source| serde_json::from_str(source))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                view.limit = Some((MAX_CANONICAL_DEPENDENCY_ROWS + 1) as u64);
                Ok(view)
            })
            .collect::<Result<Vec<_>>>()?;
        let result = access.read_views(&views).await?;
        query_count += 1;
        for ((request_scope, expected_sources), rows) in batch.iter().zip(result) {
            let rows = rows
                .into_iter()
                .map(|row| row.into_canonical())
                .collect::<Vec<_>>();
            queried_rows = queried_rows.saturating_add(rows.len());
            if rows.len() > MAX_CANONICAL_DEPENDENCY_ROWS {
                bail!(
                    "canonical output request {} exceeds bounded dependency read of {MAX_CANONICAL_DEPENDENCY_ROWS} rows",
                    request_scope.0
                );
            }
            for row in rows {
                anyhow::ensure!(
                    row.segment.request_doc_id == request_scope.0
                        && row.segment.node_did == request_scope.1
                        && row.segment.requester_did == request_scope.2,
                    "canonical output source crossed its authorized request scope"
                );
                if expected_sources.contains(&serde_json::to_string(&row.segment.source)?) {
                    insert_exact_segment(&mut records, row)?;
                }
            }
        }
    }
    dependencies.output_segments = records.into_values().collect();
    let mut origin_request_docs = BTreeMap::<String, BTreeSet<String>>::new();
    for header in &dependencies.origin_headers {
        if header.message.role == gents_protocol::output::MessageRole::User {
            if let Some(doc) = &header.message.request_doc_id {
                origin_request_docs
                    .entry(header.message.node_did.clone())
                    .or_default()
                    .insert(doc.clone());
            }
        }
    }
    for (node, docs) in origin_request_docs {
        let docs = docs.into_iter().collect::<Vec<_>>();
        let (requests, _) =
            read_request_documents(access, &node, &docs, "fork input attribution").await?;
        for request in &requests {
            anyhow::ensure!(
                dependencies.origin_headers.iter().any(|header| {
                    header.message.request_doc_id == request.doc_id
                        && request.node_did.as_deref() == Some(header.message.node_did.as_str())
                        && request.session_id.as_deref() == Some(header.message.session_id.as_str())
                        && request.requester_did == header.message.requester_did
                }),
                "fork input attribution crossed its source scope"
            );
        }
        query_count += docs.len().div_ceil(128) as u64;
        queried_rows = queried_rows.saturating_add(requests.len());
        dependencies.origin_requests.extend(requests);
    }
    Ok((dependencies, query_count, queried_rows))
}

pub(super) fn tool_group_cursor_sequence(cursor: &str) -> Option<i64> {
    cursor
        .strip_prefix("tools-")
        .and_then(|value| value.parse::<i64>().ok())
}

async fn resolve_transcript_cursor_sequence(
    access: &ConfigAccess,
    session_id: &str,
    node_did: Option<&str>,
    requester_did: Option<&str>,
    cursor: &str,
) -> Result<i64> {
    let scope = scoped_session(session_id, node_did, requester_did)?;
    let mut messages = MessageRows::new(scope.clone());
    messages.limit = Some(1);
    if let Some(sequence) = tool_group_cursor_sequence(cursor) {
        messages.sequence = SequenceRange::At(sequence);
        let mut tools = ToolCallRows::new(scope);
        tools.sequence = SequenceRange::At(sequence);
        tools.limit = Some(1);
        let (messages, tools) = access
            .read_views(&(MessageSequences(messages), ToolSequences(tools)))
            .await?;
        if messages.iter().any(|row| row.sequence == Some(sequence))
            || tools
                .iter()
                .any(|row| row.message_sequence == Some(sequence))
        {
            return Ok(sequence);
        }
    } else {
        messages.message_key = Some(cursor.to_owned());
        if let Some(sequence) = access
            .read_view(&MessageSequences(messages))
            .await?
            .first()
            .and_then(|row| row.sequence)
        {
            return Ok(sequence);
        }
    }
    bail!("session transcript cursor is no longer present: {cursor}")
}

/// The stretch of a session's message order a transcript read covers: the
/// newest messages, or those before or after a message number. Message
/// numbers are the session's sequences, so a reader holding rows can ask for
/// the next stretch without resolving a row key first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptWindow {
    Tip,
    Before(i64),
    After(i64),
}

pub async fn load_session_transcript_page_on(
    access: &ConfigAccess,
    session_id: &str,
    node_did: Option<&str>,
    requester_did: Option<&str>,
    before_item_key: Option<&str>,
    requested_limit: Option<usize>,
) -> Result<SessionTranscriptQueryPage> {
    let window = match before_item_key {
        Some(cursor) => TranscriptWindow::Before(
            resolve_transcript_cursor_sequence(
                access,
                session_id.trim(),
                node_did,
                requester_did,
                cursor,
            )
            .await?,
        ),
        None => TranscriptWindow::Tip,
    };
    let mut page = load_session_transcript_window_with_access(
        access,
        session_id,
        node_did,
        requester_did,
        window,
        requested_limit,
        false,
    )
    .await?;
    page.query_count += u64::from(before_item_key.is_some());
    Ok(page)
}

pub async fn load_session_transcript_window_on(
    access: &ConfigAccess,
    session_id: &str,
    node_did: Option<&str>,
    requester_did: Option<&str>,
    window: TranscriptWindow,
    requested_limit: Option<usize>,
) -> Result<SessionTranscriptQueryPage> {
    load_session_transcript_window_with_access(
        access,
        session_id,
        node_did,
        requester_did,
        window,
        requested_limit,
        true,
    )
    .await
}

async fn load_session_transcript_window_with_access(
    access: &ConfigAccess,
    session_id: &str,
    node_did: Option<&str>,
    requester_did: Option<&str>,
    window: TranscriptWindow,
    requested_limit: Option<usize>,
    with_requests: bool,
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
    /* a forward read walks up the order from its cursor; the others walk
    down from the newest message or from theirs */
    let forward = matches!(window, TranscriptWindow::After(_));
    let beyond = |sequence: i64, boundary: i64| {
        if forward {
            sequence < boundary
        } else {
            sequence > boundary
        }
    };
    let scope = scoped_session(session_id, node_did, requester_did)?;
    let sequence = match window {
        TranscriptWindow::Tip => SequenceRange::All,
        TranscriptWindow::Before(n) => SequenceRange::Before(n),
        TranscriptWindow::After(n) => SequenceRange::After(n),
    };
    let direction = if forward {
        Direction::Asc
    } else {
        Direction::Desc
    };
    let mut message_view = MessageRows::new(scope.clone());
    message_view.sequence = sequence;
    message_view.direction = direction;
    message_view.limit = Some(message_query_limit as u64);
    let mut tool_view = ToolCallRows::new(scope.clone());
    tool_view.sequence = sequence;
    tool_view.direction = direction;
    tool_view.limit = Some(tool_call_query_limit as u64);
    let started = std::time::Instant::now();
    let (queried_messages, queried_tool_calls) =
        access.read_views(&(message_view, tool_view)).await?;
    let queried_messages = queried_messages
        .into_iter()
        .map(|row| row.into_canonical())
        .collect::<Vec<_>>();
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
    let crossed_message_boundary = deferred_message_sequence.is_some_and(|boundary| {
        queried_tool_calls
            .last()
            .and_then(|row| row.message_sequence)
            .is_some_and(|sequence| !beyond(sequence, i64::from(boundary)))
    });
    let tools_exhausted =
        queried_tool_calls.len() < tool_call_query_limit || crossed_message_boundary;
    let mut tool_calls = queried_tool_calls
        .iter()
        .filter(|row| {
            deferred_message_sequence.is_none_or(|boundary| {
                row.message_sequence
                    .is_some_and(|sequence| beyond(sequence, i64::from(boundary)))
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
        messages.retain(|row| beyond(i64::from(row.message.sequence), deferred_sequence));
        if tool_calls.is_empty() {
            bail!(
                "session transcript has more than {} tool calls at sequence {deferred_sequence}; the sequence-atomic tool budget cannot represent it losslessly",
                SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET
            );
        }
    }
    /* A tool's result arrives in a later delivery message, which a page cut
    can leave out; those deliveries are read too, as sources for results
    only, never as rows. Each delivery answers at least one call. */
    let answered = messages
        .iter()
        .flat_map(|row| row.message.blocks.iter())
        .filter_map(|block| match block {
            gents_protocol::output::MessageBlock::ToolResult {
                tool_call_doc_id, ..
            } => Some(tool_call_doc_id.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let unanswered = tool_calls
        .iter()
        .filter(|row| {
            row.request_doc_id.is_some()
                && row
                    .doc_id
                    .as_deref()
                    .is_some_and(|doc_id| !answered.contains(doc_id))
        })
        .collect::<Vec<_>>();
    let mut result_messages = Vec::new();
    let mut result_query_count = 0;
    if with_requests && !unanswered.is_empty() {
        let after = messages
            .iter()
            .map(|row| i64::from(row.message.sequence))
            .chain(unanswered.iter().filter_map(|row| row.message_sequence))
            .max()
            .unwrap_or_default();
        let wanted = unanswered
            .iter()
            .filter_map(|row| row.doc_id.as_deref())
            .collect::<BTreeSet<_>>();
        let mut view = MessageRows::new(scope);
        view.request_doc_ids = Some(
            unanswered
                .iter()
                .filter_map(|row| row.request_doc_id.clone())
                .collect(),
        );
        view.role = Some(gents_protocol::output::MessageRole::User);
        view.sequence = SequenceRange::After(after);
        view.limit = Some(unanswered.len() as u64);
        result_query_count = 1;
        result_messages = access
            .read_view(&view)
            .await?
            .into_iter()
            .map(|row| row.into_canonical())
            .filter(|row| {
                row.message.blocks.iter().any(|block| {
                    matches!(
                        block,
                        gents_protocol::output::MessageBlock::ToolResult { tool_call_doc_id, .. }
                            if wanted.contains(tool_call_doc_id.as_str())
                    )
                })
            })
            .collect::<Vec<_>>();
    }
    let (canonical_dependencies, dependency_query_count, dependency_rows) =
        load_page_canonical_dependencies(
            access,
            &messages
                .iter()
                .chain(result_messages.iter())
                .cloned()
                .collect::<Vec<_>>(),
            &tool_calls,
        )
        .await?;
    /* the requests these rows belong to: rows name their request, and a
    runtime control or delegated input is told apart by it */
    let request_doc_ids = if !with_requests {
        Vec::new()
    } else {
        messages
            .iter()
            .filter_map(|row| row.message.request_doc_id.clone())
            .chain(
                tool_calls
                    .iter()
                    .filter_map(|row| row.request_doc_id.clone()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    };
    let (requests, _) = if request_doc_ids.is_empty() {
        (Vec::new(), 0)
    } else {
        read_request_documents(
            access,
            node_did.context("session read requires a node DID")?,
            &request_doc_ids,
            "session transcript requests",
        )
        .await?
    };
    let request_query_count = request_doc_ids.len().div_ceil(128) as u64;
    let (question_sources, question_query_count) = read_legacy_question_sources(
        access,
        requests
            .iter()
            .chain(&canonical_dependencies.origin_requests),
    )
    .await?;
    let source_exhausted = messages_exhausted && tools_exhausted;
    let queried_rows = queried_messages
        .len()
        .saturating_add(queried_tool_calls.len())
        .saturating_add(dependency_rows)
        .saturating_add(requests.len())
        .saturating_add(question_sources.len())
        .saturating_add(result_messages.len());
    let query_count = 1
        + dependency_query_count
        + request_query_count
        + result_query_count
        + question_query_count;
    /* the message numbers this read proves: every message inside the span is
    in the page, so a gap inside it is empty rather than unread */
    let kept = messages
        .iter()
        .map(|row| i64::from(row.message.sequence))
        .chain(tool_calls.iter().filter_map(|row| row.message_sequence));
    let (low, high) = kept.fold(
        (None, None),
        |(low, high): (Option<i64>, Option<i64>), at| {
            (
                Some(low.map_or(at, |low| low.min(at))),
                Some(high.map_or(at, |high| high.max(at))),
            )
        },
    );
    let covered = match window {
        TranscriptWindow::Tip => TranscriptSpan {
            from: if source_exhausted { Some(0) } else { low },
            through: None,
        },
        TranscriptWindow::Before(cursor) => TranscriptSpan {
            from: if source_exhausted {
                Some(0)
            } else {
                low.or(Some(cursor))
            },
            through: Some(cursor - 1),
        },
        TranscriptWindow::After(cursor) => TranscriptSpan {
            from: Some(cursor + 1),
            through: if source_exhausted {
                None
            } else {
                high.or(Some(cursor))
            },
        },
    };
    tracing::debug!(
        target: "gents_desktop_core::query",
        session_id,
        ?window,
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
            mailbox_items: question_sources,
            ..ClientStoreRows::default()
        }),
        requests,
        result_messages,
        canonical_dependencies,
        query_count,
        queried_rows,
        message_query_limit,
        tool_call_query_limit,
        source_exhausted,
        has_newer: !matches!(window, TranscriptWindow::Tip),
        covered,
    })
}

pub async fn load_session_context_store_on(
    access: &ConfigAccess,
    session_id: &str,
    node_did: Option<&str>,
    requester_did: Option<&str>,
) -> Result<ClientStore> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        bail!("session context query requires a session id");
    }
    let scope = scoped_session(session_id, node_did, requester_did)?;
    let started = std::time::Instant::now();
    let (messages, output_segments, compaction_entries) = access
        .read_views(&(
            MessageRows::new(scope.clone()),
            SegmentRows::new(scope.clone()),
            CompactionRows::new(scope),
        ))
        .await?;
    let messages = messages
        .into_iter()
        .map(|row| row.into_canonical())
        .collect::<Vec<_>>();
    let output_segments = output_segments
        .into_iter()
        .map(|row| row.into_canonical())
        .collect::<Vec<_>>();
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

pub async fn load_session_compaction_entries_on(
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<gents_protocol::row::CompactionEntryRow>> {
    anyhow::ensure!(
        !session_id.trim().is_empty(),
        "session compactions require a session id"
    );
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "session compactions require a node DID"
    );
    access
        .read_view(&CompactionRows::new(scoped_session(
            session_id.trim(),
            Some(node_did.trim()),
            requester_did,
        )?))
        .await
        .map_err(Into::into)
}

pub async fn load_session_diagnostics_store_on(
    access: &ConfigAccess,
    session_id: &str,
    node_did: Option<&str>,
    requester_did: Option<&str>,
) -> Result<ClientStore> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        bail!("session diagnostics query requires a session id");
    }
    let scope = scoped_session(session_id, node_did, requester_did)?;
    let started = std::time::Instant::now();
    let (messages, tool_calls, output_segments) = access
        .read_views(&(
            MessageRows::new(scope.clone()),
            ToolCallRows::new(scope.clone()),
            SegmentRows::new(scope),
        ))
        .await?;
    let messages = messages
        .into_iter()
        .map(|row| row.into_canonical())
        .collect::<Vec<_>>();
    let output_segments = output_segments
        .into_iter()
        .map(|row| row.into_canonical())
        .collect::<Vec<_>>();
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

    #[tokio::test]
    async fn context_compactions_keep_the_exact_requester_scope() -> Result<()> {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        for (key, owner, requester) in [
            ("mine", "did:owner", Some("did:requester")),
            ("another-requester", "did:owner", Some("did:other")),
            ("no-requester", "did:owner", None),
            ("another-owner", "did:other", Some("did:requester")),
        ] {
            let key = escape_graphql_string(key);
            let owner = escape_graphql_string(owner);
            let requester = requester
                .map(|did| format!("\"{}\"", escape_graphql_string(did)))
                .unwrap_or_else(|| "null".to_owned());
            let mutation = format!(
                "mutation {{ create_CompactionEntry(input: {{ compaction_key: \"{key}\", session_id: \"session\", node_did: \"{owner}\", requester_did: {requester}, sequence: 1, summary: \"retained\" }}) {{ _docID }} }}"
            );
            gents_store::config_client::ConfigAccess::write_local(
                &node,
                "test.context.compaction",
                &mutation,
            )
            .await?;
        }
        for (requester, expected) in [(Some("did:requester"), "mine"), (None, "no-requester")] {
            let store = load_session_context_store_on(
                &ConfigAccess::local_as(node.clone(), node.node_identity_did().unwrap().parse()?),
                "session",
                Some("did:owner"),
                requester,
            )
            .await?;
            assert_eq!(store.compaction_entries.len(), 1);
            assert_eq!(store.compaction_entries[0].compaction_key, expected);
        }
        node.shutdown().await.unwrap();
        Ok(())
    }

    fn header(doc_id: &str, message_key: &str, sequence: u32) -> TranscriptMessageRow {
        TranscriptMessageRow {
            doc_id: doc_id.to_string(),
            message: TranscriptMessage {
                native_request_id: None,
                message_key: message_key.to_string(),
                session_id: "session".to_string(),
                node_did: "agent".to_string(),
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
                node_did: "agent".to_string(),
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
