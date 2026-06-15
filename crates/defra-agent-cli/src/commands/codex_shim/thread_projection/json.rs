use codex_app_server_protocol as codex;
use serde_json::{json, Value};

use crate::commands::codex_shim::protocol::{absolute_path, thread_json};

use super::CodexThreadRecord;

pub(in crate::commands::codex_shim) fn codex_thread_json(
    record: &CodexThreadRecord,
    _include_turns: bool,
) -> Value {
    codex_thread_json_with_turns(record, Vec::new())
}

pub(in crate::commands::codex_shim) fn codex_thread_json_with_turns(
    record: &CodexThreadRecord,
    turns: Vec<codex::Turn>,
) -> Value {
    let conversation = record.conversation.as_ref();
    let preview = conversation.and_then(|conversation| {
        let preview = conversation.preview_text.trim();
        (!preview.is_empty()).then_some(preview)
    });
    let mut thread = thread_json(
        &record.cwd,
        &record.session_id,
        preview,
        codex::ThreadStatus::Idle,
        turns,
    );
    let object = thread
        .as_object_mut()
        .expect("thread_json returns an object");
    if let Some(created_at) = thread_created_at(record) {
        object.insert("createdAt".to_string(), json!(created_at));
    }
    if let Some(updated_at) = thread_updated_at(record) {
        object.insert("updatedAt".to_string(), json!(updated_at));
    }
    if !record.name.trim().is_empty() {
        object.insert("name".to_string(), Value::String(record.name.clone()));
    }
    if let Some(conversation) = conversation {
        if record.name.trim().is_empty() && !conversation.title.trim().is_empty() {
            object.insert(
                "name".to_string(),
                Value::String(conversation.title.clone()),
            );
        }
        if preview.is_none() && !conversation.title.trim().is_empty() {
            object.insert(
                "preview".to_string(),
                Value::String(conversation.title.clone()),
            );
        }
        if let Some(parent) = conversation
            .forked_from_session_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            object.insert(
                "forkedFromId".to_string(),
                Value::String(parent.to_string()),
            );
        }
    }
    if let Some(git_info) = record.git_info.clone() {
        object.insert("gitInfo".to_string(), git_info);
    }
    thread
}

pub(in crate::commands::codex_shim) fn thread_start_response_json(
    record: &CodexThreadRecord,
    bound_model_id: &str,
) -> Value {
    thread_response_json(record, codex_thread_json(record, false), bound_model_id)
}

pub(in crate::commands::codex_shim) fn thread_response_json(
    record: &CodexThreadRecord,
    thread: Value,
    bound_model_id: &str,
) -> Value {
    json!({
        "thread": thread,
        "model": bound_model_id,
        "modelProvider": "defra",
        "serviceTier": null,
        "cwd": absolute_path(&record.cwd),
        "runtimeWorkspaceRoots": [],
        "instructionSources": [],
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandbox": { "type": "dangerFullAccess" },
        "activePermissionProfile": null,
        "reasoningEffort": null
    })
}

pub(in crate::commands::codex_shim) fn thread_resume_response_json(
    record: &CodexThreadRecord,
    turns: Vec<codex::Turn>,
    bound_model_id: &str,
) -> Value {
    thread_response_json(
        record,
        codex_thread_json_with_turns(record, turns),
        bound_model_id,
    )
}

fn thread_created_at(record: &CodexThreadRecord) -> Option<i64> {
    record
        .conversation
        .as_ref()
        .and_then(|conversation| conversation.created_at.as_deref())
        .or(record.projection_started.as_deref())
        .and_then(parse_timestamp_seconds)
}

fn thread_updated_at(record: &CodexThreadRecord) -> Option<i64> {
    record
        .conversation
        .as_ref()
        .and_then(|conversation| conversation.updated_at.as_deref())
        .or_else(|| {
            record
                .conversation
                .as_ref()
                .and_then(|conversation| conversation.created_at.as_deref())
        })
        .or(record.projection_started.as_deref())
        .and_then(parse_timestamp_seconds)
}

fn parse_timestamp_seconds(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}
