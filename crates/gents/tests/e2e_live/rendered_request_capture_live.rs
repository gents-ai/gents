use std::time::Duration;

use gents_documents::{FileToolMode, FileTools, HostTools, Tools};
use gents_inference::rendered_request::{decode_capture_json, CapturePayloadKind};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_tools::surface::ToolCeiling;
use serde_json::Value;

use crate::support::agent_requests::submit_user_request;
use crate::support::fixtures::configure_agent_tools_as;
use crate::support::live_inference::{
    bind_target, boot_live_agent_with_ceiling, live_target, wait_for_request_terminal,
};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_tool_turn_captures_are_attributed_and_reconstructable() {
    let target = live_target().await;
    let db = test_db("live-tool-turn-captures").await;
    let actors = bind_target(&db, &target).await;
    let access = &actors.fixture.access;
    let workspace = tempfile::tempdir().unwrap();
    let marker = format!("capture-file-{}", uuid::Uuid::new_v4());
    tokio::fs::write(workspace.path().join("capture.txt"), &marker)
        .await
        .unwrap();
    configure_agent_tools_as(
        access,
        actors.node_did(),
        actors.agent_did(),
        Some("Follow the requested tool call, then acknowledge completion briefly.".into()),
        Tools {
            tools_id: "live-capture-tools".into(),
            node_did: actors.node_did().into(),
            host: Some(HostTools {
                root: Some(workspace.path().display().to_string()),
                files: Some(FileTools {
                    mode: FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let runtime = boot_live_agent_with_ceiling(
        &db,
        db.node_identity.clone(),
        ToolCeiling::readonly_at(workspace.path()),
    )
    .await
    .unwrap();
    const REQUEST: &str = "live-capture-request";
    const SESSION: &str = "live-capture-session";
    let request_doc = submit_user_request(
        actors.node_did(),
        &actors.fixture,
        SESSION,
        REQUEST,
        "Call read_file with path capture.txt, then acknowledge that you read it. Do not guess the file contents.",
        Default::default(),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(access, REQUEST, Duration::from_secs(120)).await,
        "completed"
    );
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ request_id: {{ _eq: "{request}" }}, tool_name: {{ _eq: "read_file" }}, lifecycle_state: {{ _eq: "completed" }} }}) {{ _docID }}
            RenderedRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}) {{
                request_doc_id request_commit_cid request_id session_id node_did requester_did agent_did
                capture_version request_json provenance_json
            }}
            InferenceCall(filter: {{ request_id: {{ _eq: "{request}" }} }}) {{ call_id call_kind request_doc_id }}
            _commits(docID: "{doc}", filter: {{ fieldName: {{ _eq: "_C" }} }}) {{ cid }}
        }}"#,
        request = escape_graphql_string(REQUEST),
        doc = escape_graphql_string(&request_doc),
    );
    let observed = access.execute(&query).await.unwrap();
    let calls = observed["data"]["AgentToolCall"].as_array().unwrap();
    assert!(
        !calls.is_empty(),
        "the model must complete the requested file read"
    );
    let mut read_marker = false;
    for call in calls {
        let result = gents_session::tool_call::query::result::load_tool_call_result(
            access,
            call["_docID"].as_str().unwrap(),
            actors.node_did(),
            SESSION,
            Some(actors.fixture.user.did()),
        )
        .await
        .unwrap();
        read_marker |= gents_session::tool_call::query::result::render_tool_result(&result)
            .unwrap()
            .contains(&marker);
    }
    assert!(
        read_marker,
        "the retained tool result must contain the file bytes"
    );
    let rows = observed["data"]["RenderedRequest"].as_array().unwrap();
    let inference_calls = observed["data"]["InferenceCall"].as_array().unwrap();
    let commits = observed["data"]["_commits"].as_array().unwrap();
    assert!(
        !rows.is_empty(),
        "the completed request must retain its provider inputs"
    );
    let mut before_tool_result = false;
    let mut after_tool_result = false;
    for row in rows {
        assert_eq!(row["request_doc_id"], request_doc);
        assert_eq!(row["request_id"], REQUEST);
        assert_eq!(row["session_id"], SESSION);
        assert_eq!(row["node_did"], actors.node_did());
        assert_eq!(row["agent_did"], actors.agent_did());
        assert_eq!(row["requester_did"], actors.fixture.user.did());
        assert!(commits
            .iter()
            .any(|commit| commit["cid"] == row["request_commit_cid"]));
        let provenance: Value =
            serde_json::from_str(row["provenance_json"].as_str().unwrap()).unwrap();
        let call_id = provenance["admission"]["call_id"].as_str().unwrap();
        assert!(inference_calls.iter().any(|call| {
            call["call_id"] == call_id
                && call["call_kind"] == "inference"
                && call["request_doc_id"] == request_doc
        }));
        let body = decode_capture_json(
            access,
            u32::try_from(row["capture_version"].as_u64().unwrap()).unwrap(),
            row["request_json"].as_str().unwrap(),
            CapturePayloadKind::RequestBody,
        )
        .await
        .unwrap();
        let text = serde_json::to_string(&body).unwrap();
        before_tool_result |= !text.contains(&marker);
        after_tool_result |= text.contains(&marker);
    }
    assert!(
        before_tool_result,
        "retain the input before the file result existed"
    );
    assert!(
        after_tool_result,
        "reconstruct the later input containing the real tool result"
    );
    runtime.shutdown().await;
}
