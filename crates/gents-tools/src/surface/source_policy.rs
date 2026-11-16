use serde_json::Value;

use super::{configured_node_operation_sources, configured_plugin_refs, ToolCeiling};
use gents_documents::{DatastoreToolSurfaceDocument, Tools};
use gents_store::local::{ProjectedOperationSource, ProjectedPluginRef, ToolSourcePolicy};

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

    fn selected_tools(&self, node: &str, context: &Value) -> Option<String> {
        gents_documents::references_of(
            gents_store::collection::Collection::AgentContext,
            context,
            node,
        )
        .ok()?
        .into_iter()
        .find(|reference| reference.collection == gents_store::collection::Collection::Tools)
        .map(|reference| reference.id)
        .filter(|id| !id.is_empty())
    }
}

#[cfg(test)]
mod projection_tests;

#[cfg(test)]
mod runtime_projection_tests;

#[cfg(test)]
mod backend_protection_tests;
