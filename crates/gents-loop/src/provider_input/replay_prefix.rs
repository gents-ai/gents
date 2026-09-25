use anyhow::{Context, Result};
use serde_json::Value;

use crate::claude_messages_body::ReplayWire;

/// The capture and prospective request must use the same wire projection.
/// Context bytes retain unknown fields; only non-prefix generation controls
/// and structural cache hints are excluded. User/tool payloads are opaque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayPrefixProjection {
    pub context: Vec<u8>,
    pub messages: Vec<Vec<u8>>,
}

impl ReplayPrefixProjection {
    pub fn compatible_with(&self, current: &Self) -> bool {
        self.context == current.context && current.messages.starts_with(&self.messages)
    }
}

fn canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(&crate::rendered_request::canonical_json(value))
        .context("encoding replay prefix projection")
}

fn remove_block_cache_hints(value: &mut Value) {
    if let Some(blocks) = value.as_array_mut() {
        for block in blocks {
            if let Some(block) = block.as_object_mut() {
                block.remove("cache_control");
            }
        }
    }
}

/// Consumes a body emitted by `ProviderInputCounter::project_body` or decoded
/// from exact transport capture. It never recursively removes similarly named
/// keys from tool arguments, schemas, or embedded user JSON.
pub fn project(body: &Value, wire: ReplayWire) -> Result<ReplayPrefixProjection> {
    let mut context = body
        .as_object()
        .context("replay prefix body is not an object")?
        .clone();
    let message_key = match wire {
        ReplayWire::ClaudeMessages => "messages",
        ReplayWire::Responses => "input",
    };
    let messages = context
        .remove(message_key)
        .context("replay prefix body has no conversation")?;
    let messages = messages
        .as_array()
        .context("replay prefix conversation is not an array")?;

    for field in [
        "model",
        "max_tokens",
        "max_output_tokens",
        "stream",
        "stream_options",
        "temperature",
        "top_p",
        "top_k",
        "thinking",
        "reasoning",
        "store",
        "include",
    ] {
        context.remove(field);
    }
    if let Some(output_config) = context
        .get_mut("output_config")
        .and_then(Value::as_object_mut)
    {
        output_config.remove("effort");
        if output_config.is_empty() {
            context.remove("output_config");
        }
    }
    if wire == ReplayWire::ClaudeMessages {
        for field in ["system", "tools"] {
            if let Some(value) = context.get_mut(field) {
                remove_block_cache_hints(value);
            }
        }
    }

    let mut projected = Vec::with_capacity(messages.len());
    for message in messages {
        if wire == ReplayWire::Responses && message["type"].as_str() == Some("reasoning") {
            continue;
        }
        let mut message = message.clone();
        if wire == ReplayWire::ClaudeMessages {
            let assistant = message["role"].as_str() == Some("assistant");
            if let Some(content) = message.get_mut("content") {
                remove_block_cache_hints(content);
                if assistant {
                    if let Some(blocks) = content.as_array_mut() {
                        blocks.retain(|block| {
                            !matches!(
                                block["type"].as_str(),
                                Some("thinking" | "redacted_thinking")
                            )
                        });
                        if blocks.is_empty() {
                            continue;
                        }
                    }
                }
            }
        }
        projected.push(canonical_bytes(&message)?);
    }
    Ok(ReplayPrefixProjection {
        context: canonical_bytes(&Value::Object(context))?,
        messages: projected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structural_hints_do_not_erase_user_or_tool_payload() {
        let input = json!({"cache_control": {"type": "user-data"}});
        let schema = json!({"properties": {"cache_control": {"type": "string"}}});
        let body = json!({
            "model": "model",
            "system": [{"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "inspect", "input_schema": schema,
                "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "audit", "signature": "signature"},
                {"type": "tool_use", "id": "call", "name": "inspect", "input": input,
                    "cache_control": {"type": "ephemeral"}}
            ]}],
            "output_config": {"effort": "high", "format": {"type": "json_schema", "schema": schema}}
        });
        let projection = project(&body, ReplayWire::ClaudeMessages).unwrap();
        let context: Value = serde_json::from_slice(&projection.context).unwrap();
        let message: Value = serde_json::from_slice(&projection.messages[0]).unwrap();
        assert_eq!(context["tools"][0]["input_schema"], schema);
        assert_eq!(context["output_config"]["format"]["schema"], schema);
        assert!(context["output_config"].get("effort").is_none());
        assert!(context["tools"][0].get("cache_control").is_none());
        assert!(context["system"][0].get("cache_control").is_none());
        assert_eq!(message["content"].as_array().unwrap().len(), 1);
        assert_eq!(message["content"][0]["input"], input);
        assert!(message["content"][0].get("cache_control").is_none());
        assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn responses_projection_retains_exact_argument_string_and_unknown_context() {
        let arguments = "{\"cache_control\": \"retain spacing\"}";
        let call = json!({"type": "function_call", "call_id": "call", "name": "inspect", "arguments": arguments});
        let body = json!({
            "model": "model",
            "instructions": "system",
            "future_context_field": {"cache_control": "must survive"},
            "input": [{"type": "reasoning", "encrypted_content": "audit-only-here"}, call]
        });
        let projection = project(&body, ReplayWire::Responses).unwrap();
        assert_eq!(projection.messages, vec![canonical_bytes(&call).unwrap()]);
        let context: Value = serde_json::from_slice(&projection.context).unwrap();
        assert_eq!(
            context["future_context_field"],
            body["future_context_field"]
        );
    }
}
