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

pub(super) struct CanonicalLiveText {
    pub content: String,
    pub reasoning: String,
    pub cursor: String,
}

pub(super) fn canonical_live_text(
    request_store: &gents_desktop_core::client::ClientStore,
    canonical_store: &gents_desktop_core::client::ClientStore,
    session_id: &str,
    agent_did: Option<&str>,
    request_id: &str,
) -> Option<CanonicalLiveText> {
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
    let identity = serde_json::to_vec(&(
        session_id,
        request_agent_did,
        request.requester_did.as_deref(),
        request_id,
        request_doc_id,
        execution_generation,
        source,
        writer,
        message_id,
    ))
    .expect("canonical live identity serializes");
    Some(CanonicalLiveText {
        content,
        reasoning,
        cursor: blake3::hash(&identity).to_hex().to_string(),
    })
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
    let value = normalize_optional(value).unwrap_or_default();
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

/// The cursor binds the source selected by the canonical owner. Revisions
/// describe observer activity only; full session reads own history freshness.
/// This is the native adapter of `ClientLiveDelta.accepts`.
fn accepts_live_cursor(base: Option<&str>, current: Option<&str>, terminal: bool) -> bool {
    !terminal && base.is_some() && base == current
}

#[allow(clippy::too_many_arguments)]
pub async fn build_session_live_delta(
    core: &ClientCore,
    session_id: &str,
    agent_did: Option<&str>,
    request_id: &str,
    base_live_cursor: &str,
    base_content_byte_len: usize,
    base_content_hash: &str,
    base_reasoning_byte_len: usize,
    base_reasoning_hash: &str,
) -> anyhow::Result<SessionLiveDeltaView> {
    let started = std::time::Instant::now();
    let (observed, revision) = core.store().snapshot_with_revision();
    let mut live_store = ClientStore::default();
    if let Some(agent_did) = agent_did {
        if core
            .session_unreadable_reason(session_id, agent_did)
            .is_none()
        {
            let matches = observed
                .requests
                .iter()
                .filter(|row| {
                    row.request_id == request_id
                        && row.session_id.as_deref() == Some(session_id)
                        && row.agent_did.as_deref() == Some(agent_did)
                })
                .collect::<Vec<_>>();
            if let [request] = matches.as_slice() {
                let operator = core
                    .operator_graphql(agent_did)
                    .map(gents::config_client::ConfigAccess::Graphql);
                let principal = core.transcript_principal_scope(agent_did);
                let session = observed
                    .sessions
                    .iter()
                    .find(|row| row.session_id == session_id && row.agent_did == agent_did);
                let requester = gents_desktop_core::client::session_transcript_requester_scope(
                    session,
                    Some(agent_did),
                    principal.as_deref(),
                    operator.is_some(),
                );
                if request.requester_did == requester {
                    live_store = match operator.as_ref() {
                        Some(access) => {
                            gents_desktop_core::client::load_session_live_store_on(access, request)
                                .await?
                        }
                        None => {
                            gents_desktop_core::client::load_session_live_store(
                                core.node(),
                                request,
                            )
                            .await?
                        }
                    };
                }
            }
        }
    }
    let delta = build_session_live_delta_from_store(
        &live_store,
        revision,
        session_id,
        agent_did,
        request_id,
        base_live_cursor,
        base_content_byte_len,
        base_content_hash,
        base_reasoning_byte_len,
        base_reasoning_hash,
    );
    tracing::debug!(
        target: "gents_desktop::chat", session_id, request_id,
        outcome = %delta.outcome,
        output_segments = live_store.output_segments.len(),
        message_headers = live_store.transcript_messages.len(),
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "projected scoped live session output"
    );
    Ok(delta)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_session_live_delta_from_store(
    store: &ClientStore,
    revision: gents_desktop_core::client::StoreProjectionRevision,
    session_id: &str,
    agent_did: Option<&str>,
    request_id: &str,
    base_live_cursor: &str,
    base_content_byte_len: usize,
    base_content_hash: &str,
    base_reasoning_byte_len: usize,
    base_reasoning_hash: &str,
) -> SessionLiveDeltaView {
    let revision = SessionProjectionRevisionView {
        store_version: revision.store_version,
    };
    let turn_state = agent_did.map_or_else(
        || store.derive_turn_for_request(request_id),
        |agent| store.derive_turn_for_request_for_agent(request_id, agent),
    );
    let mut result = SessionLiveDeltaView {
        outcome: "snapshotRequired".into(),
        revision,
        request_id: request_id.into(),
        turn_state: turn_state.map(turn_state_label).map(str::to_owned),
        status: None,
        content: None,
        reasoning: None,
        live_cursor: None,
    };
    if !is_live_turn_state(turn_state) {
        return result;
    }
    let Some(live) = canonical_live_text(store, store, session_id, agent_did, request_id) else {
        return result;
    };
    if !accepts_live_cursor(Some(base_live_cursor), Some(&live.cursor), false) {
        return result;
    }
    let content = live_text_patch(
        Some(&live.content),
        base_content_byte_len,
        base_content_hash,
    );
    let reasoning = live_text_patch(
        Some(&live.reasoning),
        base_reasoning_byte_len,
        base_reasoning_hash,
    );
    result.outcome = if content.mode == "unchanged" && reasoning.mode == "unchanged" {
        "unchanged"
    } else {
        "delta"
    }
    .into();
    result.content = Some(content);
    result.reasoning = Some(reasoning);
    result.live_cursor = Some(live.cursor);
    result
}

#[cfg(test)]
#[path = "live_delta/tests.rs"]
mod tests;
