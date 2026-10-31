use super::super::DesiredStateManifest;
use super::{automation, projection, tooling};

pub(crate) fn validate_manifest(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    let plan =
        gents::config_client::DesiredStateApplyPlan::from_pack_config(manifest).and_then(|plan| {
            gents::document_config::ConfigReferences::from_documents(
                &manifest.node.node_did,
                plan.documents()
                    .iter()
                    .map(|doc| (doc.collection, doc.add.clone())),
            )
        });
    if let Err(error) = plan {
        errors.push(format!("{error:#}"));
        return;
    }
    tooling::validate_surfaces(manifest, errors);
    tooling::validate_tools(manifest, errors);
    tooling::validate_tool_service_registries(manifest, errors);
    projection::validate_projection_bindings(manifest, errors);
    automation::validate_automation(manifest, errors);
}
