use super::event_loaders::{resolve_tool_payloads, ToolLifecycleObservation};
use crate::run_timeline::{TimelineMessageRow, TimelineToolCallRow};
use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadRef, TranscriptMessage,
};

fn admitted() -> (ToolLifecycleObservation, TimelineMessageRow) {
    let arguments = serde_json::json!({"command": "echo checked"});
    let header = TranscriptMessage {
        message_key: "accept".into(),
        session_id: "session".into(),
        agent_did: "did:test:agent".into(),
        requester_did: None,
        request_doc_id: Some("request-physical".into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 3,
        role: MessageRole::Assistant,
        native_id: None,
        created_at: "2026-09-21T00:00:00Z".into(),
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: "tool-physical".into(),
            id: "native-call".into(),
            call_id: None,
            name: "bash".into(),
            arguments: PayloadRef {
                close_doc_id: "close".into(),
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
    };
    let native = Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall::new(
            "native-call".into(),
            ToolFunction::new("bash".into(), arguments),
        ))],
    };
    let row = TimelineToolCallRow {
        doc_id: Some("tool-physical".into()),
        request_id: Some("logical-request".into()),
        request_doc_id: Some("request-physical".into()),
        session_id: "session".into(),
        message_sequence: Some(3),
        tool_name: "bash".into(),
        tool_call_id: "native-call".into(),
        lifecycle_state: Some("running".into()),
        ..Default::default()
    };
    (
        ToolLifecycleObservation {
            row,
            spawned_by_tool_call_doc_id: None,
            delegated_input: None,
        },
        TimelineMessageRow::from_canonical("header-physical".into(), header, native),
    )
}

#[test]
fn timeline_arguments_use_physical_binding_and_undelivered_output_stays_absent() {
    let (row, message) = admitted();
    let projected = resolve_tool_payloads(row, &[message]).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&projected.args).unwrap(),
        serde_json::json!({"command": "echo checked"})
    );
    assert_eq!(projected.request_id.as_deref(), Some("logical-request"));
    assert_eq!(projected.result, None);
}

#[test]
fn timeline_rejects_reused_native_id_on_a_different_physical_tool() {
    let (mut row, message) = admitted();
    row.row.doc_id = Some("other-tool-physical".into());
    assert!(resolve_tool_payloads(row, &[message]).is_err());
}

#[test]
fn timeline_rejects_multiple_acceptance_headers_instead_of_choosing_one() {
    let (row, message) = admitted();
    let mut twin = message.clone();
    twin.doc_id = Some("other-header-physical".into());
    assert!(resolve_tool_payloads(row, &[message, twin]).is_err());
}

#[test]
fn delegated_arguments_do_not_follow_private_coordinator_source() {
    let (mut row, _) = admitted();
    row.delegated_input = Some(gents_protocol::output::DelegatedToolInput {
        source: PayloadRef {
            close_doc_id: "not-authorized-here".into(),
            stream: 7,
        },
        arguments: "{ \"command\": \"echo delegated\" }".into(),
        parent_subagent_depth: 0,
    });
    let projected = resolve_tool_payloads(row, &[]).unwrap();
    assert_eq!(projected.args, "{ \"command\": \"echo delegated\" }");
    assert_eq!(projected.result, None);
}
