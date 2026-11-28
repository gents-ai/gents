use std::sync::Arc;

use crate::client_runtime::ClientRuntime;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobState};
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, ToolControlIntent, ToolControlReceipt,
    ToolControlReceiptOutcome,
};
use gents_store::client_error::BridgeErrorCode;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;

use crate::job_control::job_control;
use crate::types::DesktopJobControlRequest;

/// A runtime node served over HTTP that the desktop client controls through
/// its client-signed GraphQL path, with a selected Agent key that answers
/// control intents in place of the runtime's control owner.
struct ControlRuntime {
    core: Arc<ClientRuntime>,
    runtime: Arc<gents::defra_node::Node>,
    node_did: String,
    client_did: String,
    owner: gents_store::identity::KeyIdentity,
    agent: Arc<dyn gents_store::identity::NodeIdentity>,
    _directory: tempfile::TempDir,
}

async fn control_runtime() -> ControlRuntime {
    use gents_store::identity::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            gents_store::client_journal::paths::DesktopPaths::from_root(
                directory.path().join("desktop"),
            ),
            crate::client_test_support::file_journal_options(),
        )
        .await
        .unwrap(),
    );
    let owner = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("owner.key"),
        None,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let graphql = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let routes = gents_store::test_support::local_authentication_route(Arc::new(
        gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("owner.key"),
            None,
        )
        .unwrap(),
    ));
    let runtime = Arc::new(
        gents::defra_node::Node::builder()
            .enable_signing()
            .with_http(
                gents::defra_node::HttpConfig::with_listener(listener.into_std().unwrap())
                    .unwrap()
                    .with_extra_routes(routes),
            )
            .with_node_identity_did(owner.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&runtime)
        .await
        .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&owner).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&owner, &binding).unwrap();
    let home = directory.path().join("runtime");
    std::fs::create_dir(&home).unwrap();
    core.connect_runtime(gents_protocol::runtime_connection::RuntimeConnection {
        node_did: (owner.did()).to_string(),
        label: ("Served runtime").to_string(),
        graphql: graphql.to_string(),
        local_node_home: Some((home.to_str().unwrap()).to_string()),
        access_request: None,
        access_state: None,
    })
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
            let mut changes = gents_store::subscription_source::subscribe_collections(
                &runtime,
                ["AgentToolControl"],
            )
            .unwrap();
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
async fn assert_client_signed_intent(
    control: &ControlRuntime,
    intent: &ToolControlIntent,
    tool_call_doc_id: &str,
    call: &str,
    action: &str,
) {
    use gents_store::identity::NodeIdentity as _;

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
async fn job_and_process_control_publish_client_signed_intents_and_require_agent_receipts() {
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

    // Process control shares the same signed-intent / verified-receipt channel.
    let process_cmd = |doc: &str, call: &str| OperatorProcessControlRequest {
        node_did: control.node_did.clone(),
        session_id: "session".into(),
        tool_call_id: call.into(),
        tool_call_doc_id: doc.into(),
    };
    for (call, outcome) in [
        (
            "cancelled",
            ProcessControlOutcome::Cancelled {
                live_execution_cancelled: true,
            },
        ),
        ("unverified", ProcessControlOutcome::Unverified),
        ("lost", ProcessControlOutcome::Lost),
    ] {
        let doc = control.invocation(call, true).await;
        let answered = control.answer(
            &doc,
            ToolControlReceiptOutcome::Process(outcome.clone()),
            None,
        );
        assert_eq!(
            crate::process_control::process_control(&control.core, &process_cmd(&doc, call))
                .await
                .unwrap(),
            outcome
        );
        let intent = answered.await.unwrap();
        assert_client_signed_intent(&control, &intent, &doc, call, "process_stop").await;
    }

    let process_forged = control.invocation("process-forged", true).await;
    let answered = control.answer(
        &process_forged,
        ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true,
        }),
        Some(ToolControlReceiptOutcome::Process(
            ProcessControlOutcome::Lost,
        )),
    );
    let error = crate::process_control::process_control(
        &control.core,
        &process_cmd(&process_forged, "process-forged"),
    )
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

    // The displayed logical identity must match the physical row.
    let mismatched = control.invocation("mismatched", true).await;
    let process_unreadable = control.invocation("process-unreadable", false).await;
    for request in [
        process_cmd(&mismatched, "other-call"),
        process_cmd(&process_unreadable, "process-unreadable"),
    ] {
        assert_eq!(
            crate::process_control::process_control(&control.core, &request)
                .await
                .unwrap_err()
                .code,
            BridgeErrorCode::NotFound
        );
    }

    control.shutdown().await;
}
