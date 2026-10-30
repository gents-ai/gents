use clap::Subcommand;
use gents::eval::optimization::target::JobTarget;

use super::{
    parse_policy, parse_proposer, parse_subject, parse_target, EvalScopeArgs, PolicyArg,
    ProposerArg, SubjectArg,
};

/// `gents optimization run`'s exit statuses: scripts must not read a job left
/// running as a success.
const OPTIMIZATION_RUN_AFTER_HELP: &str = "Exit status: 0 when the job settled; 1 when it stopped still running (Ctrl-C; the same command with --job-id resumes it) or the command was refused; 2 on a usage error.";

#[derive(Subcommand)]
pub(crate) enum OptimizationCommand {
    #[command(
        about = "Run an optimization job over a subject pack; Ctrl-C stops it for a resume",
        long_about = "Run an optimization job over a subject pack; Ctrl-C stops it for a resume.\n\n\
                      Each stage's captures come from the eval definition (its stage's \
                      `capture` list); this command adds none.",
        after_help = OPTIMIZATION_RUN_AFTER_HELP
    )]
    Run(Box<OptimizationRunArgs>),
    #[command(about = "Show a job's journal and every decision recomputed from its runs")]
    Show(OptimizationShowArgs),
    #[command(about = "Promote a ready job's checkpoint into the live configuration")]
    Promote(OptimizationDigestArgs),
    #[command(about = "Restore the text a promotion replaced; Reverted is final")]
    Revert(OptimizationDigestArgs),
    #[command(
        about = "Delete a settled job's directory (candidate packs, staging); its documents stay"
    )]
    Rm(OptimizationRmArgs),
}

impl OptimizationCommand {
    pub(crate) fn scope(&self) -> &EvalScopeArgs {
        match self {
            Self::Run(args) => &args.scope,
            Self::Show(args) => &args.scope,
            Self::Promote(args) | Self::Revert(args) => &args.scope,
            Self::Rm(args) => &args.scope,
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct OptimizationRunArgs {
    pub(crate) definition_id: String,
    /// `<pack>[:<agent>]`: a pack name, resolved as `gents pack install`
    /// resolves one, or a pack directory written as a path. Without a
    /// agent, the pack's only inference-slot agent.
    #[arg(long, value_parser = parse_subject)]
    pub(crate) subject: SubjectArg,
    /// `context`: the agent's system prompt; `task:<task_id>`: the prompt
    /// template of that task of the agent.
    #[arg(long, value_parser = parse_target, default_value = "context")]
    pub(crate) target: JobTarget,
    #[arg(long, default_value_t = 3)]
    pub(crate) rounds: u32,
    #[arg(long, default_value_t = 2)]
    pub(crate) trials: u32,
    #[arg(long, default_value_t = 1000)]
    pub(crate) seed_base: i64,
    /// `defaults` (with max_rounds set to --rounds) when absent.
    #[arg(long, value_parser = parse_policy)]
    pub(crate) policy: Option<PolicyArg>,
    /// Defaults to `<definition_id>-<unix ms>-<4 random hex>`, fresh every
    /// time; name the job with `--job-id` and repeat the same flags to
    /// resume it.
    #[arg(long)]
    pub(crate) job_id: Option<String>,
    /// `scripted:<file>`: a JSON array of `{"text", "rationale"}`, one per
    /// round; a file holding fewer than `--rounds` is refused before the job
    /// is frozen. `agent:<pack>[:<agent>]`: an agent of a pack
    /// (`<pack>` resolves like `gents pack install`, e.g. `prompt_proposer`
    /// or `gents/prompt_proposer@1.0.0`, or is a directory path), installed
    /// into the home and asked once per round on the served home.
    #[arg(long, value_parser = parse_proposer)]
    pub(crate) proposer: Option<ProposerArg>,
    /// The inference profile the proposer agent runs on; the home's
    /// default when absent.
    #[arg(long)]
    pub(crate) proposer_profile: Option<String>,
    /// Seconds without progress before a proposer reply times out.
    #[arg(long, default_value_t = 600)]
    pub(crate) proposer_timeout_secs: u64,
    /// The inference profile both arms run on; the home's default when absent.
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long, default_value_t = 10_000)]
    pub(crate) max_case_trials: u64,
    /// The job's token budget; unlimited when absent.
    #[arg(long)]
    pub(crate) max_tokens: Option<u64>,
    /// The structural gate's cap on a proposed text, in bytes.
    #[arg(long, default_value_t = crate::commands::optimization::DEFAULT_MAX_TEXT_BYTES)]
    pub(crate) max_text_bytes: usize,
    /// The pack registry to fall back to for a pack the home's store does not
    /// hold.
    #[arg(long)]
    pub(crate) registry: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct OptimizationShowArgs {
    pub(crate) job_id: String,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct OptimizationDigestArgs {
    pub(crate) job_id: String,
    /// promote: the checkpoint's pack digest (`optimization show`);
    /// revert: the target digest the promotion printed.
    #[arg(long)]
    pub(crate) digest: String,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct OptimizationRmArgs {
    pub(crate) job_id: String,
    /// Delete even while the job is running or waiting to be promoted; a
    /// job waiting to be promoted can no longer be promoted afterwards, since
    /// its retained checkpoint pack is gone. A job whose directory is not
    /// under this home is never removed.
    #[arg(long)]
    pub(crate) force: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
