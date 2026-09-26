//! Lean `PromptAssembly.ReplayFrontier`: flat provider input, the two
//! capture checks that decide reasoning replay, and the maximal admissible
//! suffix of accepted turns.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::claude_messages_body::ReplayWire;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlatItem {
    Ordinary(Vec<u8>),
    Reasoning(Vec<u8>),
}

pub fn ords(items: &[FlatItem]) -> Vec<&[u8]> {
    items
        .iter()
        .filter_map(|item| match item {
            FlatItem::Ordinary(bytes) => Some(bytes.as_slice()),
            FlatItem::Reasoning(_) => None,
        })
        .collect()
}

/// Each reasoning item with the number of ordinary items before it.
pub fn anchored(items: &[FlatItem]) -> Vec<(usize, &[u8])> {
    let mut ordinary = 0;
    let mut anchored = Vec::new();
    for item in items {
        match item {
            FlatItem::Ordinary(_) => ordinary += 1,
            FlatItem::Reasoning(bytes) => anchored.push((ordinary, bytes.as_slice())),
        }
    }
    anchored
}

pub fn drop_leading_reasoning(mut count: usize, items: &[FlatItem]) -> Vec<FlatItem> {
    items
        .iter()
        .filter(|item| {
            if count > 0 && matches!(item, FlatItem::Reasoning(_)) {
                count -= 1;
                false
            } else {
                true
            }
        })
        .cloned()
        .collect()
}

/// One accepted turn located in the assembled body. `base` is the provenance
/// and payload decision; `captured` is the flattened accepted request that
/// produced the turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub base: bool,
    pub prefix_ords: Vec<Vec<u8>>,
    pub items: Vec<(usize, Vec<u8>)>,
    pub captured: Option<Vec<FlatItem>>,
}

/// Number of leading turns to drop so the rest is the maximal admissible
/// suffix under `turnOk`. Admissibility is downward closed over suffixes
/// (`admissible_of_suffix`), so scanning from the end is exact: each turn
/// either extends the kept suffix or ends it.
pub fn admissible_turn_drop(turns: &[Turn]) -> usize {
    let mut dropped = turns.len();
    let mut bound = 0;
    for index in (0..turns.len()).rev() {
        let Some(minimum) = minimum_drop(turns, index) else {
            break;
        };
        bound = bound.max(minimum);
        if index < bound {
            break;
        }
        dropped = index;
    }
    dropped
}

/// The smallest drop count `d ≤ index` for which the items of `turns[d..index]`
/// form a suffix of this turn's anchored capture reasoning, or `None` when the
/// turn fails its own checks.
fn minimum_drop(turns: &[Turn], index: usize) -> Option<usize> {
    let turn = &turns[index];
    let captured = turn.captured.as_ref()?;
    if !turn.base
        || ords(captured)
            .into_iter()
            .ne(turn.prefix_ords.iter().map(Vec::as_slice))
    {
        return None;
    }
    let captured = anchored(captured);
    let mut matched = 0;
    let mut minimum = index;
    for earlier in (0..index).rev() {
        let items = &turns[earlier].items;
        if matched + items.len() > captured.len() {
            break;
        }
        let end = captured.len() - matched;
        let window = &captured[end - items.len()..end];
        if window
            .iter()
            .zip(items)
            .any(|((anchor, bytes), (item_anchor, item))| anchor != item_anchor || bytes != item)
        {
            break;
        }
        matched += items.len();
        minimum = earlier;
    }
    Some(minimum)
}

/// Fields outside the replay-bound prefix: generation controls, output and
/// transport settings. Everything else in the request context is compared.
const NON_PREFIX_FIELDS: &[&str] = &[
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
    "tool_choice",
    "metadata",
    "stop_sequences",
    "service_tier",
    "parallel_tool_calls",
];

fn canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(&crate::rendered_request::canonical_json(value))
        .context("encoding replay flat item")
}

fn without_cache_hint(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("cache_control");
    }
    value
}

/// Flatten a provider body emitted by `ProviderInputCounter::project_body` or
/// decoded from an exact transport capture. Cache hints are removed only where
/// the wire places them (top-level system/tool/content blocks), never from
/// nested tool arguments, schemas or user JSON.
pub fn flatten(body: &Value, wire: ReplayWire) -> Result<Vec<FlatItem>> {
    let mut context = body
        .as_object()
        .context("replay body is not an object")?
        .clone();
    let conversation_key = match wire {
        ReplayWire::ClaudeMessages => "messages",
        ReplayWire::Responses => "input",
    };
    let conversation = context
        .remove(conversation_key)
        .context("replay body has no conversation")?;
    for field in NON_PREFIX_FIELDS {
        context.remove(*field);
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
            if let Some(blocks) = context.get_mut(field).and_then(Value::as_array_mut) {
                for block in blocks.iter_mut() {
                    *block = without_cache_hint(block);
                }
            }
        }
    }
    let mut items = vec![FlatItem::Ordinary(canonical_bytes(&Value::Object(
        context,
    ))?)];
    match (wire, &conversation) {
        (ReplayWire::ClaudeMessages, Value::Array(messages)) => {
            for message in messages {
                let mut header = message
                    .as_object()
                    .context("replay message is not an object")?
                    .clone();
                let content = header.remove("content");
                items.push(FlatItem::Ordinary(canonical_bytes(&Value::Object(header))?));
                match content {
                    Some(Value::Array(blocks)) => {
                        for block in &blocks {
                            let block = without_cache_hint(block);
                            let bytes = canonical_bytes(&block)?;
                            items.push(match block.get("type").and_then(Value::as_str) {
                                Some("thinking" | "redacted_thinking") => {
                                    FlatItem::Reasoning(bytes)
                                }
                                _ => FlatItem::Ordinary(bytes),
                            });
                        }
                    }
                    Some(other) => items.push(FlatItem::Ordinary(canonical_bytes(&other)?)),
                    None => {}
                }
            }
        }
        (ReplayWire::Responses, Value::Array(inputs)) => {
            for input in inputs {
                let bytes = canonical_bytes(input)?;
                items.push(
                    if input.get("type").and_then(Value::as_str) == Some("reasoning") {
                        FlatItem::Reasoning(bytes)
                    } else {
                        FlatItem::Ordinary(bytes)
                    },
                );
            }
        }
        (_, other) => items.push(FlatItem::Ordinary(canonical_bytes(other)?)),
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ordinary(value: &str) -> FlatItem {
        FlatItem::Ordinary(value.as_bytes().to_vec())
    }

    fn reasoning(value: &str) -> FlatItem {
        FlatItem::Reasoning(value.as_bytes().to_vec())
    }

    #[test]
    fn claude_flattening_keeps_payload_keys_and_drops_structural_hints() {
        let input = json!({"cache_control": {"type": "user-data"}});
        let schema = json!({"properties": {"cache_control": {"type": "string"}}});
        let body = json!({
            "model": "model",
            "max_tokens": 10,
            "system": [{"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "inspect", "input_schema": schema,
                "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {"role": "user", "content": "plain"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "audit", "signature": "signature"},
                    {"type": "tool_use", "id": "call", "name": "inspect", "input": input,
                        "cache_control": {"type": "ephemeral"}}
                ]}
            ],
            "output_config": {"effort": "high", "format": {"type": "json_schema", "schema": schema}}
        });
        let items = flatten(&body, ReplayWire::ClaudeMessages).unwrap();
        let FlatItem::Ordinary(context) = &items[0] else {
            panic!("context first")
        };
        let context: Value = serde_json::from_slice(context).unwrap();
        assert_eq!(context["tools"][0]["input_schema"], schema);
        assert_eq!(context["output_config"]["format"]["schema"], schema);
        assert!(context["output_config"].get("effort").is_none());
        assert!(context.get("model").is_none() && context.get("max_tokens").is_none());
        assert!(context["system"][0].get("cache_control").is_none());
        assert!(context["tools"][0].get("cache_control").is_none());
        assert_eq!(items.len(), 6);
        assert!(matches!(&items[3], FlatItem::Ordinary(header)
            if header == br#"{"role":"assistant"}"#));
        assert!(matches!(&items[4], FlatItem::Reasoning(_)));
        let FlatItem::Ordinary(tool) = &items[5] else {
            panic!("tool use is ordinary")
        };
        let tool: Value = serde_json::from_slice(tool).unwrap();
        assert_eq!(tool["input"], input);
        assert!(tool.get("cache_control").is_none());
    }

    #[test]
    fn responses_flattening_keeps_argument_strings_and_unknown_context() {
        let arguments = "{\"cache_control\": \"retain spacing\"}";
        let call = json!({"type": "function_call", "call_id": "call", "name": "inspect",
            "arguments": arguments});
        let body = json!({
            "model": "model",
            "instructions": "system",
            "future_context_field": {"cache_control": "must survive"},
            "input": [{"type": "reasoning", "encrypted_content": "sealed"}, call]
        });
        let items = flatten(&body, ReplayWire::Responses).unwrap();
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[1], FlatItem::Reasoning(_)));
        assert_eq!(
            items[2],
            FlatItem::Ordinary(canonical_bytes(&call).unwrap())
        );
        let FlatItem::Ordinary(context) = &items[0] else {
            panic!("context first")
        };
        let context: Value = serde_json::from_slice(context).unwrap();
        assert_eq!(
            context["future_context_field"],
            body["future_context_field"]
        );
    }

    #[test]
    fn leading_removal_is_what_the_checks_accept() {
        let captured = vec![
            ordinary("context"),
            ordinary("user"),
            ordinary("assistant"),
            reasoning("r1"),
            ordinary("text"),
            ordinary("assistant"),
            reasoning("r2"),
            ordinary("text"),
        ];
        let current = drop_leading_reasoning(1, &captured);
        assert_eq!(ords(&current), ords(&captured));
        assert!(anchored(&captured).ends_with(&anchored(&current)));
        let moved = vec![
            ordinary("context"),
            ordinary("user"),
            ordinary("assistant"),
            ordinary("text"),
            reasoning("r1"),
            ordinary("assistant"),
            reasoning("r2"),
            ordinary("text"),
        ];
        assert!(!anchored(&captured).ends_with(&anchored(&moved)));
    }
}
