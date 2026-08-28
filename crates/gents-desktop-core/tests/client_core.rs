use std::time::Duration;

use anyhow::{bail, Context, Result};
use gents_desktop_core::client::{
    ClientCore, ClientCoreOptions, DesktopPaths, PeerDirectory, PeerRecord, PrincipalIdentity,
};
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
async fn peer_directory_round_trip() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let path = tempdir.path().join("peers.json");
    let mut directory = PeerDirectory::load(&path).await?;
    let record = PeerRecord::new("Workshop Bay", "iroh://alpha", "did:test:alpha");
    let peer_id = record.peer_id.clone();

    directory.upsert(record).await?;
    directory.remove(&peer_id).await?;

    let reloaded = PeerDirectory::load(&path).await?;
    assert!(reloaded.is_empty());
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

    let progress = core.hydration_progress();
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
async fn passive_hydration_observation_preserves_rejection_until_explicit_retry() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;
    let requester_did = gents::graphql::escape_graphql_string(core.principal().did());
    let agent_did = "did:test:amy";
    let session_id = "session-rejected";
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
                    status: "rejected"
                    status_detail: "membership missing"
                    served_doc_count: 0
                    processed_at: "2026-08-28T00:00:01Z"
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
    assert_eq!(core.hydration_progress().phase.as_str(), "failed");
    assert_eq!(
        hydration_request_status(&core, &request_key).await?,
        Some("rejected".to_string()),
        "passive focus must not rewrite a terminal request"
    );

    let failed_progress = core.hydration_progress();
    assert!(core
        .ensure_session_hydration_started(session_id, "did:test:wrong-agent")
        .await
        .is_err());
    assert_eq!(
        core.hydration_progress(),
        failed_progress,
        "a mismatched passive start must not retarget published progress"
    );
    assert!(core
        .retry_session_hydration(session_id, "did:test:wrong-agent")
        .await
        .is_err());
    assert_eq!(
        core.hydration_progress(),
        failed_progress,
        "a rejected retry must not retarget published progress"
    );
    assert_eq!(
        hydration_request_status(&core, &request_key).await?,
        Some("rejected".to_string()),
        "a rejected retry must not rewrite the terminal request"
    );

    core.retry_session_hydration(session_id, agent_did).await?;
    assert_eq!(core.hydration_progress().phase.as_str(), "requested");
    assert_eq!(
        hydration_request_status(&core, &request_key).await?,
        Some("pending".to_string()),
        "explicit retry owns the terminal reset"
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
