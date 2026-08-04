use gents_codex_protocol as codex;
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
        codex_thread_status(record),
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
    if let Some(link) = record.subagent.as_ref() {
        object.insert(
            "sessionId".to_string(),
            Value::String(link.root_session_id.clone()),
        );
        object.insert(
            "source".to_string(),
            json!({
                "subAgent": {
                    "thread_spawn": {
                        "parent_thread_id": link.parent_session_id,
                        "depth": link.depth,
                        "agent_nickname": link.nickname,
                        "agent_role": link.behavior_id
                    }
                }
            }),
        );
        object.insert(
            "threadSource".to_string(),
            Value::String("subagent".to_string()),
        );
        object.insert(
            "agentNickname".to_string(),
            Value::String(link.nickname.clone()),
        );
        object.insert(
            "agentRole".to_string(),
            Value::String(link.behavior_id.clone()),
        );
    }
    thread
}

pub(in crate::commands::codex_shim) fn codex_thread_status(
    record: &CodexThreadRecord,
) -> codex::ThreadStatus {
    if let Some(link) = record.subagent.as_ref() {
        return projected_thread_status(Some(&link.lifecycle_state), "");
    }
    let conversation = record.conversation.as_ref();
    projected_thread_status(
        conversation.and_then(|row| row.latest_request_lifecycle_state.as_deref()),
        conversation.map(|row| row.status.as_str()).unwrap_or(""),
    )
}

pub(in crate::commands::codex_shim) fn projected_thread_status(
    request_state: Option<&str>,
    conversation_status: &str,
) -> codex::ThreadStatus {
    match request_state.map(str::trim) {
        Some("pending" | "claimed" | "processing") => codex::ThreadStatus::Active {
            active_flags: Vec::new(),
        },
        Some("inputRequired") => codex::ThreadStatus::Active {
            active_flags: vec![codex::ThreadActiveFlag::WaitingOnUserInput],
        },
        Some("failed" | "dead") => codex::ThreadStatus::SystemError,
        Some("completed" | "superseded" | "interrupted") => codex::ThreadStatus::Idle,
        _ if conversation_status.trim() == "error" => codex::ThreadStatus::SystemError,
        _ => codex::ThreadStatus::Idle,
    }
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
        "modelProvider": "gents",
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::commands::codex_shim::subagent_projection::LinkedSubagentThread;

    #[test]
    fn subagent_thread_serializes_codex_navigation_metadata() {
        let root_session_id = uuid::Uuid::new_v4().to_string();
        let parent_session_id = root_session_id.clone();
        let child_session_id = uuid::Uuid::new_v4().to_string();
        let record = CodexThreadRecord {
            session_id: child_session_id.clone(),
            cwd: PathBuf::from("/tmp/project"),
            archived: false,
            loaded: true,
            memory_mode: "disabled".to_string(),
            name: "reviewer".to_string(),
            settings_json: String::new(),
            git_info: None,
            projection_started: None,
            conversation: None,
            subagent: Some(LinkedSubagentThread {
                request_id: "child-request".to_string(),
                latest_request_id: "child-request".to_string(),
                latest_request_content: "Inspect the patch".to_string(),
                latest_request_created_at: None,
                session_id: child_session_id.clone(),
                parent_request_id: "parent-request".to_string(),
                parent_tool_call_id: "spawn-call".to_string(),
                parent_session_id: parent_session_id.clone(),
                root_session_id: root_session_id.clone(),
                depth: 1,
                agent_did: "did:child".to_string(),
                behavior_id: "code-review".to_string(),
                model: Some("child-model".to_string()),
                nickname: "reviewer".to_string(),
                lifecycle_state: "processing".to_string(),
                failure_reason: None,
                created_at: None,
            }),
        };

        let value = codex_thread_json(&record, false);
        serde_json::from_value::<codex::Thread>(value.clone())
            .expect("subagent projection must be a valid pinned Codex Thread");
        assert_eq!(value["id"], child_session_id);
        assert_eq!(value["sessionId"], root_session_id);
        assert!(value.get("parentThreadId").is_none());
        assert_eq!(value["threadSource"], "subagent");
        assert_eq!(value["agentNickname"], "reviewer");
        assert_eq!(value["agentRole"], "code-review");
        assert_eq!(
            value.pointer("/source/subAgent/thread_spawn/parent_thread_id"),
            Some(&Value::String(parent_session_id))
        );
        assert_eq!(
            value.pointer("/source/subAgent/thread_spawn/depth"),
            Some(&json!(1))
        );
        assert_eq!(value.pointer("/status/type"), Some(&json!("active")));
    }

    #[test]
    fn thread_status_projects_runtime_request_lifecycle() {
        let cases = [
            (Some("pending"), "active", None),
            (Some("claimed"), "active", None),
            (Some("processing"), "active", None),
            (Some("inputRequired"), "active", Some("waitingOnUserInput")),
            (Some("completed"), "idle", None),
            (Some("superseded"), "idle", None),
            (Some("interrupted"), "idle", None),
            (Some("failed"), "systemError", None),
            (Some("dead"), "systemError", None),
        ];

        for (runtime_state, expected_type, expected_flag) in cases {
            let encoded = serde_json::to_value(projected_thread_status(runtime_state, ""))
                .expect("encode thread status");
            assert_eq!(
                encoded.pointer("/type"),
                Some(&json!(expected_type)),
                "runtime state {runtime_state:?}"
            );
            if let Some(flag) = expected_flag {
                assert_eq!(encoded.pointer("/activeFlags/0"), Some(&json!(flag)));
            }
        }

        assert_eq!(
            serde_json::to_value(projected_thread_status(None, "error")).unwrap()["type"],
            "systemError"
        );
        assert_eq!(
            serde_json::to_value(projected_thread_status(None, "active")).unwrap()["type"],
            "idle"
        );
    }
}
