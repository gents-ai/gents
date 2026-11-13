use gents_store::config_client::access_classification_fixture::{
    classify_erased, classify_owner, AccessClassification, AccessFailure, Erasure,
};

use crate::request::hooks::owed::RedriveError;
use crate::request::hooks::stage::HookError;
use crate::request::lifecycle::queue::UserMessageInputError;
use crate::request::watcher::WatcherScanError;
use crate::session::message::commit::CommitError;
use crate::session::SessionControlError;

/// `AccessClassification.sessionWrapped`.
#[test]
fn every_session_owner_variant_preserves_the_access_classification() {
    gents_lean_vocab::families::access_classification_session_wrapped::replay::<
        AccessClassification,
        Erasure,
        AccessFailure,
        String,
    >(|input| {
        let access = input.failure.error();
        let erasure = input.erasure;
        match input.wrapper.as_str() {
            "commit_error_access" => classify_owner(erasure, CommitError::Access(access)),
            "session_control_error_access" => {
                classify_owner(erasure, SessionControlError::Access(access))
            }
            "redrive_error_store" => classify_erased(erasure, RedriveError::Store(access)),
            "hook_error_store" => classify_erased(erasure, HookError::Store(access)),
            "user_message_input_error_read" => {
                classify_erased(erasure, UserMessageInputError::Read(access))
            }
            "watcher_scan_error_query" => classify_erased(
                erasure,
                WatcherScanError::Query {
                    actor: "did:key:z6Mk".into(),
                    source: access,
                },
            ),
            other => panic!(
                "session owner variant {other} has no constructor here; add it with its \
                 `AccessClassification.SessionWrapper` constructor"
            ),
        }
    });
}

/// `AccessClassification.Chain.found_survives_every_wrapping` at a job step:
/// an access error raised by a staging statement reaches the transaction owner
/// through `CommitError::Other` and the step's application error.
#[test]
fn a_conflict_staged_under_a_commit_error_reaches_the_transaction_owner() {
    let staged = CommitError::Other(anyhow::Error::new(
        AccessFailure::TransactionConflict.error(),
    ));
    assert_eq!(
        classify_erased(Erasure::Anyhow, staged),
        AccessClassification::Conflict
    );
}
