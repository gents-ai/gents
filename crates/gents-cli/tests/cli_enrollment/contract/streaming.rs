use super::*;

/// Title generation embeds the conversation prompt in a different user message;
/// only the actual conversation turn may hold this fixture's streaming barrier.
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

#[test]
fn conversation_barriers_match_exact_turns_without_gating_title_prompts() {
    for prompt in ["First conversation turn", FOLLOWUP_PROMPT, OFFLINE_PROMPT] {
        let direct = serde_json::json!({"messages":[{"role":"user","content":prompt}]});
        assert_eq!(latest_user_text(&direct).as_deref(), Some(prompt));
        let blocks = serde_json::json!({"messages":[{"role":"user","content":[{"type":"text","text":prompt}]}]});
        assert_eq!(latest_user_text(&blocks).as_deref(), Some(prompt));
        let title = serde_json::json!({"messages":[{"role":"user","content":format!("Generate a concise session title for this conversation.\nFirst user request:\n{prompt}")}]});
        assert_ne!(latest_user_text(&title).as_deref(), Some(prompt));
        let followup = serde_json::json!({"messages":[{"role":"user","content":prompt},{"role":"assistant","content":"prior reply"},{"role":"user","content":"different turn"}]});
        assert_eq!(
            latest_user_text(&followup).as_deref(),
            Some("different turn")
        );
    }
}

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

async fn emission_deadline(
    mut emission: tokio::sync::watch::Receiver<Option<std::time::Instant>>,
    budget: Duration,
) -> Result<(std::time::Instant, tokio::time::Instant)> {
    let emitted = *emission.wait_for(|timestamp| timestamp.is_some()).await?;
    let emitted = emitted.context("provider first-frame timestamp missing")?;
    let deadline = tokio::time::Instant::from_std(emitted + budget);
    anyhow::ensure!(
        tokio::time::Instant::now() < deadline,
        "provider content visibility deadline already elapsed before observation"
    );
    Ok((emitted, deadline))
}

#[tokio::test]
async fn delayed_first_frame_starts_visibility_budget_at_emission() -> Result<()> {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let model_gate = gate.clone();
    let (emitted, emission) = tokio::sync::watch::channel(None);
    let delay = Duration::from_millis(650);
    let model = FakeLlm::start(
        "emission-contract",
        None,
        Arc::new(move |_| {
            ChatAction::GatedSse(
                vec![(delay, "data: first-frame\n\n".to_owned())],
                model_gate.clone(),
                "data: [DONE]\n\n".to_owned(),
                emitted.clone(),
            )
        }),
    )?;
    let started = std::time::Instant::now();
    let mut response = reqwest::Client::new()
        .post(format!("{}/chat/completions", model.endpoint()))
        .json(&serde_json::json!({"messages": []}))
        .send()
        .await?
        .error_for_status()?;
    let (timestamp, deadline) = timeout(
        Duration::from_secs(2),
        emission_deadline(emission, Duration::from_millis(500)),
    )
    .await??;
    assert!(timestamp.duration_since(started) >= delay);
    assert_eq!(
        deadline.into_std().duration_since(timestamp),
        Duration::from_millis(500)
    );
    tokio::time::timeout_at(deadline, async {
        let mut received = Vec::new();
        while !String::from_utf8_lossy(&received).contains("first-frame") {
            received.extend(response.chunk().await?.context("missing first frame")?);
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert_eq!(gate.available_permits(), 0, "completion must remain held");
    gate.add_permits(1);
    while response.chunk().await?.is_some() {}
    Ok(())
}

#[tokio::test]
async fn late_visibility_observer_cannot_restart_the_emission_budget() {
    let (_sender, receiver) =
        tokio::sync::watch::channel(Some(std::time::Instant::now() - Duration::from_secs(1)));
    assert!(emission_deadline(receiver, Duration::from_millis(500))
        .await
        .is_err());
}

pub(super) async fn wait_for_visible_content(
    core: &ClientCore,
    request: &str,
    expected: &str,
    emission: tokio::sync::watch::Receiver<Option<std::time::Instant>>,
    visibility_budget: Duration,
) -> Result<()> {
    let (started, deadline) = emission_deadline(emission, visibility_budget).await?;
    tracing::info!(
        request,
        budget_ms = visibility_budget.as_millis(),
        "canonical live visibility deadline started"
    );
    // Measure propagation from the actual first emitted frame, not provider
    // startup. TURN_BUDGET still bounds startup and the whole conversation turn.
    // The caller selects either the strict first-visible budget or the paced
    // cadence budget. In both cases the provider completion gate remains
    // closed, so terminal persistence cannot satisfy this observation.
    tokio::time::timeout_at(deadline, async {
        loop {
            let content = canonical_live_content(core, request).await?;
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "provider content visibility deadline elapsed during observation"
            );
            if content.is_some_and(|content| content.contains(expected)) {
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
    let (Some(request_doc_id), Some(session_id), Some(node_did), Some(generation)) = (
        request.doc_id.as_deref(),
        request.session_id.as_deref(),
        request.node_did.as_deref(),
        request.execution_generation.as_deref(),
    ) else {
        return Ok(None);
    };
    let canonical = gents_desktop_core::client::load_session_context_store(
        core.node(),
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
