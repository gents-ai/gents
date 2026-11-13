use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};

use crate::operations::pairing::PairingReadError;
use crate::p2p_reconcile::endpoint::EndpointError;
use crate::p2p_reconcile::templates::AppCollectionAdmissionError;

/// `AccessClassification.p2pWrapped`.
#[test]
fn every_p2p_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_p2p_wrapped::replay::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >(|input| {
        let access = input.failure.error();
        let erasure = input.erasure;
        match input.wrapper.as_str() {
            "endpoint_error_access" => classify_erased(erasure, EndpointError::Access(access)),
            "pairing_read_error_access" => {
                classify_erased(erasure, PairingReadError::Access(access))
            }
            "app_collection_admission_error_unreadable" => classify_erased(
                erasure,
                AppCollectionAdmissionError::Unreadable {
                    collection: "Note".into(),
                    source: access,
                },
            ),
            other => panic!(
                "p2p owner variant {other} has no constructor here; add it with its \
                 `AccessClassification.P2pWrapper` constructor"
            ),
        }
    });
}
