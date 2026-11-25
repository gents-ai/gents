use std::path::Path;
use std::sync::Arc;

use crate::config_client::{ConfigAccess, GraphqlEndpoint};
use crate::identity::NodeIdentity;
use crate::local::home::{HomeServing, NodeConfigure, NodeProjection, ServedHome};
use crate::local::{ProjectedOperationSource, ProjectedPluginRef, ToolSourcePolicy};
use crate::node_extension::Extensions;

pub struct NoToolSources;

impl ToolSourcePolicy for NoToolSources {
    fn node_operation_sources(
        &self,
        _tools: &serde_json::Value,
        _surfaces: &[serde_json::Value],
    ) -> anyhow::Result<Vec<ProjectedOperationSource>> {
        Ok(Vec::new())
    }

    fn plugin_refs(&self, _tools: &serde_json::Value) -> anyhow::Result<Vec<ProjectedPluginRef>> {
        Ok(Vec::new())
    }

    fn selected_tools(&self, _node: &str, _context: &serde_json::Value) -> Option<String> {
        None
    }
}

/// One protected-start home on a temporary directory, opened by the
/// production home owner ([`ServedHome`]) with no installed plugins.
pub struct TestStore {
    home: ServedHome,
    tool_policy: Arc<dyn ToolSourcePolicy>,
    composition: Extensions,
    pub process_generation: u64,
}

impl std::ops::Deref for TestStore {
    type Target = ServedHome;

    fn deref(&self) -> &ServedHome {
        &self.home
    }
}

impl TestStore {
    pub async fn start(name: &str, composition: Extensions) -> Self {
        Self::start_in(name, None, None, composition, Arc::new(NoToolSources)).await
    }

    /// A store whose permission projector uses `tool_policy`, so a fixture
    /// Agent's Tools selection projects the node permissions its tool sources
    /// need, as the runtime's ceiling does in production.
    pub async fn start_with_tool_policy(
        name: &str,
        composition: Extensions,
        tool_policy: Arc<dyn ToolSourcePolicy>,
    ) -> Self {
        Self::start_in(name, None, None, composition, tool_policy).await
    }

    /// A store that runs as `node_did`, whose key this process holds, for
    /// fixtures whose Agents derive from a fixed root.
    pub async fn start_as(name: &str, node_did: &str, composition: Extensions) -> Self {
        Self::start_in(
            name,
            Some(node_did),
            None,
            composition,
            Arc::new(NoToolSources),
        )
        .await
    }

    pub async fn start_configured(
        name: &str,
        composition: Extensions,
        configure: NodeConfigure,
    ) -> Self {
        Self::start_in(
            name,
            None,
            Some(configure),
            composition,
            Arc::new(NoToolSources),
        )
        .await
    }

    /// [`Self::start_configured`] with the permission projector using
    /// `tool_policy`.
    pub async fn start_configured_with_tool_policy(
        name: &str,
        composition: Extensions,
        configure: NodeConfigure,
        tool_policy: Arc<dyn ToolSourcePolicy>,
    ) -> Self {
        Self::start_in(name, None, Some(configure), composition, tool_policy).await
    }

    /// Starts the node over the home at `path`, creating it when absent and
    /// reopening it when a previous store left it there. The home is retained
    /// after the node stops.
    pub async fn open_at(
        path: &Path,
        configure: Option<NodeConfigure>,
        composition: Extensions,
    ) -> anyhow::Result<Self> {
        Self::open(path, configure, composition, Arc::new(NoToolSources)).await
    }

    /// [`Self::open_at`] with the permission projector using `tool_policy`,
    /// for a home another owner reopens with that projection.
    pub async fn open_at_with_tool_policy(
        path: &Path,
        composition: Extensions,
        tool_policy: Arc<dyn ToolSourcePolicy>,
    ) -> anyhow::Result<Self> {
        Self::open(path, None, composition, tool_policy).await
    }

    async fn start_in(
        name: &str,
        node_did: Option<&str>,
        configure: Option<NodeConfigure>,
        composition: Extensions,
        tool_policy: Arc<dyn ToolSourcePolicy>,
    ) -> Self {
        let tempdir = tempfile::Builder::new()
            .prefix(&format!("gents-{name}-"))
            .tempdir()
            .expect("tempdir");
        if let Some(node_did) = node_did {
            let (did, key) = (node_did.to_owned(), tempdir.path().join("node.key"));
            tokio::task::spawn_blocking(move || super::write_held_identity_key(&did, &key))
                .await
                .expect("write the node key");
        }
        let mut store = Self::open(tempdir.path(), configure, composition, tool_policy)
            .await
            .expect("test store");
        store.home.hold_tempdir(tempdir);
        store
    }

    async fn open(
        path: &Path,
        configure: Option<NodeConfigure>,
        composition: Extensions,
        tool_policy: Arc<dyn ToolSourcePolicy>,
    ) -> anyhow::Result<Self> {
        let projection = NodeProjection {
            tool_policy: tool_policy.clone(),
            plugins: Vec::new(),
            composition: composition.clone(),
        };
        tokio::fs::create_dir_all(path).await?;
        let serving = HomeServing {
            routes: None,
            configure,
        };
        let home = ServedHome::open_at(path.to_path_buf(), serving, projection).await?;
        crate::schema::ensure_runtime_schemas(&home.node).await?;
        Ok(Self {
            home,
            tool_policy,
            composition,
            process_generation: 0,
        })
    }

    /// The client-signed access `identity` holds over this store's endpoint,
    /// as a client's key signs it.
    pub fn client_access(&self, identity: &dyn NodeIdentity) -> ConfigAccess {
        ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(self.graphql_endpoint(), identity.did())
                .with_client_signing_capability(identity.client_signing_capability().unwrap())
                .unwrap()
                .with_client_signed_agent_creates()
                .with_extensions(&self.composition)
                .unwrap(),
        )
    }

    /// Stops the node and starts a new one over the same store, as a process
    /// restart does.
    pub async fn simulate_process_crash(&mut self) -> anyhow::Result<()> {
        self.home
            .reopen(NodeProjection {
                tool_policy: self.tool_policy.clone(),
                plugins: Vec::new(),
                composition: self.composition.clone(),
            })
            .await?;
        crate::schema::ensure_runtime_schemas(&self.home.node).await?;
        self.process_generation += 1;
        Ok(())
    }

    pub async fn read_when_changed<T, L, F>(&self, collection: &str, what: &str, read: F) -> T
    where
        F: AsyncFnMut() -> Result<T, L>,
        L: std::fmt::Display,
    {
        super::read_when_changed(self.home.node.as_ref(), collection, what, read).await
    }

    pub async fn rows_when<F>(
        &self,
        collection: &str,
        query: &str,
        predicate: F,
    ) -> serde_json::Value
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        super::rows_when(self.home.node.as_ref(), collection, query, predicate).await
    }
}
