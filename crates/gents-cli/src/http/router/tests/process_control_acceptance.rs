use std::sync::Arc;
use std::time::Duration;

use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents::tools::call::control::client::{submit, ClientControlError};
use gents::NodeIdentity as _;
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, ToolControlReceiptOutcome,
};
use serde_json::Value;

use crate::commands::test_shim_fixtures::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse,
};

#[cfg(unix)]
#[tokio::test]
async fn authenticated_process_control_stops_only_the_selected_native_worker() {
    let dir = tempfile::tempdir().unwrap();
    // The enrolled User publishes the Node and its managed Agent through its
    // own client-signed endpoint; the runtime starts over the home's fresh
    // backend admission.
    let home = crate::shared::test_support::ClientHome::new().await;
    let owner = home.home.identity.clone();
    let user = &home.user;
    let foreign = gents::KeyIdentity::load_or_create(dir.path().join("foreign.key"), None).unwrap();
    let did = owner.did().to_owned();
    let binding = home.agent("process-control").await;
    let agent_did = binding.agent_did.clone();
    let graphql = home.graphql().to_owned();
    let node = home.node().clone();
    let admin = ConfigAccess::local_as(node.clone(), did.parse().unwrap());
    for actor in [foreign.did(), agent_did.as_str()] {
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
    let user_access = home.access.clone();
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
    let root = std::fs::canonicalize(dir.path()).unwrap();
    configure_background_bash(&home, &binding, &root, backend.endpoint(), "control-model").await;
    home.mark_backend_healthy(&binding).await;
    let identity: Arc<dyn gents::NodeIdentity> = owner.clone();
    let records = dir.path().join("process-records");
    let runtime = gents::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            // Host tools run only under an explicit workspace root.
            tool_ceiling: gents::ToolCeiling::readonly_at(&root),
            backend_admission: Some(home.home.backend_admission()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_background_process_records(records.clone());
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut running = tokio::spawn(runtime.run(shutdown_rx));
    // A runtime that refuses its configuration exits; report that refusal
    // rather than waiting out readiness.
    tokio::select! {
        ready = gents::eval::runner::embedded::wait_for_runtime_ready(&node, &did) => ready.unwrap(),
        exited = &mut running => panic!("runtime exited before readiness: {:?}",
            exited.map(|run| run.map_err(|error| format!("{error:#}")))),
    }

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
        gents::RequestSigner::Identity(user),
    )
    .await
    .unwrap();
    gents::request::lifecycle::submit_client_request(&user_access, &request)
        .await
        .unwrap();

    let workers = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response = user_access.execute(&format!(r#"{{AgentToolCall(filter: {{session_id: {{_eq: "{session}"}}, tool_name: {{_eq: "bash"}}, lifecycle_state: {{_eq: "running"}}}}) {{_docID tool_call_id await_mode}}}}"#)).await.unwrap();
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
        user,
        &did,
        &target_call.tool_call_doc_id,
        Some((&target_call.session_id, &target_call.tool_call_id)),
        "process_stop",
    )
    .await
    .unwrap();
    let sibling = gents::tools::call::control::client::prepare(
        &user_access,
        user,
        &did,
        &sibling_call.tool_call_doc_id,
        Some((&sibling_call.session_id, &sibling_call.tool_call_id)),
        "process_stop",
    )
    .await
    .unwrap();
    // The client publishes each signed intent through its own client-signed
    // endpoint; the runtime only consumes it.
    let owner_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(&graphql, user.did())
            .with_client_signing_capability(user.client_signing_capability().unwrap())
            .unwrap(),
    );
    let foreign_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(&graphql, foreign.did())
            .with_client_signing_capability(foreign.client_signing_capability().unwrap())
            .unwrap(),
    );
    assert!(matches!(
        submit(&ConfigAccess::graphql(&graphql), &target).await,
        Err(ClientControlError::NotAuthorized)
    ));
    assert!(matches!(
        submit(&foreign_access, &target).await,
        Err(ClientControlError::NotAuthorized)
    ));
    let mut foreign_intent = target.clone();
    foreign_intent.client_did = foreign.did().into();
    foreign_intent.signature = foreign
        .sign(&foreign_intent.signing_payload().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        submit(&foreign_access, &foreign_intent).await,
        Err(ClientControlError::NotFound)
    ));
    let mut foreign_node = target.clone();
    foreign_node.node_did = foreign.did().to_owned();
    foreign_node.signature = user
        .sign(&foreign_node.signing_payload().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        submit(&owner_access, &foreign_node).await,
        Err(ClientControlError::NotFound)
    ));
    let mut wrong = target.clone();
    wrong.tool_call_doc_id = sibling.tool_call_doc_id.clone();
    wrong.signature = user.sign(&wrong.signing_payload().unwrap()).await.unwrap();
    assert!(matches!(
        submit(&owner_access, &wrong).await,
        Err(ClientControlError::NotFound)
    ));
    assert!(process_alive(workers[0].1));
    assert!(process_alive(workers[1].1));
    let mut changes = gents::subscribe_collections(&node, ["AgentToolControl"]).unwrap();
    let intent_doc_id = submit(&owner_access, &target)
        .await
        .expect("control must save a durable intent");
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
    let rows = user_access.execute(&format!(r#"{{AgentToolCall(filter: {{session_id: {{_eq: "{session}"}}, tool_name: {{_eq: "bash"}}}}) {{_docID lifecycle_state}}}}"#)).await.unwrap();
    let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
    assert!(rows.iter().any(
        |row| row["_docID"] == target.tool_call_doc_id && row["lifecycle_state"] == "cancelled"
    ));
    assert!(rows.iter().any(
        |row| row["_docID"] == sibling.tool_call_doc_id && row["lifecycle_state"] == "running"
    ));
    let intent_doc_id = submit(&owner_access, &sibling)
        .await
        .expect("sibling control must save a durable intent");
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
    node.shutdown().await.expect("node shutdown");
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // Signal zero checks the recorded child without sending it a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Points the User's Agent at `endpoint` with background bash under `root`, as the
/// User configures its Agent's tools, context, backend and Node default.
async fn configure_background_bash(
    home: &crate::shared::test_support::ClientHome,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    root: &std::path::Path,
    endpoint: &str,
    model: &str,
) {
    use gents::documents::apply_desired_state_plan;
    use gents::documents::{AgentContext, BashTools, HostTools, Tools};
    use gents::store::config_client::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::Collection;

    fn plan(docs: Vec<(Collection, Value)>) -> anyhow::Result<DesiredStateApplyPlan> {
        Ok(DesiredStateApplyPlan::new(
            docs.into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )?)
    }

    let node_did = home.node_did().to_owned();
    let agent_did = binding.agent_did.clone();
    let tools_id = format!("{agent_did}:tools");
    let context_id = format!("{agent_did}:context");
    let tools = Tools {
        tools_id: tools_id.clone(),
        node_did: node_did.clone(),
        host: Some(HostTools {
            root: Some(root.display().to_string()),
            bash: Some(BashTools {
                mode: gents::BashMode::ReadOnly,
                read_only_commands: Some(vec!["sleep".into(), "printf".into()]),
                background_enabled: true,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let context = AgentContext {
        context_id: context_id.clone(),
        node_did: node_did.clone(),
        display_name: None,
        description: None,
        system_prompt: None,
        tools_id: Some(tools_id),
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    let projected = plan(vec![
        (Collection::Tools, serde_json::to_value(tools).unwrap()),
        (Collection::AgentContext, serde_json::to_value(context).unwrap()),
    ])
    .unwrap();
    home.access
        .transact_write("test.process_control.projected", |txn| {
            let projected = &projected;
            Box::pin(async move { apply_desired_state_plan(txn, projected).await.map(|_| ()) })
        })
        .await
        .unwrap();
    // Host tools run only under a User-published root their Agent may read.
    gents::tools::surface::set_workspace_root_with_readers(
        &home.access,
        &node_did,
        &root.display().to_string(),
        None,
        true,
        std::slice::from_ref(&agent_did),
    )
    .await
    .unwrap();
    home.access
        .transact_write("test.process_control.agent", |txn| {
            let (node_did, agent_did, context_id) = (&node_did, &agent_did, &context_id);
            Box::pin(async move {
                let read = |collection, id: String| async move {
                    read_desired_state_record_in_txn(txn, collection, node_did, &id)
                        .await
                        .map(|found| found.expect("published fixture document").1)
                };
                let mut node = read(Collection::Node, node_did.clone()).await?;
                node["default_agent_did"] = agent_did.as_str().into();
                let mut agent = read(Collection::Agent, agent_did.clone()).await?;
                agent["context_id"] = context_id.as_str().into();
                let mut backend =
                    read(Collection::InferenceBackend, format!("{agent_did}:backend")).await?;
                backend["endpoint"] = endpoint.into();
                backend["openai_wire_api"] = "chat_completions".into();
                let mut profile =
                    read(Collection::InferenceProfile, format!("{agent_did}:inference")).await?;
                profile["model_name"] = model.into();
                let plan = plan(vec![
                    (Collection::InferenceBackend, backend),
                    (Collection::InferenceProfile, profile),
                    (Collection::Agent, agent),
                    (Collection::Node, node),
                ])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .unwrap();
}
