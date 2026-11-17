use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use futures::future::BoxFuture;
use gents_protocol::delegation_audience::ReportedDelegationAudience;
use gents_store::config_client::{
    ClientSigningCapability, ConfigAccess, GraphqlEndpoint, ResolveAudience,
};
use gents_store::local::backend_protection::{FreshBackendStore, ProtectedBackendStore};
use gents_store::local::document_acp::{DocumentAcp, HttpEdge};

use super::{client_key, create_task, home, owner_on_vera, tasks, Home};

/// The runtime's public status as the host projects it: its lifecycle, and
/// the audience its running node reports (`document_acp::status_reports`).
/// `names_audience` off stands for a runtime that names none.
#[derive(Clone, Default)]
struct Status {
    node: Arc<OnceLock<Weak<defra_node::Node>>>,
    names_audience: Arc<AtomicBool>,
}

impl Status {
    fn payload(&self) -> serde_json::Value {
        let mut status = serde_json::json!({ "lifecycle": "ready" });
        let node = self.node.get().and_then(Weak::upgrade);
        if let (true, Some(node)) = (self.names_audience.load(Ordering::SeqCst), node) {
            let audience = gents_store::local::document_acp::node_delegation_audience(&node);
            if let Some(reported) =
                gents_store::local::document_acp::status_reports(DocumentAcp::Vera, audience)
                    .status_value()
            {
                status[gents_protocol::delegation_audience::STATUS_DELEGATION_AUDIENCE_FIELD] =
                    reported;
            }
        }
        status
    }
}

/// Start a runtime on `home` at `address`, serving its public status.
async fn serve_at(
    home: &Home,
    address: std::net::SocketAddr,
) -> anyhow::Result<(Arc<defra_node::Node>, Status)> {
    let listener =
        tokio::task::spawn_blocking(move || std::net::TcpListener::bind(address)).await??;
    let status = Status {
        names_audience: Arc::new(AtomicBool::new(true)),
        ..Status::default()
    };
    let served = status.clone();
    let (node, _admission) = ProtectedBackendStore::open(&home.data, &home.key, &home.lock)
        .await?
        .preflight(&home.key, &home.node, DocumentAcp::Vera)
        .await?
        .start_node_with_projection(
            &home.key,
            home.node.clone(),
            Some(
                defra_node::HttpConfig::with_listener(listener)?.with_router_transform(Arc::new({
                    let edge = HttpEdge::new(DocumentAcp::Vera, home.node.did());
                    move |router: axum::Router| {
                        let served = served.clone();
                        edge.wrap(router.route(
                            "/status",
                            axum::routing::get(move || {
                                let served = served.clone();
                                async move { axum::Json(served.payload()) }
                            }),
                        ))
                    }
                })),
            ),
            None,
            Arc::new(crate::surface::ToolCeiling::meta_only()),
            Vec::new(),
            &gents_session::test_support::no_lifecycle_plugins(),
        )
        .await?;
    let _ = status.node.set(Arc::downgrade(&node));
    Ok((node, status))
}

/// Reads the runtime's public status over HTTP with no credential, as a
/// client does after a refusal.
struct PublicStatus {
    url: String,
}

impl ResolveAudience for PublicStatus {
    fn resolve(&self) -> BoxFuture<'_, Option<ReportedDelegationAudience>> {
        Box::pin(async move {
            let status = reqwest::get(&self.url)
                .await
                .ok()?
                .json::<serde_json::Value>()
                .await
                .ok()?;
            Some(ReportedDelegationAudience::observe(&status))
        })
    }
}

async fn vera_home(root: &std::path::Path, name: &str) -> anyhow::Result<Home> {
    let home = home(root, name).await?;
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, DocumentAcp::Vera)
        .await?;
    Ok(home)
}

async fn admit(
    node: &Arc<defra_node::Node>,
    home: &Home,
    user: &ClientSigningCapability,
) -> anyhow::Result<()> {
    ConfigAccess::local_as(node.clone(), identity::Did::new(home.node.did())?)
        .grant_node_admin(&identity::Did::new(user.did())?)
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_client_recovers_when_its_runtime_comes_back_as_another_deployment() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let first = vera_home(directory.path(), "first").await?;
    let second = vera_home(directory.path(), "second").await?;
    let address = tokio::task::spawn_blocking(|| {
        std::net::TcpListener::bind("127.0.0.1:0").and_then(|listener| listener.local_addr())
    })
    .await??;
    let url = format!("http://{address}/api/v0/graphql");
    let user = client_key(directory.path(), "user.key").await?;
    let user_did = identity::Did::new(user.did())?;
    let client = GraphqlEndpoint::as_principal(&url, user.did())
        .with_client_signing_capability(user.clone())?
        .with_reported_delegation_audience(ReportedDelegationAudience::Absent)?
        .with_audience_resolver(Arc::new(PublicStatus {
            url: format!("http://{address}/status"),
        }));
    let access = ConfigAccess::Graphql(client.clone());

    let (node, _status) = serve_at(&first, address).await?;
    admit(&node, &first, &user).await?;
    let before = gents_store::local::document_acp::node_delegation_audience(&node);
    let learned = create_task(&access, "before").await?;
    assert_eq!(
        owner_on_vera(&node, &learned).await?,
        Some(user_did.clone())
    );
    node.shutdown().await?;
    drop(node);

    let (node, status) = serve_at(&second, address).await?;
    admit(&node, &second, &user).await?;
    let after = gents_store::local::document_acp::node_delegation_audience(&node);
    assert!(before.is_some() && after.is_some() && before != after);
    let recovered = create_task(&access, "after").await?;
    assert_eq!(owner_on_vera(&node, &recovered).await?, Some(user_did));
    assert_eq!(tasks(&node).await?, ["after"]);

    status.names_audience.store(false, Ordering::SeqCst);
    let unresolved = GraphqlEndpoint::as_principal(&url, user.did())
        .with_client_signing_capability(user.clone())?
        .with_reported_delegation_audience(ReportedDelegationAudience::Absent)?
        .with_audience_resolver(Arc::new(PublicStatus {
            url: format!("http://{address}/status"),
        }));
    assert!(
        create_task(&ConfigAccess::Graphql(unresolved), "unresolved")
            .await
            .is_err(),
        "a status naming no audience leaves the identity token refused"
    );
    assert_eq!(tasks(&node).await?, ["after"]);
    node.shutdown().await?;
    Ok(())
}
