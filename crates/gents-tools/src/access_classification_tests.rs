use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};
use gents_store::views::ViewError;

use crate::builtin::schema_help::SchemaIdentityError;
use crate::mcp::registry::RegistryReadError;

/// `AccessClassification.toolsWrapped`.
#[test]
fn every_tools_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_tools_wrapped::replay::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >(|input| {
        let access = input.failure.error();
        match input.wrapper.as_str() {
            "schema_identity_error_config_client" => {
                classify_erased(input.erasure, SchemaIdentityError::ConfigClient(access))
            }
            "registry_read_error_view" => classify_erased(
                input.erasure,
                RegistryReadError::View(ViewError::Execute(access)),
            ),
            other => panic!(
                "tools owner variant {other} has no constructor here; add it with its \
                 `AccessClassification.ToolsWrapper` constructor"
            ),
        }
    });
}
