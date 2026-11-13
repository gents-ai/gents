use gents_store::config_client::access_classification_fixture::{
    classify_erased, classify_owner, AccessClassification, AccessFailure, Erasure,
};
use gents_store::views::ViewError;

use crate::auth::credential::runtime_account::RuntimeAccountMetadataError;
use crate::resolved::visibility::ConfigurationVisibilityError;
use crate::views::backends::InventoryError;

/// `AccessClassification.inferenceWrapped`.
#[test]
fn every_inference_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_inference_wrapped::replay::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >(|input| {
        let access = input.failure.error();
        match input.wrapper.as_str() {
            "inventory_error_store" => classify_owner(input.erasure, InventoryError::Store(access)),
            "inventory_error_view" => classify_owner(
                input.erasure,
                InventoryError::View(ViewError::Execute(access)),
            ),
            "runtime_account_metadata_error_access" => {
                classify_erased(input.erasure, RuntimeAccountMetadataError::Access(access))
            }
            "configuration_visibility_error_access" => {
                classify_owner(input.erasure, ConfigurationVisibilityError::Access(access))
            }
            other => panic!(
                "inference owner variant {other} has no constructor here; add it with its \
                 `AccessClassification.InferenceWrapper` constructor"
            ),
        }
    });
}
