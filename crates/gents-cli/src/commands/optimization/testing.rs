use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use gents_eval::checks::CheckRegistry;
use gents_eval::optimization::{Proposal, Proposer, ScriptedProposer};
use gents_store::collection::Collection;
use tokio_util::sync::CancellationToken;

use super::execute;
use crate::cli::{Cli, Command, OptimizationCommand, UtilsCommand};
use crate::commands::eval::testing::{
    deps, executor, Fixture, CANDIDATE_PROMPT, DEFINITION, VALIDATION_CASES,
};
use crate::commands::eval::Deps;

pub(crate) fn optimization_command(argv: &[&str]) -> OptimizationCommand {
    let cli = Cli::try_parse_from(
        ["gents", "utils", "optimization"]
            .into_iter()
            .chain(argv.iter().copied()),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    match cli.command {
        Some(Command::Utils {
            command: UtilsCommand::Optimization { command },
        }) => command,
        _ => panic!("not an optimization command"),
    }
}

pub(crate) async fn optimization_with(
    fixture: &Fixture,
    argv: &[&str],
    deps: &Deps<'_>,
) -> anyhow::Result<String> {
    let mut out = Vec::new();
    execute(&fixture.ctx, optimization_command(argv), deps, &mut out).await?;
    Ok(String::from_utf8(out)?)
}

pub(crate) async fn optimization(fixture: &Fixture, argv: &[&str]) -> anyhow::Result<String> {
    let executor = executor(&[]);
    let registry = CheckRegistry::builtin();
    optimization_with(
        fixture,
        argv,
        &deps(&executor, &registry, CancellationToken::new()),
    )
    .await
}

pub(crate) fn proposer_file(fixture: &Fixture) -> PathBuf {
    std::fs::create_dir_all(&fixture.ctx.home_dir).unwrap();
    let path = fixture.ctx.home_dir.join("proposals.json");
    let proposals: Vec<serde_json::Value> = (0..3)
        .map(|_| {
            serde_json::json!({
                "text": CANDIDATE_PROMPT,
                "rationale": "name the collection the monitor should read",
            })
        })
        .collect();
    std::fs::write(&path, serde_json::to_vec(&proposals).unwrap()).unwrap();
    path
}

/// A proposer holding the fixture's three proposals, as a job's default
/// `--rounds` consumes them.
pub(crate) fn proposer(fixture: &Fixture) -> ScriptedProposer {
    scripted_proposer(&proposer_file(fixture), 3).unwrap()
}

pub(crate) fn scripted_proposer(path: &Path, rounds: u32) -> Result<ScriptedProposer> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading proposer script {}", path.display()))?;
    let proposals: Vec<Proposal> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing proposer script {}", path.display()))?;
    if proposals.len() < usize::try_from(rounds).unwrap_or(usize::MAX) {
        anyhow::bail!(
            "proposer script {} holds {} proposals, fewer than --rounds {rounds}",
            path.display(),
            proposals.len()
        );
    }
    Ok(ScriptedProposer::new(
        proposals
            .into_iter()
            .map(|proposal| (proposal.text, proposal.rationale))
            .collect(),
    ))
}

pub(crate) fn with_proposer<'a>(base: Deps<'a>, proposer: &'a dyn Proposer) -> Deps<'a> {
    Deps {
        proposer: Some(proposer),
        ..base
    }
}

/// Runs `argv` with the fixture's scripted proposer injected in place of a
/// `--proposer` flag and the executor `scripted`.
pub(crate) async fn optimization_scripted(
    fixture: &Fixture,
    argv: &[&str],
    scripted: &dyn gents_eval::runner::TrialExecutor,
    cancel: CancellationToken,
) -> anyhow::Result<String> {
    let proposer = proposer(fixture);
    let registry = CheckRegistry::builtin();
    optimization_with(
        fixture,
        argv,
        &with_proposer(deps(scripted, &registry, cancel), &proposer),
    )
    .await
}

pub(crate) async fn accepted_job(fixture: &Fixture, job_id: &str) -> String {
    let scripted = executor(&VALIDATION_CASES);
    let pack = fixture.pack_arg();
    optimization_scripted(
        fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--profile",
            "local",
            "--job-id",
            job_id,
        ],
        &scripted,
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

pub(crate) async fn delete_definition(fixture: &Fixture) {
    let deleted = gents_documents::operations::delete::delete(
        &fixture.ctx.access,
        "cli.optimization.test_delete_definition",
        fixture.ctx.owner.as_str(),
        &[(Collection::EvalDefinition, DEFINITION.to_owned())],
        gents_protocol::operations::document::OperationActor::User,
        None,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap()
    .targets
    .len();
    assert_eq!(deleted, 1, "the fixture's eval definition");
}
