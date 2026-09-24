use super::*;

/// Runs a text completion through the owned stream without any persistence
/// hook. This auxiliary is structurally nonpersistent: it consumes
/// `run_loop_stream(...None...)` and never touches `DefraSessionHook`, never
/// calls `hook.persist_message`, and never publishes a transcript. Durable
/// assistant/user persistence is owned exclusively by the StreamProcessor
/// consumer (`crates/gents/src/agent/stream_processor.rs`), which production
/// one-shot runs use via `crates/gents/src/oneshot.rs`.
pub async fn run_loop_to_text<M>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    let provider_profile = config.provider_input_counter.profile();
    let stream = run_loop_stream::<M, crate::session_hook::NoopSessionHook>(
        model, None, prompt, history, tools, config,
    );
    futures::pin_mut!(stream);
    let mut accumulator = AssistantTurnAccumulator::default();
    let mut final_text = None;
    let mut last_attempt_error: Option<InferenceError> = None;

    while let Some(item) = stream.next().await {
        let item = item.map_err(|error| {
            let error = anyhow::Error::new(error);
            match last_attempt_error.as_ref() {
                Some(last_error) => error.context(format!(
                    "one-shot loop stream error after retry failure ({last_error})"
                )),
                None => error.context("one-shot loop stream error"),
            }
        })?;
        match item {
            LoopStreamItem::AuthoredInputReady { .. } => {
                // This event carries persistence authority and may only be
                // consumed by the owned StreamProcessor. The auxiliary has no
                // hook, so observing it here means authority leaked past the
                // ownership boundary: fail loudly instead of discarding it.
                anyhow::bail!(
                    "AuthoredInputReady reached the nonpersistent one-shot auxiliary; \
                     authored-input persistence belongs to the owned StreamProcessor consumer"
                );
            }
            LoopStreamItem::ProviderAttemptStarted { .. }
            | LoopStreamItem::ProviderTurnReady { .. } => continue,
            LoopStreamItem::TurnRetracted { .. } => {
                accumulator = AssistantTurnAccumulator::default();
                continue;
            }
            LoopStreamItem::OutputObligationPending { .. } => {
                accumulator = AssistantTurnAccumulator::default();
                continue;
            }
            LoopStreamItem::AttemptFailed { error, .. } => {
                last_attempt_error = Some(error);
                continue;
            }
            LoopStreamItem::Item(item) => match item {
                MultiTurnStreamItem::StreamAssistantItem(content) => match content {
                    StreamedAssistantContent::Text(text) => accumulator.push_text(&text.text),
                    StreamedAssistantContent::Reasoning(reasoning) => accumulator
                        .push_provider_reasoning(
                            provider_profile,
                            rig_compat::from_rig_reasoning(&reasoning),
                        )?,
                    StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                        accumulator.push_provider_reasoning_delta(provider_profile, id, &reasoning)
                    }
                    StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id: _,
                    } => {
                        accumulator.push_tool_call(rig_compat::from_rig_tool_call(&tool_call));
                    }
                    _ => {}
                },
                MultiTurnStreamItem::FinalResponse(final_response) => {
                    accumulator.reconcile_text(final_response.response());
                    final_text = Some(final_response.response().to_string());
                }
                _ => {}
            },
        }
    }
    final_text.ok_or_else(|| {
        anyhow::anyhow!("provider stream ended without an explicit terminal response")
    })
}

/// Runs a typed completion without surrendering the runtime's owned-loop
/// chokepoint to Rig's `Agent` orchestration. Rig's schema is attached to every
/// provider request, while the owned loop validates before accepting a final
/// turn and applies its normal bounded recovery policy on malformed output.
pub async fn run_loop_to_typed<M, T>(
    model: M,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    mut config: LoopConfig,
) -> anyhow::Result<T>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
    T: DeserializeOwned + schemars::JsonSchema + 'static,
{
    config.structured_output = Some(StructuredOutputConfig::for_type::<T>());
    let raw = run_loop_to_text(model, prompt, history, tools, config).await?;
    serde_json::from_str(&raw).map_err(|error| {
        anyhow::anyhow!(
            "decoding validated structured output as {} failed: {error}",
            std::any::type_name::<T>()
        )
    })
}
