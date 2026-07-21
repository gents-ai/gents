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
