use super::*;

fn select_canonical_live_target(
    store: &gents_desktop_core::client::ClientStore,
    request_doc_id: &str,
    execution_generation: &str,
) -> gents_protocol::output::live::LiveTargetSelection {
    let records = store
        .output_segments
        .iter()
        .map(
            |row| gents_protocol::output::reconstruction::ObservedSegment {
                doc_id: row.doc_id.as_str(),
                segment: &row.segment,
            },
        )
        .collect::<Vec<_>>();
    let messages = store
        .transcript_messages
        .iter()
        .map(|row| (row.doc_id.as_str(), &row.message))
        .collect::<Vec<_>>();
    gents_protocol::output::live::select_live_target(
        request_doc_id,
        execution_generation,
        &records,
        &messages,
    )
}

pub(super) fn canonical_live_text(
    request_store: &gents_desktop_core::client::ClientStore,
    canonical_store: &gents_desktop_core::client::ClientStore,
    session_id: &str,
    agent_did: Option<&str>,
    request_id: &str,
) -> Option<(String, String)> {
    use gents_protocol::output::live::{LiveView, OwnerLiveness};
    use gents_protocol::output::StreamPayload;

    let request = request_store.requests.iter().find(|request| {
        request.request_id == request_id
            && request.session_id.as_deref() == Some(session_id)
            && agent_did.is_none_or(|agent_did| request.agent_did.as_deref() == Some(agent_did))
    })?;
    let request_doc_id = request.doc_id.as_deref()?;
    let execution_generation = request.execution_generation.as_deref()?;
    let request_agent_did = request.agent_did.as_deref()?;
    let gents_protocol::output::live::LiveTargetSelection::Selected {
        source,
        writer,
        message_id,
    } = select_canonical_live_target(canonical_store, request_doc_id, execution_generation)
    else {
        return None;
    };
    let request_terminal = request
        .lifecycle_state
        .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal);
    let view = gents_desktop_core::client::canonical_output::project_canonical_live(
        request_doc_id,
        session_id,
        request_doc_id,
        &source,
        &writer,
        message_id.as_deref(),
        request_agent_did,
        request.requester_did.as_deref(),
        &canonical_store.transcript_messages,
        &canonical_store.output_segments,
        &[],
        &[],
        &[],
        OwnerLiveness {
            current_request: gents_protocol::output::live::observed_request_execution_owner(
                request,
            ),
            live_tools: Vec::new(),
        },
        request_terminal,
        request.terminal_output.clone(),
    );
    let streams = match view {
        LiveView::Live { streams } | LiveView::Settling { streams } => streams,
        _ => return None,
    };
    let mut content = String::new();
    let mut reasoning = String::new();
    for stream in streams {
        match stream.declaration.payload {
            StreamPayload::Text => content.push_str(&stream.text),
            StreamPayload::Reasoning | StreamPayload::ReasoningSummary => {
                reasoning.push_str(&stream.text);
            }
            _ => {}
        }
    }
    Some((content, reasoning))
}

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
    if !is_live_turn_state(turn_state) {
        return snapshot_required(turn_state_label, None);
    }

    let request = request.expect("request presence checked above");
    let Some(request_doc_id) = request.doc_id.as_deref() else {
        return snapshot_required(turn_state_label, None);
    };
    let Some(execution_generation) = request.execution_generation.as_deref() else {
        return snapshot_required(turn_state_label, None);
    };
    let Some(request_agent_did) = request.agent_did.as_deref() else {
        return snapshot_required(turn_state_label, None);
    };
    if let gents_protocol::output::live::LiveTargetSelection::Selected {
        source,
        writer,
        message_id,
    } = select_canonical_live_target(store, request_doc_id, execution_generation)
    {
        use gents_protocol::output::live::{LiveView, OwnerLiveness};
        use gents_protocol::output::StreamPayload;

        let request_terminal = request
            .lifecycle_state
            .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal);
        let view = gents_desktop_core::client::canonical_output::project_canonical_live(
            request_doc_id,
            session_id,
            request_doc_id,
            &source,
            &writer,
            message_id.as_deref(),
            request_agent_did,
            request.requester_did.as_deref(),
            &store.transcript_messages,
            &store.output_segments,
            &[],
            &[],
            &[],
            OwnerLiveness {
                current_request: gents_protocol::output::live::observed_request_execution_owner(
                    request,
                ),
                live_tools: Vec::new(),
            },
            request_terminal,
            request.terminal_output.clone(),
        );
        let streams = match view {
            LiveView::Live { streams } | LiveView::Settling { streams } => streams,
            _ => return snapshot_required(turn_state_label, None),
        };
        let mut content = String::new();
        let mut reasoning = String::new();
        for stream in streams {
            match stream.declaration.payload {
                StreamPayload::Text => content.push_str(&stream.text),
                StreamPayload::Reasoning | StreamPayload::ReasoningSummary => {
                    reasoning.push_str(&stream.text);
                }
                _ => {}
            }
        }
        let content_patch =
            live_text_patch(Some(&content), base_content_byte_len, base_content_hash);
        let reasoning_patch = live_text_patch(
            Some(&reasoning),
            base_reasoning_byte_len,
            base_reasoning_hash,
        );
        let unchanged = content_patch.mode == "unchanged" && reasoning_patch.mode == "unchanged";
        return SessionLiveDeltaView {
            outcome: if unchanged { "unchanged" } else { "delta" }.to_owned(),
            revision: revision_view,
            request_id: request_id.to_owned(),
            progress_seq: None,
            turn_state: turn_state_label,
            status: None,
            content: Some(content_patch),
            reasoning: Some(reasoning_patch),
        };
    }

    // Canonical output has no mutable response tail. The shared live
    // projector owns contiguous-prefix reconstruction; until that projection
    // is supplied, force a bounded snapshot rather than repairing text in the
    // desktop bridge.
    let _ = (
        base_content_byte_len,
        base_content_hash,
        base_reasoning_byte_len,
        base_reasoning_hash,
    );
    snapshot_required(turn_state_label, None)
}
