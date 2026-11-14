use super::*;

pub(super) fn latest_user_text(request: &Value) -> Option<String> {
    let content = request
        .get("messages")?
        .as_array()?
        .iter()
        .rev()
        .find(|message| message["role"] == "user")?
        .get("content")?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|part| part.get("text")?.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.concat()),
        _ => None,
    }
}

/// Waits for the gated provider's streamed content in the app's live
/// projection, then checks the order: the provider emitted it, and the runtime
/// request is still open because the test holds the completion gate.
///
/// The projection joins the app's observed request row with the runtime's
/// transcript, so either the app store or a runtime document may be the last
/// input to arrive; the wait wakes on both. It has no deadline of its own: the
/// caller's turn bound ends a turn whose content never becomes visible.
pub(super) async fn wait_for_visible_content(
    core: &ClientCore,
    graphql: &str,
    request: &str,
    expected: &str,
    gate: &tokio::sync::Semaphore,
    emission: &tokio::sync::watch::Receiver<Option<std::time::Instant>>,
) -> Result<()> {
    let access = ConfigAccess::Graphql(crate::support::graphql::served_endpoint(graphql));
    let mut changes = observed_changes(
        &access,
        &["AgentRequest", "AgentMessage", "AgentOutputSegment"],
    )
    .await;
    let mut store_updates = core.store_updates();
    loop {
        store_updates.borrow_and_update();
        let content = canonical_live_content(core, graphql, request).await?;
        if content.is_some_and(|content| content.contains(expected)) {
            break;
        }
        tokio::select! {
            changed = store_updates.changed() => {
                changed.context("app projection update channel closed")?;
            }
            () = observe_change(&mut changes) => {}
        }
    }
    let emitted = (*emission.borrow())
        .context("streamed content was visible before the provider emitted it")?;
    anyhow::ensure!(
        gate.available_permits() == 0,
        "provider completion gate was released before the streamed content was visible"
    );
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{ _docID request_id lifecycle_state }} }}"#,
            escape_graphql_string(request),
        ),
    )
    .await?;
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(first_graphql_row(&response, "AgentRequest")?.clone())
            .context("decoding canonical AgentRequest row")?;
    anyhow::ensure!(
        !row.is_terminal(),
        "runtime request completed while the provider completion gate was held: {response}"
    );
    tracing::info!(
        request,
        since_provider_emission_ms = emitted.elapsed().as_millis(),
        "canonical live content observed while provider completion remains gated"
    );
    Ok(())
}

/// The app reads live output from the runtime as its own User; P2P carries
/// only discovery and enrollment, so its local database holds no transcript.
async fn canonical_live_content(
    core: &ClientCore,
    graphql: &str,
    request_id: &str,
) -> Result<Option<String>> {
    use gents_protocol::output::live::{
        observed_request_execution_owner, select_live_target, LiveTargetSelection, LiveView,
        OwnerLiveness,
    };
    use gents_protocol::output::reconstruction::ObservedSegment;
    use gents_protocol::output::StreamPayload;

    let Some(request) = core.stores().node_dids().into_iter().find_map(|node_did| {
        core.stores()
            .snapshot(&node_did)
            .requests
            .iter()
            .find(|row| row.request_id == request_id)
            .cloned()
    }) else {
        return Ok(None);
    };
    let (Some(request_doc_id), Some(session_id), Some(node_did), Some(generation)) = (
        request.doc_id.as_deref(),
        request.session_id.as_deref(),
        request.node_did.as_deref(),
        request.execution_generation.as_deref(),
    ) else {
        return Ok(None);
    };
    let app = core.node_identity();
    let access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(graphql, app.did())
            .with_client_signing_capability(
                gents_store::identity::NodeIdentity::client_signing_capability(app)?,
            )?,
    );
    let canonical = gents_desktop_core::client::load_session_context_store_on(
        &access,
        session_id,
        Some(node_did),
        request.requester_did.as_deref(),
    )
    .await?;
    let records = canonical
        .output_segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: row.doc_id.as_str(),
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let messages = canonical
        .transcript_messages
        .iter()
        .map(|row| (row.doc_id.as_str(), &row.message))
        .collect::<Vec<_>>();
    let LiveTargetSelection::Selected {
        source,
        writer,
        message_id,
    } = select_live_target(request_doc_id, generation, &records, &messages)
    else {
        return Ok(None);
    };
    let terminal = request
        .lifecycle_state
        .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal);
    let view = gents_desktop_core::client::canonical_output::project_canonical_live(
        request_doc_id,
        session_id,
        request_doc_id,
        &source,
        &writer,
        message_id.as_deref(),
        node_did,
        request.requester_did.as_deref(),
        &canonical.transcript_messages,
        &canonical.output_segments,
        &[],
        &[],
        &[],
        OwnerLiveness {
            current_request: observed_request_execution_owner(&request),
            live_tools: Vec::new(),
        },
        terminal,
        request.terminal_output.clone(),
    );
    let streams = match view {
        LiveView::Live { streams } | LiveView::Settling { streams } => streams,
        _ => return Ok(None),
    };
    Ok(Some(
        streams
            .into_iter()
            .filter(|stream| stream.declaration.payload == StreamPayload::Text)
            .map(|stream| stream.text)
            .collect::<String>(),
    ))
}
