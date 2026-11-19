use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use futures::future::BoxFuture;
use gents_protocol::delegation_audience::ReportedDelegationAudience;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint, ResolveAudience};
use gents_store::local::backend_protection::{FreshBackendStore, ProtectedBackendStore};
use gents_store::local::document_acp::{DocumentAcp, HttpEdge};

use super::{client_key, create_task, home, owner_on_vera, tasks, Home};

/// The runtime's public status as the host projects it: its lifecycle, and
/// the audience its running node reports (`document_acp::status_reports`).
/// `names_audience` off stands for a runtime that names none.
#[derive(Clone)]
struct Status {
    identity: Arc<dyn gents_store::identity::NodeIdentity>,
    node: Arc<OnceLock<Weak<defra_node::Node>>>,
    names_audience: Arc<AtomicBool>,
    /// The node the status names when it is not this runtime's own: an
    /// impostor can name any node, but signs only with its own key.
    claims: Option<String>,
}

impl Status {
    /// The status signed by the node key for the read that sent `nonce`.
    async fn payload(&self, nonce: Option<&str>) -> serde_json::Value {
        let node = self.node.get().and_then(Weak::upgrade);
        let reports = match (self.names_audience.load(Ordering::SeqCst), node) {
            (true, Some(node)) => gents_store::local::document_acp::status_reports(
                DocumentAcp::Vera,
                gents_store::local::document_acp::node_delegation_audience(&node),
            ),
            _ => ReportedDelegationAudience::Absent,
        };
        let Some(nonce) =
            nonce.and_then(|nonce| gents_protocol::home_attestation::HomeNonce::parse(nonce).ok())
        else {
            return serde_json::json!({ "lifecycle": "ready" });
        };
        let mut status = gents_store::test_support::signed_home_status(
            self.identity.as_ref(),
            &reports,
            None,
            &nonce,
        )
        .await;
        status["lifecycle"] = "ready".into();
        if let Some(claimed) = &self.claims {
            status["node_did"] = claimed.as_str().into();
            status[gents_protocol::home_attestation::STATUS_HOME_ATTESTATION_FIELD]["statement"]
                ["node_did"] = claimed.as_str().into();
        }
        status
    }
}

/// Start a runtime on `home` at `address`, serving its public status.
async fn serve_at(
    home: &Home,
    address: std::net::SocketAddr,
) -> anyhow::Result<(Arc<defra_node::Node>, Status)> {
    serve_claiming(home, address, None).await
}

async fn serve_claiming(
    home: &Home,
    address: std::net::SocketAddr,
    claims: Option<String>,
) -> anyhow::Result<(Arc<defra_node::Node>, Status)> {
    let listener =
        tokio::task::spawn_blocking(move || std::net::TcpListener::bind(address)).await??;
    let status = Status {
        identity: home.node.clone(),
        node: Arc::default(),
        names_audience: Arc::new(AtomicBool::new(true)),
        claims,
    };
    let served = status.clone();
    let (node, _admission) = ProtectedBackendStore::open(&home.data, &home.key, &home.lock)
        .await?
        .preflight(&home.key, &home.node, DocumentAcp::Vera)
        .await?
        .start_node_with_projection(
            &home.key,
            home.node.clone(),
            {
                let http = defra_node::HttpConfig::with_listener(listener)?.with_router_transform(
                    Arc::new({
                        let edge = HttpEdge::new(DocumentAcp::Vera, home.node.did());
                        move |router: axum::Router| {
                            let served = served.clone();
                            edge.wrap(router.route(
                                "/status",
                                axum::routing::get(
                                    move |query: axum::extract::Query<
                                        rapidhash::RapidHashMap<String, String>,
                                    >| {
                                        let served = served.clone();
                                        async move {
                                            axum::Json(
                                                served
                                                    .payload(
                                                        query
                                                            .get(gents_protocol::home_attestation::HOME_NONCE_QUERY)
                                                            .map(String::as_str),
                                                    )
                                                    .await,
                                            )
                                        }
                                    },
                                ),
                            ))
                        }
                    }),
                );
                move |builder: defra_node::NodeBuilder| builder.with_http(http)
            },
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
    fn resolve<'a>(
        &'a self,
        nonce: &'a gents_protocol::home_attestation::HomeNonce,
    ) -> BoxFuture<'a, Option<serde_json::Value>> {
        Box::pin(async move {
            reqwest::Client::new()
                .get(&self.url)
                .query(&[(
                    gents_protocol::home_attestation::HOME_NONCE_QUERY,
                    nonce.as_str(),
                )])
                .send()
                .await
                .ok()?
                .json::<serde_json::Value>()
                .await
                .ok()
        })
    }
}

async fn vera_home(
    root: &std::path::Path,
    name: &str,
    owner: &identity::Did,
) -> anyhow::Result<Home> {
    let mut home = home(root, name).await?;
    home.owner = owner.clone();
    FreshBackendStore::observe(&home.data, &home.lock)
        .await?
        .initialize(&home.key, &home.node, &home.owner, DocumentAcp::Vera)
        .await?;
    Ok(home)
}

#[tokio::test]
async fn a_client_recovers_when_its_runtime_comes_back_as_another_deployment() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let user = client_key(directory.path(), "user.key").await?;
    let user_did = identity::Did::new(user.did())?;
    let first = vera_home(directory.path(), "first", &user_did).await?;
    let (first_key, second_key) = (
        directory.path().join("first/node.key"),
        directory.path().join("second/node.key"),
    );
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(second_key.parent().expect("a home directory"))?;
        std::fs::copy(first_key, second_key).map(drop)
    })
    .await??;
    let second = vera_home(directory.path(), "second", &user_did).await?;
    assert_eq!(
        first.node.did(),
        second.node.did(),
        "the same home's node key"
    );
    let address = tokio::task::spawn_blocking(|| {
        std::net::TcpListener::bind("127.0.0.1:0").and_then(|listener| listener.local_addr())
    })
    .await??;
    let url = format!("http://{address}/api/v0/graphql");
    let client = GraphqlEndpoint::as_principal(&url, user.did())
        .with_client_signing_capability(user.clone())?
        .with_reported_delegation_audience(ReportedDelegationAudience::Absent)?
        .with_audience_resolver(
            first.node.did(),
            Arc::new(PublicStatus {
                url: format!("http://{address}/status"),
            }),
        );
    let access = ConfigAccess::Graphql(client.clone());

    let (node, _status) = serve_at(&first, address).await?;
    let before = gents_store::local::document_acp::node_delegation_audience(&node);
    let learned = create_task(&access, "before").await?;
    assert_eq!(
        owner_on_vera(&node, &learned).await?,
        Some(user_did.clone())
    );
    node.shutdown().await?;
    drop(node);

    let (node, status) = serve_at(&second, address).await?;
    let after = gents_store::local::document_acp::node_delegation_audience(&node);
    assert!(before.is_some() && after.is_some() && before != after);
    let recovered = create_task(&access, "after").await?;
    assert_eq!(owner_on_vera(&node, &recovered).await?, Some(user_did));
    assert_eq!(tasks(&node, &second.owner).await?, ["after"]);

    status.names_audience.store(false, Ordering::SeqCst);
    let unresolved = GraphqlEndpoint::as_principal(&url, user.did())
        .with_client_signing_capability(user.clone())?
        .with_reported_delegation_audience(ReportedDelegationAudience::Absent)?
        .with_audience_resolver(
            first.node.did(),
            Arc::new(PublicStatus {
                url: format!("http://{address}/status"),
            }),
        );
    assert!(
        create_task(&ConfigAccess::Graphql(unresolved), "unresolved")
            .await
            .is_err(),
        "a status naming no audience leaves the identity token refused"
    );
    assert_eq!(tasks(&node, &second.owner).await?, ["after"]);
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn another_vera_home_answering_for_the_saved_home_is_given_nothing() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let user = client_key(directory.path(), "user.key").await?;
    let user_did = identity::Did::new(user.did())?;
    let saved = vera_home(directory.path(), "saved", &user_did).await?;
    let other = vera_home(directory.path(), "other", &user_did).await?;
    assert_ne!(saved.node.did(), other.node.did());
    let address = tokio::task::spawn_blocking(|| {
        std::net::TcpListener::bind("127.0.0.1:0").and_then(|listener| listener.local_addr())
    })
    .await??;
    let url = format!("http://{address}/api/v0/graphql");
    let client = GraphqlEndpoint::as_principal(&url, user.did())
        .with_client_signing_capability(user.clone())?
        .with_reported_delegation_audience(ReportedDelegationAudience::Absent)?
        .with_audience_resolver(
            saved.node.did(),
            Arc::new(PublicStatus {
                url: format!("http://{address}/status"),
            }),
        );

    let (node, _status) =
        serve_claiming(&other, address, Some(saved.node.did().to_owned())).await?;
    let refused = create_task(&ConfigAccess::Graphql(client.clone()), "elsewhere")
        .await
        .expect_err("a home that cannot sign as the saved node is given no delegation");
    assert!(
        format!("{refused:#}").contains("the home's status could not be authenticated"),
        "{refused:#}"
    );
    assert!(
        !client.mints_delegations(),
        "no audience of the other home is kept"
    );
    assert_eq!(tasks(&node, &other.owner).await?, Vec::<String>::new());
    node.shutdown().await?;
    Ok(())
}
