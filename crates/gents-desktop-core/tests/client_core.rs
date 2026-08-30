use std::time::Duration;

use anyhow::{bail, Context, Result};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths, PrincipalIdentity};
use p2p::iroh::parse_public_peer_addr;
use tokio::time::{sleep, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identity_persistence_round_trip() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());

    let first = PrincipalIdentity::load_or_create(&paths).await?;
    let second = PrincipalIdentity::load_or_create(&paths).await?;

    assert_eq!(first.did(), second.did());
    assert_eq!(first.public_key_bytes(), second.public_key_bytes());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_core_persists_managed_server_peer_through_watched_owner() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;
    let mut updates = core.sync_state_updates();

    let record = core
        .persist_local_standard_peer(
            "Managed local runtime",
            "endpoint:managed",
            "did:key:managed",
            "http://127.0.0.1:9191/api/v0/graphql",
            "/tmp/test-agent-home",
        )
        .await?;

    updates.changed().await?;
    let snapshot = updates.borrow_and_update().clone();
    assert_eq!(snapshot.directory, vec![record.clone()]);
    assert_eq!(snapshot.peers.len(), 1);
    assert_eq!(snapshot.peers[0].peer_id, record.peer_id);
    assert!(!snapshot.peers[0].dial_succeeded);
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_core_starts_and_registers_schemas() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;

    let response = core
        .node()
        .execute("query { AgentRequest { _docID } }")
        .await;
    assert!(!response.has_errors(), "schema registration should succeed");
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initial_session_hydration_starts_when_local_transcript_rows_already_exist() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;
    let requester_did = gents::graphql::escape_graphql_string(core.principal().did());
    let agent_did = "did:test:amy";
    persist_local_route(&core, agent_did).await?;
    let session_id = "session-with-local-failed-turn";
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
                request: create_AgentRequest(input: {{
                    request_id: "local-request"
                    requester_did: "{requester_did}"
                    agent_did: "{agent_did}"
                    behavior_id: "default"
                    session_id: "{session_id}"
                    content: "hello"
                    status: "error"
                    lifecycle_state: "failed"
                }}) {{ _docID }}
                response: create_AgentResponse(input: {{
                    response_key: "local-request:response"
                    request_id: "local-request"
                    requester_did: "{requester_did}"
                    agent_did: "{agent_did}"
                    behavior_id: "default"
                    session_id: "{session_id}"
                    status: "error"
                    error_message: "backend unavailable"
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "seed local transcript rows: {:?}",
        response.errors
    );

    core.ensure_session_hydration_started(session_id, agent_did)
        .await?;

    let progress = core
        .session_hydration_progress(session_id, agent_did)
        .await?;
    assert_eq!(progress.phase.as_str(), "serving");
    assert_eq!(progress.merged_count, 2);
    assert_eq!(progress.served_count, None);
    let request_key =
        gents::graphql::escape_graphql_string(&format!("{}:{session_id}", core.local_peer_id()));
    let response = core
        .node()
        .execute(&format!(
            r#"{{
                SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key}" }} }}) {{
                    request_key status served_doc_count
                }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query hydration request: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("SessionHydrationRequest"))
        .and_then(serde_json::Value::as_array)
        .context("SessionHydrationRequest rows")?;
    assert_eq!(
        rows.len(),
        1,
        "initial start must persist exactly one request"
    );
    assert_eq!(
        rows[0].get("status").and_then(serde_json::Value::as_str),
        Some("pending")
    );

    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_served_hydration_row_drives_exact_session_progress() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let requester_did = gents::graphql::escape_graphql_string(core.principal().did());
    let agent_did = "did:test:amy";
    let session_id = "session-served-empty";
    let request_key =
        gents::graphql::escape_graphql_string(&format!("{}:{session_id}", core.local_peer_id()));
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
                create_SessionHydrationRequest(input: {{
                    request_key: "{request_key}"
                    requester_did: "{requester_did}"
                    agent_did: "{agent_did}"
                    session_id: "{session_id}"
                    created_at: "2026-08-28T00:00:00Z"
                    status: "served"
                    status_detail: "served 0 documents"
                    served_doc_count: 0
                    processed_at: "2026-08-28T00:00:01Z"
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "seed served hydration request: {:?}",
        response.errors
    );

    let progress = core
        .session_hydration_progress(session_id, agent_did)
        .await?;
    assert_eq!(progress.phase.as_str(), "complete");
    assert_eq!(progress.merged_count, 0);
    assert_eq!(progress.served_count, Some(0));

    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn passive_hydration_observation_preserves_rejection_until_explicit_retry() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;
    let requester_did = gents::graphql::escape_graphql_string(core.principal().did());
    let agent_did = "did:test:amy";
    persist_local_route(&core, agent_did).await?;
    let session_id = "session-rejected";
    let other_session_id = "session-other";
    let request_key =
        gents::graphql::escape_graphql_string(&format!("{}:{session_id}", core.local_peer_id()));
    let other_request_key = gents::graphql::escape_graphql_string(&format!(
        "{}:{other_session_id}",
        core.local_peer_id()
    ));
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
                create_SessionHydrationRequest(input: {{
                    request_key: "{request_key}"
                    requester_did: "{requester_did}"
                    agent_did: "{agent_did}"
                    session_id: "{session_id}"
                    created_at: "2026-08-28T00:00:00Z"
                    status: "rejected"
                    status_detail: "membership missing"
                    served_doc_count: 0
                    processed_at: "2026-08-28T00:00:01Z"
                }}) {{ _docID }}
                other: create_SessionHydrationRequest(input: {{
                    request_key: "{other_request_key}"
                    requester_did: "{requester_did}"
                    agent_did: "{agent_did}"
                    session_id: "{other_session_id}"
                    created_at: "2026-08-28T00:00:00Z"
                    status: "pending"
                    status_detail: ""
                    served_doc_count: 0
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "seed rejected hydration request: {:?}",
        response.errors
    );

    core.ensure_session_hydration_started(session_id, agent_did)
        .await?;
    core.ensure_session_hydration_started(session_id, agent_did)
        .await?;
    assert_eq!(
        core.session_hydration_progress(session_id, agent_did)
            .await?
            .phase
            .as_str(),
        "failed"
    );
    assert_eq!(
        hydration_request_status(&core, &request_key).await?,
        Some("rejected".to_string()),
        "passive focus must not rewrite a terminal request"
    );

    let failed_progress = core
        .session_hydration_progress(session_id, agent_did)
        .await?;
    let other_progress = core
        .session_hydration_progress(other_session_id, agent_did)
        .await?;
    assert_eq!(other_progress.phase.as_str(), "requested");
    assert!(core
        .ensure_session_hydration_started(session_id, "did:test:wrong-agent")
        .await
        .is_err());
    assert_eq!(
        core.session_hydration_progress(session_id, agent_did)
            .await?,
        failed_progress,
        "a mismatched passive start must not alter the original target"
    );
    assert!(core
        .retry_session_hydration(session_id, "did:test:wrong-agent")
        .await
        .is_err());
    assert_eq!(
        core.session_hydration_progress(session_id, agent_did)
            .await?,
        failed_progress,
        "a rejected retry must not alter the original target"
    );
    assert_eq!(
        hydration_request_status(&core, &request_key).await?,
        Some("rejected".to_string()),
        "a rejected retry must not rewrite the terminal request"
    );
    assert_eq!(
        core.session_hydration_progress(other_session_id, agent_did)
            .await?,
        other_progress,
        "observing and rejecting another target must not overwrite this session"
    );

    core.retry_session_hydration(session_id, agent_did).await?;
    assert_eq!(
        core.session_hydration_progress(session_id, agent_did)
            .await?
            .phase
            .as_str(),
        "requested"
    );
    assert_eq!(
        hydration_request_status(&core, &request_key).await?,
        Some("pending".to_string()),
        "explicit retry owns the terminal reset"
    );
    assert_eq!(
        hydration_request_status(&core, &other_request_key).await?,
        Some("pending".to_string()),
        "retrying one target must not rewrite another request row"
    );

    core.shutdown().await?;
    Ok(())
}

async fn hydration_request_status(core: &ClientCore, request_key: &str) -> Result<Option<String>> {
    let response = core
        .node()
        .execute(&format!(
            r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key}" }} }}) {{
                status
            }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query hydration request status: {:?}",
        response.errors
    );
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("SessionHydrationRequest"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("status"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

async fn persist_local_route(core: &ClientCore, agent_did: &str) -> Result<()> {
    core.persist_local_standard_peer(
        "Hydration test runtime",
        "endpoint:hydration-test",
        agent_did,
        "http://127.0.0.1:9191/api/v0/graphql",
        "/tmp/test-agent-home",
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_client_cores_connect_over_iroh() -> Result<()> {
    let tempdir_a = tempfile::tempdir()?;
    let tempdir_b = tempfile::tempdir()?;
    let core_a = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir_a.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let core_b = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir_b.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let peer_a = core_a.local_peer_id().to_string();
    let addr_a = wait_for_connectable_iroh_addr(&core_a).await?;

    core_b
        .p2p()
        .connect_peer(&addr_a)
        .await
        .context("connecting core_b to core_a")?;

    wait_for_connected_peer(&core_b, &peer_a).await?;
    core_b.shutdown().await?;
    core_a.shutdown().await?;
    Ok(())
}

async fn wait_for_connectable_iroh_addr(core: &ClientCore) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = core.p2p().listen_addresses().await?;
        if let Some(addr) = addrs
            .into_iter()
            .find(|addr| addr.contains("/p2p/") || addr.starts_with("endpoint"))
        {
            return Ok(addr);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for desktop listen address");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_connected_peer(core: &ClientCore, peer_id: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let peers = core.p2p().connected_peers().await?;
        if peers.iter().any(|peer| {
            parse_public_peer_addr(peer)
                .map(|(parsed_peer_id, _)| parsed_peer_id.as_str() == peer_id)
                .unwrap_or_else(|_| peer.contains(peer_id))
        }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for peer connection to {peer_id}");
        }
        sleep(Duration::from_millis(100)).await;
    }
}
