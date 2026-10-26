use std::sync::Arc;

use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::{
    JobBudget, JobControlKind, JobControlOutcome, JobProgress, JobReadError, JobState,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadRef, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
    TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

use crate::job_control::job_control;
use crate::tests::support::{boot_core, create_tool_call, seed_job_row, OPERATOR};
use crate::types::DesktopJobControlRequest;

fn request(
    core: &Arc<ClientCore>,
    tool_call_doc_id: &str,
    action: JobControlKind,
) -> DesktopJobControlRequest {
    DesktopJobControlRequest {
        node_did: core.node_arc().node_identity_did().unwrap().into(),
        tool_call_doc_id: tool_call_doc_id.into(),
        action,
    }
}

fn completed_progress() -> JobProgress {
    JobProgress {
        title: "Job".into(),
        state: JobState::Completed,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 2,
        budget: JobBudget {
            total: 4,
            spent: 2,
            remaining: 2,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    }
}

pub(super) async fn seed_admission(
    core: &Arc<ClientCore>,
    session_id: &str,
    row_doc_id: &str,
    call: &str,
    sequence: u32,
) {
    seed_admission_for_node(core, OPERATOR, session_id, row_doc_id, call, sequence).await;
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_control_on_a_missing_row_reports_not_found() {
    let (core, _tmp) = boot_core().await;

    let error = job_control(&core, &request(&core, "missing-doc", JobControlKind::Stop))
        .await
        .expect_err("no row");

    assert_eq!(
        error,
        format!("not found: {}", JobReadError::NotYetObservable)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_control_on_a_plain_tool_row_reports_unknown_run() {
    let (core, _tmp) = boot_core().await;
    let doc = create_tool_call(&core, "tc_plain", "shell").await;
    let did = core.node_arc().node_identity_did().unwrap().to_owned();
    ConfigAccess::local(core.node_arc()).write("test.plain_job_owner", &format!(
        r#"mutation {{ update_AgentToolCall(docID: "{}", input: {{ node_did: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&doc), escape_graphql_string(&did),
    )).await.unwrap();

    let error = job_control(&core, &request(&core, &doc, JobControlKind::Stop))
        .await
        .expect_err("plain row");

    assert_eq!(error, "workflow_control_unknown_run");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_control_on_an_ended_job_row_is_already_terminal() {
    let (core, _tmp) = boot_core().await;
    let did = core.node_arc().node_identity_did().unwrap().to_owned();
    let row = crate::tests::support::seed_job_row_for_node(
        &core,
        &did,
        "sess_job",
        "call-done",
        "completed",
        &completed_progress(),
        None,
    )
    .await;
    seed_admission_for_node(&core, &did, "sess_job", &row, "call-done", 1).await;

    for action in [JobControlKind::Stop, JobControlKind::Pause] {
        let outcome = job_control(&core, &request(&core, &row, action))
            .await
            .expect("ended row answers");
        assert_eq!(
            outcome,
            JobControlOutcome::AlreadyTerminal {
                state: JobState::Completed
            }
        );
    }
}

#[tokio::test]
async fn job_control_routes_to_the_owner_without_a_replica_row() {
    use wiremock::matchers::{body_json, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let expected = JobControlOutcome::Applied {
        state: JobState::Cancelled,
        killed_children: 1,
    };
    Mock::given(method("POST")).and(path(gents_protocol::job::JOB_CONTROL_PATH))
        .and(header_exists("authorization"))
        .and(body_json(serde_json::json!({"nodeDid": OPERATOR, "toolCallDocId": "runtime-only-doc", "action": "stop"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::to_value(&expected).unwrap()))
        .expect(1).mount(&server).await;
    let (core, temp) = boot_core().await;
    core.add_managed_enrollment_peer_for_test(
        OPERATOR,
        &format!("{}/api/v0/graphql", server.uri()),
        temp.path().join("runtime").to_str().unwrap(),
        1,
    )
    .await
    .unwrap();
    let mut command = request(&core, "runtime-only-doc", JobControlKind::Stop);
    command.node_did = OPERATOR.into();
    let actual = job_control(&core, &command).await.unwrap();
    assert_eq!(actual, expected);
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn job_control_refuses_a_physical_row_owned_by_another_node() {
    let (core, _temp) = boot_core().await;
    let row = seed_job_row(
        &core,
        "same-session",
        "same-call",
        "completed",
        &completed_progress(),
        None,
    )
    .await;
    seed_admission(&core, "same-session", &row, "same-call", 1).await;
    let mut command = request(&core, &row, JobControlKind::Stop);
    command.node_did = "did:test:other-runtime".into();
    assert_eq!(
        job_control(&core, &command).await.unwrap_err(),
        "workflow_control_unknown_run"
    );
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_peer_replica_cannot_be_used_as_the_executing_runtime() {
    let (core, _temp) = boot_core().await;
    let peer = "did:test:unknown-peer";
    let row = crate::tests::support::seed_job_row_for_node(
        &core,
        peer,
        "replicated-session",
        "call",
        "running",
        &completed_progress(),
        None,
    )
    .await;
    let mut command = request(&core, &row, JobControlKind::Stop);
    command.node_did = peer.into();
    assert_eq!(
        job_control(&core, &command).await.unwrap_err(),
        "workflow_control_unknown_run"
    );
    let observed = ConfigAccess::local(core.node_arc())
        .execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state }} }}"#,
            escape_graphql_string(&row),
        ))
        .await
        .unwrap();
    assert_eq!(
        observed["data"]["AgentToolCall"][0]["lifecycle_state"],
        "running"
    );
    core.shutdown().await.unwrap();
}
