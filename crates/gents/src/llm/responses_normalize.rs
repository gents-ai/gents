use serde_json::Value;

pub(crate) fn normalize_responses_assistant_items(value: &mut Value) {
    let Some(input) = value.get_mut("input") else {
        return;
    };
    let Some(items) = input.as_array_mut() else {
        return;
    };

    for (index, item) in items.iter_mut().enumerate() {
        normalize_assistant_item(item, index);
    }
}

fn normalize_assistant_item(item: &mut Value, index: usize) {
    let Some(object) = item.as_object_mut() else {
        return;
    };
    if object.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }

    object
        .entry("type".to_string())
        .or_insert_with(|| Value::String("message".to_string()));
    if object.get("id").is_none_or(|value| value.is_null()) {
        object.insert(
            "id".to_string(),
            Value::String(format!("msg_gents_{index}")),
        );
    }
    if object.get("status").is_none_or(|value| value.is_null()) {
        object.insert("status".to_string(), Value::String("completed".to_string()));
    }

    if let Some(content) = object.get_mut("content").and_then(Value::as_array_mut) {
        for item in content {
            normalize_output_text_item(item);
        }
    }
}

fn normalize_output_text_item(item: &mut Value) {
    let Some(object) = item.as_object_mut() else {
        return;
    };
    if object.get("type").and_then(Value::as_str) != Some("output_text") {
        return;
    }
    object
        .entry("annotations".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if object.get("annotations").is_some_and(Value::is_null) {
        object.insert("annotations".to_string(), Value::Array(Vec::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_prior_assistant_message_items_for_vllm() {
        let mut value = json!({
            "model": "test-model",
            "input": [
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}]
                },
                {
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "hi"}]
                }
            ]
        });

        normalize_responses_assistant_items(&mut value);

        let assistant = &value["input"][1];
        assert_eq!(assistant["type"], "message");
        assert_eq!(assistant["id"], "msg_gents_1");
        assert_eq!(assistant["status"], "completed");
        assert_eq!(assistant["content"][0]["annotations"], json!([]));
    }

    #[test]
    fn replaces_null_fields_required_by_responses_wire() {
        let mut value = json!({
            "input": [{
                "role": "assistant",
                "id": null,
                "status": null,
                "content": [{
                    "type": "output_text",
                    "text": "hi",
                    "annotations": null
                }]
            }]
        });

        normalize_responses_assistant_items(&mut value);

        let assistant = &value["input"][0];
        assert_eq!(assistant["id"], "msg_gents_0");
        assert_eq!(assistant["status"], "completed");
        assert_eq!(assistant["content"][0]["annotations"], json!([]));
    }

    #[test]
    fn leaves_hosted_openai_shaped_items_unchanged() {
        let mut value = json!({
            "input": [{
                "type": "message",
                "id": "msg_existing",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "already shaped",
                    "annotations": []
                }]
            }]
        });
        let original = value.clone();

        normalize_responses_assistant_items(&mut value);

        assert_eq!(value, original);
    }

    #[test]
    fn ignores_non_assistant_items() {
        let mut value = json!({
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }]
        });
        let original = value.clone();

        normalize_responses_assistant_items(&mut value);

        assert_eq!(value, original);
    }
}
