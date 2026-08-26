use super::*;

fn live_text_hash(value: &str) -> String {
    // FNV-1a is used only as a compact projection continuity checksum, never as
    // a security primitive. Length plus checksum cheaply checks that an append
    // patch is based on the same text version held by the webview.
    let mut hash = 0x811c9dc5_u32;
    for byte in value.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    format!("{hash:08x}")
}

fn live_text_patch(
    value: Option<&str>,
    base_byte_len: usize,
    base_hash: &str,
) -> SessionLiveTextPatchView {
    let value = value
        .map(normalize_markdown_text)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let byte_len = value.len();
    let hash = live_text_hash(&value);
    let prefix_matches = base_byte_len <= byte_len
        && value.is_char_boundary(base_byte_len)
        && live_text_hash(&value[..base_byte_len]) == base_hash;
    let (mode, patch_value) = if prefix_matches && base_byte_len == byte_len {
        ("unchanged", String::new())
    } else if prefix_matches {
        ("append", value[base_byte_len..].to_string())
    } else {
        ("replace", value)
    };
    SessionLiveTextPatchView {
        mode: mode.to_string(),
        value: patch_value,
        byte_len,
        hash,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_session_live_delta(
    core: &ClientCore,
    session_id: &str,
    agent_did: Option<&str>,
    request_id: &str,
    base_reconcile_version: u64,
    base_content_byte_len: usize,
    base_content_hash: &str,
    base_reasoning_byte_len: usize,
    base_reasoning_hash: &str,
) -> SessionLiveDeltaView {
    let (store, revision) = core.store().snapshot_with_revision();
    build_session_live_delta_from_store(
        store.as_ref(),
        revision,
        session_id,
        agent_did,
        request_id,
        base_reconcile_version,
        base_content_byte_len,
        base_content_hash,
        base_reasoning_byte_len,
        base_reasoning_hash,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_session_live_delta_from_store(
    store: &gents_desktop_core::client::ClientStore,
    revision: gents_desktop_core::client::StoreProjectionRevision,
    session_id: &str,
    agent_did: Option<&str>,
    request_id: &str,
    base_reconcile_version: u64,
    base_content_byte_len: usize,
    base_content_hash: &str,
    base_reasoning_byte_len: usize,
    base_reasoning_hash: &str,
) -> SessionLiveDeltaView {
    let revision_view = SessionProjectionRevisionView {
        store_version: revision.store_version,
        reconcile_version: revision.reconcile_version,
    };
    let snapshot_required =
        |turn_state: Option<String>, status: Option<String>| SessionLiveDeltaView {
            outcome: "snapshotRequired".to_string(),
            revision: revision_view.clone(),
            request_id: request_id.to_string(),
            progress_seq: None,
            turn_state,
            status,
            content: None,
            reasoning: None,
        };

    if revision.reconcile_version != base_reconcile_version {
        return snapshot_required(None, None);
    }

    let request = store.requests.iter().find(|request| {
        request.request_id == request_id
            && request.session_id.as_deref() == Some(session_id)
            && agent_did.is_none_or(|agent_did| request.agent_did.as_deref() == Some(agent_did))
    });
    if request.is_none() {
        return snapshot_required(None, None);
    }
    let turn_state = agent_did.map_or_else(
        || store.derive_turn_for_request(request_id),
        |agent_did| store.derive_turn_for_request_for_agent(request_id, agent_did),
    );
    let turn_state_label = turn_state.map(turn_state_label).map(str::to_owned);
    if !matches!(
        turn_state,
        Some(gents_protocol::client_protocol::ClientTurnState::WaitingForClaim)
            | Some(gents_protocol::client_protocol::ClientTurnState::Streaming)
    ) {
        return snapshot_required(turn_state_label, None);
    }

    let response = agent_did.map_or_else(
        || store.latest_response_for_request(request_id),
        |agent_did| store.latest_response_for_request_for_agent(request_id, agent_did),
    );
    let Some(response) = response else {
        return snapshot_required(turn_state_label, None);
    };
    let status = normalize_optional(response.status.as_deref());
    let terminal_response = status.as_deref().is_some_and(|status| {
        matches!(
            status.to_ascii_lowercase().as_str(),
            "complete" | "completed" | "error" | "failed" | "interrupted"
        )
    });
    if terminal_response
        || response.materialized_message_sequence.is_some()
        || response.materialized_at.is_some()
        || response.interrupted_at.is_some()
    {
        return snapshot_required(turn_state_label, status);
    }

    let content = live_text_patch(
        response.content.as_deref(),
        base_content_byte_len,
        base_content_hash,
    );
    let reasoning = live_text_patch(
        response.reasoning.as_deref(),
        base_reasoning_byte_len,
        base_reasoning_hash,
    );
    let outcome = if content.mode == "unchanged" && reasoning.mode == "unchanged" {
        "unchanged"
    } else {
        "delta"
    };
    SessionLiveDeltaView {
        outcome: outcome.to_string(),
        revision: revision_view,
        request_id: request_id.to_string(),
        progress_seq: response.progress_seq,
        turn_state: turn_state_label,
        status,
        content: Some(content),
        reasoning: Some(reasoning),
    }
}
