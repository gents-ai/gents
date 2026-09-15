use super::*;

pub(super) fn add_usage_saturating(aggregate: &mut Usage, usage: Usage) {
    aggregate.input_tokens = aggregate.input_tokens.saturating_add(usage.input_tokens);
    aggregate.output_tokens = aggregate.output_tokens.saturating_add(usage.output_tokens);
    aggregate.total_tokens = aggregate.total_tokens.saturating_add(usage.total_tokens);
    aggregate.cached_input_tokens = aggregate
        .cached_input_tokens
        .saturating_add(usage.cached_input_tokens);
    aggregate.cache_creation_input_tokens = aggregate
        .cache_creation_input_tokens
        .saturating_add(usage.cache_creation_input_tokens);
}

pub(super) fn close_streaming_turn<R>(
    new_messages: &mut Vec<Message>,
    accumulator: &mut AssistantTurnAccumulator,
    message_id: Option<String>,
    pending_results: Vec<(ToolCall, String, String)>,
) -> Vec<LoopStreamItem<R>> {
    // Thread the assistant turn (text + reasoning + tool calls) ahead of its
    // tool results, matching rig's history ordering. Carry the provider
    // message id (captured into `stream.message_id` from the stream's
    // `MessageId` event) onto the threaded message — rig threads this same id,
    // and OpenAI Responses / ChatGPT Codex follow-up requests reference prior
    // `msg_` ids, so dropping it breaks them.
    if let Some(mut assistant_message) = accumulator.take_message() {
        if let Message::Assistant { id, .. } = &mut assistant_message {
            *id = message_id;
        }
        new_messages.push(assistant_message);
    }

    pending_results
        .into_iter()
        .map(|(tool_call, internal_call_id, bounded_result)| {
            let content = ToolResultContent::from_tool_output(bounded_result);
            let user_content = match tool_call.call_id.clone() {
                Some(call_id) => UserContent::tool_result_with_call_id(
                    tool_call.id.clone(),
                    call_id,
                    content.clone(),
                ),
                None => UserContent::tool_result(tool_call.id.clone(), content.clone()),
            };
            new_messages.push(Message::User {
                content: vec![user_content],
            });

            let tool_result = ToolResult {
                id: tool_call.id.clone(),
                call_id: tool_call.call_id.clone(),
                content,
            };
            LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
                StreamedUserContent::ToolResult {
                    tool_result: rig_compat::to_rig_tool_result(&tool_result),
                    internal_call_id,
                },
            ))
        })
        .collect()
}
