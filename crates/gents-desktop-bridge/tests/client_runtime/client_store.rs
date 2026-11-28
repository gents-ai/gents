use std::sync::Arc;

use anyhow::{Context, Result};
use gents::client_test_support::ServedRuntimeHome;
use gents_desktop_bridge::client_runtime::ClientRuntime;
use gents_observe::client_projection::query::load_session_transcript_page_on;
use gents_observe::client_projection::store::{ClientStore, ClientStoreRows};
use gents_store::client_journal::paths::DesktopPaths;
use gents_store::test_support::STALL_CEILING;
use tokio::time::timeout;

#[test]
fn focused_request_id_defaults_to_none() {
    let store =
        gents_observe::client_projection::observe::ObservedStores::new().ensure("did:test:node");
    assert!(store.focused_request_id().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_request_refresh_preserves_projection_boundaries_and_database_truth() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientRuntime::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        gents_desktop_bridge::client_test_support::file_journal_options(),
    )
    .await?;
    let user = core.node_identity().did().to_owned();
    let runtime = gents::client_test_support::ServedRuntimeHome::start_unpublished_for(Arc::new(
        core.node_identity().clone(),
    ))
    .await;
    runtime.publish_node(serde_json::json!({})).await;
    gents_desktop_bridge::client_test_support::connect_runtime(&runtime, &core).await;
    let node_did = runtime.node_did().to_owned();
    core.save_backend(&serde_json::from_value(serde_json::json!({
        "node_did": node_did, "backend_id": "backend", "name": "Backend",
        "provider_kind": "OpenAiCompatible", "endpoint": "http://localhost:8000/v1",
        "auth": {"kind": "unauthenticated"}
    }))?)
    .await?;
    core.save_inference_profile(&serde_json::from_value(serde_json::json!({
        "node_did": node_did, "profile_id": "profile", "backend_id": "backend",
        "model_name": "model"
    }))?)
    .await?;
    let agent = runtime.provision_agent().await;
    core.save_agent(
        &serde_json::from_value(runtime.agent_value_for_create(&agent, "profile").await)?,
        None,
    )
    .await?;
    let session_doc = runtime.agent_session(&agent, "session-1", &user).await;
    let agent_doc = runtime.agent_doc_id(&agent).await;
    let (node, user, agent_did) = (&node_did, &user, &agent.agent_did);
    let (session_doc, agent_doc) = (&session_doc, &agent_doc);
    runtime
        .agent_access(&agent)
        .await
        .transact_write("test.request_refresh.transcript", |txn| {
            Box::pin(async move {
                let create = |collection: &'static str, input: serde_json::Value| async move {
                    let response = txn
                        .execute_with_variables(
                            &format!("mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{_docID}} }}"),
                            &serde_json::json!({"input": input}),
                        )
                        .await?;
                    gents_store::graphql::created_doc_id(&response, collection)
                };
                let request = create("AgentRequest", serde_json::json!({
                    "purpose": "normal", "request_id": "request-1", "node_did": node,
                    "requester_did": user, "agent_did": agent_did, "session_id": "session-1",
                    "content": "run it", "lifecycle_state": "processing",
                    "created_at": "2026-08-21T18:07:35Z"
                }))
                .await?;
                txn.add_resource_relationship("AgentRequest", &request, "parent", "agents", agent_doc)
                    .await?;
                txn.add_resource_relationship("AgentRequest", &request, "session", "sessions", session_doc)
                    .await?;
                let child = |extra: serde_json::Value| {
                    let mut value = serde_json::json!({
                        "node_did": node, "requester_did": user, "session_id": "session-1",
                        "session_doc_id": session_doc, "agent_did": agent_did,
                        "creator_did": agent_did, "parent_linked": true,
                        "request_doc_id": request, "created_at": "2026-08-21T18:07:36Z",
                    });
                    value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
                    value
                };
                let segment = create("AgentOutputSegment", child(serde_json::json!({
                    "source": {"kind": "authored", "key": "answer"}, "ordinal": 0,
                    "writer": {"kind": "request_execution", "execution_generation": "generation"},
                    "runs": [{"stream": 0, "bytes": 8, "declaration": {"block_index": 0, "part_index": 0, "payload": {"kind": "text"}}}],
                    "payload": "complete",
                    "close": {"kind": "closed", "outcome": "complete", "segments": 1, "stream_bytes": [8]}
                })))
                .await?;
                txn.add_resource_relationship("AgentOutputSegment", &segment, "parent", "sessions", session_doc)
                    .await?;
                let message = create("AgentMessage", child(serde_json::json!({
                    "message_key": "session-1:2",
                    "publication": {"kind": "request_execution", "execution_generation": "generation"},
                    "outcome": "complete", "sequence": 2, "role": "assistant", "native_id": "assistant-2",
                    "blocks": [{"type": "text", "text": {"output": {"close_doc_id": segment, "stream": 0}, "presentation": {"kind": "full"}}}]
                })))
                .await?;
                txn.add_resource_relationship("AgentMessage", &message, "parent", "sessions", session_doc)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;

    core.refresh_node(&node_did).await?;
    assert!(core
        .stores()
        .snapshot(&node_did)
        .transcript_messages
        .is_empty());
    assert!(core.stores().snapshot(&node_did).output_segments.is_empty());
    core.refresh_local_request(&node_did, "session-1", Some("request-1"), None, &[])
        .await?;
    let refreshed = core.stores().snapshot(&node_did);
    assert!(refreshed.transcript_messages.is_empty());
    assert!(refreshed.output_segments.is_empty());
    assert!(refreshed
        .requests
        .iter()
        .any(|row| row.request_id == "request-1"));
    let page = gents_observe::client_projection::query::load_session_transcript_page_on(
        &runtime.user_access(),
        "session-1",
        Some(&node_did),
        Some(user),
        None,
        Some(40),
    )
    .await?;
    assert_eq!(page.store.transcript_messages.len(), 1);
    assert_eq!(page.store.output_segments.len(), 1);
    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

/// A terminal request re-reads its runtime's configuration before the selected
/// request is published. When that publication is fenced by a concurrent
/// invalidation, the scoped resync still converges on the runtime's current
/// `Node`. The desktop's local replica of runtime configuration is deleted:
/// both the starting value and the recovered value come only from the runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_operator_request_refreshes_non_replicated_agent_config() -> Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let node_key = std::sync::Arc::new(
        tokio::task::spawn_blocking(|| gents_store::test_support::test_identity("hosted-amy"))
            .await?,
    );
    let node_did: &'static str = Box::leak(
        gents_store::identity::NodeIdentity::did(node_key.as_ref())
            .to_owned()
            .into_boxed_str(),
    );
    const REQUEST_ID: &str = "request-terminal-config";
    let tempdir = tempfile::tempdir()?;
    let core = ClientRuntime::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        gents_desktop_bridge::client_test_support::file_journal_options(),
    )
    .await?;

    let server = MockServer::start().await;
    let completed_request = serde_json::json!({
        "_docID": "remote-terminal-request-doc",
        "request_id": REQUEST_ID,
        "purpose": "normal",
        "node_did": node_did,
        "requester_did": node_did,
        "agent_did": "did:test:hosted-amy:default",
        "session_id": "session-terminal-config",
        "content": "make research the default",
        "lifecycle_state": "completed"
    });
    let runtime_default = Arc::new(std::sync::Mutex::new("did:test:hosted-amy:default"));
    let store = core.stores().ensure(node_did);
    let response_request = completed_request.clone();
    let response_session = serde_json::json!({
        "_docID": "remote-terminal-session-doc",
        "session_id": "session-terminal-config",
        "node_did": node_did,
        "requester_did": node_did,
        "agent_did": "did:test:hosted-amy:default",
        "created_at": "2026-07-24T00:00:00Z"
    });
    let served_default = Arc::clone(&runtime_default);
    let terminal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fencing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (served_terminal, served_fencing) = (Arc::clone(&terminal), Arc::clone(&fencing));
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
            use std::sync::atomic::Ordering;
            let body: serde_json::Value = request.body_json().unwrap();
            let query = body["query"].as_str().unwrap_or_default();
            // Invalidate the captured read while an owner query of the selected
            // request is in flight so its patch must be rejected at publication.
            if served_fencing.load(Ordering::SeqCst) && query.contains(": AgentRequest(") {
                store.invalidate_projection(true);
            }
            let mut rows = serde_json::json!({
                "Node": [{
                    "_docID": "remote-node-doc",
                    "node_did": node_did,
                    "display_name": "Amy",
                    "enabled": true
                }],
                "DefaultAgentSelection": [{
                    "_docID": "remote-selection-doc",
                    "node_did": node_did,
                    "default_agent_did": *served_default.lock().unwrap()
                }],
            });
            if served_terminal.load(Ordering::SeqCst) {
                rows["AgentSession"] = serde_json::json!([response_session.clone()]);
                rows["AgentRequest"] = serde_json::json!([response_request.clone()]);
            }
            ResponseTemplate::new(200).set_body_json(
                gents_store::test_support::wire_database::answer(&rows, query),
            )
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(move |request: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(
                gents_store::test_support::local_home_status_now(
                    node_did,
                    &|payload| node_key.sign_now(payload).unwrap(),
                    request.url.query(),
                ),
            )
        })
        .mount(&server)
        .await;
    core.connect_runtime(gents_protocol::runtime_connection::RuntimeConnection {
        node_did: (node_did).to_string(),
        label: ("Hosted Amy").to_string(),
        graphql: format!("{}/api/v0/graphql", server.uri()),
        local_node_home: Some(("/tmp/test-hosted-amy-home").to_string()),
        access_request: None,
        access_state: None,
    })
    .await?;
    core.refresh_node(node_did).await?;
    assert_eq!(
        core.stores()
            .snapshot(node_did)
            .selected_default_agent_did(node_did),
        Some("did:test:hosted-amy:default"),
        "the starting configuration is the runtime's"
    );

    // The terminal request changed the runtime's default Agent.
    *runtime_default.lock().unwrap() = "did:test:hosted-amy:research";
    terminal.store(true, std::sync::atomic::Ordering::SeqCst);
    fencing.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut updates = core.store_updates();
    let refreshed = core
        .refresh_local_request(
            node_did,
            "session-terminal-config",
            Some(REQUEST_ID),
            None,
            &[],
        )
        .await?;
    fencing.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        refreshed.is_none(),
        "the in-flight request-event fence must reject direct publication"
    );
    timeout(STALL_CEILING, async {
        loop {
            if core
                .stores()
                .snapshot(node_did)
                .selected_default_agent_did(node_did)
                == Some("did:test:hosted-amy:research")
            {
                break;
            }
            updates.changed().await.expect("store observer is running");
        }
    })
    .await
    .context("scoped resync must recover operator configuration after stale publication")?;

    assert!(!core
        .node()
        .get_all_collection_versions()
        .await?
        .iter()
        .any(|collection| collection.name == "Node"));

    Arc::new(core).shutdown().await?;
    Ok(())
}

/// A request event of another session scope, delivered while the selected
/// session's patch is being read, leaves the patch to publish
/// (`ClientDataPublication.PatchFence.other_scopes_preserve_patch`). An event
/// of its own scope rejects it, and the next read publishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_sessions_request_event_does_not_reject_the_selected_patch() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let node_key = std::sync::Arc::new(
        tokio::task::spawn_blocking(|| gents_store::test_support::test_identity("hosted-fence"))
            .await?,
    );
    let node_did: &'static str = Box::leak(
        gents_store::identity::NodeIdentity::did(node_key.as_ref())
            .to_owned()
            .into_boxed_str(),
    );
    let tempdir = tempfile::tempdir()?;
    let core = ClientRuntime::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        gents_desktop_bridge::client_test_support::file_journal_options(),
    )
    .await?;
    let store = core.stores().ensure(node_did);
    let scope = |session: &str| {
        std::collections::BTreeSet::from([gents_protocol::session::SessionScope {
            node_did: node_did.to_owned(),
            session_id: session.to_owned(),
            requester_did: Some(node_did.to_owned()),
        }])
    };
    let (other, own) = (scope("session-other"), scope("session-selected"));
    let other_event = Arc::new(AtomicBool::new(false));
    let own_event = Arc::new(AtomicBool::new(false));
    let (served_other, served_own, served_store) = (
        Arc::clone(&other_event),
        Arc::clone(&own_event),
        store.clone(),
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "selected-read"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx/selected-read"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/graphql"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = request.body_json().unwrap();
            let query = body["query"].as_str().unwrap_or_default();
            // A request event lands while the selected request is being read.
            if query.contains(": AgentRequest(") {
                if served_other.load(Ordering::SeqCst) {
                    served_store.observe_scope_event(&other);
                }
                if served_own.load(Ordering::SeqCst) {
                    served_store.observe_scope_event(&own);
                }
            }
            let rows = serde_json::json!({
                "Node": [{
                    "_docID": "remote-node-doc", "node_did": node_did,
                    "display_name": "Fence", "enabled": true
                }],
                "AgentSession": [{
                    "_docID": "remote-session-doc", "session_id": "session-selected",
                    "node_did": node_did, "requester_did": node_did,
                    "agent_did": "did:test:hosted-fence:default",
                    "created_at": "2026-07-24T00:00:00Z"
                }],
                "AgentRequest": [{
                    "_docID": "remote-request-doc", "request_id": "request-selected",
                    "purpose": "normal", "node_did": node_did, "requester_did": node_did,
                    "agent_did": "did:test:hosted-fence:default",
                    "session_id": "session-selected", "content": "run it",
                    "lifecycle_state": "processing"
                }],
            });
            ResponseTemplate::new(200).set_body_json(
                gents_store::test_support::wire_database::answer(&rows, query),
            )
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(move |request: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(
                gents_store::test_support::local_home_status_now(
                    node_did,
                    &|payload| node_key.sign_now(payload).unwrap(),
                    request.url.query(),
                ),
            )
        })
        .mount(&server)
        .await;
    core.connect_runtime(gents_protocol::runtime_connection::RuntimeConnection {
        node_did: (node_did).to_string(),
        label: ("Hosted Fence").to_string(),
        graphql: format!("{}/api/v0/graphql", server.uri()),
        local_node_home: Some(("/tmp/test-hosted-fence-home").to_string()),
        access_request: None,
        access_state: None,
    })
    .await?;
    let read = || {
        core.refresh_local_request(
            node_did,
            "session-selected",
            Some("request-selected"),
            None,
            &[],
        )
    };

    // The mock serves no subscription, so the observer's source keeps
    // reconnecting and each gap rejects a read in flight. A read is retried
    // past those; under a store-wide fence no retry would publish, since
    // another session's event lands during every read.
    let published = || async {
        for _ in 0..40 {
            if read().await?.is_some() {
                return anyhow::Ok(true);
            }
        }
        Ok(false)
    };

    other_event.store(true, Ordering::SeqCst);
    assert!(
        published().await?,
        "another session's request event must not reject the selected session's patch"
    );
    assert_eq!(
        core.stores().snapshot(node_did).requests.len(),
        1,
        "the patch published its request"
    );
    assert!(
        published().await?,
        "the unchanged patch stays reusable under another session's events"
    );
    other_event.store(false, Ordering::SeqCst);

    own_event.store(true, Ordering::SeqCst);
    assert!(
        read().await?.is_none(),
        "a request event of the selected session rejects its patch"
    );
    own_event.store(false, Ordering::SeqCst);
    assert!(published().await?, "a later read publishes");

    Arc::new(core).shutdown().await?;
    Ok(())
}

/// A desktop whose client key is the User of a served runtime it routes to
/// as its co-hosted local runtime; the observer reads only that runtime.
async fn served_core() -> Result<(tempfile::TempDir, ClientRuntime, ServedRuntimeHome)> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientRuntime::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        gents_desktop_bridge::client_test_support::file_journal_options(),
    )
    .await?;
    let runtime =
        ServedRuntimeHome::start_unpublished_for(Arc::new(core.node_identity().clone())).await;
    runtime.publish_node(serde_json::json!({})).await;
    gents_desktop_bridge::client_test_support::connect_runtime(&runtime, &core).await;
    Ok((tempdir, core, runtime))
}

/// Waits on the store's own update notice until `done` holds.
async fn until<F>(core: &ClientRuntime, what: &str, mut done: F) -> Result<()>
where
    F: FnMut(&ClientRuntime) -> bool,
{
    let mut updates = core.store_updates();
    timeout(STALL_CEILING, async {
        while !done(core) {
            updates
                .changed()
                .await
                .context("store update channel closed")?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .with_context(|| format!("timed out waiting for {what}"))?
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

/// Covers initial observer load, update ticks, rapid session/request bursts
/// (`ObserverResync.collectionStartable`), streaming transcript exclusion,
/// conflicting immutable header detection, and single-Node route scoping on one
/// served runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn served_observer_syncs_updates_bursts_transcripts_conflicts_and_node_scoping() -> Result<()>
{
    let (_tempdir, core, runtime) = served_core().await?;
    let stores = core.stores();
    let alpha = runtime.node_did().to_owned();
    let escaped_alpha = gents_store::graphql::escape_graphql_string(&alpha);
    let requester = gents_store::graphql::escape_graphql_string(core.node_identity().did());
    let mut updates = core.store_updates();

    // Initial snapshot load and update tick.
    assert_eq!(stores.snapshot(&alpha).nodes.len(), 1);
    let baseline = *updates.borrow_and_update();
    runtime
        .publish_node(serde_json::json!({"display_name": "Test Agent", "enabled": true}))
        .await;
    timeout(STALL_CEILING, async {
        loop {
            updates.changed().await.context("watch channel closed")?;
            if *updates.borrow() > baseline {
                return Ok::<(), anyhow::Error>(());
            }
        }
    })
    .await
    .context("timed out waiting for store update")??;
    until(&core, "the refreshed snapshot", |core| {
        core.stores()
            .snapshot(&alpha)
            .nodes
            .iter()
            .any(|node| node.display_name.as_deref() == Some("Test Agent"))
    })
    .await?;
    assert_eq!(stores.snapshot(&alpha).nodes[0].node_did, alpha);

    // Rapid session writes reach the store with no lost writes.
    for i in 0..20usize {
        runtime
            .user_graphql_access()
            .write(
                "test.client_store.race_session",
                &format!(
                    r#"mutation {{ create_AgentSession(input: {{ node_did: "{escaped_alpha}", session_id: "race-{i}", requester_did: "{requester}", agent_did: "{escaped_alpha}", created_at: "2026-11-04T00:00:00Z" }}) {{ _docID }} }}"#
                ),
            )
            .await?;
    }
    until(&core, "all 20 sessions in the store", |core| {
        let snapshot = core.stores().snapshot(&alpha);
        (0..20usize).all(|i| {
            snapshot
                .sessions
                .iter()
                .any(|session| session.session_id == format!("race-{i}"))
        })
    })
    .await?;

    // A burst of request changes all reach the store.
    for i in 0..20usize {
        runtime
            .user_graphql_access()
            .write(
                "test.client_store.burst_request",
                &format!(
                    r#"mutation {{ create_AgentRequest(input: {{ request_id: "burst-{i}", purpose: "normal", node_did: "{escaped_alpha}", requester_did: "{requester}", agent_did: "{escaped_alpha}", session_id: "burst-{i}", content: "burst", lifecycle_state: "processing", created_at: "2026-11-04T00:00:{i:02}Z" }}) {{ _docID }} }}"#
                ),
            )
            .await?;
    }
    until(&core, "all 20 requests in the store", |core| {
        let snapshot = core.stores().snapshot(&alpha);
        (0..20usize).all(|i| snapshot.request_row(&format!("burst-{i}")).is_some())
    })
    .await?;

    // Incremental observer invalidates transcripts without loading them into the streaming store.
    let messages = (0..50usize)
        .map(|i| agent_message(&format!("m{i}"), &format!("long:{i}"), "long", &alpha, i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut transcript_updates = core.store_updates();
    runtime
        .user_graphql_access()
        .write(
            "test.client_store.transcript",
            &format!("mutation {{ {messages} }}"),
        )
        .await?;
    timeout(STALL_CEILING, async {
        loop {
            if core
                .observer_metrics()
                .await
                .is_some_and(|metrics| metrics.transcript_invalidations > 0)
            {
                return Ok::<(), anyhow::Error>(());
            }
            transcript_updates
                .changed()
                .await
                .context("store update channel closed")?;
        }
    })
    .await
    .context("timed out waiting for canonical transcript invalidation")??;
    assert!(core
        .stores()
        .snapshot(&alpha)
        .transcript_messages
        .is_empty());
    let page = load_session_transcript_page_on(
        &runtime.user_access(),
        "long",
        Some(&alpha),
        None,
        None,
        Some(40),
    )
    .await?;
    assert_eq!(page.message_query_limit, 41);
    assert_eq!(page.store.transcript_messages.len(), 40);
    assert_eq!(page.queried_rows, 41);

    // Conflicting immutable transcript headers are surfaced as an error.
    runtime
        .user_graphql_access()
        .write(
            "test.client_store.conflicting_headers",
            r#"mutation {
            first: create_AgentMessage(input: {
                message_key: "conflict:1", session_id: "conflict", node_did: "did:conflict",
                publication: {kind: "fork", origin_message_doc_id: "origin-a"},
                outcome: "complete", sequence: 1, role: "user", native_id: "first",
                blocks: null, created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
            second: create_AgentMessage(input: {
                message_key: "conflict:1", session_id: "conflict", node_did: "did:conflict",
                publication: {kind: "fork", origin_message_doc_id: "origin-b"},
                outcome: "complete", sequence: 2, role: "user", native_id: "second",
                blocks: null, created_at: "2026-05-07T00:00:01Z"
            }) { _docID }
        }"#,
        )
        .await?;
    assert!(
        load_session_transcript_page_on(
            &runtime.user_access(),
            "conflict",
            Some("did:conflict"),
            None,
            None,
            Some(40),
        )
        .await
        .is_err(),
        "conflicting immutable headers must be surfaced"
    );

    // Neighbouring nodes on the same runtime store stay excluded across resnapshots.
    let beta = runtime.publish_foreign_node().await;
    let transcript = [
        agent_message("alpha", "shared:alpha", "shared", &alpha, 1),
        agent_message("beta", "shared:beta", "shared", &beta, 1),
    ]
    .join("\n");
    runtime
        .user_graphql_access()
        .write(
            "test.client_store.scoped_transcript",
            &format!("mutation {{ {transcript} }}"),
        )
        .await?;
    let only_alpha = |core: &ClientRuntime| {
        let nodes = core.stores().snapshot(&alpha).nodes.clone();
        assert!(nodes.iter().all(|node| node.node_did != beta), "{nodes:?}");
        nodes.iter().any(|node| node.node_did == alpha)
    };
    until(&core, "the routed node", only_alpha).await?;

    let doc = runtime
        .user_graphql_access()
        .execute(&format!(
            "{{ Node(filter: {{node_did: {{_eq: \"{escaped_alpha}\"}}}}) {{ _docID }} }}"
        ))
        .await?;
    let doc = gents_store::graphql::escape_graphql_string(
        doc["data"]["Node"][0]["_docID"]
            .as_str()
            .context("routed Node document")?,
    );
    runtime
        .user_graphql_access()
        .write(
            "test.client_store.resnapshot",
            &format!("mutation {{ update_Node(docID: \"{doc}\", input: {{display_name: \"resnapshot\"}}) {{ _docID }} }}"),
        )
        .await?;
    until(&core, "the resnapshot of the routed node", |core| {
        only_alpha(core)
            && core
                .stores()
                .snapshot(&alpha)
                .nodes
                .iter()
                .any(|node| node.display_name.as_deref() == Some("resnapshot"))
    })
    .await?;

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
    assert_eq!(
        alpha_page.store.transcript_messages[0].message.node_did,
        alpha
    );

    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

#[test]
fn default_agent_did_does_not_require_a_gossiped_node() {
    let node_did = "did:key:z6MkAmy";
    let directory = tempfile::tempdir().unwrap();
    let root =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    let agent_did = binding.agent_did;
    let store = ClientStore::from_rows(ClientStoreRows {
        agents: vec![gents_documents::Agent {
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

#[test]
fn default_agent_selection_wins_over_the_single_enabled_agent() {
    let node_did = "did:key:z6MkAmy";
    let agent = |agent_did: &str| gents_documents::Agent {
        creation_nonce: String::new(),
        agent_did: agent_did.to_string(),
        node_did: node_did.to_string(),
        display_name: None,
        description: None,
        context_id: None,
        inference_profile_id: "profile".to_string(),
        enabled: true,
        tags: Vec::new(),
        created_at: None,
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        agents: vec![agent("did:key:first"), agent("did:key:second")],
        default_agent_selections: vec![gents_documents::DefaultAgentSelection::new(
            node_did,
            "did:key:second",
        )],
        ..ClientStoreRows::default()
    });

    assert_eq!(
        store.selected_default_agent_did(node_did),
        Some("did:key:second")
    );
    assert_eq!(
        store.default_agent_did_for_node(node_did),
        Some("did:key:second")
    );
}
