use std::path::Path;
use std::sync::Arc;

use gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::local::backend_protection::{
    BackendProtectionError, FreshBackendStore, ProtectedBackendStore,
};
use gents_store::local::document_acp::{DocumentAcp, DocumentAcpError};
use gents_store::local::home::{self, StoreLock};
use gents_store::local::store_key::StoreKey;

const RESOURCE: &str = "secret_configuration";
const DOCUMENT: &str = "bae-embedded-vera-owner-only";

struct Home {
    data: std::path::PathBuf,
    lock: StoreLock,
    key: StoreKey,
    node: Arc<dyn NodeIdentity>,
}

async fn home(root: &Path, name: &str) -> anyhow::Result<Home> {
    let home = root.join(name);
    let data = home.join("data");
    let (lock_home, lock_data, key_path) = (home.clone(), data.clone(), home.join("node.key"));
    let (lock, node) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        std::fs::create_dir_all(&lock_data)?;
        let node: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(key_path, None)?);
        Ok((home::lock_store(&lock_home, &lock_data)?, node))
    })
    .await??;
    Ok(Home {
        data,
        lock,
        key: StoreKey::from_host(&[7; 32])?,
        node,
    })
}

async fn start(
    home: &Home,
    document_acp: DocumentAcp,
) -> Result<Arc<defra_node::Node>, BackendProtectionError> {
    let (node, _admission) = ProtectedBackendStore::open(&home.data, &home.key, &home.lock)
        .await?
        .preflight(&home.key, &home.node, document_acp)
        .await?
        .start_node_with_projection(
            &home.key,
            home.node.clone(),
            None,
            None,
            Arc::new(crate::surface::ToolCeiling::meta_only()),
            Vec::new(),
            &gents_session::test_support::no_lifecycle_plugins(),
        )
        .await?;
    Ok(node)
}

async fn reads(node: &defra_node::Node, actor: &identity::Did) -> anyhow::Result<bool> {
    Ok(node
        .document_acp()
        .check_doc_access(
            &acp::Identity::Authenticated(actor.clone()),
            acp::DocumentPermission::Read,
            AGENT_OWNERSHIP_POLICY_ID,
            RESOURCE,
            DOCUMENT,
        )
        .await?)
}

async fn holds_the_node_policies(node: &defra_node::Node, owner: &str) -> anyhow::Result<()> {
    let context = defra_core::RequestContext::default().with_identity(Some(owner.to_owned()));
    for policy in &gents_migration::NODE_POLICIES {
        assert_eq!(
            node.add_dac_policy_with_counter_as(&context, policy.definition, 0)
                .await?,
            policy.id
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_opted_in_home_decides_document_access_through_its_vera_node_across_a_restart(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let home = home(directory.path(), "opted-in").await?;
    let owner = identity::Did::new(home.node.did())?;
    let stranger = {
        let path = directory.path().join("stranger.key");
        let stranger =
            tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(path, None)).await??;
        identity::Did::new(stranger.did())?
    };

    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, DocumentAcp::EmbeddedVera)
        .await?;

    let node = start(&home, DocumentAcp::EmbeddedVera).await?;
    let deployment = node
        .embedded_vera_trust()
        .expect("the node runs its Vera deployment")
        .clone();
    assert!(node.vera_acp().is_some());
    holds_the_node_policies(&node, owner.as_str()).await?;
    for entry in gents_migration::DEFAULT_BASELINE.iter() {
        assert!(node.get_collection(entry.name)?.is_some(), "{}", entry.name);
    }
    node.document_acp()
        .register_doc_object(&owner, AGENT_OWNERSHIP_POLICY_ID, RESOURCE, DOCUMENT)
        .await?;
    assert!(reads(&node, &owner).await?);
    assert!(!reads(&node, &stranger).await?);
    node.shutdown().await?;
    drop(node);

    let node = start(&home, DocumentAcp::EmbeddedVera).await?;
    assert_eq!(node.embedded_vera_trust(), Some(&deployment));
    holds_the_node_policies(&node, owner.as_str()).await?;
    assert!(reads(&node, &owner).await?);
    assert!(!reads(&node, &stranger).await?);
    node.shutdown().await?;
    drop(node);

    assert!(matches!(
        start(&home, DocumentAcp::Local).await,
        Err(BackendProtectionError::DocumentAcp(
            DocumentAcpError::Switch { .. }
        ))
    ));
    Ok(())
}

#[tokio::test]
async fn a_home_that_did_not_opt_in_starts_no_vera_node() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let home = home(directory.path(), "local").await?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, DocumentAcp::Local)
        .await?;
    let node = start(&home, DocumentAcp::Local).await?;
    assert!(node.vera_acp().is_none());
    assert!(node.embedded_vera_trust().is_none());
    holds_the_node_policies(&node, home.node.did()).await?;
    node.shutdown().await?;
    drop(node);
    assert!(!home.data.join("vera").exists());

    assert!(matches!(
        start(&home, DocumentAcp::EmbeddedVera).await,
        Err(BackendProtectionError::DocumentAcp(
            DocumentAcpError::Switch { .. }
        ))
    ));
    assert!(!home.data.join("vera").exists());
    Ok(())
}

#[tokio::test]
async fn a_vera_node_that_cannot_start_refuses_the_home() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let home = home(directory.path(), "broken").await?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, DocumentAcp::EmbeddedVera)
        .await?;
    let deployment = home.data.join("vera").join("node");
    tokio::fs::remove_dir_all(&deployment).await?;
    tokio::fs::write(&deployment, b"not a deployment").await?;

    assert!(matches!(
        start(&home, DocumentAcp::EmbeddedVera).await,
        Err(BackendProtectionError::Node(_))
    ));
    Ok(())
}

#[tokio::test]
async fn an_opted_in_home_is_refused_under_another_deployment_identity() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), "another-identity").await?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, DocumentAcp::EmbeddedVera)
        .await?;
    home.node = {
        let path = directory.path().join("another.key");
        Arc::new(
            tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(path, None)).await??,
        )
    };

    let refused = match start(&home, DocumentAcp::EmbeddedVera).await {
        Ok(_) => anyhow::bail!("the home opened under another deployment identity"),
        Err(refused) => refused,
    };
    assert!(matches!(
        refused,
        BackendProtectionError::DocumentAcp(DocumentAcpError::DeploymentDiffers { .. })
    ));
    assert_eq!(
        gents_store::local::storage_backend::incompatible_store(&refused, &home.data)
            .map(|store| store.kind),
        Some(gents_protocol::operations::install_repair::IncompatibleStoreKind::OtherVersionSchema)
    );
    Ok(())
}
