pub fn tool_policy(ceiling: &crate::surface::ToolCeiling) -> &crate::surface::ToolPolicySurface {
    ceiling.policy()
}

#[cfg(feature = "vera")]
pub mod vera_test_home;

pub struct MeasuredRead<'a> {
    access: &'a gents_store::config_client::ConfigAccess,
    pub queries: std::sync::atomic::AtomicUsize,
    pub max_bytes: std::sync::atomic::AtomicUsize,
    pub total_bytes: std::sync::atomic::AtomicUsize,
}

impl<'a> MeasuredRead<'a> {
    pub fn new(access: &'a gents_store::config_client::ConfigAccess) -> Self {
        Self {
            access,
            queries: std::sync::atomic::AtomicUsize::new(0),
            max_bytes: std::sync::atomic::AtomicUsize::new(0),
            total_bytes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn record_input(&self, bytes: usize) {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.max_bytes
            .fetch_max(bytes, std::sync::atomic::Ordering::Relaxed);
        self.total_bytes
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "queries": self.queries.load(std::sync::atomic::Ordering::Relaxed),
            "max_query_bytes": self.max_bytes.load(std::sync::atomic::Ordering::Relaxed),
            "total_query_bytes": self.total_bytes.load(std::sync::atomic::Ordering::Relaxed),
        })
    }
}

#[async_trait::async_trait]
impl gents_store::config_client::ConfigRead for MeasuredRead<'_> {
    async fn execute_read(
        &self,
        document: &str,
    ) -> Result<serde_json::Value, gents_store::config_client::ConfigClientError> {
        self.record_input(document.len());
        self.access.execute(document).await
    }

    fn reader_did(&self) -> Option<String> {
        self.access.actor_did()
    }

    async fn execute_read_with_variables(
        &self,
        document: &str,
        variables: &serde_json::Value,
    ) -> Result<serde_json::Value, gents_store::config_client::ConfigClientError> {
        self.record_input(document.len() + variables.to_string().len());
        self.access
            .execute_with_variables(document, variables)
            .await
    }
}

/// A host that carries a home and delegates everything else, so a test sees the
/// runtime's home reach the guard exactly as `serve` gives it.
pub struct HomePluginHost(pub crate::plugin_host::NoPluginHost, pub std::path::PathBuf);

#[async_trait::async_trait]
impl crate::plugin_host::PluginHost for HomePluginHost {
    fn home(&self) -> Option<&std::path::Path> {
        Some(&self.1)
    }

    fn installed(&self) -> anyhow::Result<Vec<gents_pack::plugin::store::InstalledPlugin>> {
        crate::plugin_host::PluginHost::installed(&self.0)
    }

    fn installed_record(
        &self,
        coordinate: &str,
    ) -> anyhow::Result<gents_pack::plugin::store::InstalledPlugin> {
        crate::plugin_host::PluginHost::installed_record(&self.0, coordinate)
    }

    fn resolve(
        &self,
        coordinate: &str,
        pinned: Option<&str>,
    ) -> anyhow::Result<gents_pack::plugin::store::InstalledPlugin> {
        crate::plugin_host::PluginHost::resolve(&self.0, coordinate, pinned)
    }

    async fn call_outcome(
        &self,
        record: &gents_pack::plugin::store::InstalledPlugin,
        input: serde_json::Value,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> anyhow::Result<gents_pack::plugin::PluginOutcome> {
        crate::plugin_host::PluginHost::call_outcome(&self.0, record, input, context).await
    }

    async fn run_plugin_once(
        self: std::sync::Arc<Self>,
        coordinate: &str,
        selected: &[gents_documents::PluginToolRef],
        agent: crate::plugin_host::PluginRunAgent<'_>,
        input: serde_json::Value,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> anyhow::Result<(serde_json::Value, Option<String>, Option<String>)> {
        std::sync::Arc::new(crate::plugin_host::NoPluginHost)
            .run_plugin_once(coordinate, selected, agent, input, context)
            .await
    }

    fn application_write_tool(
        &self,
        access: gents_store::config_client::ConfigAccess,
        collections: std::collections::BTreeSet<String>,
        actor: Option<identity::Did>,
    ) -> anyhow::Result<Box<dyn gents_loop::tool::ToolDyn>> {
        crate::plugin_host::PluginHost::application_write_tool(&self.0, access, collections, actor)
    }

    fn plugin_tool(
        self: std::sync::Arc<Self>,
        plugin: &gents_documents::PluginToolRef,
        read_root: Option<std::path::PathBuf>,
        own_folder: Option<std::path::PathBuf>,
        node: std::sync::Arc<defra_node::Node>,
        node_did: &str,
        agent_did: Option<&str>,
    ) -> anyhow::Result<Box<dyn gents_loop::tool::ToolDyn>> {
        std::sync::Arc::new(crate::plugin_host::NoPluginHost)
            .plugin_tool(plugin, read_root, own_folder, node, node_did, agent_did)
    }
}
