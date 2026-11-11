#[cfg(test)]
use gents_pack::test_support::fixture_pack_copy;

#[cfg(test)]
pub(crate) fn home_with_fixture_pack(
    name: &str,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::plugin::executor::PluginExecutor>,
) {
    let (_guard, dir) = fixture_pack_copy(name, &serde_json::json!({}));
    home_with_pack_dir(&dir)
}

pub(crate) fn home_with_pack_dir(
    dir: &std::path::Path,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::plugin::executor::PluginExecutor>,
) {
    let (bytes, _) = gents_pack::pack::archive::pack_dir(dir)
        .unwrap_or_else(|error| panic!("packing {}: {error:#}", dir.display()));
    let home = tempfile::tempdir().expect("home");
    gents_pack::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap_or_else(|error| panic!("storing {}: {error:#}", dir.display()));
    let plugins = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_path_buf(),
    )));
    (home, plugins)
}

pub(crate) fn default_plugin_host() -> std::sync::Arc<dyn gents_tools::plugin_host::PluginHost> {
    std::sync::Arc::new(crate::plugin::executor::PluginExecutor::default())
}

/// An Agent identity watch that publishes a fixed set of signers and never
/// changes.
pub struct FixedAgentIdentities(Vec<std::sync::Arc<dyn gents_store::identity::NodeIdentity>>);

impl FixedAgentIdentities {
    pub fn boxed(
        identities: Vec<std::sync::Arc<dyn gents_store::identity::NodeIdentity>>,
    ) -> Box<dyn gents_store::agent_identity_watch::AgentIdentityWatch> {
        Box::new(Self(identities))
    }
}

impl gents_store::agent_identity_watch::AgentIdentityWatch for FixedAgentIdentities {
    fn watcher_identities(
        &self,
    ) -> anyhow::Result<Vec<std::sync::Arc<dyn gents_store::identity::NodeIdentity>>> {
        Ok(self.0.clone())
    }

    fn watched_and_active_agents(
        &self,
    ) -> anyhow::Result<gents_store::agent_identity_watch::WatchedAndActiveAgents> {
        Ok((self.0.clone(), Default::default()))
    }

    fn changed(
        &mut self,
    ) -> futures::future::BoxFuture<
        '_,
        Result<(), gents_store::agent_identity_watch::AgentIdentityWatchClosed>,
    > {
        Box::pin(std::future::pending())
    }
}

/// Makes the process retained-record events visible to a scoped subscriber:
/// tracing caches callsite interest globally, so a scoped subscriber sees an
/// event only when a global default has enabled its target first.
#[cfg(test)]
pub(crate) fn enable_scoped_event_capture() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(CapturedTargetSubscriber)
            .expect("no other global tracing default in this test binary");
    });
}

#[cfg(test)]
struct CapturedTargetSubscriber;

#[cfg(test)]
impl tracing::Subscriber for CapturedTargetSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target().starts_with("gents_toolcalls::call::hook")
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}

/// A served embedded node for tests at this layer: the runtime's trial home
/// is a root-crate owner, so fixtures here start the node from DefraDB's
/// builder with the same node identity, ACP and HTTP GraphQL endpoint.
pub struct TestHome {
    pub node: std::sync::Arc<defra_node::Node>,
    pub node_identity: std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    endpoint: String,
    _dir: Option<tempfile::TempDir>,
}

impl TestHome {
    pub async fn create(name: &str) -> anyhow::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("gents-{name}-"))
            .tempdir()?;
        let mut home = Self::create_at(dir.path(), None).await?;
        home._dir = Some(dir);
        Ok(home)
    }

    /// Creates a home at the not yet existing `path`, retained when dropped.
    pub async fn create_at(
        path: &std::path::Path,
        p2p: Option<defra_node::P2PConfig>,
    ) -> anyhow::Result<Self> {
        use gents_store::identity::NodeIdentity as _;
        std::fs::create_dir_all(path.join("data"))?;
        let key_path = path.join("node.key");
        let identity = tokio::task::spawn_blocking(move || {
            gents_store::identity::KeyIdentity::load_or_create(key_path, None)
        })
        .await??;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
        let http = defra_node::HttpConfig::with_listener(listener.into_std()?)?;
        let mut builder = defra_node::Node::builder()
            .data_path(path.join("data"))
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_http(http);
        if let Some(p2p) = p2p {
            builder = builder.with_p2p(p2p);
        }
        let node = std::sync::Arc::new(builder.build().await?);
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        Ok(Self {
            node,
            node_identity: std::sync::Arc::new(identity),
            endpoint,
            _dir: None,
        })
    }

    pub fn client_access(
        &self,
        identity: &dyn gents_store::identity::NodeIdentity,
    ) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::Graphql(
            gents_store::config_client::GraphqlEndpoint::as_principal(
                self.endpoint.clone(),
                identity.did(),
            )
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
        )
    }
}

/// A served node whose runtime has granted `user` the operator role and whose
/// default Agent is provisioned and installed.
pub async fn signed_test_node(
    name: &str,
) -> (
    TestHome,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let home = TestHome::create(name).await.expect("test home");
    signed_test_node_from_home(home, name).await
}

pub async fn signed_test_node_from_home(
    home: TestHome,
    name: &str,
) -> (
    TestHome,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::{GrantSet, NodeIdentity};

    let fixture_name = name.to_owned();
    let user = tokio::task::spawn_blocking(move || gents_store::test_support::test_identity(&fixture_name))
        .await
        .unwrap();
    let runtime = home.node_identity.as_ref();
    assert_ne!(runtime.did(), user.did());
    let admin = ConfigAccess::local_as(home.node.clone(), identity::Did::new(runtime.did()).unwrap());
    let user_did = identity::Did::new(user.did()).unwrap();
    GrantSet::OperatorUser.grant(&admin, &user_did).await.unwrap();
    let access = home.client_access(&user);
    gents_documents::test_support::bootstrap_signed_test_node(&access, runtime.did()).await;
    let binding =
        gents_store::identity::provision_agent_identity_authorized(&home.node, runtime, &user_did)
            .await
            .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        runtime.did(),
        "Default fixture agent",
        &binding,
        &access,
    )
    .await;
    gents_documents::test_support::set_test_default_agent_as(&access, runtime.did(), &binding.agent_did)
        .await;
    (home, user, access, binding)
}
