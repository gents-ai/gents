use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use codex_app_server_protocol as codex;
use serde::Serialize;
use serde_json::{json, Value};

use super::protocol::now_millis;

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
        _ => json!({
            "method": method,
            "item_type": "other",
        }),
    }
}

fn trace_json<T>(value: &T) -> Value
where
    T: Serialize,
{
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn append_json(default_path: &Path, value: Value) {
    let path = std::env::var_os("DEFRA_CODEX_SHIM_TRACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_path.to_path_buf());
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", value);
    }
}
