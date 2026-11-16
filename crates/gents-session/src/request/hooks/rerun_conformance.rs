use gents_store::config_client::{ConfigClientError, RerunNote, TransactionFailure};

use super::stage::HookError;
use crate::session::message::CommitError;

fn carried(error: anyhow::Error, carrier: &str) -> anyhow::Error {
    match carrier {
        "context" => error.context("staging the terminal"),
        "sourced" => HookError::Operation(error).into(),
        "transparent" => CommitError::Other(error).into(),
        "stringified" => anyhow::anyhow!("{error:#}"),
        other => panic!("no production carrier for {other}"),
    }
}

#[test]
fn rerun_requests_survive_production_wrappers_as_the_lean_model_says() {
    let note = RerunNote::new("request.terminal.stage\ngoals\nrequest-1");
    gents_lean_vocab::families::hooks_contracts_rerun::replay::<String>(|input| {
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
        let error = input
            .path
            .iter()
            .fold(point, |error, carrier| carried(error, carrier));
        let found = |requested: Option<&RerunNote>| match requested {
            Some(requested) if requested == &note => true,
            Some(other) => panic!("{}: a different rerun note {other:?}", input.name),
            None => false,
        };
        let application = found(TransactionFailure::rerun_requested(&error));
        let owner = CommitError::Other(error);
        assert_eq!(
            found(TransactionFailure::rerun_requested(&owner)),
            application,
            "{}: the session-message commit owner and an application closure disagree",
            input.name
        );
        application
    });
}
