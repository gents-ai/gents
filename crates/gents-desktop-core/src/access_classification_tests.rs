use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};

use crate::client::journal::JournalError;

/// `AccessClassification.desktopCoreWrapped`.
#[test]
fn every_desktop_core_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_desktop_core_wrapped::replay::<
        AccessClassification,
        String,
        Erasure,
        AccessFailure,
    >(|input| match input.wrapper.as_str() {
        "journal_error_access" => {
            classify_erased(input.erasure, JournalError::Access(input.failure.error()))
        }
        other => panic!(
            "desktop-core owner variant {other} has no constructor here; add it with its \
             `AccessClassification.DesktopCoreWrapper` constructor"
        ),
    });
}
