use super::super::DesiredStateManifest;
use super::{automation, projection, tooling};
use gents_protocol::error_chain::ErrorChain;

pub(crate) fn validate_manifest(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    let plan = match gents::config_client::DesiredStateApplyPlan::from_pack_config(manifest) {
        Ok(plan) => plan,
        Err(error) => {
            errors.push(format!("{}", ErrorChain(&error)));
            return;
        }
    };
    if let Err(error) = gents::document_config::ConfigReferences::from_documents(
        &manifest.node.node_did,
        plan.documents()
            .iter()
            .map(|doc| (doc.collection, doc.add.clone())),
    ) {
        errors.push(format!("{}", ErrorChain(error.as_ref())));
        return;
    }
    tooling::validate_surfaces(manifest, errors);
    tooling::validate_eth_tools(manifest, errors);
    tooling::validate_tools(manifest, errors);
    tooling::validate_tool_service_registries(manifest, errors);
    projection::validate_projection_bindings(manifest, errors);
    automation::validate_automation(manifest, errors);
}
