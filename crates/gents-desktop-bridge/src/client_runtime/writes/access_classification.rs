use super::LocalHomeAccessError;
use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};

#[test]
fn owner_variants_preserve_access_classification() {
    gents_lean_vocab::families::access_classification_desktop_bridge_wrapped::replay::<
        AccessClassification,
        String,
        Erasure,
        AccessFailure,
    >(|input| match input.wrapper.as_str() {
        "local_home_access_error_signing" => classify_erased(
            input.erasure,
            LocalHomeAccessError::Signing(input.failure.error()),
        ),
        other => panic!("unhandled modeled error wrapper {other}"),
    });
}
