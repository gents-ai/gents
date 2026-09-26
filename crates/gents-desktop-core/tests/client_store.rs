use std::time::Duration;

use anyhow::{Context, Result};
use gents_desktop_core::client::{
    load_session_transcript_page, ClientCore, ClientCoreOptions, ClientStore, ClientStoreRows,
    DesktopPaths,
};
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
            agent_did: Some("did:test:amy".into()),
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
        request: create_AgentRequest(input: {
            purpose: "normal", request_id: "request-1", agent_did: "did:test:amy", behavior_id: "default",
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
    let request_doc_id = gents::graphql::escape_graphql_string(request_doc_id);
    let output = core.node().execute(&format!(r#"mutation {{
        segment: create_AgentOutputSegment(input: {{
            agent_did: "did:test:amy", session_id: "session-1",
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
    let close_doc_id = gents::graphql::escape_graphql_string(close_doc_id);
    let header = core.node().execute(&format!(r#"mutation {{
        create_AgentMessage(input: {{
            message_key: "session-1:2", session_id: "session-1", agent_did: "did:test:amy",
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
    assert!(core
        .refresh_local_request("did:test:amy", "request-1")
        .await?
        .is_some());
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
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_operator_request_refreshes_non_replicated_agent_config() -> Result<()> {
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
                create_AgentPrincipal(input: {
                    agent_did: "did:test:amy"
                    display_name: "Amy"
                    default_behavior_id: "did:test:amy:default"
                    enabled: true
                }) { _docID }
                create_AgentRequest(input: {
                    purpose: "normal", request_id: "request-terminal-config"
                    agent_did: "did:test:amy"
                    behavior_id: "did:test:amy:default"
                    session_id: "session-terminal-config"
                    content: "make research the default"
                    lifecycle_state: "processing"
                    created_at: "2026-08-21T18:07:35Z"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "seed rows: {:?}", response.errors);
    core.refresh_store().await?;
    assert_eq!(
        core.store().snapshot().agent_principals[0]
            .default_behavior_id
            .as_deref(),
        Some("did:test:amy:default")
    );

    let response = core
        .node()
        .execute(
            r#"mutation {
                update_AgentPrincipal(
                    filter: { agent_did: { _eq: "did:test:amy" } }
                    input: { default_behavior_id: "did:test:amy:research" }
                ) { _docID }
                update_AgentRequest(
                    filter: { request_id: { _eq: "request-terminal-config" } }
                    input: { lifecycle_state: "completed" }
                ) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "complete turn: {:?}",
        response.errors
    );

    assert!(core
        .refresh_local_request("did:test:amy", "request-terminal-config")
        .await?
        .is_some());
    assert_eq!(
        core.store().snapshot().agent_principals[0]
            .default_behavior_id
            .as_deref(),
        Some("did:test:amy:research")
    );

    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_loads_initial_snapshot_and_ticks_on_update() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;
    let store = core.store();
    let mut updates = core.store_updates();

    assert_eq!(store.snapshot().agent_principals.len(), 0);
    let baseline = *updates.borrow_and_update();

    let response = core
        .node()
        .execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:test:test-agent"
                    display_name: "Test Agent"
                    enabled: true
                }) { agent_did }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "agent principal mutation should succeed"
    );

    timeout(Duration::from_secs(5), async {
        loop {
            updates.changed().await.context("watch channel closed")?;
            if *updates.borrow() > baseline {
                return Ok::<(), anyhow::Error>(());
            }
        }
    })
    .await
    .context("timed out waiting for store update")??;

    timeout(Duration::from_secs(5), async {
        loop {
            if store.snapshot().agent_principals.len() == 1 {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for refreshed snapshot")??;

    assert_eq!(
        store.snapshot().agent_principals[0].agent_did,
        "did:test:test-agent"
    );
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bootstrap_then_observer_no_lost_writes() -> Result<()> {
    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path());

    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;

    for i in 0..20usize {
        let mutation = format!(
            r#"mutation {{
                add_AgentPrincipal(input: {{
                    agent_did: "did:race-{i}",
                    display_name: "race-{i}",
                    enabled: true
                }}) {{ agent_did }}
            }}"#
        );
        let response = core.node().execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "mutation {i} failed: {:?}",
            response.errors
        );
    }

    timeout(Duration::from_secs(5), async {
        loop {
            if core.store().snapshot().agent_principals.len() >= 20 {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for all 20 principals to appear in store")??;

    let snap = core.store().snapshot();
    let dids: std::collections::HashSet<&str> = snap
        .agent_principals
        .iter()
        .map(|p| p.agent_did.as_str())
        .collect();
    for i in 0..20usize {
        let want = format!("did:race-{i}");
        assert!(dids.contains(want.as_str()), "missing {want}");
    }

    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incremental_observer_keeps_transcript_out_of_streaming_store() -> Result<()> {
    let tmp = tempfile::TempDir::new()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tmp.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    for i in 0..50usize {
        let mutation = format!(
            r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "long:{i}", session_id: "long", agent_did: "did:long",
                publication: {{kind: "fork", origin_message_doc_id: "origin"}},
                outcome: "complete", sequence: {i}, role: "user",
                native_id: "native-{i}", blocks: [],
                created_at: "2026-05-07T00:00:00Z"
            }}) {{ _docID }}
        }}"#
        );
        let response = core.node().execute(&mutation).await;
        assert!(!response.has_errors(), "seed {i}: {:?}", response.errors);
    }
    timeout(Duration::from_secs(5), async {
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
    let page =
        load_session_transcript_page(core.node(), "long", None, None, None, Some(40)).await?;
    assert_eq!(page.message_query_limit, 41);
    assert_eq!(page.store.transcript_messages.len(), 40);
    assert_eq!(page.queried_rows, 41);
    core.shutdown().await?;
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
                message_key: "conflict:1", session_id: "conflict", agent_did: "did:conflict",
                publication: {kind: "fork", origin_message_doc_id: "origin-a"},
                outcome: "complete", sequence: 1, role: "user", native_id: "first",
                blocks: [], created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
            second: create_AgentMessage(input: {
                message_key: "conflict:1", session_id: "conflict", agent_did: "did:conflict",
                publication: {kind: "fork", origin_message_doc_id: "origin-b"},
                outcome: "complete", sequence: 1, role: "user", native_id: "second",
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
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_scope_isolation_under_drop_recovery() -> Result<()> {
    let tmp = tempfile::TempDir::new().expect("tmpdir");
    let paths = DesktopPaths::from_root(tmp.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;

    for did in &["did:alpha", "did:beta"] {
        let mutation = format!(
            r#"mutation {{
                create_AgentPrincipal(input: {{
                    agent_did: "{did}",
                    display_name: "{did}",
                    default_behavior_id: "default",
                    enabled: true,
                    created_at: "2026-05-07T00:00:00Z",
                    created_by: "test"
                }}) {{ _docID }}
            }}"#
        );
        let resp = core.node().execute(&mutation).await;
        assert!(!resp.has_errors(), "seed {did}: {:?}", resp.errors);
    }
    let transcript = core
        .node()
        .execute(
            r#"mutation {
            alpha: create_AgentMessage(input: {
                message_key: "shared:alpha", session_id: "shared", agent_did: "did:alpha",
                publication: {kind: "fork", origin_message_doc_id: "origin-alpha"},
                outcome: "complete", sequence: 1, role: "user", native_id: "alpha",
                blocks: [], created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
            beta: create_AgentMessage(input: {
                message_key: "shared:beta", session_id: "shared", agent_did: "did:beta",
                publication: {kind: "fork", origin_message_doc_id: "origin-beta"},
                outcome: "complete", sequence: 1, role: "user", native_id: "beta",
                blocks: [], created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
        }"#,
        )
        .await;
    assert!(
        !transcript.has_errors(),
        "seed scoped transcript: {:?}",
        transcript.errors
    );

    core.set_selected_agent_did(Some("did:alpha".to_string()));

    timeout(Duration::from_secs(5), async {
        loop {
            let snap = core.store().snapshot();
            let dids: Vec<&str> = snap
                .agent_principals
                .iter()
                .map(|p| p.agent_did.as_str())
                .collect();
            if dids.contains(&"did:alpha") && dids.contains(&"did:beta") {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for both agents in store")??;

    let snap = core.store().snapshot();
    let dids: Vec<&str> = snap
        .agent_principals
        .iter()
        .map(|p| p.agent_did.as_str())
        .collect();
    assert!(dids.contains(&"did:alpha"), "did:alpha missing");
    assert!(dids.contains(&"did:beta"), "did:beta missing");
    let alpha_page = load_session_transcript_page(
        core.node(),
        "shared",
        Some("did:alpha"),
        None,
        None,
        Some(40),
    )
    .await?;
    assert_eq!(alpha_page.store.transcript_messages.len(), 1);
    assert_eq!(
        alpha_page.store.transcript_messages[0].message.agent_did,
        "did:alpha"
    );

    core.shutdown().await?;
    Ok(())
}

#[test]
fn default_behavior_id_does_not_require_a_gossiped_agent_principal() {
    let agent_did = "did:key:z6MkAmy";
    let behavior_id = gents::default_behavior_id_for_agent(agent_did);
    let store = ClientStore::from_rows(ClientStoreRows {
        behaviors: vec![gents::document_config::AgentBehavior {
            behavior_id: behavior_id.clone(),
            agent_did: agent_did.to_string(),
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

    assert!(store.agent_principals.is_empty());
    assert_eq!(
        store.default_behavior_id_for_agent(agent_did),
        Some(behavior_id.as_str())
    );
}
