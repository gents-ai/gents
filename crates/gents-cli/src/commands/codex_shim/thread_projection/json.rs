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
        Some(head) if head.waiting_on_user_input() => codex::ThreadStatus::Active {
            active_flags: vec![codex::ThreadActiveFlag::WaitingOnUserInput],
        },
        Some(ClientHeadProjection {
            turn_state: ClientTurnState::WaitingForClaim | ClientTurnState::Streaming,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::commands::codex_shim::subagent_projection::LinkedSubagentThread;

    #[test]
    fn canonical_session_supplies_title_fork_and_dates_but_not_execution_authority() {
        let session: gents_protocol::session::AgentSession = serde_json::from_value(json!({
            "session_id":"child", "agent_did":"did:agent", "behavior_id":"configured",
            "created_at":"2026-01-01T00:00:00Z",
            "title":{"text":"Reviewed title", "source":"user"},
            "provenance":{"fork":{"source_session_id":"source", "at_user_turn":2}},
            "observation":{"last_activity_at":"2026-01-02T00:00:00Z", "preview":"Actual preview",
                "latest_request":{"request_doc_id":"old-doc", "request_id":"old", "lifecycle_state":"failed"}}
        })).unwrap();
        let record = CodexThreadRecord {
            session_id:"child".into(), cwd:PathBuf::from("/tmp"), archived:false, loaded:true,
            memory_mode:"disabled".into(), name:String::new(), settings_json:String::new(), git_info:None,
            projection_started:Some("2026-02-01T00:00:00Z".into()), session:Some(session),
            latest_request:Some(serde_json::from_value(json!({"request":{
                "_docID":"new-doc", "request_id":"new", "lifecycle_state":"pending"}, "response":null})).unwrap()),
            subagent:None,
        };
        let thread = codex_thread_json(&record, false);
        assert_eq!(thread["name"], "Reviewed title");
        assert_eq!(thread["preview"], "Actual preview");
        assert_eq!(thread["forkedFromId"], "source");
        assert_eq!(thread["createdAt"], 1767225600_i64);
        assert_eq!(thread["updatedAt"], 1767312000_i64);
        assert!(
            matches!(
                codex_thread_status(&record),
                codex::ThreadStatus::Active { .. }
            ),
            "cached failed observation must not override actual pending request"
        );
        assert_eq!(
            codex_thread_json(&record, false)["createdAt"],
            thread["createdAt"]
        );
    }

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
            session: None,
            latest_request: None,
            subagent: Some(LinkedSubagentThread {
                parent_request_doc_id: "test-parent-doc".into(),
                parent_agent_did: "did:parent".into(),
                parent_requester_did: None,
                request_doc_id: "test-request-doc".into(),
                latest_request_doc_id: "test-request-doc".into(),
                requester_did: Some("did:parent".into()),
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
                client_projection: gents_protocol::client_protocol::project_persisted_attempt(
                    "processing",
                    false,
                    None,
                ),
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
            ("pending", None, "active", None),
            ("claimed", None, "active", None),
            ("processing", None, "active", None),
            ("inputRequired", None, "active", Some("waitingOnUserInput")),
            ("completed", None, "idle", None),
            ("superseded", None, "idle", None),
            ("interrupted", None, "idle", None),
            ("failed", None, "systemError", None),
            ("dead", None, "systemError", None),
            ("processing", Some("complete"), "idle", None),
            ("processing", Some("error"), "systemError", None),
        ];

        for (runtime_state, response_status, expected_type, expected_flag) in cases {
            let head = gents_protocol::client_protocol::project_persisted_attempt(
                runtime_state,
                false,
                response_status,
            );
            let encoded =
                serde_json::to_value(projected_thread_status(head)).expect("encode thread status");
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
            serde_json::to_value(projected_thread_status(None)).unwrap()["type"],
            "idle"
        );
    }
}
