use gents_codex_protocol as codex;
use gents_protocol::client_protocol::{ClientHeadProjection, ClientTurnState};
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
    let session = record.session.as_ref();
    let preview = session
        .and_then(|session| session.observation.as_ref())
        .and_then(|observation| observation.preview.as_deref())
        .filter(|preview| !preview.trim().is_empty());
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
    if let Some(session) = session {
        if let Some(title) = session.title.as_ref() {
            if record.name.trim().is_empty() {
                object.insert("name".into(), json!(title.text));
            }
            if preview.is_none() {
                object.insert("preview".into(), json!(title.text));
            }
        }
        if let Some(fork) = session
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.fork.as_ref())
        {
            object.insert("forkedFromId".into(), json!(fork.source_session_id));
        }
    }
    if let Some(git_info) = record.git_info.clone() {
        object.insert("gitInfo".to_string(), git_info);
    }
    if let Some(link) = record.caused.as_ref() {
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
                        "agent_role": link.agent_id
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
            Value::String(link.agent_id.clone()),
        );
    }
    thread
}

pub(in crate::commands::codex_shim) fn codex_thread_status(
    record: &CodexThreadRecord,
) -> codex::ThreadStatus {
    if let Some(link) = record.caused.as_ref() {
        return projected_thread_status(link.client_projection);
    }
    projected_thread_status(
        record
            .latest_request
            .as_ref()
            .and_then(gents_protocol::graphql::GraphqlTurnState::projected_head),
    )
}

pub(in crate::commands::codex_shim) fn projected_thread_status(
    head: Option<ClientHeadProjection>,
) -> codex::ThreadStatus {
    match head {
        Some(ClientHeadProjection {
            turn_state: ClientTurnState::WaitingForClaim | ClientTurnState::Running,
            ..
        }) => codex::ThreadStatus::Active {
            active_flags: Vec::new(),
        },
        Some(ClientHeadProjection {
            turn_state: ClientTurnState::Failed,
            ..
        }) => codex::ThreadStatus::SystemError,
        Some(_) => codex::ThreadStatus::Idle,
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
        .session
        .as_ref()
        .map(|session| session.created_at.as_str())
        .or(record.projection_started.as_deref())
        .and_then(parse_timestamp_seconds)
}

fn thread_updated_at(record: &CodexThreadRecord) -> Option<i64> {
    record
        .session
        .as_ref()
        .map(|session| {
            session
                .observation
                .as_ref()
                .map(|observation| observation.last_activity_at.as_str())
                .unwrap_or(&session.created_at)
        })
        .or(record.projection_started.as_deref())
        .and_then(parse_timestamp_seconds)
}

fn parse_timestamp_seconds(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}
