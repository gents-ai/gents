//! `gents optimization`: thin commands over `gents::optimization`. A job is
//! driven by a scripted proposer supplied as a file, or by a behavior of a
//! pack asked once per round on the served home.
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
use gents::eval::runner::RunOptions;
use gents::optimization::target::JobTarget;
use gents::optimization::{
    derive_state, job_dir, job_refused, load_job, promote_refused, removable, run_job,
    show as show_job, validate_job_id, Budgets, JobOutcome, JobRequest, JobState, PolicyV2,
    Proposal, Proposer, ScriptedProposer,
};
use gents::template::catalog::{default_catalog, Site};
use gents::{default_behavior_id_for_agent, default_inference_profile_id_for_behavior};
use tokio_util::sync::CancellationToken;

use crate::cli::{
    OptimizationCommand, OptimizationDigestArgs, OptimizationRmArgs, OptimizationRunArgs,
    OptimizationShowArgs, PolicyArg, ProposerArg,
};
use crate::commands::eval::init::turn::LiveTurn;
use crate::commands::eval::init::{install_pack_slot, pack_config};
use crate::commands::eval::{
    cancel_on_ctrl_c, default_id, follow_progress, load_policy, source_commit, source_dirty,
    write_json, Deps, EvalContext, Progress,
};
use crate::commands::pack::{resolve_pack_source, resolve_subject_pack, single_slot};

use behavior_proposer::BehaviorProposer;

mod behavior_proposer;
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

/// The proposer behavior of a pack (resolved like `gents pack install`),
/// installed into the home with its one inference slot bound to
/// `--proposer-profile` or the default profile, asked on a fresh session of
/// the served home.
async fn behavior_proposer(
    ctx: &EvalContext,
    args: &OptimizationRunArgs,
    pack: &str,
    behavior: Option<&str>,
    subject_dir: &Path,
    subject_behavior: &str,
) -> Result<BehaviorProposer<LiveTurn>> {
    let resolved = resolve_pack_source(pack, args.registry.as_deref(), &ctx.home_dir).await?;
    let slot = single_slot(resolved.manifest())?;
    let behavior_id = proposer_behavior_id(pack, slot, behavior)?;
    let config = pack_config(&resolved, &ctx.owner)?;
    ensure_tool_less(&config, &ctx.owner, &behavior_id)?;
    let gents::ConfigAccess::Graphql(graphql) = &*ctx.access else {
        anyhow::bail!(
            "start `gents server` for this home and retry: a behavior proposer runs on a served home"
        );
    };
    crate::request_helpers::ensure_local_request_signer(args.scope.home.as_deref(), &ctx.owner)?;
    let preamble = subject_preamble(subject_dir, subject_behavior, &args.target)?;
    let profile = args.proposer_profile.clone().unwrap_or_else(|| {
        default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(&ctx.owner))
    });
    install_pack_slot(&ctx.access, &ctx.owner, &resolved, &slot.name, &profile).await?;
    Ok(BehaviorProposer::with_preamble(
        LiveTurn {
            graphql: graphql.clone(),
            agent_did: ctx.owner.clone(),
            behavior_id,
            session_id: uuid::Uuid::new_v4().to_string(),
            timeout_secs: args.proposer_timeout_secs,
            poll_secs: 1,
            quiet: args.json,
        },
        preamble,
    ))
}

/// A proposer is tool-less, so a proposal draws only on the turns it is
/// sent: the behavior's tool surface, resolved from the pack's documents by
/// the runtime's owner, names no tool, and its context names no skill (the
/// runtime adds `load_skill` for those). The readonly ceiling narrows a host
/// tool without removing it and needs no root; the CLI tools it filters out
/// are counted anyway, since a server started with `--cli-tool` adds them
/// back; and a subagent target counts whichever home behavior it names.
fn ensure_tool_less(
    config: &gents::document_config::PackConfig,
    owner: &str,
    behavior_id: &str,
) -> Result<()> {
    let behavior = config
        .agent_behaviors
        .iter()
        .find(|behavior| behavior.behavior_id == behavior_id)
        .with_context(|| format!("the proposer pack has no behavior {behavior_id}"))?;
    let context = behavior.context_id.as_deref().and_then(|id| {
        config
            .contexts
            .iter()
            .find(|context| context.context_id == id)
    });
    let tools_id = context.and_then(|context| context.tools_id.as_deref());
    let no_tools = gents::document_config::Tools::default();
    let tools = match tools_id {
        Some(id) => config
            .tools
            .iter()
            .find(|tools| tools.tools_id == id)
            .with_context(|| format!("behavior {behavior_id} names unknown tools {id}"))?,
        None => &no_tools,
    };
    let active = config
        .agent_behaviors
        .iter()
        .map(|behavior| &behavior.behavior_id)
        .chain(
            config
                .subagent_targets
                .iter()
                .map(|target| &target.behavior_id),
        )
        .cloned()
        .collect();
    let mut names = gents::BehaviorToolConfig::from_tools_documents(
        behavior_id,
        tools,
        &config.datastore_tool_surfaces,
        &config.eth_tools,
        &config.subagent_targets,
        &gents::ToolCeiling::readonly(),
        Vec::new(),
    )?
    .explain_with_runtime_availability(
        gents::tool_surface::RuntimeToolAvailability::all(),
        owner,
        &active,
    )
    .tool_names;
    names.extend(
        tools
            .host
            .iter()
            .flat_map(|host| &host.cli)
            .map(|cli| cli.name.clone()),
    );
    anyhow::ensure!(
        names.is_empty(),
        "proposer behavior {behavior_id} has tools {names:?}; a proposer must be tool-less"
    );
    let skills = context.map_or(&[][..], |context| &context.skill_ids[..]);
    anyhow::ensure!(
        skills.is_empty(),
        "proposer behavior {behavior_id} has skills {skills:?}; a proposer must be tool-less"
    );
    Ok(())
}

/// The session's first user turn: the subject's dossier, so a proposal
/// names the subject's real tools and surfaces rather than guessing them.
fn subject_preamble(subject_dir: &Path, behavior_id: &str, target: &JobTarget) -> Result<String> {
    let dossier = crate::commands::eval::init::dossier::render(subject_dir, Some(behavior_id))?;
    let instruction = match target {
        JobTarget::Context => "this behavior's system prompt".to_owned(),
        JobTarget::Task(task_id) => format!(
            "the prompt template of its task {task_id:?}, rendered when the task fires; \
             every {{{{ variable }}}} of the current template must stay in the new one, \
             and use no {{{{ variable }}}} the current template does not already use \
             apart from the runtime's own: {}",
            default_catalog().variables_at(Site::Task).join(", ")
        ),
    };
    Ok(format!(
        "{}\n\nThe instruction you will rewrite is {instruction}. \
         Every later turn carries the current instruction and the training feedback.",
        dossier.text
    ))
}

/// The behavior `--proposer behavior:<pack>[:<behavior>]` asks: the named
/// one when the slot declares it, else the slot's only behavior.
fn proposer_behavior_id(
    pack: &str,
    slot: &gents::pack::PackInferenceSlot,
    behavior: Option<&str>,
) -> Result<String> {
    match behavior {
        Some(behavior) => {
            anyhow::ensure!(
                slot.behaviors.iter().any(|known| known == behavior),
                "pack {pack} has no inference-slot behavior {behavior:?}; it declares {:?}",
                slot.behaviors
            );
            Ok(behavior.to_owned())
        }
        None => match slot.behaviors.as_slice() {
            [only] => Ok(only.clone()),
            [] => anyhow::bail!("pack {pack}: slot {} names no behavior", slot.name),
            many => anyhow::bail!(
                "pack {pack} declares {} inference-slot behaviors; pass --proposer behavior:{pack}:<behavior> with one of {many:?}",
                many.len()
            ),
        },
    }
}

async fn run(
    ctx: &EvalContext,
    args: &OptimizationRunArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let proposer_arg = args.proposer.as_ref().context(
        "optimization run needs a proposer: pass --proposer scripted:<file> or --proposer behavior:<pack>[:<behavior>]",
    )?;
    let subject =
        resolve_subject_pack(&ctx.home_dir, &args.subject.pack, args.registry.as_deref()).await?;
    let baseline_pack = subject.directory().to_path_buf();
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
        target: args.target.clone(),
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
    // A behavior proposer installs its pack, so a job refused on its request
    // or policy is refused first and writes nothing.
    gents::optimization::check_request(&request, &policy)?;
    let proposer: Box<dyn Proposer> = match proposer_arg {
        ProposerArg::Scripted(script) => Box::new(scripted_proposer(script, args.rounds)?),
        ProposerArg::Behavior { pack, behavior } => {
            let proposer = behavior_proposer(
                ctx,
                args,
                pack,
                behavior.as_deref(),
                &request.baseline_pack,
                &request.behavior_id,
            )
            .await?;
            Box::new(proposer)
        }
    };
    let resume = format!(
        "job {job_id} stopped before it finished; run the same command with --job-id {job_id} to resume it"
    );
    let running = run_job(
        &ctx.access,
        &request,
        deps.executor,
        proposer.as_ref(),
        deps.registry,
        &policy,
        deps.cancel.clone(),
    );
    let outcome = match follow(ctx, &job_id, !args.json, out, running).await {
        Ok(outcome) => outcome,
        // Only a job that was created can be resumed.
        Err(error) => match load_job(&ctx.access, &ctx.owner, &job_id).await {
            Ok(Some(_)) => return Err(error.context(resume)),
            _ => return Err(error),
        },
    };
    // A job left running is not a success: the view still renders, then the
    // command fails with the resume note, so a script never reads it as done.
    let stopped = (outcome.state == JobState::Running).then_some(resume);
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
    validate_job_id(&args.job_id)?;
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use gents::eval::checks::CheckRegistry;
    use tokio_util::sync::CancellationToken;

    use super::testing::{
        accepted_job, delete_definition, optimization, optimization_command, optimization_with,
        proposer_file,
    };
    use super::{
        ensure_tool_less, execute, pack_config, proposer_behavior_id, subject_preamble, JobTarget,
    };
    use crate::cli::Cli;
    use crate::commands::eval::testing::{deps, eval, executor, Fixture, DEFINITION};
    use crate::commands::eval::UNCALIBRATED_BANNER;

    #[tokio::test]
    async fn run_refuses_without_a_proposer_and_an_unknown_proposer_is_a_usage_error() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let error = optimization(&fixture, &["run", DEFINITION, "--subject", pack.as_str()])
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "optimization run needs a proposer: pass --proposer scripted:<file> or --proposer behavior:<pack>[:<behavior>]"
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
                "pass --proposer scripted:<file> or --proposer behavior:<pack>[:<behavior>]"
            ),
            "{usage}"
        );
        assert_eq!(usage.exit_code(), 2);
    }

    #[test]
    fn a_slot_with_several_behaviors_needs_the_behavior_named() {
        let slot = gents::pack::PackInferenceSlot {
            name: "proposer".to_owned(),
            description: String::new(),
            behaviors: vec!["terse".to_owned(), "verbose".to_owned()],
            optional: false,
        };
        let error = proposer_behavior_id("p", &slot, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "pack p declares 2 inference-slot behaviors; pass --proposer behavior:p:<behavior> with one of [\"terse\", \"verbose\"]"
        );
        assert_eq!(
            proposer_behavior_id("p", &slot, Some("verbose")).unwrap(),
            "verbose"
        );
        let unknown = proposer_behavior_id("p", &slot, Some("other")).unwrap_err();
        assert!(
            unknown.to_string().contains("no inference-slot behavior"),
            "{unknown}"
        );
        let one = gents::pack::PackInferenceSlot {
            behaviors: vec!["terse".to_owned()],
            optional: false,
            ..slot
        };
        assert_eq!(proposer_behavior_id("p", &one, None).unwrap(), "terse");
    }

    #[test]
    fn the_subject_preamble_is_the_dossier_naming_the_subject_tools() {
        let pipeline = crate::commands::pack::test_support::fixture_dir("documents_fixture");
        let preamble = subject_preamble(&pipeline, "fixture-worker", &JobTarget::Context).unwrap();
        assert!(
            preamble.starts_with("# Subject\n\n## Identity"),
            "{preamble}"
        );
        assert_eq!(preamble.matches("# Subject").count(), 1, "{preamble}");
        assert!(preamble.contains("type FixtureJob"), "{preamble}");
        assert!(
            preamble.ends_with(
                "The instruction you will rewrite is this behavior's system prompt. \
                 Every later turn carries the current instruction and the training feedback."
            ),
            "{preamble}"
        );
        let task =
            subject_preamble(&pipeline, "fixture-worker", &JobTarget::Task("plan".into())).unwrap();
        assert!(
            task.contains("the prompt template of its task \"plan\""),
            "{task}"
        );
        assert!(task.contains("every {{ variable }}"), "{task}");
        assert!(
            task.contains("use no {{ variable }} the current template does not already use"),
            "{task}"
        );
        assert!(
            task.contains("node.node_did, node.behavior_id, ctx.now"),
            "{task}"
        );
    }

    #[tokio::test]
    async fn a_proposer_behavior_with_tools_is_refused_before_anything_runs() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let proposer = format!(
            "behavior:{}",
            crate::commands::pack::test_support::fixture_dir("documents_fixture").display()
        );
        let error = optimization(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--subject",
                pack.as_str(),
                "--proposer",
                proposer.as_str(),
            ],
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.starts_with("proposer behavior fixture-worker has tools [")
                && message.ends_with("]; a proposer must be tool-less"),
            "{error:#}"
        );
        assert!(message.contains("\"read_file\""), "{message}");
    }

    /// What the resolved surface alone would miss: CLI tools a server started
    /// with `--cli-tool` adds, a subagent target outside the pack, and skills.
    #[test]
    fn cli_tools_a_target_outside_the_pack_or_skills_are_refused() {
        use serde_json::json;
        const OWNER: &str = "did:key:z6MkproposerOwner";
        let home = tempfile::tempdir().unwrap();
        let pack =
            crate::commands::pack::test_support::fixture_pack_source("slot_fixture", home.path());
        let base = pack_config(&pack, OWNER).unwrap();
        ensure_tool_less(&base, OWNER, "fixture-author").unwrap();

        let mut cli = base.clone();
        cli.tools[0]
            .host
            .as_mut()
            .unwrap()
            .cli
            .push(serde_json::from_value(json!({"name": "rg"})).unwrap());
        let mut subagent = base.clone();
        subagent.tools[0].subagents = Some(
            serde_json::from_value(json!({"enabled": true, "target_ids": ["helper"]})).unwrap(),
        );
        subagent.subagent_targets.push(
            serde_json::from_value(json!({
                "target_id": "helper",
                "agent_did": OWNER,
                "target_agent_did": OWNER,
                "behavior_id": "home-default",
                "name": "helper",
            }))
            .unwrap(),
        );
        let mut skills = base.clone();
        skills.contexts[0].skill_ids = vec!["review".to_owned()];
        for (config, offending) in [
            (cli, "\"rg\""),
            (subagent, "\"agent_new\""),
            (skills, "has skills [\"review\"]"),
        ] {
            let error = ensure_tool_less(&config, OWNER, "fixture-author")
                .unwrap_err()
                .to_string();
            assert!(
                error.starts_with("proposer behavior fixture-author has ")
                    && error.contains(offending)
                    && error.ends_with("; a proposer must be tool-less"),
                "{error}"
            );
        }
    }

    /// A tool-less proposer pack passes the tool-less check and reaches the next one.
    #[tokio::test]
    async fn a_behavior_proposer_needs_a_served_home() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let proposer = format!(
            "behavior:{}",
            crate::commands::pack::test_support::fixture_dir("slot_fixture").display()
        );
        let error = optimization(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--subject",
                pack.as_str(),
                "--proposer",
                proposer.as_str(),
            ],
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("start `gents server`"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_proposer_pack_missing_offline_fails_in_one_sentence() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let error = optimization(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--subject",
                pack.as_str(),
                "--proposer",
                "behavior:absent_proposer",
                "--registry",
                "http://127.0.0.1:9",
            ],
        )
        .await
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.starts_with("gents/absent_proposer is not in the pack store of ")
                && message.contains("could not be reached")
                && !message.contains('\n'),
            "{message}"
        );
    }

    /// The pack install writes documents, so a job the request or policy
    /// refuses never reaches the proposer (here, the served-home check
    /// before the install).
    #[tokio::test]
    async fn a_refused_job_is_refused_before_the_proposer_is_built() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let proposer = format!(
            "behavior:{}",
            crate::commands::pack::test_support::fixture_dir("slot_fixture").display()
        );
        std::fs::create_dir_all(&fixture.ctx.home_dir).unwrap();
        let policy = fixture.ctx.home_dir.join("policy.json");
        std::fs::write(
            &policy,
            serde_json::to_vec(&gents::optimization::PolicyV2::uncalibrated()).unwrap(),
        )
        .unwrap();
        let policy = policy.display().to_string();
        for (flag, value, refusal) in [
            (
                "--job-id",
                "a/b",
                r#"job_id "a/b" must be one ordinary path component"#,
            ),
            (
                "--policy",
                policy.as_str(),
                "policy max_rounds 3 does not match the budget's max_rounds 2; the Bonferroni divisor must be the number of candidates the job may try",
            ),
        ] {
            let error = optimization(
                &fixture,
                &[
                    "run",
                    DEFINITION,
                    "--subject",
                    pack.as_str(),
                    "--proposer",
                    proposer.as_str(),
                    "--rounds",
                    "2",
                    flag,
                    value,
                ],
            )
            .await
            .unwrap_err();
            assert_eq!(format!("{error:#}"), refusal);
        }
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
    async fn a_failure_before_the_freeze_does_not_say_resume() {
        let fixture = Fixture::new().await;
        let subject = format!("{}:no-such-behavior", fixture.pack_arg());
        let proposer = proposer_file(&fixture);
        let error = optimization(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--subject",
                subject.as_str(),
                "--profile",
                "local",
                "--proposer",
                proposer.as_str(),
                "--job-id",
                "pre-freeze",
            ],
        )
        .await
        .unwrap_err();
        assert!(!format!("{error:#}").contains("resume"), "{error:#}");
        assert!(gents::optimization::load_job(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "pre-freeze"
        )
        .await
        .unwrap()
        .is_none());
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
        let gents::ConfigAccess::Local(node) = &*fixture.ctx.access else {
            panic!("the fixture is embedded");
        };
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(elsewhere.path().join("eval").join("jobs")).unwrap();
        let other_home = crate::commands::eval::EvalContext {
            access: gents::ConfigAccess::Local(node.clone()).into(),
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
