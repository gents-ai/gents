use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::{
    apply_session_timeline_page_with_query, build_session_snapshot_for_node_with_transcript,
};
use crate::types::{DesktopSessionSnapshot, SessionRowsView, SessionRowsWindow};
use gents_protocol::error_chain::ErrorChain;

pub async fn session_snapshot(
    core: &std::sync::Arc<gents_desktop_core::client::ClientCore>,
    session_id: String,
    node_did: Option<String>,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<String>,
) -> Result<Option<DesktopSessionSnapshot>, BridgeError> {
    let started = std::time::Instant::now();
    let node_did = Some(resolve_session_node(core, &session_id, node_did)?);
    let mut request_id = request_id;

    let hydrate_started = std::time::Instant::now();
    let mut hydration_observation = None;
    if let Some(node_did) = node_did.as_deref() {
        if core
            .session_unreadable_reason(&session_id, node_did)
            .is_some()
        {
            return Ok(build_session_snapshot_for_node_with_transcript(
                core.as_ref(),
                &gents::config_client::ConfigAccess::local(core.node_arc()),
                Some(node_did),
                &session_id,
                request_id.as_deref(),
                None,
                None,
                None,
                None,
                None,
                false,
                false,
                None,
            )
            .await);
        }
        if !core.session_read_through_operator(&session_id, node_did) {
            match core
                .ensure_session_hydration_started(&session_id, node_did)
                .await
            {
                Ok(observation) => hydration_observation = Some(observation),
                Err(error) => {
                    tracing::warn!(
                        target: "gents_desktop::chat",
                        node_did,
                        session_id = %session_id,
                        error = %error,
                        "session hydration request failed; rendering whatever is already local"
                    );
                }
            }
        }
    }
    let hydrate_start_ms = hydrate_started.elapsed().as_millis() as u64;
    let transcript_started = std::time::Instant::now();
    let TranscriptReadScope {
        operator_access,
        requester_scope,
    } = transcript_read_scope(core, &session_id, node_did.as_deref().unwrap_or_default());
    let page_read = async {
        match operator_access.as_ref() {
            Some(access) => {
                gents_desktop_core::client::load_session_transcript_page_on(
                    access,
                    &session_id,
                    node_did.as_deref(),
                    requester_scope.as_deref(),
                    timeline_before_item_key.as_deref(),
                    timeline_limit,
                )
                .await
            }
            None => {
                gents_desktop_core::client::load_session_transcript_page(
                    core.node(),
                    &session_id,
                    node_did.as_deref(),
                    requester_scope.as_deref(),
                    timeline_before_item_key.as_deref(),
                    timeline_limit,
                )
                .await
            }
        }
    };
    let transcript_page = page_read
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let page_request_doc_ids = transcript_page
        .store
        .transcript_messages
        .iter()
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<Vec<_>>();
    let fallback_request_id = match (request_id.is_none(), node_did.as_deref()) {
        (true, Some(node_did)) => core
            .store()
            .snapshot()
            .latest_request_id_for_session_for_node(&session_id, node_did),
        _ => None,
    };
    let refresh_started = std::time::Instant::now();
    let refreshed_projection = if let Some(node_did) = node_did.as_deref() {
        match core
            .refresh_local_request(
                node_did,
                &session_id,
                request_id.as_deref(),
                fallback_request_id.as_deref(),
                &page_request_doc_ids,
            )
            .await
        {
            Ok(Some(projection)) => Some(projection),
            Ok(None) => {
                return Err(BridgeError::new(
                    BridgeErrorCode::ProjectionPending,
                    "selected request projection became stale; refresh is pending",
                ));
            }
            Err(error) => {
                if let Some(error) = error.downcast_ref::<BridgeError>() {
                    return Err(error.clone());
                }
                tracing::warn!(
                    target: "gents_desktop::chat",
                    node_did,
                    session_id = %session_id,
                    request_id,
                    error = %error,
                    "selected request projection unavailable"
                );
                return Err(BridgeError::untyped(format!(
                    "selected request projection unavailable: {error}"
                )));
            }
        }
    } else {
        None
    };
    let request_refresh_ms = refresh_started.elapsed().as_millis() as u64;
    if request_id.is_none() {
        request_id = refreshed_projection
            .as_ref()
            .and_then(|projection| projection.selected_request_id.clone());
    }
    let fresh_requests = refreshed_projection
        .as_ref()
        .map(|projection| projection.requests.clone());
    let selected_requester_scope = refreshed_projection
        .as_ref()
        .map(|projection| projection.requester_did.clone())
        .unwrap_or_else(|| requester_scope.clone());
    let ownership_read = async {
        let requests = if let Some(requests) = fresh_requests.as_ref() {
            requests.clone()
        } else {
            let store = core.store().snapshot();
            store
                .requests
                .iter()
                .filter(|row| {
                    row.doc_id.is_some()
                        && row.session_id.as_deref() == Some(session_id.as_str())
                        && row.node_did.as_deref() == node_did.as_deref()
                        && row.requester_did.as_deref() == selected_requester_scope.as_deref()
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        match operator_access.as_ref() {
            Some(access) => {
                gents_desktop_core::client::load_request_prompt_ownership_on(access, &requests)
                    .await
            }
            None => {
                gents_desktop_core::client::load_request_prompt_ownership(core.node(), &requests)
                    .await
            }
        }
    };
    let context_read = async {
        if timeline_before_item_key.is_some() {
            return Ok(None);
        }
        let request = request_id.as_deref().and_then(|request_id| {
            if let Some(requests) = fresh_requests.as_ref() {
                gents_desktop_core::client::ClientStore::session_tip_request_from_rows(
                    requests,
                    &session_id,
                    node_did.as_deref(),
                    selected_requester_scope.as_deref(),
                    request_id,
                )
            } else {
                core.store().snapshot().session_tip_request(
                    &session_id,
                    node_did.as_deref(),
                    selected_requester_scope.as_deref(),
                    request_id,
                )
            }
        });
        let Some(request) = request else {
            return Ok(None);
        };
        match operator_access.as_ref() {
            Some(access) => gents_desktop_core::client::load_session_tip_store_on(access, &request)
                .await
                .map(Some),
            None => gents_desktop_core::client::load_session_tip_store(core.node(), &request)
                .await
                .map(Some),
        }
    };
    let (context, ownership) = tokio::join!(context_read, ownership_read);
    let context_store = match context {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(
                target: "gents_desktop::chat",
                session_id,
                error = %error,
                "session tip query failed; returning the bounded transcript without live ownership evidence"
            );
            None
        }
    };
    let prompt_ownership = match ownership {
        Ok(facts) => Some(facts),
        Err(error) => {
            tracing::warn!(session_id, error = %error, "request prompt ownership unavailable; suppressing unproven pending inputs");
            None
        }
    };
    let transcript_page_tip_ms = transcript_started.elapsed().as_millis() as u64;
    let projection_started = std::time::Instant::now();
    let context_store = context_store.map(|tip| transcript_page.store.merge_snapshot(tip));
    let local_access = gents::config_client::ConfigAccess::local(core.node_arc());
    let session_access = operator_access.as_ref().unwrap_or(&local_access);
    let mut snapshot = build_session_snapshot_for_node_with_transcript(
        core.as_ref(),
        session_access,
        node_did.as_deref(),
        &session_id,
        request_id.as_deref(),
        Some(&transcript_page.store),
        Some(&transcript_page.canonical_dependencies),
        context_store.as_ref(),
        prompt_ownership.as_ref(),
        hydration_observation,
        false,
        timeline_before_item_key.is_none(),
        fresh_requests.as_deref(),
    )
    .await;
    if let Some(snapshot) = snapshot.as_mut() {
        apply_session_timeline_page_with_query(
            snapshot,
            timeline_before_item_key.as_deref(),
            timeline_limit,
            Some(&transcript_page),
        )
        .map_err(BridgeError::untyped)?;
    }
    let projection_ms = projection_started.elapsed().as_millis() as u64;
    let elapsed = started.elapsed();
    if elapsed > std::time::Duration::from_secs(1) {
        tracing::info!(
            target: "gents_desktop::chat",
            session_id,
            hydrate_start_ms,
            request_refresh_ms,
            transcript_page_tip_ms,
            projection_ms,
            elapsed_ms = elapsed.as_millis() as u64,
            "loaded slow desktop session snapshot"
        );
    }
    Ok(snapshot)
}

/// The node that owns a session, from the caller or the sessions this client
/// knows; a session id held by two nodes is refused rather than guessed.
fn resolve_session_node(
    core: &gents_desktop_core::client::ClientCore,
    session_id: &str,
    node_did: Option<String>,
) -> Result<String, BridgeError> {
    if let Some(node_did) = node_did {
        return Ok(node_did);
    }
    let node_dids = core
        .store()
        .snapshot()
        .sessions
        .iter()
        .filter(|session| session.session_id == session_id)
        .map(|session| session.node_did.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if node_dids.len() > 1 {
        return Err(BridgeError::untyped(format!(
            "session {session_id} is ambiguous across multiple node owners"
        )));
    }
    node_dids.into_iter().next().ok_or_else(|| {
        BridgeError::untyped(format!("session {session_id} has no resolved node owner"))
    })
}

struct TranscriptReadScope {
    operator_access: Option<gents::config_client::ConfigAccess>,
    requester_scope: Option<String>,
}

/// Where a session's transcript is read and under which requester: the
/// node's own runtime when this client operates it, else this computer's
/// copy, scoped to the requester the session was started under.
fn transcript_read_scope(
    core: &gents_desktop_core::client::ClientCore,
    session_id: &str,
    node_did: &str,
) -> TranscriptReadScope {
    let node_scope = core.transcript_node_scope(node_did);
    let operator_access = core
        .operator_observation_graphql(node_did)
        .map(gents::config_client::ConfigAccess::Graphql);
    let store = core.store().snapshot();
    let session = store
        .sessions
        .iter()
        .find(|row| row.session_id == session_id && row.node_did == node_did);
    let requester_scope = gents_desktop_core::client::session_transcript_requester_scope(
        session,
        Some(node_did),
        node_scope.as_deref(),
        operator_access.is_some(),
    );
    TranscriptReadScope {
        operator_access,
        requester_scope,
    }
}

pub async fn message_attachments(
    core: &gents_desktop_core::client::ClientCore,
    session_id: &str,
    node_did: &str,
    message_key: &str,
) -> Result<Vec<gents_protocol::request_input::RequestAttachment>, BridgeError> {
    if let Some(reason) = core.session_unreadable_reason(session_id, node_did) {
        return Err(BridgeError::untyped(reason));
    }
    let scope = transcript_read_scope(core, session_id, node_did);
    let access = scope
        .operator_access
        .unwrap_or_else(|| gents::config_client::ConfigAccess::local(core.node_arc()));
    let escape = gents::graphql::escape_graphql_string;
    let requester = scope
        .requester_scope
        .as_deref()
        .map(|did| format!(", requester_did: {{ _eq: \"{}\" }}", escape(did)))
        .unwrap_or_default();
    let query = format!("{{ AgentMessage(filter: {{ session_id: {{ _eq: \"{}\" }}, node_did: {{ _eq: \"{}\" }}, message_key: {{ _eq: \"{}\" }}{requester} }}, limit: 2) {{ _docID }} }}", escape(session_id), escape(node_did), escape(message_key));
    let rows = access
        .execute(&query)
        .await
        .map_err(|e| BridgeError::untyped(ErrorChain(e.as_ref()).to_string()))?;
    let rows = rows["data"]["AgentMessage"]
        .as_array()
        .ok_or_else(|| BridgeError::untyped("Message attachments are unavailable"))?;
    if rows.len() != 1 {
        return Err(BridgeError::untyped(
            "The attachment message is missing or ambiguous",
        ));
    }
    let doc_id = rows[0]["_docID"]
        .as_str()
        .ok_or_else(|| BridgeError::untyped("The attachment message has no identity"))?;
    let (header, message) = gents::session::load_canonical_message(
        &access,
        doc_id,
        node_did,
        scope.requester_scope.as_deref(),
    )
    .await
    .map_err(|e| BridgeError::untyped(ErrorChain(e.as_ref()).to_string()))?;
    if header.session_id != session_id || header.message_key != message_key {
        return Err(BridgeError::untyped(
            "The attachment message belongs to another session",
        ));
    }
    Ok(gents_protocol::request_input::RequestAttachment::from_message(&message))
}

/// A stretch of a session's display rows by message number, read without
/// the session's request state, so a reader can page in either direction and
/// open around any message. The rows of the newest stretch match what the
/// session snapshot shows, less its pending turns and live reply.
pub async fn session_rows(
    core: &std::sync::Arc<gents_desktop_core::client::ClientCore>,
    session_id: String,
    node_did: Option<String>,
    window: SessionRowsWindow,
    limit: Option<usize>,
) -> Result<SessionRowsView, BridgeError> {
    use gents_desktop_core::client::{SessionTranscriptQueryPage, TranscriptWindow};
    let node_did = resolve_session_node(core, &session_id, node_did)?;
    let empty = |unreadable: Option<String>| SessionRowsView {
        session_id: session_id.clone(),
        node_did: Some(node_did.clone()),
        rows: Vec::new(),
        covered_from: None,
        covered_through: None,
        has_older: false,
        has_newer: false,
        complete: false,
        unreadable,
        query_count: 0,
    };
    if let Some(reason) = core.session_unreadable_reason(&session_id, &node_did) {
        return Ok(empty(Some(reason.to_string())));
    }
    let TranscriptReadScope {
        operator_access,
        requester_scope,
    } = transcript_read_scope(core, &session_id, &node_did);
    let read = |window: TranscriptWindow, limit: Option<usize>| {
        let operator_access = operator_access.clone();
        let requester_scope = requester_scope.clone();
        let session_id = session_id.clone();
        let node_did = node_did.clone();
        async move {
            match operator_access.as_ref() {
                Some(access) => {
                    gents_desktop_core::client::load_session_transcript_window_on(
                        access,
                        &session_id,
                        Some(&node_did),
                        requester_scope.as_deref(),
                        window,
                        limit,
                    )
                    .await
                }
                None => {
                    gents_desktop_core::client::load_session_transcript_window(
                        core.node(),
                        &session_id,
                        Some(&node_did),
                        requester_scope.as_deref(),
                        window,
                        limit,
                    )
                    .await
                }
            }
        }
    };
    let error = |error: anyhow::Error| match error.downcast::<BridgeError>() {
        Ok(error) => error,
        Err(error) => BridgeError::untyped(ErrorChain(error.as_ref()).to_string()),
    };
    let store = core.store().snapshot();
    let render = |page: &SessionTranscriptQueryPage| {
        crate::snapshot::render_transcript_rows(&store, page, &session_id)
    };
    let mut view = empty(None);
    let mut messages = Vec::new();
    /* a read against the node's own runtime sees every message it has; a
    read of this computer's copy may run ahead of what has arrived */
    view.complete = operator_access.is_some();
    match window {
        SessionRowsWindow::Tip
        | SessionRowsWindow::Before { .. }
        | SessionRowsWindow::After { .. } => {
            let page = read(
                match window {
                    SessionRowsWindow::Tip => TranscriptWindow::Tip,
                    SessionRowsWindow::Before { sequence } => TranscriptWindow::Before(sequence),
                    SessionRowsWindow::After { sequence } => TranscriptWindow::After(sequence),
                    SessionRowsWindow::Around { .. } => unreachable!(),
                },
                limit,
            )
            .await
            .map_err(error)?;
            view.rows = render(&page);
            view.covered_from = page.covered.from;
            view.covered_through = page.covered.through;
            view.has_older = match window {
                SessionRowsWindow::After { .. } => true,
                _ => !page.source_exhausted,
            };
            view.has_newer = match window {
                SessionRowsWindow::Tip => false,
                SessionRowsWindow::Before { .. } => true,
                _ => !page.source_exhausted,
            };
            view.query_count = page.query_count as i64;
            messages = page.store.transcript_messages;
        }
        SessionRowsWindow::Around { sequence } => {
            /* the message itself opens the newer half, so it is drawn even
            when the older half is cut short */
            let half = limit.map(|limit| limit.div_ceil(2));
            let older = read(TranscriptWindow::Before(sequence), half)
                .await
                .map_err(error)?;
            let newer = read(TranscriptWindow::After(sequence - 1), half)
                .await
                .map_err(error)?;
            let mut rows = render(&older);
            rows.extend(render(&newer));
            view.rows = rows;
            view.covered_from = older.covered.from;
            view.covered_through = newer.covered.through;
            view.has_older = !older.source_exhausted;
            view.has_newer = !newer.source_exhausted;
            view.query_count = (older.query_count + newer.query_count) as i64;
            messages.extend(older.store.transcript_messages);
            messages.extend(newer.store.transcript_messages);
        }
    }
    let requester = store
        .sessions
        .iter()
        .find(|session| session.session_id == session_id && session.node_did == node_did)
        .map(|session| session.requester_did.clone());
    if let Some(requester) = requester {
        let local_access = gents::config_client::ConfigAccess::local(core.node_arc());
        crate::snapshot::attach_context_history_rows(
            operator_access.as_ref().unwrap_or(&local_access),
            &session_id,
            &node_did,
            requester.as_deref(),
            &messages,
            &mut view.rows,
        )
        .await;
    }
    Ok(view)
}
