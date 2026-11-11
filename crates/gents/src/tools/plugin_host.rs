//! The installed-plugin runtime as the tool layer sees it: record lookup,
//! producer calls, a one-off tool run, and construction of the plugin and
//! application-write tools. The plugin executor implements it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use defra_node::Node as EmbeddedNode;

use crate::documents::PluginToolRef;
use crate::inference::llm::tool::ToolDyn;
use crate::plugin::store::InstalledPlugin;
use crate::plugin::PluginOutcome;
use crate::store::config_client::ConfigAccess;

#[async_trait]
pub trait PluginHost: Send + Sync {
    fn home(&self) -> Option<&Path>;

    /// Every plugin record in the plugin home.
    fn installed(&self) -> Result<Vec<InstalledPlugin>>;

    /// The record of `coordinate` as the plugin home holds it, staged or not.
    fn installed_record(&self, coordinate: &str) -> Result<InstalledPlugin>;

    fn resolve(&self, coordinate: &str, pinned: Option<&str>) -> Result<InstalledPlugin>;

    /// One call of `record` with `input` and no caller-bound services.
    async fn call_outcome(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
    ) -> Result<PluginOutcome>;

    /// `coordinate` run once as the calling agent's Tools select it (or
    /// unselected when they do not): the output, a note for an unserved
    /// request field, and the binding note.
    async fn run_plugin_once(
        self: Arc<Self>,
        coordinate: &str,
        selected: &[PluginToolRef],
        node: Arc<EmbeddedNode>,
        node_did: &str,
        agent_did: Option<&str>,
        input: serde_json::Value,
    ) -> Result<(serde_json::Value, Option<String>, Option<String>)>;

    /// The application-write tool over `collections` for `access`.
    fn application_write_tool(
        &self,
        access: ConfigAccess,
        collections: BTreeSet<String>,
        actor: Option<identity::Did>,
    ) -> Result<Box<dyn ToolDyn>>;

    /// The tool a plugin tool reference resolves to, bound to the calling agent.
    fn plugin_tool(
        self: Arc<Self>,
        plugin: &PluginToolRef,
        read_root: Option<PathBuf>,
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
    ) -> Result<PluginOutcome> {
        Err(no_runtime())
    }

    async fn run_plugin_once(
        self: Arc<Self>,
        _coordinate: &str,
        _selected: &[PluginToolRef],
        _node: Arc<EmbeddedNode>,
        _node_did: &str,
        _agent_did: Option<&str>,
        _input: serde_json::Value,
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
        _node: Arc<EmbeddedNode>,
        _node_did: &str,
        _agent_did: Option<&str>,
    ) -> Result<Box<dyn ToolDyn>> {
        Err(no_runtime())
    }
}
