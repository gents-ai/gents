use std::sync::Arc;

use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobState};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadRef, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
    TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

use crate::job_control::job_control;
use crate::tests::support::{boot_core, OPERATOR};
use crate::types::DesktopJobControlRequest;

pub(super) async fn seed_admission_for_node(
    core: &Arc<ClientCore>,
    node_did: &str,
    session_id: &str,
    row_doc_id: &str,
    call: &str,
    sequence: u32,
) {
    let access = ConfigAccess::local(core.node_arc());
    let read = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_doc_id }} }}"#,
        escape_graphql_string(row_doc_id)
    );
    let response = access
        .transact_write("test.fixture.job_row_request", |txn| {
            let read = read.clone();
            Box::pin(async move {
                txn.execute_with_variables(&read, &serde_json::json!({}))
                    .await
            })
        })
        .await
        .expect("read job row request");
    let request_doc_id = response
        .pointer("/data/AgentToolCall/0/request_doc_id")
        .and_then(|value| value.as_str())
        .expect("job row request_doc_id")
        .to_owned();
    let deadline = format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ deadline_at: "2126-06-01T10:00:00Z", message_sequence: {sequence} }}) {{ _docID }} }}"#,
        escape_graphql_string(row_doc_id)
    );
    access
        .transact_write("test.fixture.job_row_deadline", |txn| {
            let deadline = deadline.clone();
            Box::pin(async move { txn.execute(&deadline).await })
        })
        .await
        .expect("seed job row deadline");
    let arguments = "{}";
    let generation = format!("cli-job-fixture:{request_doc_id}");
    let created_at = "2026-06-01T10:00:00Z";
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: Some(OPERATOR.into()),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: arguments.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: call.into(),
                    call_id: Some(call.into()),
                    name: "workflow".into(),
                },
            }),
        }],
        payload: arguments.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![arguments.len() as u64],
        }),
        created_at: created_at.into(),
    };
    let variables = output_segment_create_variables(&segment).expect("segment variables");
    let response = access
        .transact_write("test.fixture.job_admission_segment", |txn| {
            let variables = variables.clone();
            Box::pin(async move {
                txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables)
                    .await
            })
        })
        .await
        .expect("seed admission segment");
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentOutputSegment")
            .expect("segment doc id");
    let message = TranscriptMessage {
        native_request_id: None,
        message_key: gents::session::sequence_message_key(
            node_did,
            session_id,
            Some(OPERATOR),
            sequence,
        ),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some(OPERATOR.into()),
        request_doc_id: Some(request_doc_id),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: row_doc_id.into(),
            id: call.into(),
            call_id: Some(call.into()),
            name: "workflow".into(),
            arguments: PayloadRef {
                close_doc_id,
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
        created_at: created_at.into(),
    };
    let variables = transcript_message_create_variables(&message).expect("message variables");
    access
        .transact_write("test.fixture.job_admission_header", |txn| {
            let variables = variables.clone();
            Box::pin(async move {
                txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &variables)
                    .await
            })
        })
        .await
        .expect("seed admission header");
}

#[tokio::test]
async fn job_control_uses_the_client_and_requires_a_matching_agent_receipt() {
    use gents::store::identity::{KeyIdentity, NodeIdentity};
    use gents_protocol::process_control::{
        ToolControlIntent, ToolControlReceipt, ToolControlReceiptOutcome,
    };
    use gents_desktop_core::error::BridgeErrorCode;
    use wiremock::matchers::{
        body_partial_json, body_string_contains, header, header_exists, method, path,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let (core, temp) = boot_core().await;
    let owner = KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap();
    let agent = KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap();
    let home = temp.path().join("runtime");
    std::fs::create_dir(&home).unwrap();
    let graphql = format!("{}/api/v0/graphql", server.uri());
    core.add_managed_enrollment_peer_for_test(owner.did(), &graphql, home.to_str().unwrap(), 1)
        .await
        .unwrap();
    let client_did = core.node_identity().did().to_owned();
    assert_ne!(client_did, owner.did());
    let expected = JobControlOutcome::Applied {
        state: JobState::Cancelled,
        killed_children: 1,
    };
    let mut receipt = ToolControlReceipt {
        version: 1,
        intent_doc_id: "intent-job".into(),
        client_did: client_did.clone(),
        agent_did: agent.did().into(),
        node_did: owner.did().into(),
        session_id: "session".into(),
        session_doc_id: "session-doc".into(),
        request_doc_id: "request-doc".into(),
        tool_call_id: "call".into(),
        tool_call_doc_id: "runtime-only-doc".into(),
        action: "job_stop".into(),
        outcome: ToolControlReceiptOutcome::Job(expected.clone()),
        signature: Vec::new(),
    };
    receipt.signature = agent
        .sign(&receipt.signing_payload().unwrap())
        .await
        .unwrap();
    let invocation = serde_json::json!({
        "_docID": "runtime-only-doc", "node_did": owner.did(), "agent_did": agent.did(),
        "session_id": "session", "session_doc_id": "session-doc",
        "request_doc_id": "request-doc", "tool_call_id": "call"
    });
    let mut intent = invocation.clone();
    intent["_docID"] = "intent-job".into();
    intent["client_did"] = client_did.clone().into();
    intent["tool_call_doc_id"] = "runtime-only-doc".into();
    intent["action"] = "job_stop".into();
    intent["outcome_json"] = serde_json::to_string(&receipt).unwrap().into();
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .and(header("accept", "text/event-stream"))
        .and(body_string_contains("intent-job"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .and(header_exists("authorization"))
        .and(body_string_contains("intent-job"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"AgentToolControl": [intent], "AgentToolCall": [invocation], "AgentToolControlClaim": []}
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    for doc in [
        "runtime-only-doc",
        "missing-doc",
        "foreign-doc",
        "unsigned-doc",
    ] {
        Mock::given(method("POST")).and(path("/api/v0/graphql"))
            .and(body_string_contains(doc))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {"AgentToolCall": [{
                "_docID": doc, "node_did": owner.did(), "agent_did": agent.did(), "session_id": "session",
                "session_doc_id": "session-doc", "request_doc_id": "request-doc", "requester_did": client_did,
                "tool_call_id": "call"
            }]}})))
            .expect(1).mount(&server).await;
    }
    for (doc, response) in [
        (
            "runtime-only-doc",
            ResponseTemplate::new(202).set_body_json(JobControlOutcome::Queued {
                intent_doc_id: "intent-job".into(),
            }),
        ),
        (
            "missing-doc",
            ResponseTemplate::new(404).set_body_json(serde_json::json!({"error":"not found"})),
        ),
        (
            "foreign-doc",
            ResponseTemplate::new(403)
                .set_body_json(serde_json::json!({"error":"permission denied"})),
        ),
        (
            "unsigned-doc",
            ResponseTemplate::new(200).set_body_json(expected.clone()),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path(gents_protocol::job::JOB_CONTROL_PATH))
            .and(header_exists("authorization"))
            .and(body_partial_json(
                serde_json::json!({"tool_call_doc_id": doc}),
            ))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
    }
    let command = |doc: &str| DesktopJobControlRequest {
        node_did: owner.did().into(),
        tool_call_doc_id: doc.into(),
        action: JobControlKind::Stop,
    };
    assert_eq!(
        job_control(&core, &command("runtime-only-doc"))
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        job_control(&core, &command("missing-doc"))
            .await
            .unwrap_err()
            .code,
        BridgeErrorCode::NotFound
    );
    assert_eq!(
        job_control(&core, &command("foreign-doc"))
            .await
            .unwrap_err()
            .code,
        BridgeErrorCode::PermissionDenied
    );
    let unsigned_error = job_control(&core, &command("unsigned-doc"))
        .await
        .unwrap_err()
        .message;
    assert!(unsigned_error.contains("without a queued intent"), "{unsigned_error}");
    assert!(unsigned_error.contains("the action may still take effect"));
    let mut other = command("runtime-only-doc");
    other.node_did = "did:test:unknown-peer".into();
    assert!(job_control(&core, &other).await.is_err());
    for request in server.received_requests().await.unwrap() {
        use identity::Identity as _;
        let header = request
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        let signer =
            identity::from_token(header.strip_prefix("Bearer ").unwrap().as_bytes()).unwrap();
        assert_eq!(signer.did().unwrap().as_str(), client_did);
        if request.url.path() == gents_protocol::job::JOB_CONTROL_PATH {
            let intent: ToolControlIntent = serde_json::from_slice(&request.body).unwrap();
            intent.validate_shape().unwrap();
            assert_eq!(intent.client_did, client_did);
            assert_eq!(intent.node_did, owner.did());
            assert_eq!(intent.agent_did, agent.did());
            assert_eq!(intent.session_id, "session");
            assert_eq!(intent.session_doc_id, "session-doc");
            assert_eq!(intent.request_doc_id, "request-doc");
            assert_eq!(intent.requester_did.as_deref(), Some(client_did.as_str()));
            assert_eq!(intent.tool_call_id, "call");
            assert_eq!(intent.action, "job_stop");
            if intent.tool_call_doc_id == "unsigned-doc" {
                assert!(unsigned_error.contains(&intent.intent_id));
            }
            assert!(core
                .node_identity()
                .verify(
                    &client_did,
                    &intent.signing_payload().unwrap(),
                    &intent.signature,
                )
                .await
                .unwrap());
        }
    }
    core.shutdown().await.unwrap();
}
