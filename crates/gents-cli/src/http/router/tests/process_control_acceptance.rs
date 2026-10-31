use std::sync::Arc;
use std::time::Duration;

use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents::NodeIdentity as _;
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, PROCESS_CONTROL_PATH,
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
    let foreign = gents::KeyIdentity::load_or_create(dir.path().join("foreign.key"), None).unwrap();
    let did = owner.did().to_owned();
    let agent_id = gents::default_agent_id_for_node(&did);
    let node = Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .data_path(dir.path().join("node"))
            .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
            .with_node_identity_did(&did)
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
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
        &agent_id,
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

    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let escaped_did = gents::store::graphql::escape_graphql_string(&did);
    let escaped_agent = gents::store::graphql::escape_graphql_string(&agent_id);
    let session = "process-control-session";
    let mutation = format!(
        r#"mutation {{create_AgentSession(input: {{session_id: "{session}", node_did: "{escaped_did}", requester_did: "{escaped_did}", agent_id: "{escaped_agent}", created_at: "{created_at}", title: {{text: "fixture", source: "generated"}}}}) {{_docID}}}}"#
    );
    ConfigAccess::local(node.clone())
        .write("test.process_control.session", &mutation)
        .await
        .unwrap();
    let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "process-control-request",
        &did,
        &did,
        &agent_id,
        session,
        prompt,
        "interactive",
        created_at,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did),
    );
    gents::sign_agent_request_create(owner.as_ref(), &mut request)
        .await
        .unwrap();
    ConfigAccess::local(node.clone())
        .write(
            "test.process_control.request",
            &request.graphql_mutation().unwrap(),
        )
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
    let target = command(&workers[0].0);
    let sibling = command(&workers[1].0);
    let url = format!("http://{addr}{PROCESS_CONTROL_PATH}");
    let graphql = format!("http://{addr}/api/v0/graphql");
    let client = reqwest::Client::new();
    let owner_access = GraphqlEndpoint::as_principal(&graphql, &did);
    let foreign_access = GraphqlEndpoint::as_principal(&graphql, foreign.did());
    let post = |endpoint: &GraphqlEndpoint, request: &OperatorProcessControlRequest| {
        endpoint.authorize(client.post(&url)).unwrap().json(request)
    };
    let anonymous = client.post(&url).json(&target).send().await.unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let denied = post(&foreign_access, &target).send().await.unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let mut foreign_node = target.clone();
    foreign_node.node_did = foreign.did().to_owned();
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
    let wrong_response = post(&owner_access, &wrong).send().await.unwrap();
    assert_eq!(wrong_response.status(), StatusCode::NOT_FOUND);
    assert!(process_alive(workers[0].1));
    assert!(process_alive(workers[1].1));
    let response = post(&owner_access, &target).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<ProcessControlOutcome>().await.unwrap(),
        ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true
        }
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
    assert_eq!(
        post(&owner_access, &sibling).send().await.unwrap().status(),
        StatusCode::OK
    );
    shutdown.send(true).unwrap();
    running.await.unwrap().unwrap();
    drop(server);
    node.shutdown().await;
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // Signal zero checks the recorded child without sending it a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}
