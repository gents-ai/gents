use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};

use crate::optimization::promote::PromoteTxnError;

/// `AccessClassification.evalWrapped`.
#[test]
fn every_eval_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_eval_wrapped::replay::<
        AccessClassification,
        Erasure,
        String,
        AccessFailure,
    >(|input| match input.wrapper.as_str() {
        "promote_txn_error_access" => classify_erased(
            input.erasure,
            PromoteTxnError::Access(input.failure.error()),
        ),
        other => panic!(
            "eval owner variant {other} has no constructor here; add it with its \
             `AccessClassification.EvalWrapper` constructor"
        ),
    });
}
