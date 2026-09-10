use super::super::DesiredStateManifest;

// Offline checks validate authored values only. The existing apply transaction
// validates references, expansion, collisions and obligations against the full
// retained candidate; packs may refer to documents already installed there.
pub(super) fn validate_surfaces(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for surface in &manifest.datastore_tool_surfaces {
        for entry in surface.entries.as_deref().unwrap_or(&[]) {
            if let Err(error) = entry.validate() {
                errors.push(format!(
                    "DatastoreToolSurface {}: {error:#}",
                    surface.surface_id
                ));
            }
        }
    }
}

pub(super) fn validate_eth_tools(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for binding in &manifest.chain_key_bindings {
        if let Err(error) = binding.validate() {
            errors.push(format!("ChainKeyBinding {}: {error:#}", binding.binding_id));
        }
    }
    for tool in &manifest.eth_tools {
        if let Err(error) = tool.validate() {
            errors.push(format!("EthTool {}: {error:#}", tool.tool_id));
        }
    }
}

pub(super) fn validate_tools(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for tools in &manifest.tools {
        if let Err(error) = tools.validate() {
            errors.push(format!("{error:#}"));
        }
    }
}

pub(super) fn validate_tool_service_registries(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) {
    for service in &manifest.tool_service_registries {
        if let Err(error) = service.validate() {
            errors.push(format!(
                "ToolServiceRegistry {}: {error:#}",
                service.service_id
            ));
        }
    }
}
