use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use super::{acquire_store, HomeError, StoreLock, STORE_RELEASE_WAIT};
use crate::identity::{IdentityError, KeyIdentity, NodeIdentity};
use crate::local::backend_protection::{
    BackendAdmission, BackendProtectionError, FreshBackendStore, ProtectedBackendStore,
};
use crate::local::document_acp::DocumentAcp;
use crate::local::store_key::{StoreEncryption, StoreKey, StoreKeyCustodyChoice, StoreKeyError};
use crate::local::{ProjectedPlugin, ToolSourcePolicy};
use crate::node_extension::Extensions;

/// Node-owned services can hold `Node` handles briefly after shutdown.
const NODE_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

/// The node a home's HTTP routes serve, set once the node has started. The
/// node owns the HTTP server that holds this cell, so it is held weakly: a
/// strong reference would keep a dropped home's node (and its threads and
/// descriptors) alive.
pub type ServedNode = Arc<OnceLock<(Weak<defra_node::Node>, Arc<dyn NodeIdentity>)>>;

/// Authenticated routes a layer above serves beside the home's own.
pub type HomeRoutes = Arc<dyn Fn(ServedNode) -> axum::Router + Send + Sync>;

/// Configures the node builder from the home's directory, so a reopen
/// restores the same persisted transport state.
pub type NodeConfigure =
    Arc<dyn Fn(&Path, defra_node::NodeBuilder) -> defra_node::NodeBuilder + Send + Sync>;

pub struct NodeProjection {
    pub tool_policy: Arc<dyn ToolSourcePolicy>,
    pub plugins: Vec<ProjectedPlugin>,
    pub composition: Extensions,
}

#[derive(Clone, Default)]
pub struct HomeServing {
    pub routes: Option<HomeRoutes>,
    pub configure: Option<NodeConfigure>,
}

#[derive(Debug, thiserror::Error)]
pub enum ServedHomeError {
    #[error("legacy trial store has no backend protection provenance")]
    LegacyStore,
    #[error("retained store {} is unavailable", path.display())]
    RetainedStoreUnavailable { path: PathBuf },
    #[error("the store lock was released")]
    LockReleased,
    #[error(
        "stopped node retained live owners (strong_count={strong}); a reopen would not clear process state"
    )]
    RetainedOwners { strong: usize },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Home(#[from] HomeError),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Did(#[from] identity::Error),
    #[error(transparent)]
    StoreKey(#[from] StoreKeyError),
    #[error("store custody record")]
    Custody(#[from] serde_json::Error),
    #[error(transparent)]
    Protection(#[from] BackendProtectionError),
    #[error(transparent)]
    Node(#[from] defra_node::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

type Result<T, E = ServedHomeError> = std::result::Result<T, E>;

/// A home's protected node serving GraphQL and the runtime's client routes
/// over a file-custodied store key.
///
/// Dropping a home whose node was not shut down hands the node to a release
/// thread, which shuts it down once no other holder remains, then releases
/// the store lock and temporary directory. DefraDB's HTTP transaction sweep is
/// a pool task that holds its own pool and is stopped only by
/// `Node::shutdown`, so an HTTP-serving node dropped without it keeps its
/// worker threads, store and descriptors for the process life.
pub struct ServedHome {
    pub node: Arc<defra_node::Node>,
    pub node_identity: Arc<dyn NodeIdentity>,
    path: PathBuf,
    data_path: PathBuf,
    endpoint: String,
    store_key: StoreKey,
    store_lock: Option<StoreLock>,
    backend_admission: BackendAdmission,
    serving: HomeServing,
    tempdir: Option<tempfile::TempDir>,
}

impl std::fmt::Debug for ServedHome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServedHome")
            .field("did", &self.node_identity.did())
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ServedHome {
    /// Opens the home at `path`, creating its identity and store when absent
    /// and reopening them when a previous home left them there.
    pub async fn open_at(
        path: PathBuf,
        serving: HomeServing,
        projection: NodeProjection,
    ) -> Result<Self> {
        if tokio::fs::try_exists(path.join("MANIFEST")).await? {
            return Err(ServedHomeError::LegacyStore);
        }
        let retained = tokio::fs::try_exists(path.join("data").join("MANIFEST")).await?;
        let identity_path = path.join("node.key");
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            tokio::task::spawn_blocking(move || {
                if retained {
                    KeyIdentity::load_existing(identity_path, None)
                } else {
                    KeyIdentity::load_or_create(identity_path, None)
                }
            })
            .await??,
        );
        Self::open_with_identity(path, identity, true, serving, projection).await
    }

    /// Opens the home at `path` as `identity`; only a `create` open may
    /// initialize a fresh store.
    pub async fn open_with_identity(
        path: PathBuf,
        identity: Arc<dyn NodeIdentity>,
        create: bool,
        serving: HomeServing,
        projection: NodeProjection,
    ) -> Result<Self> {
        if tokio::fs::try_exists(path.join("MANIFEST")).await? {
            return Err(ServedHomeError::LegacyStore);
        }
        let data_path = path.join("data");
        let unavailable = || ServedHomeError::RetainedStoreUnavailable {
            path: data_path.clone(),
        };
        if !tokio::fs::try_exists(&data_path).await? {
            if !create {
                return Err(unavailable());
            }
            tokio::fs::create_dir_all(&data_path).await?;
        }
        let store_lock = acquire_store(&path, &data_path, STORE_RELEASE_WAIT).await?;
        let fresh = tokio::fs::read_dir(&data_path)
            .await?
            .next_entry()
            .await?
            .is_none();
        if fresh && !create {
            return Err(unavailable());
        }
        let fresh_store = if fresh {
            Some(FreshBackendStore::observe(&data_path, &store_lock).await?)
        } else {
            None
        };
        let store_key = custodied_store_key(&path, &data_path, fresh).await?;
        if let Some(fresh_store) = fresh_store {
            // A home's NAC is owned by its own owner key, never its Node
            // (`Identity.NodeProvisioning.nacOwner`).
            let owner_path = path.join("owner.key");
            let owner =
                tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(owner_path, None))
                    .await??;
            fresh_store
                .initialize(
                    &store_key,
                    &identity,
                    &::identity::Did::new(owner.did())?,
                    DocumentAcp::Local,
                )
                .await?;
        }
        let (node, endpoint, backend_admission) = start(
            &path,
            &data_path,
            &store_key,
            &store_lock,
            identity.clone(),
            &serving,
            projection,
        )
        .await?;
        Ok(Self {
            node,
            node_identity: identity,
            path,
            data_path,
            endpoint,
            store_key,
            store_lock: Some(store_lock),
            backend_admission,
            serving,
            tempdir: None,
        })
    }

    /// Keeps `tempdir` until the home and its node are released.
    pub fn hold_tempdir(&mut self, tempdir: tempfile::TempDir) {
        self.tempdir = Some(tempdir);
    }

    pub fn did(&self) -> &str {
        self.node_identity.did()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn data_path(&self) -> &Path {
        &self.data_path
    }

    pub fn graphql_endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The admission is bound to this home's protected native node.
    pub fn backend_admission(&self) -> BackendAdmission {
        self.backend_admission.clone()
    }

    /// Stops the node and starts a new one over the same store, as a process
    /// restart does.
    pub async fn reopen(&mut self, projection: NodeProjection) -> Result<()> {
        // Stopping the node closes its subscriptions and other node-owned
        // services; wait for their handles to be released before claiming an
        // exclusive durable-store reopen.
        self.node.shutdown().await?;
        let release_deadline = tokio::time::Instant::now() + NODE_RELEASE_TIMEOUT;
        while Arc::strong_count(&self.node) != 1 && tokio::time::Instant::now() < release_deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let stand_in = Arc::new(defra_node::Node::builder().build().await?);
        match Arc::try_unwrap(std::mem::replace(&mut self.node, stand_in)) {
            Ok(owned) => drop(owned),
            Err(shared) => {
                let strong = Arc::strong_count(&shared);
                self.node = shared;
                return Err(ServedHomeError::RetainedOwners { strong });
            }
        }
        let (node, endpoint, admission) = start(
            &self.path,
            &self.data_path,
            &self.store_key,
            self.store_lock
                .as_ref()
                .ok_or(ServedHomeError::LockReleased)?,
            self.node_identity.clone(),
            &self.serving,
            projection,
        )
        .await?;
        self.node = node;
        self.endpoint = endpoint;
        self.backend_admission = admission;
        Ok(())
    }
}

impl Drop for ServedHome {
    fn drop(&mut self) {
        if self.node.is_shutdown() {
            return;
        }
        let node = self.node.clone();
        let held = (self.store_lock.take(), self.tempdir.take());
        let release = std::thread::Builder::new()
            .name("gents-home-release".to_owned())
            .spawn(move || release_unstopped_node(node, held));
        if let Err(error) = release {
            tracing::warn!(
                %error,
                did = %self.node_identity.did(),
                "could not start the home node release thread; its node stays running"
            );
        }
    }
}

/// Runs on its own OS thread, outside any Tokio runtime, so the shutdown
/// completes even when the dropping runtime is itself shutting down. Callers
/// may keep using the node after its home is dropped; `Arc` offers no release
/// notification, so this thread polls for being the last holder.
#[expect(
    clippy::disallowed_methods,
    reason = "waits for and drives Node::shutdown on a dedicated release thread; no runtime worker waits"
)]
fn release_unstopped_node<H>(node: Arc<defra_node::Node>, held: H) {
    let mut wait = Duration::from_millis(5);
    while Arc::strong_count(&node) > 1 && !node.is_shutdown() {
        std::thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_millis(100));
    }
    if let Err(error) = futures::executor::block_on(node.shutdown()) {
        tracing::warn!(error = %error, "dropped home node did not shut down cleanly");
    }
    drop(node);
    drop(held);
}

async fn start(
    path: &Path,
    data_path: &Path,
    key: &StoreKey,
    lock: &StoreLock,
    identity: Arc<dyn NodeIdentity>,
    serving: &HomeServing,
    projection: NodeProjection,
) -> Result<(Arc<defra_node::Node>, String, BackendAdmission)> {
    let protected = ProtectedBackendStore::open(data_path, key, lock)
        .await?
        .preflight(key, &identity, DocumentAcp::Local)
        .await?;
    let served = ServedNode::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let owner_staged = crate::config_client::OwnerStagedRoute::new();
    let mut authenticated = agent_routes(served.clone()).merge(owner_staged.router());
    if let Some(routes) = &serving.routes {
        authenticated = authenticated.merge(routes(served.clone()));
    }
    let fence_route = owner_staged.clone();
    let http = defra_node::HttpConfig::with_listener(listener.into_std()?)?
        .with_extra_routes(status_route(served.clone()))
        .with_authenticated_routes(authenticated)
        .with_router_transform(Arc::new(move |router| {
            fence_route.wrap_relationship_fence(router)
        }));
    let configure = serving.configure.clone();
    let path = path.to_owned();
    let (node, admission) = protected
        .start_node_with_projection(
            key,
            identity.clone(),
            move |builder| {
                let builder = builder.with_http(http);
                match &configure {
                    Some(configure) => configure(&path, builder),
                    None => builder,
                }
            },
            projection.tool_policy,
            projection.plugins,
            &projection.composition,
        )
        .await?;
    let _ = served.set((Arc::downgrade(&node), identity.clone()));
    owner_staged.serve(&node, identity);
    Ok((node, endpoint, admission))
}

async fn custodied_store_key(path: &Path, data: &Path, fresh: bool) -> Result<StoreKey> {
    use tokio::io::AsyncWriteExt;
    let key_file = path.join("store.key");
    let record_path = path.join("store-custody.json");
    let record = if fresh {
        let (key_file, data) = (key_file.clone(), data.to_owned());
        let record = tokio::task::spawn_blocking(move || {
            StoreEncryption::prepare(StoreKeyCustodyChoice::File, &key_file, &data)
        })
        .await??;
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&record_path)
            .await?;
        file.write_all(&serde_json::to_vec(&record)?).await?;
        file.sync_all().await?;
        record
    } else {
        serde_json::from_slice::<StoreEncryption>(&tokio::fs::read(&record_path).await?)?
    };
    let data = data.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        if fresh {
            record.initialize(&key_file, &data)
        } else {
            record.load(&key_file, &data)
        }
    })
    .await??)
}

fn status_route(served: ServedNode) -> axum::Router {
    axum::Router::new().route(
        "/status",
        axum::routing::get(
            move |query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>| {
                let served = served.clone();
                async move {
                    use axum::response::IntoResponse;
                    let Ok(nonce) = query
                        .get(gents_protocol::home_attestation::HOME_NONCE_QUERY)
                        .map(|nonce| gents_protocol::home_attestation::HomeNonce::parse(nonce))
                        .transpose()
                    else {
                        return axum::http::StatusCode::BAD_REQUEST.into_response();
                    };
                    let Some((node, runtime)) = served
                        .get()
                        .and_then(|(node, runtime)| Some((node.upgrade()?, runtime)))
                    else {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    };
                    let Ok(authentication) =
                        crate::config_client::client_authentication::runtime_authentication(
                            &crate::config_client::ConfigAccess::local(node),
                        )
                        .await
                    else {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    };
                    let mut status = serde_json::json!({
                        "node_did": runtime.did(),
                        (gents_protocol::client_authentication::STATUS_FIELD): authentication,
                    });
                    if let Some(nonce) = &nonce {
                        crate::identity::home_status::attest(
                            Some(runtime.as_ref()),
                            nonce,
                            &mut status,
                        )
                        .await;
                    }
                    axum::Json(status).into_response()
                }
            },
        ),
    )
}

/// A home serves Agent identity provisioning and agent admission facts as a
/// runtime does, so a client installing or configuring an Agent goes through
/// the same owners (`provision_agent_identity_authorized`,
/// `observe_agent_admission`). The admission route observes native ACP for
/// the authenticated caller only; it decides nothing itself.
pub fn agent_routes(served: ServedNode) -> axum::Router {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let failure = |status: StatusCode, message: &str| {
        (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
    };
    let admission = served.clone();
    axum::Router::new()
        .route(
            gents_protocol::agent_identity::AGENT_ADMISSION_PATH,
            axum::routing::post(
                move |identity: defra_http::ExtractIdentity,
                      axum::Json(request): axum::Json<
                    gents_protocol::agent_identity::AgentAdmissionRequest,
                >| {
                    let served = admission.clone();
                    async move {
                        let Some(caller) = identity
                            .did()
                            .and_then(|did| ::identity::Did::new(did.as_ref()).ok())
                        else {
                            return failure(
                                StatusCode::UNAUTHORIZED,
                                "Connect this client before configuring agents.",
                            );
                        };
                        let Some((node, runtime)) = served
                            .get()
                            .and_then(|(node, runtime)| Some((node.upgrade()?, runtime)))
                        else {
                            return failure(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "The runtime is still starting.",
                            );
                        };
                        if request.node_did != runtime.did() {
                            return failure(
                                StatusCode::BAD_REQUEST,
                                "The requested node does not serve this endpoint.",
                            );
                        }
                        match crate::identity::observe_agent_admission(
                            &node,
                            runtime.did(),
                            &caller,
                            &request.agent_dids,
                        )
                        .await
                        {
                            Ok(facts) => axum::Json(facts).into_response(),
                            Err(error) => {
                                tracing::warn!(%error, "agent admission observation failed");
                                failure(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "The runtime cannot observe agent grants.",
                                )
                            }
                        }
                    }
                },
            ),
        )
        .route(
            gents_protocol::agent_identity::AGENT_IDENTITY_PATH,
            axum::routing::post(
                move |identity: defra_http::ExtractIdentity,
                      axum::Json(request): axum::Json<
                    gents_protocol::agent_identity::ProvisionAgentIdentityRequest,
                >| {
                    let served = served.clone();
                    async move {
                        let Some(caller) = identity
                            .did()
                            .and_then(|did| ::identity::Did::new(did.as_ref()).ok())
                        else {
                            return failure(
                                StatusCode::UNAUTHORIZED,
                                "Connect this client before creating an agent.",
                            );
                        };
                        let Some((node, runtime)) = served
                            .get()
                            .and_then(|(node, runtime)| Some((node.upgrade()?, runtime)))
                        else {
                            return failure(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "The runtime is still starting.",
                            );
                        };
                        if request.node_did != runtime.did() {
                            return failure(
                                StatusCode::BAD_REQUEST,
                                "The requested node does not serve this endpoint.",
                            );
                        }
                        match crate::identity::provision_agent_identity_authorized(
                            &node,
                            runtime.as_ref(),
                            &caller,
                        )
                        .await
                        {
                            Ok(binding) => axum::Json(binding).into_response(),
                            Err(crate::identity::AgentProvisionError::Unauthorized) => failure(
                                StatusCode::FORBIDDEN,
                                "This client cannot create an agent on this node.",
                            ),
                            Err(error) => {
                                tracing::warn!(%error, "agent identity provisioning failed");
                                failure(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "The runtime cannot provision an agent identity.",
                                )
                            }
                        }
                    }
                },
            ),
        )
}
