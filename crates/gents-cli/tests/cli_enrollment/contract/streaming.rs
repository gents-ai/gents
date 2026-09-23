use super::*;

#[tokio::test]
async fn offline_barrier_stays_open_for_later_provider_attempts() -> Result<()> {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let model_gate = gate.clone();
    let model = FakeLlm::start(
        "barrier-contract",
        None,
        Arc::new(move |_| {
            ChatAction::WaitThenSse(model_gate.clone(), completion_text_sse("visible"))
        }),
    )?;
    gate.add_permits(1);
    let client = reqwest::Client::new();
    for _ in 0..2 {
        let body = timeout(Duration::from_secs(2), async {
            client
                .post(format!("{}/chat/completions", model.endpoint()))
                .json(&serde_json::json!({"messages": []}))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        })
        .await
        .context("an opened offline barrier blocked a later attempt")??;
        anyhow::ensure!(body.contains("visible"), "missing fixture content");
    }
    Ok(())
}

pub(super) async fn wait_for_visible_content(
    core: &ClientCore,
    request: &str,
    expected: &str,
    visibility_budget: Duration,
) -> Result<()> {
    let started = Instant::now();
    // The caller selects either the strict first-visible budget or the paced
    // cadence budget. In both cases the provider completion gate remains
    // closed, so terminal persistence cannot satisfy this observation.
    timeout(visibility_budget, async {
        loop {
            if canonical_live_content(core, request)
                .await?
                .is_some_and(|content| content.contains(expected))
            {
                tracing::info!(
                    request,
                    elapsed_ms = started.elapsed().as_millis(),
                    "canonical live content observed while provider completion remains gated"
                );
                return Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("streaming content did not reach client projection before provider completion")??;
    Ok(())
}

/// Read the same short-lived, requester-scoped canonical context as the
/// desktop session projection, then ask the protocol live owner for its
/// contiguous visible prefix. OutputSegment payloads are append-only deltas:
/// a paced reply need not occur whole in any one physical row.
async fn canonical_live_content(core: &ClientCore, request_id: &str) -> Result<Option<String>> {
    use gents_protocol::output::live::{
        observed_request_execution_owner, select_live_target, LiveTargetSelection, LiveView,
        OwnerLiveness,
    };
    use gents_protocol::output::reconstruction::ObservedSegment;
    use gents_protocol::output::StreamPayload;

    let snapshot = core.store().snapshot();
    let Some(request) = snapshot
        .requests
        .iter()
        .find(|row| row.request_id == request_id)
    else {
        return Ok(None);
    };
    let (Some(request_doc_id), Some(session_id), Some(agent_did), Some(generation)) = (
        request.doc_id.as_deref(),
        request.session_id.as_deref(),
        request.agent_did.as_deref(),
        request.execution_generation.as_deref(),
    ) else {
        return Ok(None);
    };
    let canonical = gents_desktop_core::client::load_session_context_store(
        core.node(),
        session_id,
        Some(agent_did),
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
        agent_did,
        request.requester_did.as_deref(),
        &canonical.transcript_messages,
        &canonical.output_segments,
        &[],
        &[],
        &[],
        OwnerLiveness {
            current_request: observed_request_execution_owner(request),
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
