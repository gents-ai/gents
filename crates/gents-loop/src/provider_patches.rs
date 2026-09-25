//! Deterministic post-serialization body rewrites `provider_input` applies
//! before projecting a request's byte size, mirroring what the live
//! transport does at send time (`gents::chatgpt_codex`,
//! `gents::xai_grok_oauth`, native, wrap the actual HTTP client around the
//! same two functions so accounting and the wire never diverge).

use bytes::Bytes;
use serde_json::Value;

pub fn patch_instructions_body(body: &[u8]) -> Option<Bytes> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    let mut changed = false;

    if value.get("instructions").is_none() {
        let instructions = first_system_text(value.get("input")?)?;
        value["instructions"] = Value::String(instructions);
        if let Some(input) = value.get_mut("input") {
            strip_system_items(input);
        }
        changed = true;
    }
    if value.get("store").is_none() {
        value["store"] = Value::Bool(false);
        changed = true;
    }
    if value.get("stream").is_none() {
        value["stream"] = Value::Bool(true);
        changed = true;
    }
    changed |= request_encrypted_reasoning_when_stateless(&mut value);
    for unsupported in CHATGPT_CODEX_UNSUPPORTED_PARAMS {
        if let Some(object) = value.as_object_mut() {
            if object.remove(*unsupported).is_some() {
                changed = true;
            }
        }
    }
    if let Some(tools) = value.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(object) = tool.as_object_mut() {
                if object.get("strict") != Some(&Value::Bool(false)) {
                    object.insert("strict".to_string(), Value::Bool(false));
                    changed = true;
                }
            }
        }
    }
    if !changed {
        return None;
    }
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

// pub, not private: gents' own chatgpt_codex test pins this exact list.
pub const CHATGPT_CODEX_UNSUPPORTED_PARAMS: &[&str] =
    &["max_output_tokens", "temperature", "top_p"];

fn first_system_text(input: &Value) -> Option<String> {
    match input {
        Value::Array(items) => items.iter().find_map(system_item_text),
        Value::Object(_) => system_item_text(input),
        _ => None,
    }
}

fn system_item_text(item: &Value) -> Option<String> {
    if item.get("role").and_then(Value::as_str) != Some("system") {
        return None;
    }
    content_text(item.get("content")?)
}

fn strip_system_items(input: &mut Value) {
    match input {
        Value::Array(items) => {
            items.retain(|item| item.get("role").and_then(Value::as_str) != Some("system"));
        }
        Value::Object(item) if item.get("role").and_then(Value::as_str) == Some("system") => {
            item.clear();
        }
        Value::Object(_) => {}
        _ => {}
    }
}

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        Value::Object(part) => part
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

/// Grok/xAI quirk: force `store:false` when the caller left it unset, which
/// makes the request stateless.
pub fn patch_store_false(body: &[u8]) -> Option<Bytes> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    let mut changed = false;
    if value.get("store").is_none() {
        value["store"] = Value::Bool(false);
        changed = true;
    }
    changed |= request_encrypted_reasoning_when_stateless(&mut value);
    if !changed {
        return None;
    }
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

pub const ENCRYPTED_REASONING_INCLUDE: &str = "reasoning.encrypted_content";

/// A `store:false` Responses server keeps no reasoning items, so replayed
/// reasoning is only resolvable from the `encrypted_content` the previous
/// response returned, and the server returns it only when `include` asks,
/// whether or not the request sets `reasoning`. Stored requests are left
/// alone: some non-reasoning models reject encrypted reasoning content.
pub fn request_encrypted_reasoning_when_stateless(value: &mut Value) -> bool {
    if value.get("store") != Some(&Value::Bool(false)) {
        return false;
    }
    let requested = Value::String(ENCRYPTED_REASONING_INCLUDE.to_string());
    match value.get_mut("include") {
        Some(Value::Array(include)) => {
            if include.contains(&requested) {
                return false;
            }
            include.push(requested);
        }
        _ => value["include"] = Value::Array(vec![requested]),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_instructions_body_hoists_system_and_sets_defaults() {
        let body = json!({
            "input": [
                {"role": "system", "content": "be helpful"},
                {"role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ],
            "tools": [{"name": "t", "strict": true}]
        });
        let patched = patch_instructions_body(&serde_json::to_vec(&body).unwrap()).unwrap();
        let value: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["instructions"], "be helpful");
        assert_eq!(value["store"], false);
        assert_eq!(value["stream"], true);
        assert_eq!(value["tools"][0]["strict"], false);
        assert_eq!(value["input"].as_array().unwrap().len(), 1);
        assert_eq!(value["include"], json!([ENCRYPTED_REASONING_INCLUDE]));
    }

    #[test]
    fn patch_instructions_body_is_none_when_nothing_changes() {
        let body = json!({
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "instructions": "be helpful",
            "store": false,
            "stream": true,
            "include": [ENCRYPTED_REASONING_INCLUDE]
        });
        assert!(patch_instructions_body(&serde_json::to_vec(&body).unwrap()).is_none());
    }

    #[test]
    fn patch_store_false_sets_store_when_absent() {
        let body = json!({"model": "grok"});
        let patched = patch_store_false(&serde_json::to_vec(&body).unwrap()).unwrap();
        let value: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["store"], false);
        assert_eq!(value["include"], json!([ENCRYPTED_REASONING_INCLUDE]));
    }

    #[test]
    fn patch_store_false_is_idempotent_when_present() {
        let body = json!({"model": "grok", "store": true});
        assert!(patch_store_false(&serde_json::to_vec(&body).unwrap()).is_none());
    }

    #[test]
    fn stateless_include_appends_once_and_skips_stored_requests() {
        let mut value = json!({"store": false, "include": ["message.output_text.logprobs"]});
        assert!(request_encrypted_reasoning_when_stateless(&mut value));
        assert!(!request_encrypted_reasoning_when_stateless(&mut value));
        assert_eq!(
            value["include"],
            json!(["message.output_text.logprobs", ENCRYPTED_REASONING_INCLUDE])
        );

        for mut stored in [json!({"store": true}), json!({"model": "m"})] {
            assert!(!request_encrypted_reasoning_when_stateless(&mut stored));
            assert!(stored.get("include").is_none());
        }
    }
}
