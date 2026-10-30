use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use gents::documents::template::catalog::{default_catalog, Site};
use gents::eval::checks::CheckRegistry;
use gents::eval::documents::default_breaker_threshold;
use gents::eval::optimization::target::JobTarget;
use gents::eval::optimization::{
    derive_state, job_dir, job_refused, load_job, promote_refused, removable, run_job,
    show as show_job, validate_job_id, Budgets, JobOutcome, JobRequest, JobState, PolicyV2,
    Proposal, Proposer, ScriptedProposer,
};
use gents::eval::runner::embedded::EmbeddedExecutor;
use gents::eval::runner::RunOptions;
use gents::{default_agent_id_for_node, default_inference_profile_id_for_agent};
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

use behavior_proposer::AgentProposer;

mod behavior_proposer;
mod render;
#[cfg(test)]
pub(crate) mod testing;

pub(crate) const DEFAULT_MAX_TEXT_BYTES: usize = 32 * 1024;

pub(crate) async fn dispatch(
    command: OptimizationCommand,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    let ctx = EvalContext::resolve(command.scope(), runtime).await?;
    let executor = EmbeddedExecutor::new(
        gents::DocumentRuntimeOptions::default(),
        ctx.runs_dir(),
        ctx.runtime.clone(),
    );
    let registry = CheckRegistry::builtin();
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

fn surface_refusal(error: anyhow::Error) -> anyhow::Error {
    let verbatim = job_refused(&error)
        .map(ToString::to_string)
        .or_else(|| promote_refused(&error).map(ToString::to_string));
    match verbatim {
        Some(text) => anyhow::anyhow!(text),
        None => crate::commands::eval::surface_refusal(error),
    }
}

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

async fn behavior_proposer(
    ctx: &EvalContext,
    args: &OptimizationRunArgs,
    pack: &str,
    agent: Option<&str>,
    subject_dir: &Path,
    subject_agent: &str,
) -> Result<AgentProposer<LiveTurn>> {
    let resolved = resolve_pack_source(pack, args.registry.as_deref(), &ctx.home_dir).await?;
    let slot = single_slot(resolved.manifest())?;
    let agent_id = proposer_agent_id(pack, slot, agent)?;
    let config = pack_config(&resolved, &ctx.owner)?;
    ensure_tool_less(&config, &ctx.owner, &agent_id)?;
    let gents::ConfigAccess::Graphql(graphql) = &ctx.access else {
        anyhow::bail!(
            "start `gents server` for this home and retry: an agent proposer runs on a served home"
        );
    };
    crate::request_helpers::ensure_local_request_signer(args.scope.home.as_deref(), &ctx.owner)?;
    let preamble = subject_preamble(subject_dir, subject_agent, &args.target)?;
    let profile = args.proposer_profile.clone().unwrap_or_else(|| {
        default_inference_profile_id_for_agent(&default_agent_id_for_node(&ctx.owner))
    });
    install_pack_slot(&ctx.access, &ctx.owner, &resolved, &slot.name, &profile).await?;
    Ok(AgentProposer::with_preamble(
        LiveTurn {
            graphql: graphql.clone(),
            node_did: ctx.owner.clone(),
            agent_id,
            session_id: uuid::Uuid::new_v4().to_string(),
            timeout_secs: args.proposer_timeout_secs,
            poll_secs: 1,
            quiet: args.json,
        },
        preamble,
    ))
}

fn ensure_tool_less(
    config: &gents::documents::PackConfig,
    owner: &str,
    agent_id: &str,
) -> Result<()> {
    let agent = config
        .agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .with_context(|| format!("the proposer pack has no agent {agent_id}"))?;
    let context = agent.context_id.as_deref().and_then(|id| {
        config
            .contexts
            .iter()
            .find(|context| context.context_id == id)
    });
    let tools_id = context.and_then(|context| context.tools_id.as_deref());
    let no_tools = gents::documents::Tools::default();
    let tools = match tools_id {
        Some(id) => config
            .tools
            .iter()
            .find(|tools| tools.tools_id == id)
            .with_context(|| format!("agent {agent_id} names unknown tools {id}"))?,
        None => &no_tools,
    };
    let active = config
        .agents
        .iter()
        .map(|agent| &agent.agent_id)
        .chain(config.agent_targets.iter().map(|target| &target.agent_id))
        .cloned()
        .collect();
    let mut names = gents::AgentToolSurfaceConfig::from_tools_documents(
        agent_id,
        tools,
        &config.datastore_tool_surfaces,
        &config.agent_targets,
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
        "proposer agent {agent_id} has tools {names:?}; a proposer must be tool-less"
    );
    let skills = context.map_or(&[][..], |context| &context.skill_ids[..]);
    anyhow::ensure!(
        skills.is_empty(),
        "proposer agent {agent_id} has skills {skills:?}; a proposer must be tool-less"
    );
    Ok(())
}

fn subject_preamble(subject_dir: &Path, agent_id: &str, target: &JobTarget) -> Result<String> {
    let dossier = crate::commands::eval::init::dossier::render(subject_dir, Some(agent_id))?;
    let instruction = match target {
        JobTarget::Context => "this agent's system prompt".to_owned(),
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

fn proposer_agent_id(
    pack: &str,
    slot: &gents::pack::PackInferenceSlot,
    agent: Option<&str>,
) -> Result<String> {
    match agent {
        Some(agent) => {
            anyhow::ensure!(
                slot.agents.iter().any(|known| known == agent),
                "pack {pack} has no inference-slot agent {agent:?}; it declares {:?}",
                slot.agents
            );
            Ok(agent.to_owned())
        }
        None => match slot.agents.as_slice() {
            [only] => Ok(only.clone()),
            [] => anyhow::bail!("pack {pack}: slot {} names no agent", slot.name),
            many => anyhow::bail!(
                "pack {pack} declares {} inference-slot agents; pass --proposer agent:{pack}:<agent> with one of {many:?}",
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
        "optimization run needs a proposer: pass --proposer scripted:<file> or --proposer agent:<pack>[:<agent>]",
    )?;
    let subject =
        resolve_subject_pack(&ctx.home_dir, &args.subject.pack, args.registry.as_deref()).await?;
    let baseline_pack = subject.directory().to_path_buf();
    let agent_id = match &args.subject.agent {
        Some(agent) => agent.clone(),
        None => subject.default_agent()?,
    };
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
        agent_id,
        target: args.target.clone(),
        definition_id: args.definition_id.clone(),
        inference_profile_id: args.profile.clone().unwrap_or_else(|| {
            default_inference_profile_id_for_agent(&default_agent_id_for_node(&ctx.owner))
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
        captures: Vec::new(),
        run_options: deps.options.clone(),
    };
    gents::eval::optimization::check_request(&request, &policy)?;
    let proposer: Box<dyn Proposer> = match proposer_arg {
        ProposerArg::Scripted(script) => Box::new(scripted_proposer(script, args.rounds)?),
        ProposerArg::Agent { pack, agent } => {
            let proposer = behavior_proposer(
                ctx,
                args,
                pack,
                agent.as_deref(),
                &request.baseline_pack,
                &request.agent_id,
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
        Err(error) => match load_job(&ctx.access, &ctx.owner, &job_id).await {
            Ok(Some(_)) => return Err(error.context(resume)),
            _ => return Err(error),
        },
    };
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
    let policy = load_job(&ctx.access, &ctx.owner, &args.job_id)
        .await?
        .map(|job| job.origin.policy);
    let promotion = gents::eval::optimization::promote(
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
    gents::eval::optimization::revert(
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
mod tests;
