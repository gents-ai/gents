use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use gents_codex_protocol as codex;
use serde::Serialize;
use serde_json::{json, Value};

use super::protocol::now_millis;

static TRACE_APPEND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn shim_event(default_path: &Path, message: impl AsRef<str>) {
    append_json(
        default_path,
        json!({
            "ts_ms": now_millis(),
            "direction": "internal",
            "event": message.as_ref(),
        }),
    );
}

pub(super) fn shim_event_fields(default_path: &Path, event: impl AsRef<str>, fields: Value) {
    let mut value = json!({
        "ts_ms": now_millis(),
        "direction": "internal",
        "event": event.as_ref(),
    });
    if let (Some(target), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        for (key, field_value) in fields {
            target.insert(key.clone(), field_value.clone());
        }
    } else {
        value["detail"] = fields;
    }
    append_json(default_path, value);
}

pub(super) fn codex_notification(default_path: &Path, notification: &codex::ServerNotification) {
    let event = match notification {
        codex::ServerNotification::TurnStarted(notification) => json!({
            "ts_ms": now_millis(),
            "direction": "out",
            "method": "turn/started",
            "thread_id": notification.thread_id,
            "turn_id": notification.turn.id,
            "turn_status": trace_json(&notification.turn.status),
            "turn_items": notification.turn.items.len(),
        }),
        codex::ServerNotification::AgentMessageDelta(notification) => json!({
            "ts_ms": now_millis(),
            "direction": "out",
            "method": "agent_message/delta",
            "thread_id": notification.thread_id,
            "turn_id": notification.turn_id,
            "item_id": notification.item_id,
            "delta_len": notification.delta.len(),
        }),
        codex::ServerNotification::ReasoningTextDelta(notification) => json!({
            "ts_ms": now_millis(),
            "direction": "out",
            "method": "reasoning/text_delta",
            "thread_id": notification.thread_id,
            "turn_id": notification.turn_id,
            "item_id": notification.item_id,
            "content_index": notification.content_index,
            "delta_len": notification.delta.len(),
        }),
        codex::ServerNotification::ItemStarted(notification) => {
            let mut event = trace_thread_item("item/started", &notification.item);
            event["ts_ms"] = json!(now_millis());
            event["direction"] = json!("out");
            event["thread_id"] = json!(notification.thread_id);
            event["turn_id"] = json!(notification.turn_id);
            event
        }
        codex::ServerNotification::ItemCompleted(notification) => {
            let mut event = trace_thread_item("item/completed", &notification.item);
            event["ts_ms"] = json!(now_millis());
            event["direction"] = json!("out");
            event["thread_id"] = json!(notification.thread_id);
            event["turn_id"] = json!(notification.turn_id);
            event
        }
        codex::ServerNotification::TurnCompleted(notification) => json!({
            "ts_ms": now_millis(),
            "direction": "out",
            "method": "turn/completed",
            "thread_id": notification.thread_id,
            "turn_id": notification.turn.id,
            "turn_status": trace_json(&notification.turn.status),
            "turn_items": notification.turn.items.iter().map(|item| {
                trace_thread_item("turn/item", item)
            }).collect::<Vec<_>>(),
        }),
        _ => json!({
            "ts_ms": now_millis(),
            "direction": "out",
            "method": "other",
        }),
    };
    append_json(default_path, event);
}

fn trace_thread_item(method: &str, item: &codex::ThreadItem) -> Value {
    match item {
        codex::ThreadItem::AgentMessage { id, text, .. } => json!({
            "method": method,
            "item_type": "agentMessage",
            "item_id": id,
            "text_len": text.len(),
        }),
        codex::ThreadItem::Reasoning {
            id,
            summary,
            content,
        } => json!({
            "method": method,
            "item_type": "reasoning",
            "item_id": id,
            "summary_parts": summary.len(),
            "content_parts": content.len(),
            "summary_len": summary.iter().map(String::len).sum::<usize>(),
            "content_len": content.iter().map(String::len).sum::<usize>(),
        }),
        codex::ThreadItem::McpToolCall {
            id,
            server,
            tool,
            status,
            ..
        } => json!({
            "method": method,
            "item_type": "mcpToolCall",
            "item_id": id,
            "server": server,
            "tool": tool,
            "status": trace_json(status),
        }),
        codex::ThreadItem::CommandExecution {
            id,
            command,
            source,
            status,
            ..
        } => json!({
            "method": method,
            "item_type": "commandExecution",
            "item_id": id,
            "command": command,
            "source": trace_json(source),
            "status": trace_json(status),
        }),
        codex::ThreadItem::CollabAgentToolCall {
            id,
            tool,
            status,
            sender_thread_id,
            receiver_thread_ids,
            model,
            agents_states,
            ..
        } => json!({
            "method": method,
            "item_type": "collabAgentToolCall",
            "item_id": id,
            "tool": trace_json(tool),
            "status": trace_json(status),
            "sender_thread_id": sender_thread_id,
            "receiver_thread_ids": receiver_thread_ids,
            "model": model,
            "agents_states": trace_json(agents_states),
        }),
        _ => json!({
            "method": method,
            "item_type": "other",
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn collaboration_items_are_named_in_debug_traces() {
        let item = codex::ThreadItem::CollabAgentToolCall {
            id: "spawn-1".to_string(),
            tool: codex::CollabAgentTool::SpawnAgent,
            status: codex::CollabAgentToolCallStatus::Completed,
            sender_thread_id: "parent".to_string(),
            receiver_thread_ids: vec!["child".to_string()],
            prompt: Some("delegate".to_string()),
            model: Some("backend::model".to_string()),
            reasoning_effort: None,
            agents_states: HashMap::from([(
                "child".to_string(),
                codex::CollabAgentState {
                    status: codex::CollabAgentStatus::Completed,
                    message: None,
                },
            )]),
        };

        let trace = trace_thread_item("item/completed", &item);
        assert_eq!(trace["item_type"], "collabAgentToolCall");
        assert_eq!(trace["item_id"], "spawn-1");
        assert_eq!(trace["receiver_thread_ids"], json!(["child"]));
        assert_eq!(trace["model"], "backend::model");
    }

    #[test]
    fn reasoning_items_are_named_without_logging_reasoning_text() {
        let item = codex::ThreadItem::Reasoning {
            id: "reasoning-1".to_string(),
            summary: Vec::new(),
            content: vec!["private chain of thought".to_string()],
        };

        let trace = trace_thread_item("item/completed", &item);
        assert_eq!(trace["item_type"], "reasoning");
        assert_eq!(trace["item_id"], "reasoning-1");
        assert_eq!(trace["summary_parts"], 0);
        assert_eq!(trace["content_parts"], 1);
        assert_eq!(trace["content_len"], 24);
        assert!(!trace.to_string().contains("private chain of thought"));
    }
}

fn trace_json<T>(value: &T) -> Value
where
    T: Serialize,
{
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn append_json(default_path: &Path, value: Value) {
    let path = std::env::var_os("GENTS_CODEX_SHIM_TRACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_path.to_path_buf());
    let _guard = match TRACE_APPEND_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(value.to_string().as_bytes());
        let _ = file.write_all(b"\n");
    }
}
