use super::*;

const ENDPOINT: &str = "https://runtime.example/api/v0/graphql";
const EXPIRY: &str = "2099-01-01T00:00:00Z";

async fn approve(
    directory: &mut PeerDirectory,
    peer: &str,
    node: &str,
    digest: &str,
) -> Result<PeerRecord> {
    directory
        .upsert_enrollment_peer(
            peer,
            "Remote",
            "iroh://ticket",
            node,
            "network",
            "request",
            digest,
            "did:key:admin",
            1,
            EXPIRY,
        )
        .await
}

#[tokio::test]
async fn enrollment_endpoint_survives_pending_approval_and_reopen_without_operator_authority(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("peers.json");
    let mut directory = PeerDirectory::open_writer(&path).await?;
    directory
        .remember_enrollment_endpoint("peer", "did:key:node", "digest", EXPIRY, ENDPOINT)
        .await?;
    assert!(directory.records().is_empty());
    drop(directory);
    let mut directory = PeerDirectory::open_writer(&path).await?;
    let record = approve(&mut directory, "peer", "did:key:node", "digest").await?;
    assert_eq!(record.graphql.as_deref(), Some(ENDPOINT));
    assert!(record.local_node_home.is_none());
    assert!(record.operator_graphql().is_none());
    assert!(!record.is_managed_runtime());
    assert!(!record.pairing_ready);
    assert!(directory.enrollment_endpoints.is_empty());
    drop(directory);
    let mut directory = PeerDirectory::open_writer(&path).await?;
    let again = approve(&mut directory, "peer", "did:key:node", "digest").await?;
    assert_eq!(again.graphql.as_deref(), Some(ENDPOINT));
    assert!(again.operator_graphql().is_none());
    Ok(())
}

#[tokio::test]
async fn enrollment_endpoint_cannot_cross_its_verified_request_scope() -> Result<()> {
    for (peer, node, digest) in [
        ("other", "did:key:node", "digest"),
        ("peer", "did:key:other", "digest"),
        ("peer", "did:key:node", "other"),
    ] {
        let home = tempfile::tempdir()?;
        let mut directory = PeerDirectory::open_writer(home.path().join("peers.json")).await?;
        directory
            .remember_enrollment_endpoint("peer", "did:key:node", "digest", EXPIRY, ENDPOINT)
            .await?;
        let record = approve(&mut directory, peer, node, digest).await?;
        assert!(record.graphql.is_none());
        assert!(record.operator_graphql().is_none());
    }
    Ok(())
}

#[tokio::test]
async fn expired_enrollment_endpoints_do_not_attach_to_an_approval() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut directory = PeerDirectory::open_writer(home.path().join("peers.json")).await?;
    directory
        .remember_enrollment_endpoint("peer", "did:key:node", "digest", EXPIRY, ENDPOINT)
        .await?;
    directory.enrollment_endpoints[0].expires_at = "2000-01-01T00:00:00Z".into();
    let record = approve(&mut directory, "peer", "did:key:node", "digest").await?;
    assert!(record.graphql.is_none());
    assert!(directory.enrollment_endpoints.is_empty());
    Ok(())
}

#[tokio::test]
async fn failed_endpoint_save_does_not_leak_observation_into_a_later_approval() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut directory = PeerDirectory::open_writer(home.path().join("peers.json")).await?;
    directory.set_fail_persist(true);
    assert!(directory
        .remember_enrollment_endpoint("peer", "did:key:node", "digest", EXPIRY, ENDPOINT)
        .await
        .is_err());
    directory.set_fail_persist(false);
    let record = approve(&mut directory, "peer", "did:key:node", "digest").await?;
    assert!(record.graphql.is_none());
    for endpoint in [
        "https://user:password@runtime.example/api/v0/graphql",
        "not a URL",
    ] {
        assert!(directory
            .remember_enrollment_endpoint("peer", "did:key:node", "digest", EXPIRY, endpoint)
            .await
            .is_err());
    }
    Ok(())
}
