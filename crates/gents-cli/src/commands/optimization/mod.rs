//! `gents optimization`: thin commands over `gents::optimization`. A job is
//! driven only by a scripted proposer supplied as a file until an LLM
//! proposer exists.
//!
//! Refusals the library returns are re-raised with exactly their text
//! ([`surface_refusal`]); `main` prints them and exits 1. Clap exits 2 on a
//! usage error.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use gents::eval::checks::CheckRegistry;
use gents::eval::documents::default_breaker_threshold;
use gents::eval::runner::embedded::EmbeddedExecutor;
use gents::eval::runner::{run_dir, RunOptions};
use gents::optimization::{
    derive_state, job_dir, job_refused, load_job, promote_refused, removable, run_job,
    show as show_job, Budgets, JobOutcome, JobRequest, JobState, PolicyV2, Proposal,
    ScriptedProposer,
};
use gents::{default_behavior_id_for_agent, default_inference_profile_id_for_behavior};
use tokio_util::sync::CancellationToken;

use crate::cli::{
    OptimizationCommand, OptimizationDigestArgs, OptimizationRmArgs, OptimizationRunArgs,
    OptimizationShowArgs, PolicyArg,
};
use crate::commands::eval::{
    cancel_on_ctrl_c, default_id, follow_progress, load_policy, source_commit, source_dirty,
    write_json, Deps, EvalContext, Progress,
};
use crate::commands::pack::resolve_subject_pack;

mod render;
#[cfg(test)]
pub(crate) mod testing;

/// The structural gate's cap on a proposed text, in bytes: the default of
/// `optimization run --max-text-bytes`.
pub(crate) const DEFAULT_MAX_TEXT_BYTES: usize = 32 * 1024;

pub(crate) async fn dispatch(command: OptimizationCommand) -> Result<()> {
    let ctx = EvalContext::resolve(command.scope()).await?;
    let executor = EmbeddedExecutor::new(gents::DocumentRuntimeOptions::default(), ctx.runs_dir());
    let registry = CheckRegistry::builtin();
    // Only a command that hosts a loop replaces the default interrupt: a
    // read-only command stays killable by Ctrl-C.
    let cancel = if matches!(command, OptimizationCommand::Run(_)) {
        cancel_on_ctrl_c(
            "interrupt: the job stops launching and will print the command that resumes it; interrupt again to exit now",
        )
    } else {
        CancellationToken::new()
    };
    let deps = Deps {
        executor: &executor,
        registry: &registry,
        cancel,
        options: RunOptions::default(),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    execute(&ctx, command, &deps, &mut out).await
}

pub(crate) async fn execute(
    ctx: &EvalContext,
    command: OptimizationCommand,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let result = match command {
        OptimizationCommand::Run(args) => run(ctx, &args, deps, out).await,
        OptimizationCommand::Show(args) => show(ctx, &args, out).await,
        OptimizationCommand::Promote(args) => promote(ctx, &args, out).await,
        OptimizationCommand::Revert(args) => revert(ctx, &args, out).await,
        OptimizationCommand::Rm(args) => rm(ctx, &args, out).await,
    };
    result.map_err(surface_refusal)
}

/// The driver's and the promotion's refusals verbatim, then the eval layer's.
fn surface_refusal(error: anyhow::Error) -> anyhow::Error {
    let verbatim = job_refused(&error)
        .map(ToString::to_string)
        .or_else(|| promote_refused(&error).map(ToString::to_string));
    match verbatim {
        Some(text) => anyhow::anyhow!(text),
        None => crate::commands::eval::surface_refusal(error),
    }
}

/// One proposal per round: a script shorter than `rounds` is refused here,
/// before the job is frozen, rather than failing the job at the round it
/// cannot answer.
fn scripted_proposer(path: &Path, rounds: u32) -> Result<ScriptedProposer> {
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

async fn run(
    ctx: &EvalContext,
    args: &OptimizationRunArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let proposer_arg = args.proposer.as_ref().context(
        "optimization run needs a proposer: no model-driven proposer is available yet; pass --proposer scripted:<file>",
    )?;
    let proposer = scripted_proposer(&proposer_arg.script, args.rounds)?;
    let subject = resolve_subject_pack(
        &ctx.home_dir,
        &args.subject.pack,
        args.registry.as_deref(),
        true,
    )
    .await?;
    let baseline_pack = subject
        .directory()
        .context("the subject pack did not resolve to a directory")?
        .to_path_buf();
    let behavior_id = match &args.subject.behavior {
        Some(behavior) => behavior.clone(),
        None => subject.default_behavior()?,
    };
    // Bonferroni: the divisor must be the number of candidates the budget
    // allows, and the driver refuses a policy that disagrees.
    // `defaults` is the uncalibrated policy sized to `--rounds`; a policy
    // file is taken as written, and the driver refuses its `max_rounds` when
    // it is not `--rounds`.
    let policy = match &args.policy {
        None | Some(PolicyArg::Defaults) => PolicyV2 {
            max_rounds: args.rounds,
            ..PolicyV2::uncalibrated()
        },
        Some(arg @ PolicyArg::File(_)) => load_policy(arg)?,
    };
    let job_id = args
        .job_id
        .clone()
        .unwrap_or_else(|| default_id(&args.definition_id));
    let request = JobRequest {
        job_id: job_id.clone(),
        owner: ctx.owner.clone(),
        evaluator_did: ctx.owner.clone(),
        behavior_id,
        definition_id: args.definition_id.clone(),
        inference_profile_id: args.profile.clone().unwrap_or_else(|| {
            default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(&ctx.owner))
        }),
        baseline_pack,
        trials_per_case: args.trials,
        budgets: Budgets {
            max_rounds: args.rounds,
            max_case_trials: args.max_case_trials,
            max_tokens: args.max_tokens.unwrap_or(u64::MAX),
            deadline_unix_secs: None,
        },
        max_text_bytes: args.max_text_bytes,
        seed_base: args.seed_base,
        jobs_dir: ctx.jobs_dir(),
        runs_dir: ctx.runs_dir(),
        source_commit: source_commit(),
        source_dirty: source_dirty(),
        concurrency: 1,
        max_infra_retries: 1,
        breaker_threshold: default_breaker_threshold(),
        deadline_secs: None,
        // Each stage's captures come from the definition; no request-level
        // fallback, so a resume repeats the frozen origin exactly.
        captures: Vec::new(),
        run_options: deps.options.clone(),
    };
    let outcome = follow(
        ctx,
        &job_id,
        !args.json,
        out,
        run_job(
            &ctx.access,
            &request,
            deps.executor,
            &proposer,
            deps.registry,
            &policy,
            deps.cancel.clone(),
        ),
    )
    .await?;
    // A job left running is not a success: the view still renders, then the
    // command fails with the resume note, so a script never reads it as done.
    let stopped = (outcome.state == JobState::Running).then(|| {
        format!(
            "job {job_id} stopped before it finished; run the same command with --job-id {job_id} to resume it"
        )
    });
    let view = show_job(&ctx.access, &ctx.owner, &job_id).await?;
    if args.json {
        write_json(out, &view)?;
    } else {
        render::job_table(&view, out)?;
    }
    match stopped {
        Some(note) => Err(anyhow::anyhow!(note)),
        None => Ok(()),
    }
}

/// Drive the job, printing each journal entry as it lands.
async fn follow<F>(
    ctx: &EvalContext,
    job_id: &str,
    print: bool,
    out: &mut dyn Write,
    running: F,
) -> Result<JobOutcome>
where
    F: std::future::Future<Output = Result<JobOutcome>>,
{
    let mut progress = JournalProgress {
        ctx,
        job_id,
        printed: 0,
        out,
    };
    follow_progress(print, &mut progress, running).await
}

/// The job's journal entries, printed once each in journal order.
struct JournalProgress<'a> {
    ctx: &'a EvalContext,
    job_id: &'a str,
    printed: usize,
    out: &'a mut dyn Write,
}

impl Progress for JournalProgress<'_> {
    async fn report(&mut self) {
        print_new_entries(self.ctx, self.job_id, &mut self.printed, &mut *self.out).await;
    }
}

async fn print_new_entries(
    ctx: &EvalContext,
    job_id: &str,
    printed: &mut usize,
    out: &mut dyn Write,
) {
    let journal = match load_job(&ctx.access, &ctx.owner, job_id).await {
        Ok(Some(job)) => job.journal,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(job_id, error = %format!("{error:#}"), "could not read the job's journal for progress");
            return;
        }
    };
    for entry in journal.iter().skip(*printed) {
        if let Err(error) = writeln!(out, "{}", render::journal_line(entry)) {
            tracing::warn!(error = %error, "could not write a progress line");
            return;
        }
        *printed += 1;
    }
}

async fn show(ctx: &EvalContext, args: &OptimizationShowArgs, out: &mut dyn Write) -> Result<()> {
    let view = show_job(&ctx.access, &ctx.owner, &args.job_id).await?;
    if args.json {
        write_json(out, &view)
    } else {
        Ok(render::job_table(&view, out)?)
    }
}

async fn promote(
    ctx: &EvalContext,
    args: &OptimizationDigestArgs,
    out: &mut dyn Write,
) -> Result<()> {
    // Read first, so a promotion that lands is never followed by an error;
    // a job that is not there is `promote`'s to refuse.
    let policy = load_job(&ctx.access, &ctx.owner, &args.job_id)
        .await?
        .map(|job| job.origin.policy);
    let promotion = gents::optimization::promote(
        &ctx.access,
        &ctx.owner,
        &args.job_id,
        &args.digest,
        &ctx.owner,
    )
    .await?;
    if let Some(policy) = &policy {
        render::uncalibrated_banner(policy, out)?;
    }
    writeln!(
        out,
        "promoted job {}: the target now digests to {} (was {}); `gents optimization revert {} --digest {}` undoes it",
        args.job_id,
        promotion.target_digest,
        promotion.previous_digest,
        args.job_id,
        promotion.target_digest
    )?;
    Ok(())
}

async fn revert(
    ctx: &EvalContext,
    args: &OptimizationDigestArgs,
    out: &mut dyn Write,
) -> Result<()> {
    gents::optimization::revert(
        &ctx.access,
        &ctx.owner,
        &args.job_id,
        &args.digest,
        &ctx.owner,
    )
    .await?;
    writeln!(
        out,
        "reverted job {}; Reverted is final, and a further promotion is a new job",
        args.job_id
    )?;
    Ok(())
}

/// Delete `<jobs_dir>/<job_id>/`, the job's own jobs directory as its origin
/// recorded it, when that is under this home. Its documents and runs stay.
async fn rm(ctx: &EvalContext, args: &OptimizationRmArgs, out: &mut dyn Write) -> Result<()> {
    validate_job_id(ctx, &args.job_id)?;
    let job = load_job(&ctx.access, &ctx.owner, &args.job_id)
        .await?
        .with_context(|| format!("no optimization job {:?} for {}", args.job_id, ctx.owner))?;
    let dir = job_dir(&job.origin.jobs_dir, &args.job_id);
    anyhow::ensure!(
        dir.is_dir(),
        "job {} has no directory {}",
        args.job_id,
        dir.display()
    );
    // Another home's directory is never this home's to delete, --force or
    // not: the path comes from the job's document.
    let dir = crate::commands::eval::manage::job_dir_in_this_home(ctx, &job)?;
    let state = derive_state(&job.journal);
    if !args.force && !removable(&state) {
        anyhow::bail!(
            "job {} is {}: only a job that is nothing_to_promote, exhausted, failed, stale, promoted or reverted is removed without --force",
            args.job_id,
            state.label()
        );
    }
    let bytes = crate::commands::eval::manage::dir_size(&dir)?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    tracing::warn!(job_id = %args.job_id, bytes, "optimization job directory removed; its documents stay");
    let unpromotable = if state == JobState::ReadyToPromote {
        format!(
            "; a later `gents optimization promote {}` will refuse: the retained checkpoint pack under {} is gone",
            args.job_id,
            dir.display()
        )
    } else {
        String::new()
    };
    writeln!(
        out,
        "removed {} ({bytes} bytes reclaimed); the job's documents and runs stay{unpromotable}",
        dir.display()
    )?;
    Ok(())
}

/// The driver's job-id rule, checked before any I/O or path: not blank, and
/// one ordinary path component, whose owner is [`run_dir`].
fn validate_job_id(ctx: &EvalContext, job_id: &str) -> Result<()> {
    anyhow::ensure!(
        !job_id.trim().is_empty(),
        "job_id {job_id:?} must not be blank"
    );
    run_dir(&ctx.jobs_dir(), job_id)
        .map_err(|_| anyhow::anyhow!("job_id {job_id:?} must be one ordinary path component"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use gents::eval::checks::CheckRegistry;
    use tokio_util::sync::CancellationToken;

    use super::execute;
    use super::testing::{
        accepted_job, delete_definition, optimization, optimization_command, optimization_with,
        proposer_file,
    };
    use crate::cli::Cli;
    use crate::commands::eval::testing::{deps, eval, executor, Fixture, DEFINITION};
    use crate::commands::eval::UNCALIBRATED_BANNER;

    #[tokio::test]
    async fn run_refuses_without_a_proposer_and_parses_only_the_scripted_one() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let error = optimization(&fixture, &["run", DEFINITION, "--subject", pack.as_str()])
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "optimization run needs a proposer: no model-driven proposer is available yet; pass --proposer scripted:<file>"
        );
        let usage = match Cli::try_parse_from([
            "gents",
            "optimization",
            "run",
            DEFINITION,
            "--subject",
            "monitor",
            "--proposer",
            "llm",
        ]) {
            Ok(_) => panic!("an unknown proposer must not parse"),
            Err(error) => error,
        };
        assert!(
            usage.to_string().contains(
                "no model-driven proposer is available yet; pass --proposer scripted:<file>"
            ),
            "{usage}"
        );
        assert_eq!(usage.exit_code(), 2);
    }

    #[tokio::test]
    async fn a_scripted_job_runs_to_ready_to_promote_and_show_recomputes_its_decisions() {
        let fixture = Fixture::new().await;
        let output = accepted_job(&fixture, "job-1").await;
        assert!(output.lines().any(|line| line == "frozen"), "{output}");
        assert!(output.contains("finalized ready_to_promote"), "{output}");
        assert!(
            output.contains("job job-1 state ready_to_promote"),
            "{output}"
        );

        assert!(output.contains(UNCALIBRATED_BANNER), "{output}");

        let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
        assert_eq!(shown.lines().next(), Some(UNCALIBRATED_BANNER), "{shown}");
        assert!(shown.contains("state ready_to_promote"), "{shown}");
        assert!(shown.contains("checkpoint round 1 pack "), "{shown}");
        let accepted = shown
            .lines()
            .find(|line| line.contains("journaled accept"))
            .unwrap_or_else(|| panic!("{shown}"));
        assert!(accepted.contains("recomputed accept"), "{accepted}");
        assert!(!shown.contains("MISMATCH"), "{shown}");

        let json = optimization(&fixture, &["show", "job-1", "--json"])
            .await
            .unwrap();
        assert!(!json.contains(UNCALIBRATED_BANNER), "{json}");
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["state"]["state"], "ready_to_promote");
        assert_eq!(json["job"]["job_id"], "job-1");

        // With its definition deleted the job still shows, and no decision
        // claims a recomputation it could not make.
        delete_definition(&fixture).await;
        let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
        assert!(
            shown.contains("the eval definition changed since the job froze it"),
            "{shown}"
        );
        let accepted = shown
            .lines()
            .find(|line| line.contains("journaled accept"))
            .unwrap_or_else(|| panic!("{shown}"));
        assert!(
            accepted
                .contains("recomputed not recomputable: definition changed or runs invalidated"),
            "{accepted}"
        );
        assert!(!shown.contains("MISMATCH"), "{shown}");
    }

    #[tokio::test]
    async fn policy_defaults_is_sized_to_rounds_and_a_policy_file_is_taken_as_written() {
        use crate::commands::eval::testing::VALIDATION_CASES;

        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let proposer = proposer_file(&fixture);
        let scripted = executor(&VALIDATION_CASES);
        let registry = CheckRegistry::builtin();
        fn argv<'a>(
            pack: &'a str,
            proposer: &'a str,
            job_id: &'a str,
            policy: &'a str,
        ) -> Vec<&'a str> {
            vec![
                "run",
                DEFINITION,
                "--subject",
                pack,
                "--profile",
                "local",
                "--proposer",
                proposer,
                "--rounds",
                "2",
                "--policy",
                policy,
                "--job-id",
                job_id,
            ]
        }
        let output = optimization_with(
            &fixture,
            &argv(&pack, &proposer, "defaults-2", "defaults"),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert!(output.contains("finalized "), "{output}");
        assert!(
            output.contains("job defaults-2 state ready_to_promote"),
            "{output}"
        );
        assert!(
            output.contains(UNCALIBRATED_BANNER),
            "the defaults sized to --rounds are still uncalibrated: {output}"
        );

        // A file holding the uncalibrated policy says max_rounds 3.
        let path = fixture.ctx.home_dir.join("policy.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&gents::optimization::PolicyV2::uncalibrated()).unwrap(),
        )
        .unwrap();
        let path = path.display().to_string();
        let error = optimization_with(
            &fixture,
            &argv(&pack, &proposer, "file-2", &path),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "policy max_rounds 3 does not match the budget's max_rounds 2; the Bonferroni divisor must be the number of candidates the job may try"
        );
        assert_eq!(format!("{error:#}"), error.to_string());

        // A policy other than the defaults is calibrated: no banner.
        std::fs::write(
            &path,
            serde_json::to_vec(&gents::optimization::PolicyV2 {
                max_rounds: 2,
                alpha_ppm: 40_000,
                ..gents::optimization::PolicyV2::uncalibrated()
            })
            .unwrap(),
        )
        .unwrap();
        let output = optimization_with(
            &fixture,
            &argv(&pack, &proposer, "calibrated-2", &path),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert!(output.contains("job calibrated-2 state "), "{output}");
        assert!(!output.contains(UNCALIBRATED_BANNER), "{output}");
    }

    #[tokio::test]
    async fn a_job_id_that_is_not_one_path_component_is_refused_verbatim() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let proposer = super::testing::proposer_file(&fixture);
        let error = optimization(
            &fixture,
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
                "a/b",
            ],
        )
        .await
        .unwrap_err();
        let refusal = r#"job_id "a/b" must be one ordinary path component"#;
        assert_eq!(error.to_string(), refusal);
        assert_eq!(
            format!("{error:#}"),
            refusal,
            "a refusal carries no context"
        );
        // The driver's refusal, under any context, surfaces as its own text.
        let wrapped = anyhow::Error::from(gents::optimization::JobRefused(refusal.to_owned()))
            .context("driving the job");
        assert_eq!(format!("{:#}", super::surface_refusal(wrapped)), refusal);
    }

    #[tokio::test]
    async fn an_interrupted_job_says_how_to_resume_and_the_same_command_finishes_it() {
        use crate::commands::eval::testing::VALIDATION_CASES;

        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let proposer = proposer_file(&fixture);
        let registry = CheckRegistry::builtin();
        let argv = |job_id: &'static str, json: bool| {
            let mut argv = vec![
                "run".to_owned(),
                DEFINITION.to_owned(),
                "--subject".to_owned(),
                pack.clone(),
                "--profile".to_owned(),
                "local".to_owned(),
                "--proposer".to_owned(),
                proposer.clone(),
                "--job-id".to_owned(),
                job_id.to_owned(),
            ];
            if json {
                argv.push("--json".to_owned());
            }
            argv
        };
        let interrupted = || {
            let token = CancellationToken::new();
            token.cancel();
            token
        };
        fn refs(argv: &[String]) -> Vec<&str> {
            argv.iter().map(String::as_str).collect()
        }

        let stopped = executor(&[]);
        let mut out = Vec::new();
        let error = execute(
            &fixture.ctx,
            optimization_command(&refs(&argv("job-stop", false))),
            &deps(&stopped, &registry, interrupted()),
            &mut out,
        )
        .await
        .unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "job job-stop stopped before it finished; run the same command with --job-id job-stop to resume it"
        );
        let output = String::from_utf8(out).unwrap();
        assert!(output.contains("job job-stop state running "), "{output}");
        assert!(!output.contains("stopped before it finished"), "{output}");
        let shown = optimization(&fixture, &["show", "job-stop"]).await.unwrap();
        assert!(shown.contains("job job-stop state running "), "{shown}");

        let scripted = executor(&VALIDATION_CASES);
        let output = optimization_with(
            &fixture,
            &refs(&argv("job-stop", false)),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert!(
            output.contains("job job-stop state ready_to_promote"),
            "{output}"
        );
        assert!(!output.contains("stopped before it finished"), "{output}");
        let journal =
            gents::optimization::load_job(&fixture.ctx.access, &fixture.ctx.owner, "job-stop")
                .await
                .unwrap()
                .expect("the job")
                .journal;
        assert_eq!(
            journal
                .iter()
                .filter(|entry| matches!(entry, gents::optimization::JournalEntry::Frozen))
                .count(),
            1,
            "the resume froze nothing again: {journal:?}"
        );

        // With --json, stdout is the view alone: the note is the error.
        let mut out = Vec::new();
        let error = execute(
            &fixture.ctx,
            optimization_command(&refs(&argv("job-json", true))),
            &deps(&stopped, &registry, interrupted()),
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("job job-json stopped before it finished"),
            "{error:#}"
        );
        let output = String::from_utf8(out).unwrap();
        let json: serde_json::Value = serde_json::from_str(&output)
            .unwrap_or_else(|error| panic!("stdout is not pure JSON ({error}): {output}"));
        assert_eq!(json["state"]["state"], "running");
        assert_eq!(json["job"]["job_id"], "job-json");
    }

    #[tokio::test]
    async fn a_script_with_fewer_proposals_than_rounds_is_refused_before_the_freeze() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let proposer = proposer_file(&fixture);
        let error = optimization(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--subject",
                pack.as_str(),
                "--profile",
                "local",
                "--proposer",
                proposer.as_str(),
                "--rounds",
                "4",
                "--job-id",
                "short-script",
            ],
        )
        .await
        .unwrap_err();
        let path = proposer.trim_start_matches("scripted:");
        assert_eq!(
            error.to_string(),
            format!("proposer script {path} holds 3 proposals, fewer than --rounds 4")
        );
        assert!(!fixture.ctx.jobs_dir().join("short-script").exists());
        assert!(gents::optimization::load_job(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "short-script"
        )
        .await
        .unwrap()
        .is_none());
    }

    #[tokio::test]
    async fn promote_refuses_a_wrong_digest_then_promotes_and_revert_closes_the_job() {
        let fixture = Fixture::new().await;
        accepted_job(&fixture, "job-1").await;
        let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap();
        let digest = view
            .checkpoint
            .expect("an accepted job retains a checkpoint")
            .pack_digest;

        let wrong = optimization(&fixture, &["promote", "job-1", "--digest", "sha256:wrong"])
            .await
            .unwrap_err();
        assert!(wrong.to_string().starts_with("wrong_digest: "), "{wrong:#}");

        let promoted = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
            .await
            .unwrap();
        assert_eq!(
            promoted.lines().next(),
            Some(UNCALIBRATED_BANNER),
            "{promoted}"
        );
        assert!(
            promoted.lines().nth(1).is_some_and(
                |line| line.starts_with("promoted job job-1: the target now digests to ")
            ),
            "{promoted}"
        );
        let target = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap()
            .job
            .journal
            .iter()
            .rev()
            .find_map(|entry| match entry {
                gents::optimization::JournalEntry::Promoted { target_digest, .. } => {
                    Some(target_digest.clone())
                }
                _ => None,
            })
            .expect("a promotion is journaled");
        assert!(
            promoted.contains(&format!("--digest {target}")),
            "{promoted}"
        );

        let again = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
            .await
            .unwrap_err();
        assert!(again.to_string().starts_with("not_ready: "), "{again:#}");

        let wrong = optimization(&fixture, &["revert", "job-1", "--digest", "sha256:wrong"])
            .await
            .unwrap_err();
        assert!(wrong.to_string().starts_with("wrong_digest: "), "{wrong:#}");
        let reverted = optimization(&fixture, &["revert", "job-1", "--digest", target.as_str()])
            .await
            .unwrap();
        assert_eq!(
            reverted.trim_end(),
            "reverted job job-1; Reverted is final, and a further promotion is a new job"
        );
        let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
        assert!(shown.contains("state reverted"), "{shown}");
    }

    #[tokio::test]
    async fn rm_keeps_running_and_ready_jobs_and_removes_a_reverted_one() {
        let fixture = Fixture::new().await;
        // What Ctrl-C leaves: a job frozen and still running.
        let pack = fixture.pack_arg();
        let proposer = proposer_file(&fixture);
        let scripted = executor(&[]);
        let registry = CheckRegistry::builtin();
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        optimization_with(
            &fixture,
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
                "job-running",
            ],
            &deps(&scripted, &registry, interrupted),
        )
        .await
        .unwrap_err();
        let running_dir = fixture.ctx.jobs_dir().join("job-running");
        assert!(
            running_dir.is_dir(),
            "freeze copied the baseline into the job directory"
        );
        let refused = optimization(&fixture, &["rm", "job-running"])
            .await
            .unwrap_err();
        assert_eq!(
            refused.to_string(),
            "job job-running is running: only a job that is nothing_to_promote, exhausted, failed, stale, promoted or reverted is removed without --force"
        );
        let forced = optimization(&fixture, &["rm", "job-running", "--force"])
            .await
            .unwrap();
        assert!(forced.contains("bytes reclaimed"), "{forced}");
        assert!(!running_dir.exists());

        // A job waiting to be promoted keeps its directory.
        accepted_job(&fixture, "job-1").await;
        let kept = optimization(&fixture, &["rm", "job-1"]).await.unwrap_err();
        assert!(
            kept.to_string()
                .starts_with("job job-1 is ready_to_promote: "),
            "{kept:#}"
        );

        // `eval rm` refuses a run a job still needs.
        let held = gents::optimization::held_runs(&fixture.ctx.access, &fixture.ctx.owner)
            .await
            .unwrap();
        let (run_id, _) = held
            .iter()
            .find(|(_, (job, _))| job == "job-1")
            .expect("the ready job holds its runs");
        let refused = eval(&fixture, &["rm", run_id.as_str()]).await.unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "run {run_id} is evidence of optimization job job-1, which is ready_to_promote; pass --force to delete its directory anyway"
            )
        );

        // Promoted, then reverted: removable.
        let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap();
        let digest = view.checkpoint.expect("a retained checkpoint").pack_digest;
        let promotion = gents::optimization::promote(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        gents::optimization::revert(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &promotion.target_digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        let removed = optimization(&fixture, &["rm", "job-1"]).await.unwrap();
        assert!(removed.starts_with("removed "), "{removed}");
        assert!(!fixture.ctx.jobs_dir().join("job-1").exists());
        let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
        assert!(
            shown.contains("state reverted"),
            "the documents stay: {shown}"
        );
    }

    #[tokio::test]
    async fn rm_force_of_a_ready_job_says_a_later_promote_will_refuse() {
        let fixture = Fixture::new().await;
        accepted_job(&fixture, "job-1").await;
        let digest = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap()
            .checkpoint
            .expect("an accepted job retains a checkpoint")
            .pack_digest;
        let removed = optimization(&fixture, &["rm", "job-1", "--force"])
            .await
            .unwrap();
        let dir = fixture.ctx.jobs_dir().join("job-1");
        assert!(
            removed.lines().any(|line| line.contains(&format!(
                "; a later `gents optimization promote job-1` will refuse: the retained checkpoint pack under {} is gone",
                dir.display()
            ))),
            "{removed}"
        );
        let refused = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
            .await
            .unwrap_err();
        assert!(
            refused.to_string().starts_with("rebuild_mismatch: "),
            "{refused:#}"
        );
    }

    #[test]
    fn run_and_rm_help_say_what_absence_and_force_mean() {
        use clap::CommandFactory;

        let mut cli = Cli::command();
        let optimization = cli
            .find_subcommand_mut("optimization")
            .expect("optimization");
        let run = optimization
            .find_subcommand_mut("run")
            .expect("run")
            .render_long_help()
            .to_string();
        assert!(!run.contains(&u64::MAX.to_string()), "{run}");
        assert!(run.contains("unlimited when absent"), "{run}");
        let rm = optimization
            .find_subcommand_mut("rm")
            .expect("rm")
            .render_long_help()
            .to_string();
        assert!(
            rm.contains("a job waiting to be promoted can no longer be promoted"),
            "{rm}"
        );

        let command =
            super::testing::optimization_command(&["run", DEFINITION, "--subject", "monitor"]);
        let crate::cli::OptimizationCommand::Run(args) = command else {
            panic!("not run");
        };
        assert_eq!(args.max_tokens, None);
    }

    /// Another home sharing the documents never removes this home's job
    /// directory, even with --force: the path comes from the document.
    #[tokio::test]
    async fn rm_refuses_a_job_directory_that_is_not_under_this_home() {
        let fixture = Fixture::new().await;
        accepted_job(&fixture, "job-1").await;
        let gents::ConfigAccess::Local(node) = &fixture.ctx.access else {
            panic!("the fixture is embedded");
        };
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(elsewhere.path().join("eval").join("jobs")).unwrap();
        let other_home = crate::commands::eval::EvalContext {
            access: gents::ConfigAccess::Local(node.clone()),
            home_dir: elsewhere.path().to_path_buf(),
            owner: fixture.ctx.owner.clone(),
        };
        for argv in [&["rm", "job-1"][..], &["rm", "job-1", "--force"][..]] {
            let crate::cli::OptimizationCommand::Rm(args) =
                super::testing::optimization_command(argv)
            else {
                panic!("not rm");
            };
            let error = super::rm(&other_home, &args, &mut Vec::new())
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "job job-1: its directory is not under this home",
                "{argv:?}"
            );
            assert!(fixture.ctx.jobs_dir().join("job-1").is_dir(), "{argv:?}");
        }
    }

    #[tokio::test]
    async fn rm_refuses_a_job_id_that_is_not_one_path_component() {
        let fixture = Fixture::new().await;
        let outside = fixture.ctx.home_dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        for job_id in ["../outside", "a/b", ".."] {
            let error = optimization(&fixture, &["rm", job_id, "--force"])
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("job_id {job_id:?} must be one ordinary path component"),
            );
        }
        // Blank first, as the driver refuses a job id.
        for job_id in ["", " "] {
            let error = optimization(&fixture, &["rm", job_id, "--force"])
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("job_id {job_id:?} must not be blank"),
            );
        }
        assert!(outside.is_dir());
    }
}
