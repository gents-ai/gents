use super::super::DesiredStateManifest;
use gents_protocol::error_chain::ErrorChain;

pub(super) fn validate_projection_bindings(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) {
    for binding in &manifest.projection_acp_bindings {
        if let Err(error) = binding.validate() {
            errors.push(format!("{}", ErrorChain(&error)));
        }
    }
}
