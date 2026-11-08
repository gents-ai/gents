use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents_desktop_core::client::{
    load_session_transcript_page, load_session_transcript_page_on, ClientCore, ClientCoreOptions,
    ClientStore, ClientStoreRows, DesktopPaths,
};
use gents_desktop_core::test_fixtures::ServedRuntimeHome;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use tokio::time::{sleep, timeout};

#[test]
fn focused_request_id_defaults_to_none() {
    let (store, _) = gents_desktop_core::client::ObservedStore::new(ClientStore::default());
    assert!(store.focused_request_id().is_none());
}

#[test]
fn request_lifecycle_is_the_only_turn_state_owner() {
    let store = ClientStore::from_rows(ClientStoreRows {
        requests: vec![AgentRequestRow {
            doc_id: Some("request-doc".into()),
            request_id: "request".into(),
            node_did: Some("did:test:amy".into()),
            session_id: Some("session".into()),
            lifecycle_state: Some(RequestLifecycleState::Completed),
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            ..Default::default()
        }],
        ..Default::default()
    });
    assert_eq!(
        store.requests[0].lifecycle_state,
        Some(RequestLifecycleState::Completed)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_request_refresh_preserves_projection_boundaries_and_database_truth() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let created = core
        .node()
        .execute(
            r#"mutation {
        session: create_AgentSession(input: {
            session_id: "session-1", node_did: "did:test:amy", requester_did: null,
            agent_did: "default", created_at: "2026-08-21T18:07:35Z"
        }) { _docID }
        request: create_AgentRequest(input: {
            purpose: "normal", request_id: "request-1", node_did: "did:test:amy", agent_did: "default",
            session_id: "session-1", content: "run it", lifecycle_state: "processing",
            created_at: "2026-08-21T18:07:35Z"
        }) { _docID }
    }"#,
        )
        .await;
    assert!(!created.has_errors(), "seed request: {:?}", created.errors);
    let request_doc_id = created
        .data
        .as_ref()
        .and_then(|value| value.get("request"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|value| value.as_str())
        .context("created request doc id")?;
    let request_doc_id = gents::store::graphql::escape_graphql_string(request_doc_id);
    let output = core.node().execute(&format!(r#"mutation {{
        segment: create_AgentOutputSegment(input: {{
            node_did: "did:test:amy", session_id: "session-1",
            request_doc_id: "{request_doc_id}",
            source: {{kind: "authored", key: "answer"}},
            ordinal: 0,
            writer: {{kind: "request_execution", execution_generation: "generation"}},
            runs: [{{stream: 0, bytes: 8, declaration: {{block_index: 0, part_index: 0, payload: {{kind: "text"}}}}}}],
            payload: "complete",
            close: {{kind: "closed", outcome: "complete", segments: 1, stream_bytes: [8]}},
            created_at: "2026-08-21T18:07:36Z"
        }}) {{ _docID }}
    }}"#)).await;
    assert!(!output.has_errors(), "seed output: {:?}", output.errors);
    let close_doc_id = output
        .data
        .as_ref()
        .and_then(|v| v.get("segment"))
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
        .and_then(|v| v.get("_docID"))
        .and_then(|v| v.as_str())
        .context("created close doc id")?;
    let close_doc_id = gents::store::graphql::escape_graphql_string(close_doc_id);
    let header = core.node().execute(&format!(r#"mutation {{
        create_AgentMessage(input: {{
            message_key: "session-1:2", session_id: "session-1", node_did: "did:test:amy",
            request_doc_id: "{request_doc_id}",
            publication: {{kind: "request_execution", execution_generation: "generation"}},
            outcome: "complete", sequence: 2, role: "assistant", native_id: "assistant-2",
            blocks: [{{type: "text", text: {{output: {{close_doc_id: "{close_doc_id}", stream: 0}}, presentation: {{kind: "full"}}}}}}],
            created_at: "2026-08-21T18:07:36Z"
        }}) {{ _docID }}
    }}"#)).await;
    assert!(!header.has_errors(), "seed header: {:?}", header.errors);

    core.refresh_store().await?;
    assert!(core.store().snapshot().transcript_messages.is_empty());
    assert!(core.store().snapshot().output_segments.is_empty());
    core.refresh_local_request("did:test:amy", "session-1", Some("request-1"), None, &[])
        .await?;
    let refreshed = core.store().snapshot();
    assert!(refreshed.transcript_messages.is_empty());
    assert!(refreshed.output_segments.is_empty());
    assert!(refreshed
        .requests
        .iter()
        .any(|row| row.request_id == "request-1"));
    let page = load_session_transcript_page(
        core.node(),
        "session-1",
        Some("did:test:amy"),
        None,
        None,
        Some(40),
    )
    .await?;
    assert_eq!(page.store.transcript_messages.len(), 1);
    assert_eq!(page.store.output_segments.len(), 1);
    Arc::new(core).shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_operator_request_refreshes_non_replicated_agent_config() -> Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const NODE_DID: &str = "did:test:hosted-amy";
    const REQUEST_ID: &str = "request-terminal-config";
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let response = core
        .node()
        .execute(
            r#"mutation {
                create_Node(input: {
                    node_did: "did:test:hosted-amy"
                    display_name: "Amy"
                    default_agent_did: "did:test:hosted-amy:default"
                    enabled: true
                }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "seed local node: {:?}",
        response.errors
    );
    core.refresh_store().await?;
    assert_eq!(
        core.store()
            .snapshot()
            .nodes
            .iter()
            .find(|node| node.node_did == NODE_DID)
            .and_then(|node| node.default_agent_did.as_deref()),
        Some("did:test:hosted-amy:default")
    );

    let server = MockServer::start().await;
    let completed_request = serde_json::json!({
        "_docID": "remote-terminal-request-doc",
        "request_id": REQUEST_ID,
        "purpose": "normal",
        "node_did": NODE_DID,
        "requester_did": NODE_DID,
        "agent_did": "did:test:hosted-amy:default",
        "session_id": "session-terminal-config",
        "content": "make research the default",
        "lifecycle_state": "completed"
    });
    let mut operator_data = serde_json::json!({
        "nodes": [{
            "node_did": NODE_DID,
            "display_name": "Amy",
            "default_agent_did": "did:test:hosted-amy:research",
            "enabled": true
        }]
    });
    for key in [
        "agents",
        "runtimes",
        "node_readiness",
        "contexts",
        "tools",
        "inference_backends",
        "inference_profiles",
        "inference_sampling",
        "inference_execution",
        "tasks",
        "schedules",
        "triggers",
        "skills",
        "compactions",
        "tool_service_registries",
        "event_sources",
        "agent_targets",
        "datastore_tool_surfaces",
        "sessions",
        "requests",
    ] {
        operator_data[key] = serde_json::json!([]);
    }
    let store = Arc::clone(core.store());
    let response_request = completed_request.clone();
    let response_session = serde_json::json!({
        "session_id": "session-terminal-config",
        "node_did": NODE_DID,
        "requester_did": NODE_DID,
        "agent_did": "did:test:hosted-amy:default",
        "created_at": "2026-07-24T00:00:00Z"
    });
    let response_operator_data = operator_data.clone();
    Mock::given(method("POST"))
        .and(path("/api/v0/tx"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "selected-request-refresh"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx/selected-request-refresh"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .respond_with(move |request: &wiremock::Request| {
            let body = String::from_utf8_lossy(&request.body);
            let data = if body.contains("DesktopSelectedSession") {
                serde_json::json!({"AgentSession": [response_session.clone()]})
            } else if body.contains("DesktopSelectedRequest") {
                serde_json::json!({"AgentRequest": [response_request.clone()]})
            } else if body.contains("DesktopSessionRequestSeeds") {
                // Invalidate the captured read while a later owner query is in
                // flight so the selected patch must be rejected at publication.
                store.invalidate_projection(true);
                serde_json::json!({"seeds_0": [], "newest_0": []})
            } else if body.contains("RequestDependencies") {
                serde_json::json!({"r0": []})
            } else if body.contains("DesktopRequestSessionContext") {
                serde_json::json!({"goals_0": []})
            } else if body.contains("DesktopOperatorConfig") {
                response_operator_data.clone()
            } else {
                serde_json::json!({})
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": data}))
        })
        .mount(&server)
        .await;
    core.persist_local_standard_peer(
        "Hosted Amy",
        "endpoint:hosted-amy",
        NODE_DID,
        &format!("{}/api/v0/graphql", server.uri()),
        "/tmp/test-hosted-amy-home",
    )
    .await?;

    let mut updates = core.store_updates();
    assert!(
        core.refresh_local_request(
            NODE_DID,
            "session-terminal-config",
            Some(REQUEST_ID),
            None,
            &[],
        )
        .await?
        .is_none(),
        "the in-flight request-event fence must reject direct publication"
    );
    timeout(Duration::from_secs(5), async {
        loop {
            if core.store().snapshot().nodes.iter().any(|node| {
                node.node_did == NODE_DID
                    && node.default_agent_did.as_deref() == Some("did:test:hosted-amy:research")
            }) {
                break;
            }
            updates.changed().await.expect("store observer is running");
        }
    })
    .await
    .context("scoped resync must recover operator configuration after stale publication")?;

    let local_node = core
        .node()
        .execute(&format!(
            r#"query {{ Node(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ default_agent_did }} }}"#,
            gents::store::graphql::escape_graphql_string(NODE_DID)
        ))
        .await;
    assert!(
        !local_node.has_errors(),
        "read local replica: {:?}",
        local_node.errors
    );
    assert_eq!(
        local_node
            .data
            .as_ref()
            .and_then(|data| data.get("Node"))
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("default_agent_did"))
            .and_then(serde_json::Value::as_str),
        Some("did:test:hosted-amy:default"),
        "the new projection value must come from operator overlay, not local DB mutation"
    );

    Arc::new(core).shutdown().await?;
    Ok(())
}

/// A desktop whose client key is the User of a served runtime it routes to
/// as its co-hosted local runtime; the observer reads only that runtime.
async fn served_core() -> Result<(tempfile::TempDir, ClientCore, ServedRuntimeHome)> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let runtime =
        ServedRuntimeHome::start_unpublished_for(Arc::new(core.node_identity().clone())).await;
    runtime.connect(&core).await;
    Ok((tempdir, core, runtime))
}

fn agent_message(alias: &str, key: &str, session: &str, node_did: &str, sequence: usize) -> String {
    format!(
        r#"{alias}: create_AgentMessage(input: {{
            message_key: "{key}", session_id: "{session}", node_did: "{node_did}",
            publication: {{kind: "fork", origin_message_doc_id: "origin-{key}"}},
            outcome: "complete", sequence: {sequence}, role: "user",
            native_id: "native-{key}", blocks: null,
            created_at: "2026-05-07T00:00:00Z"
        }}) {{ _docID }}"#
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_loads_initial_snapshot_and_ticks_on_update() -> Result<()> {
    let (_tempdir, core, runtime) = served_core().await?;
    let store = core.store();
    let mut updates = core.store_updates();

    assert_eq!(store.snapshot().nodes.len(), 0);
    let baseline = *updates.borrow_and_update();

    runtime
        .publish_node(serde_json::json!({"display_name": "Test Agent", "enabled": true}))
        .await;

    timeout(Duration::from_secs(15), async {
        loop {
            updates.changed().await.context("watch channel closed")?;
            if *updates.borrow() > baseline {
                return Ok::<(), anyhow::Error>(());
            }
        }
    })
    .await
    .context("timed out waiting for store update")??;

    timeout(Duration::from_secs(15), async {
        loop {
            if store.snapshot().nodes.len() == 1 {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for refreshed snapshot")??;

    assert_eq!(store.snapshot().nodes[0].node_did, runtime.node_did());
    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bootstrap_then_observer_no_lost_writes() -> Result<()> {
    let (_tempdir, core, runtime) = served_core().await?;
    runtime.publish_node(serde_json::json!({"enabled": true})).await;
    let node_did = gents::store::graphql::escape_graphql_string(runtime.node_did());
    let requester = gents::store::graphql::escape_graphql_string(core.node_identity().did());
    for i in 0..20usize {
        runtime
            .user_graphql_access()
            .write(
                "test.client_store.race_session",
                &format!(
                    r#"mutation {{ create_AgentSession(input: {{ node_did: "{node_did}", session_id: "race-{i}", requester_did: "{requester}", agent_did: "{node_did}", created_at: "2026-11-04T00:00:00Z" }}) {{ _docID }} }}"#
                ),
            )
            .await?;
    }

    timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = core.store().snapshot();
            if (0..20usize).all(|i| {
                snapshot
                    .sessions
                    .iter()
                    .any(|session| session.session_id == format!("race-{i}"))
            }) {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for all 20 sessions to appear in store")??;

    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incremental_observer_keeps_transcript_out_of_streaming_store() -> Result<()> {
    let (_tempdir, core, runtime) = served_core().await?;
    runtime.publish_node(serde_json::json!({"enabled": true})).await;
    timeout(Duration::from_secs(15), async {
        while core.store().snapshot().nodes.is_empty() {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("the observer is connected")?;
    let node_did = runtime.node_did().to_owned();
    let messages = (0..50usize)
        .map(|i| agent_message(&format!("m{i}"), &format!("long:{i}"), "long", &node_did, i))
        .collect::<Vec<_>>()
        .join("\n");
    runtime
        .user_graphql_access()
        .write("test.client_store.transcript", &format!("mutation {{ {messages} }}"))
        .await?;
    timeout(Duration::from_secs(15), async {
        loop {
            if core
                .observer_metrics()
                .await
                .map(|m| m.transcript_invalidations > 0)
                .unwrap_or(false)
            {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for canonical transcript invalidation")??;
    assert!(core.store().snapshot().transcript_messages.is_empty());
    let page = load_session_transcript_page_on(
        &runtime.user_access(),
        "long",
        Some(&node_did),
        None,
        None,
        Some(40),
    )
    .await?;
    assert_eq!(page.message_query_limit, 41);
    assert_eq!(page.store.transcript_messages.len(), 40);
    assert_eq!(page.queried_rows, 41);
    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflicting_immutable_transcript_facts_are_not_silently_selected() -> Result<()> {
    let tmp = tempfile::TempDir::new()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tmp.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let response = core
        .node()
        .execute(
            r#"mutation {
            first: create_AgentMessage(input: {
                message_key: "conflict:1", session_id: "conflict", node_did: "did:conflict",
                publication: {kind: "fork", origin_message_doc_id: "origin-a"},
                outcome: "complete", sequence: 1, role: "user", native_id: "first",
                blocks: [], created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
            second: create_AgentMessage(input: {
                message_key: "conflict:1", session_id: "conflict", node_did: "did:conflict",
                publication: {kind: "fork", origin_message_doc_id: "origin-b"},
                outcome: "complete", sequence: 2, role: "user", native_id: "second",
                blocks: [], created_at: "2026-05-07T00:00:01Z"
            }) { _docID }
        }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "seed immutable twins: {:?}",
        response.errors
    );
    let result = load_session_transcript_page(
        core.node(),
        "conflict",
        Some("did:conflict"),
        None,
        None,
        Some(40),
    )
    .await;
    assert!(
        result.is_err(),
        "conflicting immutable headers must be surfaced"
    );
    Arc::new(core).shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_scope_isolation_under_drop_recovery() -> Result<()> {
    let (_tempdir, core, runtime) = served_core().await?;
    runtime.publish_node(serde_json::json!({"enabled": true})).await;
    let alpha = runtime.node_did().to_owned();
    let beta = runtime.publish_foreign_node().await;
    let transcript = [
        agent_message("alpha", "shared:alpha", "shared", &alpha, 1),
        agent_message("beta", "shared:beta", "shared", &beta, 1),
    ]
    .join("\n");
    runtime
        .user_graphql_access()
        .write("test.client_store.scoped_transcript", &format!("mutation {{ {transcript} }}"))
        .await?;

    core.set_selected_node_did(Some(alpha.clone()));

    // The route is scoped to alpha: the neighbouring Node on the same
    // runtime store never enters the observed projection.
    timeout(Duration::from_secs(15), async {
        while !core.store().snapshot().nodes.iter().any(|node| node.node_did == alpha) {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for the routed node")?;
    assert!(
        core.store().snapshot().nodes.iter().all(|node| node.node_did != beta),
        "{:?}",
        core.store().snapshot().nodes
    );

    let alpha_page = load_session_transcript_page_on(
        &runtime.user_access(),
        "shared",
        Some(&alpha),
        None,
        None,
        Some(40),
    )
    .await?;
    assert_eq!(alpha_page.store.transcript_messages.len(), 1);
    assert_eq!(alpha_page.store.transcript_messages[0].message.node_did, alpha);

    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

#[test]
fn default_agent_did_does_not_require_a_gossiped_node() {
    let node_did = "did:key:z6MkAmy";
    let directory = tempfile::tempdir().unwrap();
    let root = gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    let binding = gents::store::identity::provision_agent_identity(&root).unwrap();
    let agent_did = binding.agent_did;
    let store = ClientStore::from_rows(ClientStoreRows {
        agents: vec![gents::documents::Agent {
            creation_nonce: binding.creation_nonce,
            agent_did: agent_did.clone(),
            node_did: node_did.to_string(),
            display_name: Some("Amy".to_string()),
            description: None,
            context_id: None,
            inference_profile_id: "profile".to_string(),
            enabled: true,
            tags: Vec::new(),
            created_at: None,
        }],
        ..ClientStoreRows::default()
    });

    assert!(store.nodes.is_empty());
    assert_eq!(
        store.default_agent_did_for_node(node_did),
        Some(agent_did.as_str())
    );
}
