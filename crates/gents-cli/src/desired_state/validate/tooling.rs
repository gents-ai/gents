use super::super::DesiredStateManifest;
use gents_protocol::error_chain::ErrorChain;

pub(super) fn validate_surfaces(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for surface in &manifest.datastore_tool_surfaces {
        for entry in surface.entries.as_deref().unwrap_or(&[]) {
            if let Err(error) = entry.validate() {
                errors.push(format!(
                    "DatastoreToolSurface {}: {}",
                    surface.surface_id,
                    ErrorChain(&error)
                ));
            }
        }
    }
}

pub(super) fn validate_eth_tools(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for binding in &manifest.chain_key_bindings {
        if let Err(error) = binding.validate() {
            errors.push(format!(
                "ChainKeyBinding {}: {}",
                binding.binding_id,
                ErrorChain(&error)
            ));
        }
    }
    for tool in &manifest.eth_tools {
        if let Err(error) = tool.validate() {
            errors.push(format!(
                "EthTool {}: {}",
                tool.tool_id,
                ErrorChain(error.as_ref())
            ));
        }
    }
}

pub(super) fn validate_tools(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for tools in &manifest.tools {
        if let Err(error) = tools.validate() {
            errors.push(format!("{}", ErrorChain(&error)));
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
                "ToolServiceRegistry {}: {}",
                service.service_id,
                ErrorChain(&error)
            ));
        }
    }
}
