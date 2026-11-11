use gents_protocol::output::live::{LiveTargetSelection, LiveView, OwnerLiveness};
use gents_protocol::output::StreamPayload;

use crate::client::ClientStore;

fn select_canonical_live_target(
    store: &ClientStore,
    request_doc_id: &str,
    execution_generation: &str,
) -> LiveTargetSelection {
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

#[derive(Debug, Clone)]
pub struct CanonicalLiveText {
    pub content: String,
    pub reasoning: String,
    pub cursor: String,
}

/// The cursor is bound to `request_store`'s request row; `canonical_store`
/// supplies the output rows, so a lagging replica yields the same cursor.
pub fn canonical_live_text(
    request_store: &ClientStore,
    canonical_store: &ClientStore,
    session_id: &str,
    node_did: Option<&str>,
    request_id: &str,
) -> Option<CanonicalLiveText> {
    let request = request_store.requests.iter().find(|request| {
        request.request_id == request_id
            && request.session_id.as_deref() == Some(session_id)
            && node_did.is_none_or(|node_did| request.node_did.as_deref() == Some(node_did))
    })?;
    let request_doc_id = request.doc_id.as_deref()?;
    let execution_generation = request.execution_generation.as_deref()?;
    let request_node_did = request.node_did.as_deref()?;
    let LiveTargetSelection::Selected {
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
    let view = super::project_canonical_live(
        request_doc_id,
        session_id,
        request_doc_id,
        &source,
        &writer,
        message_id.as_deref(),
        request_node_did,
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
        request.observed_terminal_output().cloned(),
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
        request_node_did,
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
