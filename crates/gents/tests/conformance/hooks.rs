#[path = "hooks/decisions.rs"]
mod decisions;

use gents_lean_vocab::{lean_lifecycle_hook_rerun_cases, LeanLifecycleHookCarrier};
use gents_session::request::hooks::stage::HookError;
use gents_session::session::message::CommitError;
use gents_store::config_client::{ConfigClientError, RerunNote, TransactionFailure};

fn carried(error: anyhow::Error, carrier: LeanLifecycleHookCarrier) -> anyhow::Error {
    match carrier {
        LeanLifecycleHookCarrier::Context => error.context("staging the terminal"),
        LeanLifecycleHookCarrier::Sourced => HookError::Operation(error).into(),
        LeanLifecycleHookCarrier::Transparent => CommitError::Other(error).into(),
        LeanLifecycleHookCarrier::Stringified => anyhow::anyhow!("{error:#}"),
    }
}

/// Lean `Hooks.rerunSurvives`: a must-commit point's request to run the
/// transition again, carried through production wrappers, is what the store's
/// classifier finds, for an application closure and for the session-message
/// commit owner.
#[test]
fn generated_lifecycle_hook_rerun_cases_drive_the_transaction_classifier() {
    let cases = lean_lifecycle_hook_rerun_cases();
    assert_eq!(cases.len(), 85);
    let note = RerunNote::new("request.terminal.stage\ngoals\nrequest-1");
    for case in cases {
        let point = anyhow::Error::from(ConfigClientError::rerun(
            note.clone(),
            Box::new(HookError::PartialStaging {
                plugin: "goals",
                source: Box::new(HookError::Refused {
                    plugin: "goals",
                    reason: "core operation refused midway".into(),
                }),
            }),
        ));
        let error = case.path.iter().copied().fold(point, carried);
        let expected = case.rerun.then_some(&note);
        assert_eq!(
            TransactionFailure::rerun_requested(&error),
            expected,
            "{}: application closure",
            case.name
        );
        let owner = CommitError::Other(error);
        assert_eq!(
            TransactionFailure::rerun_requested(&owner),
            expected,
            "{}: session-message commit",
            case.name
        );
    }
}
