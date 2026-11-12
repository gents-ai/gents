use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::{
    apply_session_timeline_page_with_query, build_session_snapshot_for_node_with_transcript,
};
use crate::types::{DesktopSessionSnapshot, SessionRowsView, SessionRowsWindow};

pub async fn session_snapshot(
    core: &std::sync::Arc<gents_desktop_core::client::ClientCore>,
    session_id: String,
    node_did: Option<String>,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
) -> Result<Option<DesktopSessionSnapshot>, BridgeError> {
    let started = std::time::Instant::now();
    let node_did = Some(resolve_session_node(&session_id, node_did)?);
    let mut request_id = request_id;

    let transcript_started = std::time::Instant::now();
    let TranscriptReadScope {
        operator_access,
        requester_scope,
    } = transcript_read_scope(core, &session_id, node_did.as_deref().unwrap_or_default())?;
    let page_read = gents_desktop_core::client::load_session_transcript_page_on(
        &operator_access,
        &session_id,
        node_did.as_deref(),
        requester_scope.as_deref(),
        None,
        timeline_limit,
    );
    let transcript_page = page_read.await.map_err(|error| {
        let error = BridgeError::classify_transport_error(error);
        if error.code == BridgeErrorCode::PermissionDenied {
            if let Some(node) = node_did.as_deref() {
                core.stores().ensure(node).clear_after_refused_read();
            }
        }
        error
    })?;
    let page_request_doc_ids = transcript_page
        .store
        .transcript_messages
        .iter()
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<Vec<_>>();
    let fallback_request_id = match (request_id.is_none(), node_did.as_deref()) {
        (true, Some(node_did)) => core
            .stores()
            .snapshot(node_did)
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
            // A session whose node has not returned its row arrives as a typed
            // delivery state; only a genuine read failure stays untyped.
            Err(error) => {
                let error = BridgeError::classify_transport_error(error);
                if error.code == BridgeErrorCode::Unknown {
                    tracing::warn!(
                        target: "gents_desktop::chat",
                        node_did,
                        session_id = %session_id,
                        request_id,
                        error = %error,
                        "selected request projection unavailable"
                    );
                }
                return Err(error);
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
            let store = core
                .stores()
                .snapshot(node_did.as_deref().unwrap_or_default());
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
        gents_desktop_core::client::load_request_prompt_ownership_on(&operator_access, &requests)
            .await
    };
    let context_read = async {
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
                core.stores()
                    .snapshot(node_did.as_deref().unwrap_or_default())
                    .session_tip_request(
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
        gents_desktop_core::client::load_session_tip_store_on(&operator_access, &request)
            .await
            .map(Some)
    };
    let goal_read = crate::commands::session_goal(
        core.as_ref(),
        node_did.as_deref().unwrap_or_default(),
        &session_id,
    );
    let (context, ownership, goal) = tokio::join!(context_read, ownership_read, goal_read);
    let goal = snapshot_goal(&session_id, goal)?;
    let context_store = match context {
        Ok(store) => store,
        Err(error) => {
            let classified = BridgeError::classify_transport_error(error);
            if classified.code == BridgeErrorCode::PermissionDenied {
                return Err(classified);
            }
            tracing::warn!(
                target: "gents_desktop::chat",
                session_id,
                error = %classified,
                "session tip query failed; returning the bounded transcript without live ownership evidence"
            );
            None
        }
    };
    let prompt_ownership = match ownership {
        Ok(facts) => Some(facts),
        Err(error) => {
            let classified = BridgeError::classify_transport_error(error);
            if classified.code == BridgeErrorCode::PermissionDenied {
                return Err(classified);
            }
            tracing::warn!(session_id, error = %classified, "request prompt ownership unavailable; suppressing unproven pending inputs");
            None
        }
    };
    let transcript_page_tip_ms = transcript_started.elapsed().as_millis() as u64;
    let projection_started = std::time::Instant::now();
    let context_store = context_store.map(|tip| transcript_page.store.merge_snapshot(tip));
    let session_access = &operator_access;
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
        false,
        true,
        fresh_requests.as_deref(),
    )
    .await
    .map_err(BridgeError::classify_transport_error)?;
    if let Some(snapshot) = snapshot.as_mut() {
        snapshot.goal = goal;
        if let (Some(node), Some(requests), Some(selected)) = (
            node_did.as_deref(),
            fresh_requests.as_ref(),
            request_id.as_deref(),
        ) {
            if let Some(request) = requests.iter().find(|request| {
                request.request_id == selected
                    && request.session_id.as_deref() == Some(session_id.as_str())
                    && request.node_did.as_deref() == Some(node)
            }) {
                snapshot.inference_activity = crate::snapshot::read_session_inference_activity(
                    core.as_ref(),
                    request,
                    node,
                    &session_id,
                    selected,
                )
                .await
                .map_err(BridgeError::classify_transport_error)?;
            }
        }
        apply_session_timeline_page_with_query(snapshot, timeline_limit, Some(&transcript_page))
            .map_err(BridgeError::untyped)?;
    }
    let projection_ms = projection_started.elapsed().as_millis() as u64;
    let elapsed = started.elapsed();
    if elapsed > std::time::Duration::from_secs(1) {
        tracing::info!(
            target: "gents_desktop::chat",
            session_id,
            request_refresh_ms,
            transcript_page_tip_ms,
            projection_ms,
            elapsed_ms = elapsed.as_millis() as u64,
            "loaded slow desktop session snapshot"
        );
    }
    Ok(snapshot)
}

/// The node a session read names. A session is read only through its own
/// node's connection, so a read that names no node is refused rather than
/// looked up across nodes.
pub(crate) fn resolve_session_node(
    session_id: &str,
    node_did: Option<String>,
) -> Result<String, BridgeError> {
    node_did
        .map(|node_did| node_did.trim().to_owned())
        .filter(|node_did| !node_did.is_empty())
        .ok_or_else(|| BridgeError::untyped(format!("session {session_id} names no node")))
}

struct TranscriptReadScope {
    operator_access: gents_store::config_client::ConfigAccess,
    requester_scope: Option<String>,
}

fn transcript_read_scope(
    core: &gents_desktop_core::client::ClientCore,
    session_id: &str,
    node_did: &str,
) -> Result<TranscriptReadScope, BridgeError> {
    let operator_access = core
        .operator_observation_graphql(node_did)
        .map(gents_store::config_client::ConfigAccess::Graphql)
        .ok_or_else(|| BridgeError::untyped("Connect to the runtime to read this session."))?;
    let store = core.stores().snapshot(node_did);
    let requester_scope = store
        .sessions
        .iter()
        .find(|row| row.session_id == session_id && row.node_did == node_did)
        .and_then(|session| session.requester_did.clone());
    Ok(TranscriptReadScope {
        operator_access,
        requester_scope,
    })
}

pub async fn message_attachments(
    core: &gents_desktop_core::client::ClientCore,
    session_id: &str,
    node_did: &str,
    message_key: &str,
) -> Result<Vec<gents_protocol::request_input::RequestAttachment>, BridgeError> {
    if let Some(reason) = core.session_unreadable_reason(node_did) {
        return Err(BridgeError::untyped(reason));
    }
    let scope = transcript_read_scope(core, session_id, node_did)?;
    let access = scope.operator_access;
    let view_scope = match &scope.requester_scope {
        Some(requester_did) => gents_session::views::transcript::Scope::Session(
            gents_protocol::session::SessionScope {
                node_did: node_did.to_owned(),
                session_id: session_id.to_owned(),
                requester_did: Some(requester_did.clone()),
            },
        ),
        None => gents_session::views::transcript::Scope::Node(node_did.to_owned()),
    };
    let mut view = gents_session::views::transcript::MessageRows::new(view_scope);
    view.session_id = Some(session_id.to_owned());
    view.message_key = Some(message_key.to_owned());
    view.limit = Some(2);
    let rows = access
        .read_view(&gents_session::views::transcript::MessageIdentities(view))
        .await
        .map_err(|error| BridgeError::classify_transport_error(error.into()))?;
    if rows.len() != 1 {
        return Err(BridgeError::untyped(
            "The attachment message is missing or ambiguous",
        ));
    }
    let doc_id = &rows[0].doc_id;
    let (header, message) = gents_session::session::load_canonical_message(
        &access,
        doc_id,
        node_did,
        scope.requester_scope.as_deref(),
    )
    .await
    .map_err(BridgeError::classify_transport_error)?;
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
    use gents_desktop_core::client::TranscriptWindow;
    let node_did = resolve_session_node(&session_id, node_did)?;
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
    if let Some(reason) = core.session_unreadable_reason(&node_did) {
        return Ok(empty(Some(reason.to_string())));
    }
    let TranscriptReadScope {
        operator_access,
        requester_scope,
    } = transcript_read_scope(core, &session_id, &node_did)?;
    let read = |window: TranscriptWindow, limit: Option<usize>| {
        let operator_access = operator_access.clone();
        let requester_scope = requester_scope.clone();
        let session_id = session_id.clone();
        let node_did = node_did.clone();
        async move {
            gents_desktop_core::client::load_session_transcript_window_on(
                &operator_access,
                &session_id,
                Some(&node_did),
                requester_scope.as_deref(),
                window,
                limit,
            )
            .await
        }
    };
    let error = |error| {
        let error = BridgeError::classify_transport_error(error);
        if error.code == BridgeErrorCode::PermissionDenied {
            core.stores().ensure(&node_did).clear_after_refused_read();
        }
        error
    };
    let store = core.stores().snapshot(&node_did);
    let provenance_access = &operator_access;
    let mut view = empty(None);
    let mut messages = Vec::new();
    view.complete = true;
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
            view.rows = crate::snapshot::render_transcript_rows_with_provenance(
                provenance_access,
                &store,
                &page,
                &node_did,
                &session_id,
            )
            .await
            .map_err(error)?;
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
            let mut rows = crate::snapshot::render_transcript_rows_with_provenance(
                provenance_access,
                &store,
                &older,
                &node_did,
                &session_id,
            )
            .await
            .map_err(error)?;
            rows.extend(
                crate::snapshot::render_transcript_rows_with_provenance(
                    provenance_access,
                    &store,
                    &newer,
                    &node_did,
                    &session_id,
                )
                .await
                .map_err(error)?,
            );
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
        crate::snapshot::attach_context_history_rows(
            provenance_access,
            &session_id,
            &node_did,
            requester.as_deref(),
            &messages,
            &mut view.rows,
        )
        .await
        .map_err(error)?;
    }
    Ok(view)
}

pub async fn session_context_details(
    core: &std::sync::Arc<gents_desktop_core::client::ClientCore>,
    request: crate::types::SessionContextDetailsRequest,
) -> Result<Option<gents_tools::native::SessionContextDetails>, BridgeError> {
    if core.session_unreadable_reason(&request.node_did).is_some() {
        return Ok(None);
    }
    let scope = transcript_read_scope(core, &request.session_id, &request.node_did)?;
    crate::snapshot::load_session_context_details(
        core,
        &scope.operator_access,
        &request.node_did,
        scope.requester_scope.as_deref(),
        &request.session_id,
        &request.request_id,
        &request.call_id,
    )
    .await
    .map_err(BridgeError::classify_transport_error)
}

/// The snapshot's Goal state from its Goal read (Lean
/// `SessionObservation.ReadOutcome`). A refused read fails the whole snapshot,
/// as every other refused read of it does; any other failed read leaves the
/// state unread (`None`, `incomplete`), which a reader must not show as a
/// Goal or as its absence.
fn snapshot_goal(
    session_id: &str,
    read: anyhow::Result<crate::types::SessionGoalState>,
) -> Result<Option<crate::types::SessionGoalState>, BridgeError> {
    match read {
        Ok(state) => Ok(Some(state)),
        Err(error) => {
            let classified = BridgeError::classify_transport_error(error);
            if classified.code == BridgeErrorCode::PermissionDenied {
                return Err(classified);
            }
            tracing::warn!(session_id, error = %classified, "session Goal observation unavailable; the snapshot carries no Goal state");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod snapshot_goal_tests {
    use super::*;

    #[test]
    fn a_failed_goal_read_leaves_the_state_unread() {
        let read = snapshot_goal(
            "chat",
            Err(anyhow::anyhow!("the runtime's Goal read timed out")),
        );
        assert!(matches!(read, Ok(None)), "{read:?}");
    }

    #[test]
    fn a_refused_goal_read_fails_the_whole_snapshot() {
        let refused = BridgeError::new(BridgeErrorCode::PermissionDenied, "denied");
        let read = snapshot_goal("chat", Err(anyhow::Error::from(refused)));
        assert_eq!(
            read.expect_err("a refusal is not an unread state").code,
            BridgeErrorCode::PermissionDenied
        );
    }

    #[test]
    fn a_completed_goal_read_is_carried() {
        let state = crate::types::SessionGoalState::NoReadableSession {
            next: gents_automation::goal::SessionGoalNextAction::Send,
        };
        assert!(matches!(
            snapshot_goal("chat", Ok(state)),
            Ok(Some(
                crate::types::SessionGoalState::NoReadableSession { .. }
            ))
        ));
    }
}
