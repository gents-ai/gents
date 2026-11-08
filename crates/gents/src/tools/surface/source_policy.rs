use serde_json::Value;

use super::{configured_node_operation_sources, configured_plugin_refs, ToolCeiling};
use crate::documents::{DatastoreToolSurfaceDocument, Tools};
use crate::store::local::{ProjectedOperationSource, ProjectedPluginRef, ToolSourcePolicy};

impl ToolSourcePolicy for ToolCeiling {
    fn node_operation_sources(
        &self,
        tools: &Value,
        surfaces: &[Value],
    ) -> anyhow::Result<Vec<ProjectedOperationSource>> {
        let tools = serde_json::from_value::<Tools>(tools.clone())?;
        let surfaces = surfaces
            .iter()
            .filter_map(|surface| {
                serde_json::from_value::<DatastoreToolSurfaceDocument>(surface.clone()).ok()
            })
            .collect::<Vec<_>>();
        Ok(configured_node_operation_sources(&tools, &surfaces, self)?
            .into_iter()
            .map(|source| ProjectedOperationSource {
                purpose: source.purpose,
                permission: source.permission,
            })
            .collect())
    }

    fn plugin_refs(&self, tools: &Value) -> anyhow::Result<Vec<ProjectedPluginRef>> {
        let tools = serde_json::from_value::<Tools>(tools.clone())?;
        Ok(configured_plugin_refs(&tools, self)?
            .into_iter()
            .map(|plugin| ProjectedPluginRef {
                plugin: plugin.plugin,
                digest: plugin.digest,
            })
            .collect())
    }
}
