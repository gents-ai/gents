//! The installed-plugin runtime as the tool layer sees it: record lookup,
//! producer calls and a one-off tool run. The plugin executor implements it.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use defra_node::Node as EmbeddedNode;

use crate::documents::PluginToolRef;
use crate::plugin::store::InstalledPlugin;
use crate::plugin::PluginOutcome;

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
}
