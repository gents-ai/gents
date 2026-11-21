//! The installed-plugin runtime as the tool layer sees it: record lookup,
//! producer calls, a one-off tool run, and construction of the plugin and
//! application-write tools. The plugin executor implements it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use defra_node::Node as EmbeddedNode;

use gents_documents::PluginToolRef;
use gents_loop::tool::ToolDyn;
use gents_pack::plugin::store::InstalledPlugin;
use gents_pack::plugin::PluginOutcome;
use gents_store::config_client::ConfigAccess;

/// The agent a plugin run acts for: its node and identities.
pub struct PluginRunAgent<'a> {
    pub node: Arc<EmbeddedNode>,
    pub node_did: &'a str,
    pub agent_did: Option<&'a str>,
}

#[async_trait]
pub trait PluginHost: Send + Sync {
    fn home(&self) -> Option<&Path>;

    /// The plugin store identity this host reads through, when it has a home.
    fn store_pin(&self) -> Option<&gents_pack::plugin::store::StoreReadPin> {
        None
    }

    /// The writer of this host's plugin store: the runtime's, pinned to the
    /// home it started in.
    fn store_writer(&self) -> Option<gents_pack::plugin::store::StoreWriter> {
        self.store_pin()
            .cloned()
            .map(gents_pack::plugin::store::StoreWriter::Runtime)
    }

    /// Every plugin record in the plugin home.
    fn installed(&self) -> Result<Vec<InstalledPlugin>>;

    /// The record of `coordinate` as the plugin home holds it, staged or not.
    fn installed_record(&self, coordinate: &str) -> Result<InstalledPlugin>;

    fn resolve(&self, coordinate: &str, pinned: Option<&str>) -> Result<InstalledPlugin>;

    /// One call of `record` with `input` and no caller-bound services, as
    /// part of the tool call `context` describes.
    async fn call_outcome(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> Result<PluginOutcome>;

    /// `coordinate` run once as the calling agent's Tools select it (or
    /// unselected when they do not): the output, a note for an unserved
    /// request field, and the binding note. `context` is the tool call the
    /// run is part of.
    async fn run_plugin_once(
        self: Arc<Self>,
        coordinate: &str,
        selected: &[PluginToolRef],
        agent: PluginRunAgent<'_>,
        input: serde_json::Value,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> Result<(serde_json::Value, Option<String>, Option<String>)>;

    /// The application-write tool over `collections` for `access`.
    fn application_write_tool(
        &self,
        access: ConfigAccess,
        collections: BTreeSet<String>,
        actor: Option<identity::Did>,
    ) -> Result<Box<dyn ToolDyn>>;

    /// The tool a plugin tool reference resolves to, bound to the calling
    /// agent. `own_folder` is the root the agent's file-writing tools write.
    fn plugin_tool(
        self: Arc<Self>,
        plugin: &PluginToolRef,
        read_root: Option<PathBuf>,
        own_folder: Option<PathBuf>,
        node: Arc<EmbeddedNode>,
        node_did: &str,
        agent_did: Option<&str>,
    ) -> Result<Box<dyn ToolDyn>>;
}

/// The plugin runtime of a context that has none attached. Every lookup and
/// call fails with the same error, and no plugin home exists.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPluginHost;

fn no_runtime() -> anyhow::Error {
    anyhow!("no plugin runtime is attached")
}

#[async_trait]
impl PluginHost for NoPluginHost {
    fn home(&self) -> Option<&Path> {
        None
    }

    fn installed(&self) -> Result<Vec<InstalledPlugin>> {
        Ok(vec![])
    }

    fn installed_record(&self, _coordinate: &str) -> Result<InstalledPlugin> {
        Err(no_runtime())
    }

    fn resolve(&self, _coordinate: &str, _pinned: Option<&str>) -> Result<InstalledPlugin> {
        Err(no_runtime())
    }

    async fn call_outcome(
        &self,
        _record: &InstalledPlugin,
        _input: serde_json::Value,
        _context: &gents_loop::tool::ToolCallEnvelope,
    ) -> Result<PluginOutcome> {
        Err(no_runtime())
    }

    async fn run_plugin_once(
        self: Arc<Self>,
        _coordinate: &str,
        _selected: &[PluginToolRef],
        _agent: PluginRunAgent<'_>,
        _input: serde_json::Value,
        _context: &gents_loop::tool::ToolCallEnvelope,
    ) -> Result<(serde_json::Value, Option<String>, Option<String>)> {
        Err(no_runtime())
    }

    fn application_write_tool(
        &self,
        _access: ConfigAccess,
        _collections: BTreeSet<String>,
        _actor: Option<identity::Did>,
    ) -> Result<Box<dyn ToolDyn>> {
        Err(no_runtime())
    }

    fn plugin_tool(
        self: Arc<Self>,
        _plugin: &PluginToolRef,
        _read_root: Option<PathBuf>,
        _own_folder: Option<PathBuf>,
        _node: Arc<EmbeddedNode>,
        _node_did: &str,
        _agent_did: Option<&str>,
    ) -> Result<Box<dyn ToolDyn>> {
        Err(no_runtime())
    }
}
