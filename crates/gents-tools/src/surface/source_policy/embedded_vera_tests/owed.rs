use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gents_protocol::delegation_audience::ReportedDelegationAudience;
use gents_store::config_client::owed::settle_owed_while_online;
use gents_store::config_client::user_write::{PresentedDelegation, RefreshRefused};
use gents_store::config_client::{
    prepare_pack_evidence_from_history, ClientSigningCapability, ConfigAccess, ConfigClientError,
    GraphqlEndpoint, PackApprovalScope, PackMutation,
};
use gents_store::local::backend_protection::FreshBackendStore;
use gents_store::local::document_acp::DocumentAcp;

use super::{client_key, home, owner_on_vera, presented, start, tasks, Home};
use crate::test_support::vera_test_home::InProcessPass;

pub(super) fn endpoint_for(
    node: &defra_node::Node,
    user: &ClientSigningCapability,
) -> anyhow::Result<GraphqlEndpoint> {
    let audience = gents_store::local::document_acp::node_delegation_audience(node)
        .expect("a Vera node names its delegation audience");
    Ok(
        GraphqlEndpoint::as_principal("http://127.0.0.1:1/api/v0/graphql", user.did())
            .with_client_signing_capability(user.clone())?
            .with_reported_delegation_audience(ReportedDelegationAudience::Named(audience))?,
    )
}

async fn lapse(lifetime: Duration, minted: SystemTime) {
    let lapsed = minted + lifetime + Duration::from_secs(2);
    while SystemTime::now() <= lapsed {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn create(task: &str) -> PackMutation {
    PackMutation::Create {
        collection: "Task".into(),
        input: serde_json::json!({ "task_id": task }),
    }
}

fn scope(home: &Home, nonce: &str) -> PackApprovalScope {
    PackApprovalScope {
        node_did: home.node.did().to_owned(),
        pack_digests: vec!["sha256:fixture".into()],
        nonce: nonce.into(),
    }
}

/// Two creates of `user` in one transaction of the runtime's process, as a
/// hosted pack session makes them, with `between` run after the first, once
/// the delegation the session opened with has lapsed.
async fn two_creates_across_a_lapse<F>(
    node: &Arc<defra_node::Node>,
    home: &Home,
    user: &ClientSigningCapability,
    presented: PresentedDelegation,
    lifetime: Duration,
    minted: SystemTime,
    between: F,
) -> Result<Vec<String>, ConfigClientError>
where
    F: Fn(&PresentedDelegation) + Send + Sync,
{
    let writes = [
        ("first", scope(home, "owed-first")),
        ("second", scope(home, "owed-second")),
    ];
    let mut evidence = Vec::new();
    for (task, scope) in &writes {
        evidence.push(
            prepare_pack_evidence_from_history(
                user,
                scope,
                &create(task),
                None,
                gents_protocol::schemas::HomePolicyIds::embedded_vera(),
            )
            .await
            .map_err(|error| ConfigClientError::PackEvidence(Box::new(error)))?,
        );
    }
    let user_did =
        identity::Did::new(user.did()).map_err(ConfigClientError::RelationshipTargetIdentity)?;
    let access = ConfigAccess::local_as(node.clone(), user_did)
        .with_presented_delegation(Some(presented.clone()));
    let created = access
        .transact_verified_pack("test.embedded_vera.owed_across_a_lapse", |txn| {
            let (writes, evidence, presented, between) = (
                writes.clone(),
                evidence.clone(),
                presented.clone(),
                &between,
            );
            Box::pin(async move {
                let mut created = Vec::new();
                for (index, ((task, scope), evidence)) in writes.iter().zip(evidence).enumerate() {
                    if index == 1 {
                        lapse(lifetime, minted).await;
                        between(&presented);
                    }
                    let applied = txn
                        .apply(scope, &create(task), evidence)
                        .await
                        .map_err(|error| ConfigClientError::PackEvidence(Box::new(error)))?;
                    created.push(gents_store::graphql::created_doc_id(&applied, "Task")?);
                }
                Ok::<_, ConfigClientError>(created)
            })
        })
        .await
        .map_err(|failed| failed.error)?;
    Ok(created)
}

#[tokio::test]
async fn a_lapsed_delegation_leaves_an_owed_registration_its_client_settles_unasked(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut home = home(directory.path(), "owed").await?;
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
    let lifetime = Duration::from_secs(2);
    let minted = SystemTime::now();
    let opened =
        gents_store::config_client::mint_with_lifetime_for_test(&user, &audience, lifetime)?;
    let session = presented(&node, &user, opened).expect("a delegation of the session's caller");

    let refused = two_creates_across_a_lapse(
        &node,
        &home,
        &user,
        session.clone(),
        lifetime,
        minted,
        |_| {},
    )
    .await
    .expect_err("a session whose delegation lapsed writes nothing more");
    assert!(
        matches!(
            super::refusal(&refused),
            Some(gents_store::config_client::user_write::UserWriteRefusal::DelegationRefused)
        ),
        "{refused:?}"
    );
    assert_eq!(tasks(&node, &home.owner).await?, Vec::<String>::new());

    let minted = SystemTime::now();
    let opened =
        gents_store::config_client::mint_with_lifetime_for_test(&user, &audience, lifetime)?;
    let session = presented(&node, &user, opened).expect("a delegation of the session's caller");
    let endpoint = endpoint_for(&node, &user)?;
    assert!(endpoint.mints_delegations());
    let strangers = endpoint_for(&node, &stranger)?
        .authorization()?
        .expect("a bearer");
    let created =
        two_creates_across_a_lapse(&node, &home, &user, session, lifetime, minted, |session| {
            assert_eq!(
                session.refresh(&node, &strangers),
                Err(RefreshRefused),
                "a session never adopts another identity's delegation"
            );
            session
                .refresh(&node, &endpoint.authorization().unwrap().expect("a bearer"))
                .expect("the session adopts its client's fresh delegation");
        })
        .await?;
    assert_eq!(tasks(&node, &home.owner).await?, ["first", "second"]);
    assert_eq!(
        owner_on_vera(&node, &created[1]).await?,
        Some(user_did.clone()),
        "the write after the refresh registered under the fresh delegation"
    );

    assert_eq!(
        owner_on_vera(&node, &created[0]).await?,
        None,
        "the registration of the write before the lapse is owed"
    );
    let caller = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let owed = node.owed_acp_as(&caller, user.did()).await?;
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0].state, acp::OwedState::AwaitingAuthority);

    let pass = InProcessPass::new(node.clone(), endpoint);
    settle_owed_while_online(&pass).await?;
    assert_eq!(
        owner_on_vera(&node, &created[0]).await?,
        Some(user_did.clone())
    );
    assert!(node.owed_acp_as(&caller, user.did()).await?.is_empty());

    let settled = pass.passes.load(std::sync::atomic::Ordering::Relaxed);
    settle_owed_while_online(&pass).await?;
    assert_eq!(
        pass.passes.load(std::sync::atomic::Ordering::Relaxed),
        settled + 1,
        "a client with nothing owed asks once and stops"
    );

    node.shutdown().await?;
    Ok(())
}
