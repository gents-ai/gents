use std::path::Path;
use std::sync::Arc;

use crate::test_support::vera_test_home::{as_user, client_key, home, serve, Home};
use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID;
use gents_store::config_client::user_write::{PresentedDelegation, UserWriteRefusal};
use gents_store::config_client::{
    prepare_pack_evidence_from_history, ConfigClientError, PackApprovalScope, PackMutation,
};
use gents_store::config_client::{ClientSigningCapability, ConfigAccess, GraphqlEndpoint};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::local::backend_protection::{
    BackendProtectionError, FreshBackendStore, ProtectedBackendStore,
};
use gents_store::local::document_acp::{DocumentAcp, DocumentAcpError};

const RESOURCE: &str = "secret_configuration";
const DOCUMENT: &str = "bae-embedded-vera-owner-only";
const AGENT_DOCUMENT: &str = "bae-embedded-vera-agent-owned";

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
            std::convert::identity,
            Arc::new(crate::surface::ToolCeiling::meta_only()),
            Vec::new(),
            &gents_session::test_support::no_lifecycle_plugins(),
        )
        .await?;
    Ok(node)
}

async fn reads(node: &defra_node::Node, actor: &identity::Did) -> anyhow::Result<bool> {
    reads_document(node, actor, DOCUMENT).await
}

async fn reads_document(
    node: &defra_node::Node,
    actor: &identity::Did,
    document: &str,
) -> anyhow::Result<bool> {
    Ok(node
        .document_acp()
        .check_doc_access(
            &acp::Identity::Authenticated(actor.clone()),
            acp::DocumentPermission::Read,
            AGENT_OWNERSHIP_POLICY_ID,
            RESOURCE,
            document,
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
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Vera)
        .await?;

    let node = start(&home, DocumentAcp::Vera).await?;
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
    let agent = identity::Did::new(
        &gents_store::identity::provision_agent_identity(home.node.as_ref())?.agent_did,
    )?;
    node.document_acp()
        .register_doc_object(&agent, AGENT_OWNERSHIP_POLICY_ID, RESOURCE, AGENT_DOCUMENT)
        .await?;
    assert!(reads_document(&node, &agent, AGENT_DOCUMENT).await?);
    assert!(!reads_document(&node, &stranger, AGENT_DOCUMENT).await?);
    node.shutdown().await?;
    drop(node);

    let node = start(&home, DocumentAcp::Vera).await?;
    assert_eq!(node.embedded_vera_trust(), Some(&deployment));
    holds_the_node_policies(&node, owner.as_str()).await?;
    assert!(reads(&node, &owner).await?);
    assert!(!reads(&node, &stranger).await?);
    assert!(reads_document(&node, &agent, AGENT_DOCUMENT).await?);
    assert!(!reads_document(&node, &stranger, AGENT_DOCUMENT).await?);
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
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Local)
        .await?;
    let node = start(&home, DocumentAcp::Local).await?;
    assert!(node.vera_acp().is_none());
    assert!(node.embedded_vera_trust().is_none());
    holds_the_node_policies(&node, home.node.did()).await?;
    node.shutdown().await?;
    drop(node);
    assert!(!home.data.join("vera").exists());

    assert!(matches!(
        start(&home, DocumentAcp::Vera).await,
        Err(BackendProtectionError::DocumentAcp(
            DocumentAcpError::Switch { .. }
        ))
    ));
    assert!(!home.data.join("vera").exists());
    Ok(())
}

async fn refused(home: &Home) -> anyhow::Result<BackendProtectionError> {
    match start(home, DocumentAcp::Vera).await {
        Ok(_) => anyhow::bail!("the home opened"),
        Err(refused) => Ok(refused),
    }
}

fn damaged(refused: &BackendProtectionError, home: &Home) -> bool {
    gents_store::local::storage_backend::incompatible_store(refused, &home.data)
        .map(|store| store.kind)
        == Some(IncompatibleStoreKind::UnsupportedOrCorrupt)
}

async fn opted_in_home_with_a_document(root: &Path, name: &str) -> anyhow::Result<Home> {
    let home = home(root, name).await?;
    let owner = identity::Did::new(home.node.did())?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Vera)
        .await?;
    let node = start(&home, DocumentAcp::Vera).await?;
    node.document_acp()
        .register_doc_object(&owner, AGENT_OWNERSHIP_POLICY_ID, RESOURCE, DOCUMENT)
        .await?;
    assert!(reads(&node, &owner).await?);
    node.shutdown().await?;
    Ok(home)
}

#[tokio::test]
async fn a_home_whose_vera_directory_is_lost_is_refused_and_nothing_is_created(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let home = opted_in_home_with_a_document(directory.path(), "lost").await?;
    let vera = home.data.join("vera");
    let kept = directory.path().join("kept");
    tokio::fs::rename(&vera, &kept).await?;

    for _ in 0..2 {
        let refused = refused(&home).await?;
        assert!(matches!(
            refused,
            BackendProtectionError::DocumentAcp(DocumentAcpError::DeploymentMissing { .. })
        ));
        assert!(damaged(&refused, &home));
        assert!(!vera.exists());
    }

    tokio::fs::create_dir(&vera).await?;
    assert!(matches!(
        refused(&home).await?,
        BackendProtectionError::DocumentAcp(DocumentAcpError::DeploymentMissing { .. })
    ));
    assert!(!vera.join("node").exists());

    tokio::fs::remove_dir(&vera).await?;
    tokio::fs::rename(&kept, &vera).await?;
    let node = start(&home, DocumentAcp::Vera).await?;
    assert!(reads(&node, &identity::Did::new(home.node.did())?).await?);
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_home_whose_vera_directory_is_swapped_for_another_deployment_is_refused(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let home = opted_in_home_with_a_document(directory.path(), "swapped").await?;
    let twin_home = directory.path().join("twin");
    tokio::fs::create_dir(&twin_home).await?;
    tokio::fs::copy(
        directory.path().join("swapped").join("node.key"),
        twin_home.join("node.key"),
    )
    .await?;
    let twin = self::home(directory.path(), "twin").await?;
    assert_eq!(twin.node.did(), home.node.did());
    FreshBackendStore::observe(&twin.data, &twin.lock)
        .await?
        .initialize(&twin.key, &twin.node, &twin.owner, DocumentAcp::Vera)
        .await?;

    let vera = home.data.join("vera");
    let kept = directory.path().join("kept");
    tokio::fs::rename(&vera, &kept).await?;
    tokio::fs::rename(twin.data.join("vera"), &vera).await?;

    for _ in 0..2 {
        let refused = refused(&home).await?;
        assert!(matches!(
            refused,
            BackendProtectionError::DocumentAcp(DocumentAcpError::DeploymentReplaced { .. })
        ));
        assert!(damaged(&refused, &home));
    }

    tokio::fs::remove_dir_all(&vera).await?;
    tokio::fs::rename(&kept, &vera).await?;
    let node = start(&home, DocumentAcp::Vera).await?;
    assert!(reads(&node, &identity::Did::new(home.node.did())?).await?);
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn an_opted_in_home_is_refused_under_another_deployment_identity() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), "another-identity").await?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Vera)
        .await?;
    home.node = {
        let path = directory.path().join("another.key");
        Arc::new(
            tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(path, None)).await??,
        )
    };

    let refused = refused(&home).await?;
    assert!(matches!(
        refused,
        BackendProtectionError::DocumentAcp(DocumentAcpError::DeploymentDiffers { .. })
    ));
    assert_eq!(
        gents_store::local::storage_backend::incompatible_store(&refused, &home.data)
            .map(|store| store.kind),
        Some(IncompatibleStoreKind::OtherVersionSchema)
    );
    Ok(())
}

const TASK_RESOURCE: &str = "configuration";

async fn create_task(access: &ConfigAccess, task: &str) -> anyhow::Result<String> {
    let created = access
        .write_with_variables(
            "test.embedded_vera.create_task",
            "mutation($task: String) { create_Task(input: {task_id: $task}) { _docID } }",
            &serde_json::json!({ "task": task }),
        )
        .await?;
    Ok(gents_store::graphql::created_doc_id(&created, "Task")?)
}

/// The values of `field` across every row of `collection`, read as the
/// home's NAC owner, whom native NAC lets read past document ACP: what the
/// store holds, whoever owns each row.
async fn stored(
    node: &defra_node::Node,
    owner: &identity::Did,
    collection: &str,
    field: &str,
) -> anyhow::Result<Vec<String>> {
    let response = node
        .execute_request(
            query::QueryRequest::new(format!("{{ {collection} {{ {field} }} }}"))
                .with_identity(Some(owner.clone())),
        )
        .await;
    anyhow::ensure!(response.errors.is_empty(), "{:?}", response.errors);
    let data = response.data.unwrap_or_default();
    Ok(data[collection]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row[field].as_str().map(str::to_owned))
        .collect())
}

async fn tasks(node: &defra_node::Node, owner: &identity::Did) -> anyhow::Result<Vec<String>> {
    stored(node, owner, "Task", "task_id").await
}

/// The owner Vera itself records, beneath the registrations DefraDB still owes.
async fn owner_on_vera(
    node: &defra_node::Node,
    document: &str,
) -> anyhow::Result<Option<identity::Did>> {
    use acp::DocumentACP as _;
    Ok(node
        .vera_acp()
        .expect("the node decides document access through Vera")
        .get_doc_owner(AGENT_OWNERSHIP_POLICY_ID, TASK_RESOURCE, document)
        .await?)
}

async fn reads_task(
    node: &defra_node::Node,
    actor: &identity::Did,
    document: &str,
) -> anyhow::Result<bool> {
    Ok(node
        .document_acp()
        .check_doc_access(
            &acp::Identity::Authenticated(actor.clone()),
            acp::DocumentPermission::Read,
            AGENT_OWNERSHIP_POLICY_ID,
            TASK_RESOURCE,
            document,
        )
        .await?)
}

#[tokio::test]
async fn a_user_writes_over_http_under_a_delegation_its_client_minted() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), "opted-in").await?;
    let user = client_key(directory.path(), "user.key").await?;
    let stranger = client_key(directory.path(), "stranger.key").await?;
    let user_did = identity::Did::new(user.did())?;
    home.owner = user_did.clone();
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Vera)
        .await?;
    let (node, url) = serve(&home, DocumentAcp::Vera).await?;
    let stranger_did = identity::Did::new(stranger.did())?;
    assert!(defra_core::signing::get_identity(user.did()).is_none());

    let audience = gents_store::local::document_acp::node_delegation_audience(&node)
        .expect("a Vera node names its delegation audience");
    let reported =
        gents_store::local::document_acp::status_reports(DocumentAcp::Vera, Some(audience.clone()));

    let elsewhere = ReportedDelegationAudience::Named(DelegationAudience {
        worker: stranger.did().to_owned(),
        deployment: audience.deployment,
        genesis_id: None,
    });
    assert!(
        create_task(&as_user(&url, &user, elsewhere)?, "other-worker")
            .await
            .is_err()
    );

    let another_deployment = ReportedDelegationAudience::Named(DelegationAudience {
        worker: audience.worker.clone(),
        deployment: audience.deployment + 1,
        genesis_id: None,
    });
    assert!(create_task(
        &as_user(&url, &user, another_deployment)?,
        "other-deployment"
    )
    .await
    .is_err());

    let short = std::time::Duration::from_secs(1);
    let expiring =
        gents_store::config_client::mint_with_lifetime_for_test(&user, &audience, short)?;
    let expired = std::time::SystemTime::now() + short + std::time::Duration::from_secs(1);
    while std::time::SystemTime::now() <= expired {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stale = ConfigAccess::Graphql(GraphqlEndpoint::with_delegated_authorization(
        &url,
        &format!("Bearer {expiring}"),
    )?);
    assert!(create_task(&stale, "expired").await.is_err());

    assert_eq!(tasks(&node, &home.owner).await?, Vec::<String>::new());

    let created = create_task(&as_user(&url, &user, reported)?, "delegated").await?;
    assert_eq!(tasks(&node, &home.owner).await?, ["delegated"]);
    assert_eq!(
        owner_on_vera(&node, &created).await?,
        Some(user_did.clone())
    );
    assert!(reads_task(&node, &user_did, &created).await?);
    assert!(!reads_task(&node, &stranger_did, &created).await?);

    let node_did = identity::Did::new(home.node.did())?;
    let as_node = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(&url, home.node.did())
            .with_client_signing_capability(home.node.client_signing_capability()?)?,
    );
    let by_node = create_task(&as_node, "by-node").await?;
    assert_eq!(owner_on_vera(&node, &by_node).await?, Some(node_did));
    assert!(!reads_task(&node, &stranger_did, &by_node).await?);

    let refused = create_task(
        &as_user(&url, &user, ReportedDelegationAudience::Absent)?,
        "identity-token",
    )
    .await;
    assert!(
        refused.is_err(),
        "an identity token is refused on a Vera home"
    );
    assert_eq!(tasks(&node, &home.owner).await?, ["delegated", "by-node"]);

    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_user_writes_over_http_on_the_node_store_as_before() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), "local").await?;
    let user = client_key(directory.path(), "user.key").await?;
    let stranger = client_key(directory.path(), "stranger.key").await?;
    let user_did = identity::Did::new(user.did())?;
    home.owner = user_did.clone();
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Local)
        .await?;
    let (node, url) = serve(&home, DocumentAcp::Local).await?;

    let audience = gents_store::local::document_acp::node_delegation_audience(&node);
    assert_eq!(audience, None);
    let reported = gents_store::local::document_acp::status_reports(DocumentAcp::Local, audience);
    assert_eq!(reported, ReportedDelegationAudience::Absent);

    let created = create_task(&as_user(&url, &user, reported)?, "identity-token").await?;
    assert_eq!(tasks(&node, &home.owner).await?, ["identity-token"]);
    assert!(reads_task(&node, &user_did, &created).await?);
    assert!(!reads_task(&node, &identity::Did::new(stranger.did())?, &created).await?);

    node.shutdown().await?;
    Ok(())
}

fn presented(
    node: &defra_node::Node,
    user: &ClientSigningCapability,
    token: String,
) -> Option<PresentedDelegation> {
    let context = defra_core::RequestContext::default()
        .with_identity(Some(user.did().to_owned()))
        .with_bearer(Some(user.did().to_owned()), Some(token));
    PresentedDelegation::of_request(node, &context, user.did())
}

/// A User's create the runtime makes in its own process from the graph that
/// User's client signed, as a hosted pack install does.
async fn create_task_in_process(
    node: &Arc<defra_node::Node>,
    home: &Home,
    user: &ClientSigningCapability,
    presented: Option<PresentedDelegation>,
    task: &str,
) -> Result<String, ConfigClientError> {
    let scope = PackApprovalScope {
        node_did: home.node.did().to_owned(),
        pack_digests: vec!["sha256:fixture".into()],
        nonce: "embedded-vera".into(),
    };
    let mutation = PackMutation::Create {
        collection: "Task".into(),
        input: serde_json::json!({ "task_id": task }),
    };
    let evidence = prepare_pack_evidence_from_history(user, &scope, &mutation, None)
        .await
        .map_err(|error| ConfigClientError::PackEvidence(Box::new(error)))?;
    let user_did =
        identity::Did::new(user.did()).map_err(ConfigClientError::RelationshipTargetIdentity)?;
    let access =
        ConfigAccess::local_as(node.clone(), user_did).with_presented_delegation(presented);
    let created = access
        .transact_verified_pack("test.embedded_vera.in_process_create", |txn| {
            let (scope, mutation, evidence) = (scope.clone(), mutation.clone(), evidence.clone());
            Box::pin(async move {
                txn.apply(&scope, &mutation, evidence)
                    .await
                    .map_err(|error| ConfigClientError::PackEvidence(Box::new(error)))
            })
        })
        .await
        .map_err(|failed| failed.error)?;
    Ok(gents_store::graphql::created_doc_id(&created, "Task")?)
}

fn refusal(error: &ConfigClientError) -> Option<UserWriteRefusal> {
    match error {
        ConfigClientError::UserWrite(refusal) => Some(*refusal),
        ConfigClientError::PackEvidence(evidence) => match evidence.as_ref() {
            gents_store::config_client::PackEvidenceError::Config(inner) => refusal(inner),
            _ => None,
        },
        _ => None,
    }
}

#[tokio::test]
async fn a_client_signed_create_in_the_runtime_process_is_owned_by_its_user_on_vera(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), "in-process").await?;
    let user = client_key(directory.path(), "user.key").await?;
    let stranger = client_key(directory.path(), "stranger.key").await?;
    let user_did = identity::Did::new(user.did())?;
    home.owner = user_did.clone();
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Vera)
        .await?;
    let node = start(&home, DocumentAcp::Vera).await?;
    let audience = gents_store::local::document_acp::node_delegation_audience(&node)
        .expect("a Vera node names its delegation audience");
    let lifetime = gents_store::config_client::VERA_DELEGATION_LIFETIME;

    let unsigned = create_task_in_process(&node, &home, &user, None, "no-delegation")
        .await
        .expect_err("a User's write without a delegation is refused");
    assert_eq!(
        refusal(&unsigned),
        Some(UserWriteRefusal::DelegationRequired)
    );

    let elsewhere = DelegationAudience {
        worker: stranger.did().to_owned(),
        deployment: audience.deployment,
        genesis_id: None,
    };
    let foreign =
        gents_store::config_client::mint_with_lifetime_for_test(&user, &elsewhere, lifetime)?;
    let refused = create_task_in_process(
        &node,
        &home,
        &user,
        presented(&node, &user, foreign),
        "other-worker",
    )
    .await
    .expect_err("a delegation for another worker writes nothing");
    assert_eq!(refusal(&refused), Some(UserWriteRefusal::DelegationRefused));

    let short = std::time::Duration::from_secs(1);
    let expiring =
        gents_store::config_client::mint_with_lifetime_for_test(&user, &audience, short)?;
    let expired = std::time::SystemTime::now() + short + std::time::Duration::from_secs(1);
    while std::time::SystemTime::now() <= expired {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let stale = create_task_in_process(
        &node,
        &home,
        &user,
        presented(&node, &user, expiring),
        "expired",
    )
    .await
    .expect_err("an expired delegation writes nothing");
    assert_eq!(refusal(&stale), Some(UserWriteRefusal::DelegationRefused));

    let strangers =
        gents_store::config_client::mint_with_lifetime_for_test(&stranger, &audience, lifetime)?;
    let context = defra_core::RequestContext::default()
        .with_identity(Some(stranger.did().to_owned()))
        .with_bearer(Some(stranger.did().to_owned()), Some(strangers));
    let lifted = PresentedDelegation::of_request(&node, &context, stranger.did());
    let lifted = create_task_in_process(&node, &home, &user, lifted, "lifted")
        .await
        .expect_err("another identity's delegation does not carry this User's write");
    assert_eq!(refusal(&lifted), Some(UserWriteRefusal::DelegationRequired));
    assert_eq!(tasks(&node, &home.owner).await?, Vec::<String>::new());

    let token =
        gents_store::config_client::mint_with_lifetime_for_test(&user, &audience, lifetime)?;
    let created = create_task_in_process(
        &node,
        &home,
        &user,
        presented(&node, &user, token),
        "in-process",
    )
    .await?;
    assert!(defra_core::signing::get_identity(user.did()).is_none());
    assert_eq!(tasks(&node, &home.owner).await?, ["in-process"]);
    assert_eq!(
        owner_on_vera(&node, &created).await?,
        Some(user_did.clone())
    );
    assert!(reads_task(&node, &user_did, &created).await?);
    assert!(!reads_task(&node, &identity::Did::new(stranger.did())?, &created).await?);

    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_client_signed_create_in_the_runtime_process_on_the_node_store_is_unchanged(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), "in-process-local").await?;
    let user = client_key(directory.path(), "user.key").await?;
    let user_did = identity::Did::new(user.did())?;
    home.owner = user_did.clone();
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Local)
        .await?;
    let node = start(&home, DocumentAcp::Local).await?;
    let created = create_task_in_process(&node, &home, &user, None, "no-delegation").await?;
    assert_eq!(tasks(&node, &home.owner).await?, ["no-delegation"]);
    assert!(reads_task(&node, &user_did, &created).await?);
    node.shutdown().await?;
    Ok(())
}

mod client_request;
mod configuration_readers;
mod enrollment_root;
mod heartbeats;
mod in_txn;
mod latency;
mod learned_audience;
mod managers;
mod owed;
mod owner_staged;
mod parallel_owed;
mod publication;
mod quickstart;
mod rebinding;
mod restart;
mod stored_create;
mod stored_edit;
mod user_apply;
