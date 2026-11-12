//! An embedded node with a served GraphQL endpoint. It starts the node through
//! the store owner's protected start with no installed plugins and a policy
//! that projects no tools, composed with what the calling layer names: the
//! bundled lifecycle plugins where they can be named, an explicitly empty set
//! below them, or nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_documents::test_support::{bootstrap_signed_test_node, set_test_default_agent_as};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::local::store_key::StoreKey;
use gents_store::local::{ProjectedOperationSource, ProjectedPluginRef, ToolSourcePolicy};

const NODE_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

struct NoToolSources;

impl ToolSourcePolicy for NoToolSources {
    fn node_operation_sources(
        &self,
        _tools: &serde_json::Value,
        _surfaces: &[serde_json::Value],
    ) -> Result<Vec<ProjectedOperationSource>> {
        Ok(Vec::new())
    }

    fn plugin_refs(&self, _tools: &serde_json::Value) -> Result<Vec<ProjectedPluginRef>> {
        Ok(Vec::new())
    }

    fn selected_tools(&self, _node: &str, _context: &serde_json::Value) -> Option<String> {
        None
    }
}

type ServedNode = Arc<std::sync::OnceLock<(std::sync::Weak<EmbeddedNode>, Arc<dyn NodeIdentity>)>>;

pub struct TestDb {
    pub node: Arc<EmbeddedNode>,
    pub node_identity: Arc<dyn NodeIdentity>,
    pub process_generation: u64,
    composition: gents_store::node_extension::Extensions,
    endpoint: String,
    data_path: PathBuf,
    store_key: StoreKey,
    store_lock: Option<gents_store::local::home::StoreLock>,
    tempdir: Option<tempfile::TempDir>,
}

impl TestDb {
    pub fn data_path(&self) -> &Path {
        &self.data_path
    }

    pub fn client_endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn client_access(
        &self,
        identity: &dyn NodeIdentity,
    ) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::Graphql(
            gents_store::config_client::GraphqlEndpoint::as_principal(
                &self.endpoint,
                identity.did(),
            )
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates()
            .with_extensions(&self.composition)
            .unwrap(),
        )
    }

    /// Stops the node and starts a new one over the same store, as a process
    /// restart does.
    pub async fn simulate_process_crash(&mut self) -> Result<()> {
        self.node
            .shutdown()
            .await
            .context("shutting down test node")?;
        let release_deadline = tokio::time::Instant::now() + NODE_RELEASE_TIMEOUT;
        while Arc::strong_count(&self.node) != 1 && tokio::time::Instant::now() < release_deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let strong = Arc::strong_count(&self.node);
        ensure!(
            strong == 1,
            "stopped EmbeddedNode retained live owners (strong_count={strong})"
        );
        let stand_in = Arc::new(
            EmbeddedNode::builder()
                .build()
                .await
                .map_err(|e| anyhow!("stand-in node: {e}"))?,
        );
        match Arc::try_unwrap(std::mem::replace(&mut self.node, stand_in)) {
            Ok(owned) => drop(owned),
            Err(shared) => {
                self.node = shared;
                bail!("cannot exclusively drop the stopped EmbeddedNode");
            }
        }
        let (node, endpoint) = start(
            &self.data_path,
            &self.store_key,
            self.store_lock.as_ref().context("store lock released")?,
            self.node_identity.clone(),
            None,
            &self.composition,
        )
        .await?;
        self.node = node;
        self.endpoint = endpoint;
        self.process_generation += 1;
        gents_store::schema::ensure_runtime_schemas(&self.node)
            .await
            .context("ensure schemas after restart")?;
        Ok(())
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if self.node.is_shutdown() {
            return;
        }
        let node = self.node.clone();
        let held = (self.store_lock.take(), self.tempdir.take());
        let _ = std::thread::Builder::new()
            .name("gents-test-db-release".to_owned())
            .spawn(move || release_unstopped_node(node, held));
    }
}

/// Runs on its own OS thread, outside any Tokio runtime, so the shutdown
/// completes even when the dropping runtime is itself shutting down.
#[expect(
    clippy::disallowed_methods,
    reason = "waits for and drives Node::shutdown on a dedicated release thread; no runtime worker waits"
)]
fn release_unstopped_node<H>(node: Arc<EmbeddedNode>, held: H) {
    let mut wait = Duration::from_millis(5);
    while Arc::strong_count(&node) > 1 && !node.is_shutdown() {
        std::thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_millis(100));
    }
    let _ = futures::executor::block_on(node.shutdown());
    drop(node);
    drop(held);
}

async fn start(
    data_path: &Path,
    key: &StoreKey,
    lock: &gents_store::local::home::StoreLock,
    identity: Arc<dyn NodeIdentity>,
    p2p: Option<defra_node::P2PConfig>,
    composition: &gents_store::node_extension::Extensions,
) -> Result<(Arc<EmbeddedNode>, String)> {
    let did = identity.did().to_string();
    let protected =
        gents_store::local::backend_protection::ProtectedBackendStore::open(data_path, key, lock)
            .await?
            .preflight(key, Some(&did))
            .await?;
    let served: ServedNode = ServedNode::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let http = defra_node::HttpConfig::with_listener(listener.into_std()?)?
        .with_extra_routes(agent_identity_routes(served.clone()));
    let (node, _admission) = protected
        .start_node_with_projection(
            key,
            identity.clone(),
            Some(http),
            p2p,
            Arc::new(NoToolSources),
            Vec::new(),
            composition,
        )
        .await?;
    let _ = served.set((Arc::downgrade(&node), identity));
    Ok((node, endpoint))
}

/// Serves Agent identity provisioning as a runtime does, so a client creating
/// an Agent provisions its identity through the authorized owner.
fn agent_identity_routes(served: ServedNode) -> axum::Router {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let failure = |status: StatusCode, message: &str| {
        (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
    };
    axum::Router::new().route(
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
                        return failure(StatusCode::UNAUTHORIZED, "not connected");
                    };
                    let Some((node, runtime)) = served
                        .get()
                        .and_then(|(node, runtime)| Some((node.upgrade()?, runtime.clone())))
                    else {
                        return failure(StatusCode::SERVICE_UNAVAILABLE, "starting");
                    };
                    if request.node_did != runtime.did() {
                        return failure(StatusCode::BAD_REQUEST, "wrong node");
                    }
                    match gents_store::identity::provision_agent_identity_authorized(
                        &node,
                        runtime.as_ref(),
                        &caller,
                    )
                    .await
                    {
                        Ok(binding) => axum::Json(binding).into_response(),
                        Err(gents_store::identity::AgentProvisionError::Unauthorized) => {
                            failure(StatusCode::FORBIDDEN, "unauthorized")
                        }
                        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "cannot provision"),
                    }
                }
            },
        ),
    )
}

async fn store_key(path: &Path, data: &Path) -> Result<StoreKey> {
    use gents_store::local::store_key::{StoreEncryption, StoreKeyCustodyChoice};
    let key_file = path.join("store.key");
    let (kf, d) = (key_file.clone(), data.to_owned());
    let record = tokio::task::spawn_blocking(move || {
        StoreEncryption::prepare(StoreKeyCustodyChoice::File, &kf, &d)
    })
    .await??;
    let (d, record_for_init) = (data.to_owned(), record);
    tokio::task::spawn_blocking(move || record_for_init.initialize(&key_file, &d))
        .await?
        .map_err(Into::into)
}

pub async fn test_db(name: &str, composition: gents_store::node_extension::Extensions) -> TestDb {
    let tempdir = tempfile::Builder::new()
        .prefix(&format!("gents-{name}-"))
        .tempdir()
        .expect("tempdir");
    let mut db = test_db_at(tempdir.path(), None, composition)
        .await
        .expect("test node");
    db.tempdir = Some(tempdir);
    db
}

/// Starts the node over the home at `path`, creating it when absent and
/// reopening it when a previous `TestDb` left it there. The home is retained
/// after the node stops.
pub async fn test_db_at(
    path: &Path,
    p2p: Option<defra_node::P2PConfig>,
    composition: gents_store::node_extension::Extensions,
) -> Result<TestDb> {
    let path = path.to_path_buf();
    let identity_path = path.join("node.key");
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(identity_path, None))
            .await??,
    );
    let did = identity.did().to_string();
    let data_path = path.join("data");
    tokio::fs::create_dir_all(&data_path).await?;
    let (lock_home, lock_data) = (path.clone(), data_path.clone());
    let store_lock = tokio::task::spawn_blocking(move || {
        gents_store::local::home::lock_store(&lock_home, &lock_data)
    })
    .await??;
    let key = if data_path.join("MANIFEST").exists() {
        use gents_store::local::store_key::{
            StoreEncryption, StoreKeyCustody, STORE_ENCRYPTION_VERSION,
        };
        StoreEncryption {
            version: STORE_ENCRYPTION_VERSION,
            custody: StoreKeyCustody::File,
        }
        .load(&path.join("store.key"), &data_path)?
    } else {
        let fresh = gents_store::local::backend_protection::FreshBackendStore::observe(
            &data_path,
            &store_lock,
        )
        .await?;
        let key = store_key(&path, &data_path).await?;
        fresh.initialize(&key, Some(&did)).await?;
        key
    };
    let (node, endpoint) = start(
        &data_path,
        &key,
        &store_lock,
        identity.clone(),
        p2p,
        &composition,
    )
    .await?;
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    Ok(TestDb {
        node,
        node_identity: identity,
        process_generation: 0,
        composition,
        endpoint,
        data_path,
        store_key: key,
        store_lock: Some(store_lock),
        tempdir: None,
    })
}

pub async fn signed_test_node(
    name: &str,
    composition: gents_store::node_extension::Extensions,
) -> (
    TestDb,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let db = test_db(name, composition).await;
    signed_test_node_from_db(db, name).await
}

pub async fn signed_test_node_without_agent(
    name: &str,
    composition: gents_store::node_extension::Extensions,
) -> (
    TestDb,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
) {
    let db = test_db(name, composition).await;
    signed_test_node_without_agent_from_db(db, name).await
}

pub async fn signed_test_node_without_agent_from_db(
    db: TestDb,
    name: &str,
) -> (
    TestDb,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
) {
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::GrantSet;

    let fixture_name = name.to_owned();
    let user = tokio::task::spawn_blocking(move || {
        gents_store::test_support::test_identity(&fixture_name)
    })
    .await
    .unwrap();
    let runtime = db.node_identity.as_ref();
    assert_ne!(runtime.did(), user.did());
    let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did()).unwrap());
    let user_did = identity::Did::new(user.did()).unwrap();
    GrantSet::OperatorUser
        .grant(&admin, &user_did)
        .await
        .unwrap();
    let access = db.client_access(&user);
    bootstrap_signed_test_node(&access, runtime.did()).await;
    (db, user, access)
}

pub async fn signed_test_node_from_db(
    db: TestDb,
    name: &str,
) -> (
    TestDb,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let (db, user, access) = signed_test_node_without_agent_from_db(db, name).await;
    let runtime = db.node_identity.as_ref();
    let user_did = identity::Did::new(user.did()).unwrap();
    let binding =
        gents_store::identity::provision_agent_identity_authorized(&db.node, runtime, &user_did)
            .await
            .unwrap();
    super::install_test_agent_binding_with_access(
        runtime.did(),
        "Default fixture agent",
        &binding,
        &access,
    )
    .await;
    set_test_default_agent_as(&access, runtime.did(), &binding.agent_did).await;
    (db, user, access, binding)
}
