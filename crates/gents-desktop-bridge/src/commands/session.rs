use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::{
    apply_session_timeline_page_with_query, build_session_snapshot_for_node_with_transcript,
};
use crate::types::DesktopSessionSnapshot;

pub async fn session_snapshot(
    core: &std::sync::Arc<gents_desktop_core::client::ClientCore>,
    session_id: String,
    node_did: Option<String>,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<String>,
) -> Result<Option<DesktopSessionSnapshot>, BridgeError> {
    let started = std::time::Instant::now();
    let node_did = match node_did {
        Some(node_did) => Some(node_did),
        None => {
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
            node_dids.into_iter().next()
        }
    };
    if node_did.is_none() {
        return Err(BridgeError::untyped(format!(
            "session {session_id} has no resolved node owner"
        )));
    }
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
    let node_scope = node_did
        .as_deref()
        .and_then(|node_did| core.transcript_node_scope(node_did));
    let operator_access = node_did
        .as_deref()
        .and_then(|node_did| core.operator_observation_graphql(node_did))
        .map(gents::config_client::ConfigAccess::Graphql);
    let requester_scope = {
        let store = core.store().snapshot();
        let session = node_did.as_deref().and_then(|node_did| {
            store
                .sessions
                .iter()
                .find(|row| row.session_id == session_id && row.node_did == node_did)
        });
        gents_desktop_core::client::session_transcript_requester_scope(
            session,
            node_did.as_deref(),
            node_scope.as_deref(),
            operator_access.is_some(),
        )
    };
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
