use codex_app_server_protocol as codex;
use serde_json::{Value, json};

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
    if let Some(git_info) = codex_git_info_json(&record.git_info_json) {
        object.insert("gitInfo".to_string(), git_info);
    }
    thread
}

pub(in crate::commands::codex_shim) fn thread_start_response_json(
    record: &CodexThreadRecord,
    bound_profile_id: &str,
) -> Value {
    thread_response_json(record, codex_thread_json(record, false), bound_profile_id)
}

pub(in crate::commands::codex_shim) fn thread_response_json(
    record: &CodexThreadRecord,
    thread: Value,
    bound_profile_id: &str,
) -> Value {
    json!({
        "thread": thread,
        "model": bound_profile_id,
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
    bound_profile_id: &str,
) -> Value {
    thread_start_response_json(record, bound_profile_id)
}

fn codex_git_info_json(raw: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let object = value.as_object()?;
    if object.is_empty() {
        return None;
    }
    let string_field = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    };
    Some(json!({
        "sha": string_field("sha"),
        "branch": string_field("branch"),
        "originUrl": string_field("originUrl").or_else(|| string_field("origin_url")),
    }))
}
