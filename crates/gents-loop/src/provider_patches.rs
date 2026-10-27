use bytes::Bytes;
use serde_json::Value;

pub fn patch_instructions_body(body: &[u8]) -> Option<Bytes> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    let mut changed = crate::responses_normalize::normalize_function_call_item_ids(&mut value);

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

/// Only reasoning-model transports may add this include: OpenAI rejects it for
/// non-reasoning models, while stateless responses need it to replay reasoning.
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
