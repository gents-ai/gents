//! Helpers for the `gents optimization` tests, over the eval fixture.

use anyhow::Context as _;
use clap::Parser;
use gents::config_client::read_desired_state_record_in_txn;
use gents::eval::checks::CheckRegistry;
use gents::Collection;
use tokio_util::sync::CancellationToken;

use super::execute;
use crate::cli::{Cli, Command, OptimizationCommand};
use crate::commands::eval::testing::{
    deps, executor, Fixture, CANDIDATE_PROMPT, DEFINITION, VALIDATION_CASES,
};
use crate::commands::eval::Deps;

/// `gents optimization <argv…>` as clap parses it.
pub(crate) fn optimization_command(argv: &[&str]) -> OptimizationCommand {
    let cli = Cli::try_parse_from(
        ["gents", "optimization"]
            .into_iter()
            .chain(argv.iter().copied()),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    match cli.command {
        Command::Optimization { command } => command,
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

/// With a scripted executor where every trial passes.
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

/// `scripted:<file>` proposing [`CANDIDATE_PROMPT`] in each of three rounds.
pub(crate) fn proposer_file(fixture: &Fixture) -> String {
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
    format!("scripted:{}", path.display())
}

/// Run a job whose baseline fails every validation case and whose candidate
/// passes them: it reaches `ready_to_promote`. Returns what the run wrote.
pub(crate) async fn accepted_job(fixture: &Fixture, job_id: &str) -> String {
    let scripted = executor(&VALIDATION_CASES);
    let registry = CheckRegistry::builtin();
    let pack = fixture.pack_arg();
    let proposer = proposer_file(fixture);
    optimization_with(
        fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--profile",
            "local",
            "--proposer",
            proposer.as_str(),
            "--job-id",
            job_id,
        ],
        &deps(&scripted, &registry, CancellationToken::new()),
    )
    .await
    .unwrap()
}

/// Delete the fixture's eval definition, as an operator removing it would.
pub(crate) async fn delete_definition(fixture: &Fixture) {
    let owner = fixture.ctx.owner.as_str();
    fixture
        .ctx
        .access
        .transact("cli.optimization.test_delete_definition", |txn| {
            Box::pin(async move {
                let (doc_id, _) = read_desired_state_record_in_txn(
                    txn,
                    Collection::EvalDefinition,
                    owner,
                    DEFINITION,
                )
                .await?
                .context("the fixture's eval definition")?;
                txn.execute(&format!(
                    r#"mutation {{ delete_{name}(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#,
                    name = Collection::EvalDefinition.graphql_type(),
                    doc_id = gents::graphql::escape_graphql_string(&doc_id),
                ))
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
}
