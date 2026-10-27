use serde_json::Value;

pub fn normalize_responses_assistant_items(value: &mut Value) {
    normalize_function_call_item_ids(value);
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

pub fn normalize_function_call_item_ids(value: &mut Value) -> bool {
    let Some(items) = value.get_mut("input").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) == Some("function_call")
            && object
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.starts_with("fc_"))
        {
            object.remove("id");
            changed = true;
        }
    }
    changed
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
