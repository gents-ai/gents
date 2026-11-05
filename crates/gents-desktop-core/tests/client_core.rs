use std::sync::Arc;
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
    Arc::new(core).shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_preparation_uses_the_hosted_head_when_the_replica_lags() -> Result<()> {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let node_did = "did:key:hosted-message-owner";
    Mock::given(method("POST"))
        .and(path("/api/v0/tx"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":17})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v0/tx/17"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("AgentSession"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"AgentSession": []}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("AgentRequest"))
        .and(body_string_contains(node_did))
        .and(body_string_contains("hosted-session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"AgentRequest": [{"_docID": "hosted-head-doc", "request_id": "hosted-head"}]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let root = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(root.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    core.persist_local_standard_peer(
        "Hosted runtime",
        "endpoint:hosted-message-owner",
        node_did,
        &format!("{}/api/v0/graphql", server.uri()),
        "/tmp/test-hosted-message-home",
    )
    .await?;
    assert!(core.store().snapshot().requests.is_empty());
    let input = core
        .prepare_user_message_input(node_did, "hosted-session", Default::default())
        .await?;
    let queue = input.queue.context("prepared user queue")?;
    assert_eq!(
        queue.queued_after_request_id.as_deref(),
        Some("hosted-head")
    );
    assert_eq!(
        queue.delivery,
        gents_protocol::request_input::QueueDelivery::Steer
    );
    Arc::new(core).shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_config_write_never_falls_back_to_the_desktop_replica() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;
    let node_did = "did:key:managed-config-owner";
    core.persist_local_standard_peer(
        "Managed local runtime",
        "endpoint:managed-config",
        node_did,
        "http://127.0.0.1:1/api/v0/graphql",
        "/tmp/test-managed-config-home",
    )
    .await?;

    let tools: gents::Tools = serde_json::from_value(serde_json::json!({
        "node_did": node_did,
        "tools_id": "must-not-be-local",
        "host": { "files": { "mode": "ReadOnly" } }
    }))?;
    let error = core
        .save_tools(&tools)
        .await
        .expect_err("an unavailable managed operator endpoint must fail closed");
    assert!(
        format!("{error:#}").contains("127.0.0.1:1"),
        "the managed endpoint error must stay visible: {error:#}"
    );

    let response = core
        .node()
        .execute(&format!(
            r#"{{ Tools(filter: {{ node_did: {{ _eq: "{}" }}, tools_id: {{ _eq: "must-not-be-local" }} }}) {{ _docID }} }}"#,
            gents::store::graphql::escape_graphql_string(node_did)
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query local Tools: {:?}",
        response.errors
    );
    assert_eq!(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("Tools"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0),
        "failed managed writes must not appear in the desktop replica"
    );

    Arc::new(core).shutdown().await?;
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
    Arc::new(core).shutdown().await?;
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
    Arc::new(core_b).shutdown().await?;
    Arc::new(core_a).shutdown().await?;
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
