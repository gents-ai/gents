//! Anthropic Messages request-body assembly, moved out of `gents::claude_messages`
//! (G-1): `provider_input` projects this exact body for token accounting, and
//! the real transport (native, in `gents`) builds the identical body to send.
//! The SSE response parser and the OAuth-bearing HTTP client stay native.

use gents_protocol::message::{AssistantContent, Message, ToolResultContent, UserContent};
use rig::completion::{CompletionRequest, ToolDefinition};
use serde_json::{json, Value};

const DEFAULT_MAX_TOKENS: u64 = 4096;

/// First `system` block. The subscription token was minted for Claude Code;
/// without this identity the same token 429s on every model (write request #7).
/// Lean: `ClaudeMap.identity`.
pub const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Anthropic Messages JSON body from a rig `CompletionRequest`. The history
/// crosses the converter seam once (`rig_compat::from_rig_message`) and the
/// body is assembled over the native message family.
pub fn build_messages_body(model: &str, request: &CompletionRequest) -> Value {
    let history: Vec<Message> = request
        .chat_history
        .iter()
        .map(crate::rig_compat::from_rig_message)
        .collect();
    build_messages_body_native(
        model,
        request.preamble.as_deref(),
        request.max_tokens,
        &history,
        &request.tools,
    )
}

/// Body assembly over the native message family (no rig vocabulary).
///
/// Lean: `systemBlocks`, `splitSystem`, `toolsField`. Two `cache_control`
/// breakpoints: the last `system` block (identity + preamble + System rows +
/// tools prefix) and the last content block of the last message (moving
/// breakpoint across tool_result turns).
pub fn build_messages_body_native(
    model: &str,
    preamble: Option<&str>,
    max_tokens: Option<u64>,
    history: &[Message],
    tools: &[ToolDefinition],
) -> Value {
    let mut system: Vec<Value> = vec![json!({ "type": "text", "text": CLAUDE_CODE_IDENTITY })];
    if let Some(preamble) = preamble.map(str::trim).filter(|value| !value.is_empty()) {
        system.push(json!({ "type": "text", "text": preamble }));
    }
    for row in system_rows(history) {
        system.push(json!({ "type": "text", "text": row }));
    }
    mark_ephemeral(system.last_mut());

    let mut messages = anthropic_messages(history);
    if let Some(last) = messages.last_mut() {
        if let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) {
            mark_ephemeral(blocks.last_mut());
        }
    }

    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect();

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "stream": true,
        "system": system,
        "messages": messages,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    // No sampling keys: live claude-sonnet-5 400s on `temperature` / `top_p`
    // / `top_k`; `additional_params` carries those and is not merged.
    body
}

fn mark_ephemeral(block: Option<&mut Value>) {
    if let Some(Value::Object(map)) = block {
        map.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
    }
}

/// `Message::System` rows in transcript order (Lean `splitSystem`).
fn system_rows(history: &[Message]) -> Vec<String> {
    history
        .iter()
        .filter_map(|message| match message {
            Message::System { content } if !content.trim().is_empty() => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn anthropic_messages(history: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for message in history {
        match message {
            Message::User { content } => {
                let mut blocks = Vec::new();
                for block in content {
                    match block {
                        UserContent::Text(text) if !text.text.is_empty() => {
                            blocks.push(json!({"type": "text", "text": text.text}));
                        }
                        UserContent::ToolResult(result) => {
                            let body: String = result
                                .content
                                .iter()
                                .filter_map(|item| match item {
                                    ToolResultContent::Text(text) => Some(text.text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            blocks.push(json!({
                                "type": "tool_result",
                                "tool_use_id": result.id,
                                "content": body,
                            }));
                        }
                        _ => {}
                    }
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "user", "content": blocks}));
                }
            }
            Message::Assistant { content, .. } => {
                let mut blocks = Vec::new();
                for block in content {
                    match block {
                        AssistantContent::Text(text) if !text.text.is_empty() => {
                            blocks.push(json!({"type": "text", "text": text.text}));
                        }
                        AssistantContent::ToolCall(call) => {
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.function.name,
                                "input": call.function.arguments,
                            }));
                        }
                        _ => {}
                    }
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            // Rows were lifted into `system` by `system_rows`.
            Message::System { .. } => {}
        }
    }
    out
}
