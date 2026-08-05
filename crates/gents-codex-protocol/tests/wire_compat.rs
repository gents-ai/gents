use std::path::PathBuf;

use gents_codex_protocol::{
    AbsolutePathBuf, ClientRequest, CommandAction, JSONRPCMessage, MessagePhase, RequestId,
    SandboxMode, UserInput,
};
use serde_json::json;

#[test]
fn initialize_request_keeps_codex_method_and_capability_names() {
    let request: ClientRequest = serde_json::from_value(json!({
        "method": "initialize",
        "id": 7,
        "params": {
            "clientInfo": { "name": "codex-tui", "title": null, "version": "0.145.0" },
            "capabilities": { "experimentalApi": true }
        }
    }))
    .expect("initialize request should match the pinned wire contract");

    assert_eq!(request.id(), &RequestId::Integer(7));
    assert_eq!(request.method(), "initialize");
    let encoded = serde_json::to_value(request).expect("serialize initialize request");
    assert_eq!(encoded["params"]["capabilities"]["experimentalApi"], true);
    assert_eq!(encoded["params"]["clientInfo"]["name"], "codex-tui");
}

#[test]
fn jsonrpc_trace_context_stays_optional_and_camel_compatible() {
    let message: JSONRPCMessage = serde_json::from_value(json!({
        "id": "req-1",
        "method": "thread/list",
        "params": { "limit": 20 },
        "trace": {
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        }
    }))
    .expect("request envelope should deserialize");

    assert_eq!(
        serde_json::to_value(message).expect("serialize request envelope"),
        json!({
            "id": "req-1",
            "method": "thread/list",
            "params": { "limit": 20 },
            "trace": {
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            }
        })
    );
}

#[test]
fn projected_turn_items_keep_external_tagging_and_path_shape() {
    let path = AbsolutePathBuf::try_from(PathBuf::from("/workspace/src/lib.rs"))
        .expect("fixture path is absolute");
    let action = CommandAction::Read {
        command: "read_file /workspace/src/lib.rs".to_string(),
        name: "lib.rs".to_string(),
        path,
    };
    assert_eq!(
        serde_json::to_value(action).expect("serialize command action"),
        json!({
            "type": "read",
            "command": "read_file /workspace/src/lib.rs",
            "name": "lib.rs",
            "path": "/workspace/src/lib.rs"
        })
    );

    let input = UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    };
    assert_eq!(
        serde_json::to_value(input).expect("serialize user input"),
        json!({ "type": "text", "text": "hello", "text_elements": [] })
    );
}

#[test]
fn localized_leaf_enums_keep_pinned_wire_spelling() {
    assert_eq!(
        serde_json::to_value(MessagePhase::FinalAnswer).unwrap(),
        "final_answer"
    );
    assert_eq!(
        serde_json::to_value(SandboxMode::WorkspaceWrite).unwrap(),
        "workspace-write"
    );
}

#[test]
fn absolute_path_rejects_relative_values_at_construction() {
    assert!(AbsolutePathBuf::try_from(PathBuf::from("relative/path")).is_err());
}
