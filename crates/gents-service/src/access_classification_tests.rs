use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};

use crate::runtime_control::ControlFailure;

/// `AccessClassification.serviceWrapped`.
#[test]
fn every_service_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_service_wrapped::replay::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >(|input| match input.wrapper.as_str() {
        "control_failure_authorization" => classify_erased(
            input.erasure,
            ControlFailure::Authorization(Box::new(input.failure.error())),
        ),
        other => panic!(
            "service owner variant {other} has no constructor here; add it with its \
             `AccessClassification.ServiceWrapper` constructor"
        ),
    });
}
