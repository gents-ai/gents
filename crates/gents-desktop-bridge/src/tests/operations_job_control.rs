use std::sync::Arc;

use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use gents_desktop_core::error::BridgeErrorCode;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobState};
use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadRef, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
    TranscriptMessage,
};
use gents_protocol::process_control::{
    ToolControlIntent, ToolControlReceipt, ToolControlReceiptOutcome,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

use crate::job_control::job_control;
use crate::tests::support::OPERATOR;
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

/// A runtime node served over HTTP that the desktop client controls through
/// its client-signed GraphQL path, with a selected Agent key that answers
/// control intents in place of the runtime's control owner.
pub(super) struct ControlRuntime {
    pub core: Arc<ClientCore>,
    pub runtime: Arc<gents::defra_node::Node>,
    pub node_did: String,
    pub client_did: String,
    owner: gents::KeyIdentity,
    pub agent: Arc<dyn gents::NodeIdentity>,
    _directory: tempfile::TempDir,
}

pub(super) async fn control_runtime() -> ControlRuntime {
    use gents::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            gents_desktop_core::client::DesktopPaths::from_root(directory.path().join("desktop")),
            gents_desktop_core::client::ClientCoreOptions::local_only(),
        )
        .await
        .unwrap(),
    );
    let owner =
        gents::KeyIdentity::load_or_create(directory.path().join("owner.key"), None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let graphql = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let runtime = Arc::new(
        gents::defra_node::Node::builder()
            .enable_signing()
            .with_http(
                gents::defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap(),
            )
            .with_node_identity_did(owner.did())
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&runtime).await.unwrap();
    let binding = gents::store::identity::provision_agent_identity(&owner).unwrap();
    let agent = gents::store::identity::resolve_agent_identity(&owner, &binding).unwrap();
    let home = directory.path().join("runtime");
    std::fs::create_dir(&home).unwrap();
    core.add_managed_enrollment_peer_for_test(owner.did(), &graphql, home.to_str().unwrap(), 1)
        .await
        .unwrap();
    let client_did = core.node_identity().did().to_owned();
    assert_ne!(client_did, owner.did());
    ControlRuntime {
        core,
        runtime,
        node_did: owner.did().to_owned(),
        client_did,
        owner,
        agent,
        _directory: directory,
    }
}

impl ControlRuntime {
    /// Seed one invocation owned by the runtime Node and selected for the
    /// Agent; only a granted client may read it.
    pub async fn invocation(&self, call: &str, readable: bool) -> String {
        let owner = ConfigAccess::local_as(self.runtime.clone(), self.node_did.parse().unwrap());
        let mutation = format!(
            r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "session:{call}", node_did: "{node}", agent_did: "{agent}", requester_did: "{client}", session_id: "session", session_doc_id: "session-doc", request_id: "request", request_doc_id: "request-doc", tool_name: "workflow", tool_call_id: "{call}", message_sequence: 1, status: "running", lifecycle_state: "running", await_mode: "background" }}) {{ _docID }} }}"#,
            call = escape_graphql_string(call),
            node = escape_graphql_string(&self.node_did),
            agent = escape_graphql_string(self.agent.did()),
            client = escape_graphql_string(&self.client_did),
        );
        let response = owner
            .write("test.control.invocation", &mutation)
            .await
            .unwrap();
        let doc =
            gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentToolCall").unwrap();
        if readable {
            owner
                .add_actor_relationship(
                    "AgentToolCall",
                    &doc,
                    "reader",
                    &self.client_did.parse().unwrap(),
                )
                .await
                .unwrap();
        }
        doc
    }

    /// Answers the first control intent the Agent can see for `tool_call_doc_id`
    /// with a receipt it signs; `tamper` alters the outcome after signing.
    /// Returns the signed intent the client published.
    pub fn answer(
        &self,
        tool_call_doc_id: &str,
        outcome: ToolControlReceiptOutcome,
        tamper: Option<ToolControlReceiptOutcome>,
    ) -> tokio::task::JoinHandle<ToolControlIntent> {
        let runtime = self.runtime.clone();
        let agent = self.agent.clone();
        let tool_call_doc_id = tool_call_doc_id.to_owned();
        tokio::spawn(async move {
            let access = ConfigAccess::local_managed_agent(
                runtime.clone(),
                agent.did().parse().unwrap(),
                agent.as_ref(),
            )
            .unwrap();
            let mut changes = gents::subscribe_collections(&runtime, ["AgentToolControl"]).unwrap();
            // The working_agent grant follows the intent's creation, so a
            // creation event can precede the Agent's visibility.
            let mut sweep = tokio::time::interval(std::time::Duration::from_millis(50));
            let query = format!(
                r#"{{ AgentToolControl(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID intent_json }} }}"#,
                escape_graphql_string(&tool_call_doc_id)
            );
            loop {
                let rows = access.execute(&query).await.unwrap();
                if let Some(row) = rows["data"]["AgentToolControl"]
                    .as_array()
                    .and_then(|rows| rows.first())
                {
                    let intent: ToolControlIntent =
                        serde_json::from_str(row["intent_json"].as_str().unwrap()).unwrap();
                    let mut receipt = ToolControlReceipt {
                        version: 1,
                        intent_doc_id: row["_docID"].as_str().unwrap().into(),
                        client_did: intent.client_did.clone(),
                        agent_did: intent.agent_did.clone(),
                        node_did: intent.node_did.clone(),
                        session_id: intent.session_id.clone(),
                        session_doc_id: intent.session_doc_id.clone(),
                        request_doc_id: intent.request_doc_id.clone(),
                        tool_call_id: intent.tool_call_id.clone(),
                        tool_call_doc_id: intent.tool_call_doc_id.clone(),
                        action: intent.action.clone(),
                        outcome: outcome.clone(),
                        signature: Vec::new(),
                    };
                    receipt.signature = agent
                        .sign(&receipt.signing_payload().unwrap())
                        .await
                        .unwrap();
                    if let Some(tampered) = tamper.clone() {
                        receipt.outcome = tampered;
                    }
                    let update = format!(
                        r#"mutation {{ update_AgentToolControl(docID: "{}", input: {{ state: "completed", outcome_json: "{}" }}) {{ _docID }} }}"#,
                        escape_graphql_string(&receipt.intent_doc_id),
                        escape_graphql_string(&serde_json::to_string(&receipt).unwrap()),
                    );
                    let updated = access.write("test.control.receipt", &update).await.unwrap();
                    assert_eq!(
                        updated["data"]["update_AgentToolControl"][0]["_docID"],
                        receipt.intent_doc_id.as_str()
                    );
                    return intent;
                }
                tokio::select! {
                    changed = changes.recv() => assert!(changed.is_some()),
                    _ = sweep.tick() => {}
                }
            }
        })
    }

    pub async fn shutdown(self) {
        self.core.shutdown().await.unwrap();
        drop(self.owner);
    }
}

/// The signed intent names the client, the invocation's physical row and its
/// logical scope, as the runtime's claim requires.
pub(super) async fn assert_client_signed_intent(
    control: &ControlRuntime,
    intent: &ToolControlIntent,
    tool_call_doc_id: &str,
    call: &str,
    action: &str,
) {
    use gents::NodeIdentity as _;

    intent.validate_shape().unwrap();
    assert_eq!(intent.client_did, control.client_did);
    assert_eq!(intent.node_did, control.node_did);
    assert_eq!(intent.agent_did, control.agent.did());
    assert_eq!(intent.session_id, "session");
    assert_eq!(intent.session_doc_id, "session-doc");
    assert_eq!(intent.request_doc_id, "request-doc");
    assert_eq!(
        intent.requester_did.as_deref(),
        Some(control.client_did.as_str())
    );
    assert_eq!(intent.tool_call_id, call);
    assert_eq!(intent.tool_call_doc_id, tool_call_doc_id);
    assert_eq!(intent.action, action);
    assert!(control
        .core
        .node_identity()
        .verify(
            &control.client_did,
            &intent.signing_payload().unwrap(),
            &intent.signature,
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn job_control_publishes_a_client_signed_intent_and_requires_the_agent_receipt() {
    let control = control_runtime().await;
    let expected = JobControlOutcome::Applied {
        state: JobState::Cancelled,
        killed_children: 1,
    };
    let command = |doc: &str| DesktopJobControlRequest {
        node_did: control.node_did.clone(),
        tool_call_doc_id: doc.into(),
        action: JobControlKind::Stop,
    };

    let applied = control.invocation("applied", true).await;
    let answered = control.answer(
        &applied,
        ToolControlReceiptOutcome::Job(expected.clone()),
        None,
    );
    assert_eq!(
        job_control(&control.core, &command(&applied))
            .await
            .unwrap(),
        expected
    );
    let intent = answered.await.unwrap();
    assert_client_signed_intent(&control, &intent, &applied, "applied", "job_stop").await;

    let forged = control.invocation("forged", true).await;
    let answered = control.answer(
        &forged,
        ToolControlReceiptOutcome::Job(expected.clone()),
        Some(ToolControlReceiptOutcome::Job(
            JobControlOutcome::AlreadyTerminal {
                state: JobState::Cancelled,
            },
        )),
    );
    let error = job_control(&control.core, &command(&forged))
        .await
        .unwrap_err();
    let intent = answered.await.unwrap();
    assert_eq!(error.code, BridgeErrorCode::Unknown);
    assert!(
        error.message.contains("the action may still take effect"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains(&intent.intent_id),
        "{}",
        error.message
    );

    let unreadable = control.invocation("unreadable", false).await;
    for doc in [unreadable.as_str(), "bae-missing"] {
        assert_eq!(
            job_control(&control.core, &command(doc))
                .await
                .unwrap_err()
                .code,
            BridgeErrorCode::NotFound
        );
    }
    let mut other = command(&applied);
    other.node_did = "did:test:unknown-peer".into();
    assert!(job_control(&control.core, &other).await.is_err());
    control.shutdown().await;
}
