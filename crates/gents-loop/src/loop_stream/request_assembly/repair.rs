use super::*;

#[derive(Debug, thiserror::Error)]
#[error("provider_input_repair_removed_complete_prompt")]
struct ProviderInputRepairError;

pub fn repair_provider_input(
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
) -> Result<(), StreamingError> {
    let mut provider_messages = std::mem::take(history);
    provider_messages.append(new_messages);
    repair_messages(&mut provider_messages);
    let mut provider_messages = sanitize_tagged_history(provider_messages)?;
    let prompt = provider_messages.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            ProviderInputRepairError,
        )))
    })?;
    *history = provider_messages;
    new_messages.push(prompt);
    Ok(())
}

fn repair_messages(messages: &mut [TaggedMessage]) {
    for row in messages.iter_mut() {
        let Message::Assistant { content, .. } = &mut row.message else {
            continue;
        };
        for item in content {
            let AssistantContent::ToolCall(tool_call) = item else {
                continue;
            };
            let mut repaired = crate::tool::normalize_tool_call_arguments(
                "repair",
                &tool_call.function.name,
                &tool_call.function.arguments,
            );
            sanitize_json_string_leaves(&mut repaired);
            tool_call.function.arguments = repaired;
        }
    }
}

fn sanitize_json_string_leaves(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                sanitize_json_string_leaves(value);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                sanitize_json_string_leaves(value);
            }
        }
        serde_json::Value::String(text) => {
            *text = sanitize_provider_arg_string(text);
        }
        _ => {}
    }
}

fn sanitize_provider_arg_string(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => sanitized.push_str("\\n"),
            '\t' => sanitized.push_str("\\t"),
            ch if ch.is_control() => {}
            ch => sanitized.push(ch),
        }
    }
    sanitized
}

pub(in crate::loop_stream) async fn repair_and_rebuild_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError>
where
    M: CompletionModel,
{
    repair_provider_input(history, new_messages)?;
    strip_all_reasoning(history);
    strip_all_reasoning(new_messages);
    config.context_calibration.invalidate_anchor();
    build_core(model, history, new_messages, tools, config).await
}
