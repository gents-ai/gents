use super::*;

pub(crate) async fn run_loop_to_text<M>(
    model: M,
    hook: Option<DefraSessionHook>,
    prompt: Message,
    history: Vec<Message>,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    config: LoopConfig,
) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: 'static,
{
    let stream = run_loop_stream(model, hook.clone(), prompt, history, tools, config);
    futures::pin_mut!(stream);
    let mut accumulator = AssistantTurnAccumulator::default();
    let mut final_text = String::new();
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
            LoopStreamItem::TurnRetracted { .. } => {
                accumulator = AssistantTurnAccumulator::default();
                continue;
            }
            LoopStreamItem::OutputObligationPending { reminder } => {
                if let Some(hook) = hook.as_ref() {
                    if let Some(message) = accumulator.take_message() {
                        hook.apply_persistence_policy(
                            hook.persist_message(&message).await.map(|_| ()),
                            "persist one-shot assistant output-obligation proposal",
                        )?;
                    }
                    hook.apply_persistence_policy(
                        hook.persist_message(&reminder).await.map(|_| ()),
                        "persist one-shot output-obligation reminder",
                    )?;
                }
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
                    StreamedAssistantContent::Reasoning(reasoning) => {
                        accumulator.push_reasoning(rig_compat::from_rig_reasoning(&reasoning))
                    }
                    StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                        accumulator.push_reasoning_delta(id, &reasoning)
                    }
                    StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id,
                    } => {
                        if let Some(hook) = hook.as_ref() {
                            hook.register_stream_tool_call_identity(
                                &internal_call_id,
                                &tool_call.id,
                                tool_call.call_id.as_deref(),
                            )
                            .await;
                        }
                        accumulator.push_tool_call(rig_compat::from_rig_tool_call(&tool_call));
                    }
                    _ => {}
                },
                MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                    tool_result,
                    internal_call_id,
                }) => {
                    if let Some(hook) = hook.as_ref() {
                        if let Some(message) = accumulator.take_message() {
                            hook.apply_persistence_policy(
                                hook.persist_message(&message).await.map(|_| ()),
                                "persist one-shot assistant turn",
                            )?;
                        }
                        hook.apply_persistence_policy(
                            hook.persist_stream_tool_result_message(
                                &rig_compat::from_rig_tool_result(&tool_result),
                                &internal_call_id,
                            )
                            .await,
                            "persist one-shot tool result",
                        )?;
                    }
                }
                MultiTurnStreamItem::FinalResponse(final_response) => {
                    accumulator.reconcile_text(final_response.response());
                    if let Some(hook) = hook.as_ref() {
                        if let Some(message) = accumulator.take_message() {
                            hook.apply_persistence_policy(
                                hook.persist_message(&message).await.map(|_| ()),
                                "persist one-shot final assistant turn",
                            )?;
                        }
                    }
                    final_text = final_response.response().to_string();
                }
                _ => {}
            },
        }
    }
    Ok(final_text)
}

/// Runs a typed completion without surrendering the runtime's owned-loop
/// chokepoint to Rig's `Agent` orchestration. Rig's schema is attached to every
/// provider request, while the owned loop validates before accepting a final
/// turn and applies its normal bounded recovery policy on malformed output.
pub(crate) async fn run_loop_to_typed<M, T>(
    model: M,
    hook: Option<DefraSessionHook>,
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
    let raw = run_loop_to_text(model, hook, prompt, history, tools, config).await?;
    serde_json::from_str(&raw).map_err(|error| {
        anyhow::anyhow!(
            "decoding validated structured output as {} failed: {error}",
            std::any::type_name::<T>()
        )
    })
}
