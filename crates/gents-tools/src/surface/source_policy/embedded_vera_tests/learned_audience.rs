use std::sync::Arc;

use futures::future::BoxFuture;
use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_store::config_client::{
    ClientSigningCapability, ConfigAccess, GraphqlEndpoint, ResolveAudience,
};
use gents_store::local::backend_protection::FreshBackendStore;
use gents_store::local::document_acp::DocumentAcp;

use super::{client_key, create_task, home, owner_on_vera, serve, tasks};

/// What the runtime's public status reports now, as the desktop reads it.
struct PublicStatus(ReportedDelegationAudience);

impl ResolveAudience for PublicStatus {
    fn resolve(&self) -> BoxFuture<'_, Option<ReportedDelegationAudience>> {
        Box::pin(async { Some(self.0.clone()) })
    }
}

fn desktop_client(
    url: &str,
    user: &ClientSigningCapability,
    held: ReportedDelegationAudience,
    status: ReportedDelegationAudience,
) -> anyhow::Result<GraphqlEndpoint> {
    Ok(GraphqlEndpoint::as_principal(url, user.did())
        .with_client_signing_capability(user.clone())?
        .with_reported_delegation_audience(held)?
        .with_audience_resolver(Arc::new(PublicStatus(status))))
}

#[tokio::test]
async fn a_client_that_learns_its_audience_from_the_status_writes_as_its_user() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let home = home(directory.path(), "learned").await?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, DocumentAcp::Vera)
        .await?;
    let (node, url) = serve(&home, DocumentAcp::Vera).await?;
    let user = client_key(directory.path(), "user.key").await?;
    let user_did = identity::Did::new(user.did())?;
    ConfigAccess::local_as(node.clone(), identity::Did::new(home.node.did())?)
        .grant_node_admin(&user_did)
        .await?;
    let audience = gents_store::local::document_acp::node_delegation_audience(&node)
        .expect("a Vera node names its delegation audience");
    let status =
        gents_store::local::document_acp::status_reports(DocumentAcp::Vera, Some(audience.clone()));

    let fresh = desktop_client(
        &url,
        &user,
        ReportedDelegationAudience::Absent,
        status.clone(),
    )?;
    let learned = create_task(&ConfigAccess::Graphql(fresh.clone()), "learned").await?;
    assert!(
        fresh.mints_delegations(),
        "the client keeps what it learned"
    );
    assert_eq!(
        owner_on_vera(&node, &learned).await?,
        Some(user_did.clone())
    );

    let earlier_runtime = ReportedDelegationAudience::Named(DelegationAudience {
        worker: audience.worker.clone(),
        deployment: audience.deployment + 1,
    });
    let stale = desktop_client(&url, &user, earlier_runtime, status)?;
    let recovered = create_task(&ConfigAccess::Graphql(stale), "recovered").await?;
    assert_eq!(owner_on_vera(&node, &recovered).await?, Some(user_did));

    let local_status = desktop_client(
        &url,
        &user,
        ReportedDelegationAudience::Absent,
        ReportedDelegationAudience::Absent,
    )?;
    assert!(
        create_task(&ConfigAccess::Graphql(local_status), "unresolved")
            .await
            .is_err(),
        "a status naming no audience leaves the identity token refused"
    );
    assert_eq!(tasks(&node).await?, ["learned", "recovered"]);

    node.shutdown().await?;
    Ok(())
}
