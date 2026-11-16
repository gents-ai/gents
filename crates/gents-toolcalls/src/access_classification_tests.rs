use gents_store::config_client::access_classification_fixture::{
    classify_erased, AccessClassification, AccessFailure, Erasure,
};

use crate::call::lifecycle::delivery::TerminalWriteError;
use crate::plugin::application_write::WriteFailure;

/// `AccessClassification.toolcallsWrapped`.
#[test]
fn every_toolcalls_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_toolcalls_wrapped::replay::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >(|input| {
        let access = input.failure.error();
        match input.wrapper.as_str() {
            "terminal_write_error_access" => {
                classify_erased(input.erasure, TerminalWriteError::Access(access))
            }
            "write_failure_access" => classify_erased(input.erasure, WriteFailure::Access(access)),
            other => panic!(
                "toolcalls owner variant {other} has no constructor here; add it with its \
                 `AccessClassification.ToolcallsWrapper` constructor"
            ),
        }
    });
}
