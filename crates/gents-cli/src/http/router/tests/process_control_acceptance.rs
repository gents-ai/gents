use std::sync::Arc;
use std::time::Duration;

use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents::NodeIdentity as _;
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, ToolControlIntent,
    ToolControlReceiptOutcome, PROCESS_CONTROL_PATH,
};
use serde_json::Value;
use tokio_util::task::AbortOnDropHandle;

use crate::commands::test_shim_fixtures::{
    configure_runtime_agent,
    streaming_backend::{MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse},
};
use axum::{http::StatusCode, routing::post, Router};

#[cfg(unix)]
#[tokio::test]
async fn authenticated_process_control_stops_only_the_selected_native_worker() {
    let dir = tempfile::tempdir().unwrap();
    let owner =
        Arc::new(gents::KeyIdentity::load_or_create(dir.path().join("owner.key"), None).unwrap());
    let user = gents::KeyIdentity::load_or_create(dir.path().join("user.key"), None).unwrap();
    let foreign = gents::KeyIdentity::load_or_create(dir.path().join("foreign.key"), None).unwrap();
    let did = owner.did().to_owned();
    let binding = gents::store::identity::provision_agent_identity(owner.as_ref()).unwrap();
    let agent_did = binding.agent_did.clone();
    let node = Arc::new(
        gents::defra_node::Node::builder()
            .data_path(dir.path().join("node"))
            .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
            .with_node_identity_did(&did)
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let admin = ConfigAccess::local_as(node.clone(), did.parse().unwrap());
    for actor in [user.did(), foreign.did(), agent_did.as_str()] {
        for permission in [
            gents::store::config_client::NodePermission::DocumentRead,
            gents::store::config_client::NodePermission::DocumentUpdate,
            gents::store::config_client::NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let user_access = ConfigAccess::local_as(node.clone(), user.did().parse().unwrap());
    let (node_doc, agent_doc) = user_access.transact_write("test.process_control.identities", |txn| {
        Box::pin(async {
            let node_row = txn.execute_with_variables(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": did}}),
            ).await?;
            let agent_row = txn.execute_with_variables(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": did, "agent_did": agent_did,
                    "creation_nonce": binding.creation_nonce, "inference_profile_id": "fixture", "enabled": true}}),
            ).await?;
            Ok::<_, anyhow::Error>((
                gents::store::graphql::created_doc_id(&node_row, "Node")?,
                gents::store::graphql::created_doc_id(&agent_row, "Agent")?,
            ))
        })
    }).await.unwrap();
    user_access
        .add_actor_relationship("Node", &node_doc, "runtime_reader", &did.parse().unwrap())
        .await
        .unwrap();
    user_access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    user_access
        .add_actor_relationship("Agent", &agent_doc, "executor", &agent_did.parse().unwrap())
        .await
        .unwrap();
    let prompt = "start two background processes";
    let args = r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#;
    let backend = MockStreamingBackend::start_with_plans(
        "control-model",
        vec![StreamPlan::current_authored_user(
            prompt,
            vec![
                StreamResponse::streams(
                    prompt,
                    vec![
                        StreamChunk::tool_call("spawn-one", "spawn_process", args),
                        StreamChunk::tool_call("spawn-two", "spawn_process", args),
                    ],
                ),
                StreamResponse::completes(prompt, ["started"]),
            ],
        )],
    )
    .unwrap();
    configure_runtime_agent(
        &node,
        &did,
        &binding,
        "process-control-backend",
        backend.endpoint(),
        "control-model",
        false,
    )
    .await;
    let identity: Arc<dyn gents::NodeIdentity> = owner.clone();
    let records = dir.path().join("process-records");
    let runtime = gents::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_background_process_records(records.clone());
    let mut state = super::state();
    state.node_did = did.clone();
    assert!(state.activation_runtime.set(runtime.clone()).is_ok());
    let app = Router::new()
        .route(
            PROCESS_CONTROL_PATH,
            post(crate::http::process_control::process_control_handler),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(runtime.run(shutdown_rx));
    gents::eval::runner::embedded::wait_for_runtime_ready(&node, &did)
        .await
        .unwrap();

    let session = "process-control-session";
    let request = gents::request::lifecycle::build_signed_client_request(
        &user_access,
        gents::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            gents::RequestIdentity {
                request_id: "process-control-request".into(),
                node_did: did.clone(),
                requester_did: Some(user.did().into()),
                agent_did: agent_did.clone(),
                session_id: session.into(),
                content: prompt.into(),
                execution_origin: gents::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        gents::RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    gents::request::lifecycle::submit_client_request(&user_access, &request)
        .await
        .unwrap();

    let workers = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response = ConfigAccess::local(node.clone()).execute(&format!(r#"{{AgentToolCall(filter: {{session_id: {{_eq: "{session}"}}, tool_name: {{_eq: "bash"}}, lifecycle_state: {{_eq: "running"}}}}) {{_docID tool_call_id await_mode}}}}"#)).await.unwrap();
            let rows = response["data"]["AgentToolCall"].as_array().unwrap();
            if rows.len() == 2 {
                let mut found = Vec::new();
                for row in rows {
                    assert_eq!(row["await_mode"], "background");
                    let doc = row["_docID"].as_str().unwrap();
                    let name = doc.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
                    if let Ok(bytes) = tokio::fs::read(records.join(format!("{name}.json"))).await {
                        let record: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(record["tool_call_doc_id"], doc);
                        assert_eq!(record["tool_call_id"], row["tool_call_id"]);
                        found.push((row.clone(), i32::try_from(record["identity"]["pid"].as_i64().unwrap()).unwrap()));
                    }
                }
                if found.len() == 2 { break found; }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("both native workers must start and record ownership");
    let command = |row: &Value| OperatorProcessControlRequest {
        node_did: did.clone(),
        session_id: session.into(),
        tool_call_id: row["tool_call_id"].as_str().unwrap().into(),
        tool_call_doc_id: row["_docID"].as_str().unwrap().into(),
    };
    let target_call = command(&workers[0].0);
    let sibling_call = command(&workers[1].0);
    let target = gents::tools::call::control::client::prepare(
        &user_access,
        &user,
        &did,
        &target_call.tool_call_doc_id,
        Some((&target_call.session_id, &target_call.tool_call_id)),
        "process_stop",
    )
    .await
    .unwrap();
    let sibling = gents::tools::call::control::client::prepare(
        &user_access,
        &user,
        &did,
        &sibling_call.tool_call_doc_id,
        Some((&sibling_call.session_id, &sibling_call.tool_call_id)),
        "process_stop",
    )
    .await
    .unwrap();
    let url = format!("http://{addr}{PROCESS_CONTROL_PATH}");
    let graphql = format!("http://{addr}/api/v0/graphql");
    let client = reqwest::Client::new();
    let owner_access = GraphqlEndpoint::as_principal(&graphql, user.did())
        .with_client_signing_capability(user.client_signing_capability().unwrap())
        .unwrap();
    let foreign_access = GraphqlEndpoint::as_principal(&graphql, foreign.did())
        .with_client_signing_capability(foreign.client_signing_capability().unwrap())
        .unwrap();
    let post = |endpoint: &GraphqlEndpoint, request: &ToolControlIntent| {
        endpoint.authorize(client.post(&url)).unwrap().json(request)
    };
    let anonymous = client.post(&url).json(&target).send().await.unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let mismatch = post(&foreign_access, &target).send().await.unwrap();
    assert_eq!(mismatch.status(), StatusCode::FORBIDDEN);
    let mut foreign_intent = target.clone();
    foreign_intent.client_did = foreign.did().into();
    foreign_intent.signature = foreign
        .sign(&foreign_intent.signing_payload().unwrap())
        .await
        .unwrap();
    let denied = post(&foreign_access, &foreign_intent).send().await.unwrap();
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    let mut foreign_node = target.clone();
    foreign_node.node_did = foreign.did().to_owned();
    foreign_node.signature = user
        .sign(&foreign_node.signing_payload().unwrap())
        .await
        .unwrap();
    assert_eq!(
        post(&owner_access, &foreign_node)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut wrong = target.clone();
    wrong.tool_call_doc_id = sibling.tool_call_doc_id.clone();
    wrong.signature = user.sign(&wrong.signing_payload().unwrap()).await.unwrap();
    let wrong_response = post(&owner_access, &wrong).send().await.unwrap();
    assert_eq!(wrong_response.status(), StatusCode::NOT_FOUND);
    assert!(process_alive(workers[0].1));
    assert!(process_alive(workers[1].1));
    let mut changes = gents::subscribe_collections(&node, ["AgentToolControl"]).unwrap();
    let response = post(&owner_access, &target).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let ProcessControlOutcome::Queued { intent_doc_id } = response.json().await.unwrap() else {
        panic!("control must return a durable intent");
    };
    let receipt = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(receipt) = gents::tools::call::control::client::read_receipt(
                &user_access,
                &did,
                &intent_doc_id,
                &target.tool_call_doc_id,
                "process_stop",
            )
            .await
            .unwrap()
            {
                break receipt;
            }
            changes.recv().await.expect("control observation closed");
        }
    })
    .await
    .expect("selected agent must confirm control");
    assert_eq!(receipt.agent_did, agent_did);
    assert_eq!(receipt.client_did, user.did());
    assert_eq!(
        receipt.outcome,
        ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true
        })
    );
    assert!(!process_alive(workers[0].1));
    assert!(process_alive(workers[1].1));
    let rows = ConfigAccess::local(node.clone()).execute(&format!(r#"{{AgentToolCall(filter: {{session_id: {{_eq: "{session}"}}, tool_name: {{_eq: "bash"}}}}) {{_docID lifecycle_state}}}}"#)).await.unwrap();
    let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
    assert!(rows.iter().any(
        |row| row["_docID"] == target.tool_call_doc_id && row["lifecycle_state"] == "cancelled"
    ));
    assert!(rows.iter().any(
        |row| row["_docID"] == sibling.tool_call_doc_id && row["lifecycle_state"] == "running"
    ));
    let sibling_response = post(&owner_access, &sibling).send().await.unwrap();
    assert_eq!(sibling_response.status(), StatusCode::ACCEPTED);
    let ProcessControlOutcome::Queued { intent_doc_id } = sibling_response.json().await.unwrap()
    else {
        panic!("sibling control must return a durable intent");
    };
    let receipt = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(receipt) = gents::tools::call::control::client::read_receipt(
                &user_access,
                &did,
                &intent_doc_id,
                &sibling.tool_call_doc_id,
                "process_stop",
            )
            .await
            .unwrap()
            {
                break receipt;
            }
            changes.recv().await.expect("control observation closed");
        }
    })
    .await
    .expect("selected agent must confirm sibling control");
    assert_eq!(
        receipt.outcome,
        ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true
        })
    );
    assert!(!process_alive(workers[1].1));
    shutdown.send(true).unwrap();
    running.await.unwrap().unwrap();
    drop(server);
    node.shutdown().await.expect("node shutdown");
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // Signal zero checks the recorded child without sending it a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}
