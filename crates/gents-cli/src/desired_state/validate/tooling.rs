use super::super::DesiredStateManifest;

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
