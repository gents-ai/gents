//! Homes on an embedded Vera node or the node store, opened as `gents init`
//! leaves them, for tests above the store that need a live provider.

mod household;

pub use household::{describe, seen, Household, PROVIDERS, SETTLE_CEILING};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gents_protocol::delegation_audience::ReportedDelegationAudience;
use gents_store::config_client::{
    ClientSigningCapability, ConfigAccess, GraphqlEndpoint, OwnerStagedRoute,
};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::local::backend_protection::ProtectedBackendStore;
use gents_store::local::document_acp::{DocumentAcp, HttpEdge};
use gents_store::local::home::{self, StoreLock};
use gents_store::local::store_key::StoreKey;

pub struct Home {
    pub data: PathBuf,
    pub lock: StoreLock,
    pub key: StoreKey,
    pub node: Arc<dyn NodeIdentity>,
    /// The init User, who owns the home's node access control.
    pub owner: identity::Did,
}

/// A home directory under `root` with its lock, store key, Node key and a
/// separate init User key; nothing is initialized.
pub async fn home(root: &Path, name: &str) -> anyhow::Result<Home> {
    let home = root.join(name);
    let data = home.join("data");
    let (lock_home, lock_data, key_path, owner_path) = (
        home.clone(),
        data.clone(),
        home.join("node.key"),
        home.join("owner.key"),
    );
    let (lock, node, owner) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        std::fs::create_dir_all(&lock_data)?;
        let node: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(key_path, None)?);
        let owner = identity::Did::new(KeyIdentity::load_or_create(owner_path, None)?.did())?;
        Ok((home::lock_store(&lock_home, &lock_data)?, node, owner))
    })
    .await??;
    Ok(Home {
        data,
        lock,
        key: StoreKey::from_host(&[7; 32])?,
        node,
        owner,
    })
}

/// Serve `home` over HTTP behind its edge, as the runtime does.
pub async fn serve(
    home: &Home,
    document_acp: DocumentAcp,
) -> anyhow::Result<(Arc<defra_node::Node>, String)> {
    serve_with(home, document_acp, None, None).await
}

/// Serve `home` over HTTP behind its edge with the owner-staged route, as the
/// runtime does.
pub async fn serve_with_owner_staged(
    home: &Home,
    document_acp: DocumentAcp,
) -> anyhow::Result<(Arc<defra_node::Node>, String)> {
    serve_with(home, document_acp, Some(OwnerStagedRoute::new()), None).await
}

async fn serve_with(
    home: &Home,
    document_acp: DocumentAcp,
    owner_staged: Option<OwnerStagedRoute>,
    transport_account: Option<&identity::Did>,
) -> anyhow::Result<(Arc<defra_node::Node>, String)> {
    let (listener, address) = tokio::task::spawn_blocking(|| -> std::io::Result<_> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        Ok((listener, address))
    })
    .await??;
    let mut http = defra_node::HttpConfig::with_listener(listener)?;
    if let Some(route) = &owner_staged {
        http = http.with_authenticated_routes(route.router());
    }
    let fence_route = owner_staged.clone();
    let http = http.with_router_transform(Arc::new({
        let edge = HttpEdge::new(document_acp, home.node.did());
        move |router| {
            let router = match &fence_route {
                Some(route) => route.wrap_relationship_fence(router),
                None => router,
            };
            edge.wrap(router)
        }
    }));
    let p2p = transport_account.map(|_| {
        gents_p2p::test_support::transport::test_p2p_config(&Default::default(), &home.data)
    });
    let (node, _admission) = ProtectedBackendStore::open(&home.data, &home.key, &home.lock)
        .await?
        .preflight_with_account(&home.key, &home.node, document_acp, transport_account)
        .await?
        .start_node_with_projection(
            &home.key,
            home.node.clone(),
            move |builder: defra_node::NodeBuilder| {
                let builder = builder.with_http(http);
                match p2p {
                    Some(config) => builder.with_p2p(config),
                    None => builder,
                }
            },
            Arc::new(crate::surface::ToolCeiling::meta_only()),
            Vec::new(),
            &gents_session::test_support::no_lifecycle_plugins(),
        )
        .await?;
    if let Some(route) = owner_staged {
        route.serve(&node, home.node.clone());
    }
    Ok((node, format!("http://{address}/api/v0/graphql")))
}

/// A User whose key this process holds only in the capability: it is never
/// registered where DefraDB could sign with it.
pub async fn client_key(root: &Path, name: &str) -> anyhow::Result<ClientSigningCapability> {
    let path = root.join(name);
    let raw = tokio::task::spawn_blocking(move || {
        gents_store::identity::load_or_create_file_identity(&path)
    })
    .await??;
    Ok(ClientSigningCapability::from_ed25519_key(
        &raw.private_key_bytes(),
    )?)
}

/// `user` as its client acts over HTTP at `url`, with the audience its
/// runtime reported.
pub fn as_user(
    url: &str,
    user: &ClientSigningCapability,
    reported: ReportedDelegationAudience,
) -> anyhow::Result<ConfigAccess> {
    Ok(ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(url, user.did())
            .with_client_signing_capability(user.clone())?
            .with_reported_delegation_audience(reported)?,
    ))
}

/// The User as its client holds it: a key in a signing capability that
/// DefraDB never holds.
pub struct ClientUser(pub ClientSigningCapability, pub identity::RawIdentity);

#[async_trait::async_trait]
impl NodeIdentity for ClientUser {
    fn did(&self) -> &str {
        self.0.did()
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, gents_store::identity::IdentityError> {
        use identity::FullIdentity as _;
        self.1
            .sign(payload)
            .map_err(|source| gents_store::identity::IdentityError::SignPayload {
                did: self.did().to_owned(),
                source,
            })
    }

    async fn verify(
        &self,
        did: &str,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<bool, gents_store::identity::IdentityError> {
        gents_store::identity::verify_did_signature(did, payload, signature)
    }

    fn service_account(&self) -> Option<&gents_store::identity::ServiceAccount> {
        None
    }

    fn client_signing_capability(
        &self,
    ) -> Result<ClientSigningCapability, gents_store::config_client::ConfigClientError> {
        Ok(self.0.clone())
    }
}

/// The client's settlement passes, asked in this process of the store owner
/// the runtime's route calls, each under a delegation the client's endpoint
/// mints at its one minting point.
pub struct InProcessPass {
    node: Arc<defra_node::Node>,
    endpoint: GraphqlEndpoint,
    pub passes: std::sync::atomic::AtomicUsize,
}

impl InProcessPass {
    pub fn new(node: Arc<defra_node::Node>, endpoint: GraphqlEndpoint) -> Self {
        Self {
            node,
            endpoint,
            passes: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl gents_store::config_client::owed::OwedPass for InProcessPass {
    async fn pass(
        &self,
    ) -> Result<
        Vec<gents_store::config_client::owed::OwedStanding>,
        gents_store::identity::RemoteAgentProvisionError,
    > {
        self.passes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let caller = self
            .endpoint
            .held_client_signing_capability()?
            .did()
            .to_owned();
        let authorization = self
            .endpoint
            .authorization()?
            .expect("a client endpoint authorizes its requests");
        let token = authorization
            .strip_prefix("Bearer ")
            .expect("a bearer")
            .to_owned();
        let context = defra_core::RequestContext::default()
            .with_identity(Some(caller.clone()))
            .with_bearer(Some(caller.clone()), Some(token));
        Ok(
            gents_store::config_client::owed::settle_pass(&self.node, &context, &caller)
                .await
                .expect("the runtime serves a pass for its caller's own delegation"),
        )
    }
}

/// What the runtime's public status reports now, signed by its node key, as
/// the desktop reads it.
pub struct StaticStatusAudience {
    pub node: Arc<dyn NodeIdentity>,
    pub reports: ReportedDelegationAudience,
}

impl gents_store::config_client::ResolveAudience for StaticStatusAudience {
    fn resolve<'a>(
        &'a self,
        nonce: &'a gents_protocol::home_attestation::HomeNonce,
    ) -> futures::future::BoxFuture<'a, Option<serde_json::Value>> {
        Box::pin(async move {
            Some(
                gents_store::test_support::signed_home_status(
                    self.node.as_ref(),
                    &self.reports,
                    None,
                    nonce,
                )
                .await,
            )
        })
    }
}

/// Reads the runtime's public status over HTTP with no credential, as a
/// client does after a refusal.
pub struct HttpStatusAudience {
    pub url: String,
}

impl gents_store::config_client::ResolveAudience for HttpStatusAudience {
    fn resolve<'a>(
        &'a self,
        nonce: &'a gents_protocol::home_attestation::HomeNonce,
    ) -> futures::future::BoxFuture<'a, Option<serde_json::Value>> {
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

/// What the runtime's public status reports of its node now, signed by its
/// node key.
pub struct NodeStatusAudience(pub Arc<defra_node::Node>, pub Arc<dyn NodeIdentity>);

impl gents_store::config_client::ResolveAudience for NodeStatusAudience {
    fn resolve<'a>(
        &'a self,
        nonce: &'a gents_protocol::home_attestation::HomeNonce,
    ) -> futures::future::BoxFuture<'a, Option<serde_json::Value>> {
        Box::pin(async move {
            Some(
                gents_store::test_support::signed_home_status(
                    self.1.as_ref(),
                    &ReportedDelegationAudience::Named(
                        gents_store::local::document_acp::node_delegation_audience(&self.0)?,
                    ),
                    gents_store::local::document_acp::node_provider_time(&self.0),
                    nonce,
                )
                .await,
            )
        })
    }
}
