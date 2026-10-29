use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use gents::optimization::target::JobTarget;

#[derive(clap::Args, Clone, Debug, Default)]
pub(crate) struct EvalScopeArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum EvalCommand {
    #[command(
        about = "Freeze and run an eval over one or more cells; Ctrl-C cancels",
        long_about = "Freeze and run an eval over one or more cells; Ctrl-C cancels.\n\n\
                      Each stage's captures come from the eval definition (its stage's \
                      `capture` list); this command adds none.",
        after_help = EVAL_RUN_AFTER_HELP
    )]
    Run(EvalRunArgs),
    #[command(about = "Run a saved plan under one shared trial concurrency ceiling")]
    Batch(EvalBatchArgs),
    #[command(
        about = "Continue a run from what it already wrote",
        after_help = EVAL_RUN_AFTER_HELP
    )]
    Resume(EvalResumeArgs),
    #[command(about = "List eval runs; invalidated runs only with --all")]
    List(EvalListArgs),
    #[command(about = "Show a run's report: slot counts, case means, headline, usage, exposure")]
    Show(EvalShowArgs),
    #[command(about = "Show one slot's latest attempt with its stages and verdicts")]
    Trial(EvalTrialArgs),
    #[command(about = "Ask the process hosting a run to stop at its next check")]
    Cancel(EvalRunIdArgs),
    #[command(about = "Invalidate a run as this home; its documents stay")]
    Invalidate(EvalInvalidateArgs),
    #[command(about = "Delete a run's directory (homes, packs, sidecars); its documents stay")]
    Rm(EvalRmArgs),
    #[command(
        about = "Paired statistics between two cells; a policy verdict only with --policy; --by check|stage breaks it down, --case shows one case side by side"
    )]
    Compare(EvalCompareArgs),
    #[command(
        about = "Re-render a run's report and its in-flight slots until no process runs it",
        after_help = EVAL_WATCH_AFTER_HELP
    )]
    Watch(EvalWatchArgs),
    #[command(
        about = "Delete the directories of old, finished runs no optimization job references; their documents stay"
    )]
    Gc(EvalGcArgs),
    #[command(
        about = "List the checks a definition may name, with their params schema and reason codes"
    )]
    Checks(EvalChecksArgs),
    #[command(
        about = "Interview a model about one agent of a pack and write the eval definition pack it drafts",
        after_help = EVAL_INIT_AFTER_HELP
    )]
    Init(EvalInitArgs),
}

impl EvalCommand {
    pub(crate) fn scope(&self) -> &EvalScopeArgs {
        match self {
            Self::Run(args) => &args.scope,
            Self::Batch(args) => &args.scope,
            Self::Resume(args) => &args.scope,
            Self::List(args) => &args.scope,
            Self::Show(args) => &args.scope,
            Self::Trial(args) => &args.scope,
            Self::Cancel(args) => &args.scope,
            Self::Invalidate(args) => &args.scope,
            Self::Rm(args) => &args.scope,
            Self::Compare(args) => &args.scope,
            Self::Watch(args) => &args.scope,
            Self::Gc(args) => &args.scope,
            Self::Init(args) => &args.scope,
            Self::Checks(_) => &NO_SCOPE,
        }
    }
}

static NO_SCOPE: EvalScopeArgs = EvalScopeArgs {
    home: None,
    graphql: None,
};

#[derive(clap::Args)]
pub(crate) struct EvalListArgs {
    /// Only runs of this eval definition.
    #[arg(long)]
    pub(crate) definition: Option<String>,
    /// Include invalidated runs.
    #[arg(long)]
    pub(crate) all: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalShowArgs {
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalTrialArgs {
    pub(crate) run_id: String,
    pub(crate) cell: String,
    pub(crate) case_id: String,
    /// Defaults to 0.
    pub(crate) trial_index: Option<u32>,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(
        long,
        help = "Read original request content and lineage from the finished trial's retained home (maximum 1000 requests)"
    )]
    pub(crate) requests: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalRunIdArgs {
    pub(crate) run_id: String,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalInvalidateArgs {
    pub(crate) run_id: String,
    /// Operator-authored; replicates with the run.
    #[arg(long)]
    pub(crate) reason: String,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalRmArgs {
    pub(crate) run_id: String,
    /// Delete even when the run still owes slots, a live process runs it, or
    /// a job that is not settled still needs it.
    #[arg(long)]
    pub(crate) force: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

pub(crate) fn parse_interval(raw: &str) -> Result<std::time::Duration, String> {
    let interval = match raw.trim().strip_suffix("ms") {
        Some(millis) => millis
            .parse::<u64>()
            .map(std::time::Duration::from_millis)
            .map_err(|_| format!("invalid duration number in {raw}"))?,
        None => {
            crate::request_helpers::parse_duration_suffix(raw).map_err(|error| error.to_string())?
        }
    };
    if interval < std::time::Duration::from_millis(250) {
        return Err(format!("--interval {raw:?} is under 250ms"));
    }
    if interval > std::time::Duration::from_secs(3600) {
        return Err(format!("--interval {raw:?} is over 1h"));
    }
    Ok(interval)
}

const EVAL_RUN_AFTER_HELP: &str = "Exit status: 0 when the run finished; 1 when it stopped with slots still owed (Ctrl-C, a cancel marker, or abandoned slots; `gents eval resume` continues it) or the command was refused; 2 on a usage error.";

const EVAL_WATCH_AFTER_HELP: &str = "Exit status: 0 when the run ends finished, or after the one render of --once; 1 when it ends stopped (slots still owed, as after a cancel, or the report unavailable) or the watch fails; 2 on a usage error.";

#[derive(clap::Args)]
pub(crate) struct EvalWatchArgs {
    pub(crate) run_id: String,
    /// How long to wait between renders; at least 250ms, at most 1h.
    #[arg(long, value_parser = parse_interval, default_value = "2s")]
    pub(crate) interval: std::time::Duration,
    /// Render once and return.
    #[arg(long)]
    pub(crate) once: bool,
    /// One JSON object per render: the progress file, the run's view and
    /// where it was read from.
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

pub(crate) fn parse_age(raw: &str) -> Result<std::time::Duration, String> {
    let age =
        crate::request_helpers::parse_duration_suffix(raw).map_err(|error| error.to_string())?;
    chrono::Duration::from_std(age)
        .ok()
        .and_then(|age| chrono::DateTime::UNIX_EPOCH.checked_sub_signed(age))
        .ok_or_else(|| {
            format!("--older-than {raw:?} reaches before the earliest representable time")
        })?;
    Ok(age)
}

#[derive(clap::Args)]
pub(crate) struct EvalGcArgs {
    /// Only runs created at least this long ago (`30m`, `14d`; `0s` for any).
    #[arg(long, value_parser = parse_age, default_value = "14d")]
    pub(crate) older_than: std::time::Duration,
    /// Only runs of this eval definition.
    #[arg(long)]
    pub(crate) definition: Option<String>,
    /// List what would be removed; remove nothing.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Also remove the directories of this home's settled optimization jobs
    /// (nothing to promote, exhausted, failed, stale, promoted or reverted)
    /// last modified before the threshold; with --definition, only that
    /// definition's.
    #[arg(long)]
    pub(crate) jobs: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalChecksArgs {
    #[arg(long)]
    pub(crate) json: bool,
    /// Instead of the catalog, check one eval case (a JSON file, or `-` for
    /// stdin) against it: prints `{"violations": [...]}` and exits non-zero
    /// when any check is unknown or its params are refused.
    #[arg(long, value_name = "FILE|-", conflicts_with = "json")]
    pub(crate) validate_case: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyArg {
    Defaults,
    File(PathBuf),
}

pub(crate) fn parse_policy(raw: &str) -> Result<PolicyArg, String> {
    match raw.trim() {
        "" => Err("--policy takes `defaults` or a path to a policy JSON file".to_owned()),
        "defaults" => Ok(PolicyArg::Defaults),
        path => Ok(PolicyArg::File(PathBuf::from(path))),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum BreakdownArg {
    Check,
    Stage,
}

#[derive(clap::Args)]
pub(crate) struct EvalCompareArgs {
    pub(crate) baseline_run: String,
    pub(crate) candidate_run: String,
    /// Required when the baseline run has more than one cell.
    #[arg(long)]
    pub(crate) baseline_cell: Option<String>,
    /// Required when the candidate run has more than one cell.
    #[arg(long)]
    pub(crate) candidate_cell: Option<String>,
    #[arg(long, value_parser = parse_policy)]
    pub(crate) policy: Option<PolicyArg>,
    /// Aggregate the paired difference by check name or stage id.
    #[arg(long, value_enum, conflicts_with = "case_id")]
    pub(crate) by: Option<BreakdownArg>,
    /// Print this case's trials side by side with each verdict.
    #[arg(long = "case")]
    pub(crate) case_id: Option<String>,
    /// Compare a pilot run (purpose `pilot`) as if it were evidence.
    #[arg(long)]
    pub(crate) include_pilot: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CellArg {
    pub(crate) cell_id: String,
    pub(crate) pack: String,
    pub(crate) agent: Option<String>,
}

/// Split at the first `:` after the `=`: agent ids may hold colons
/// (`did:key:…:default`), pack names and ordinary paths do not.
pub(crate) fn parse_cell(raw: &str) -> Result<CellArg, String> {
    let (cell_id, rest) = raw
        .split_once('=')
        .ok_or_else(|| format!("--cell {raw:?} must be <id>=<pack>[:<agent>]"))?;
    let (pack, agent) = match rest.split_once(':') {
        Some((pack, agent)) => (pack, Some(agent)),
        None => (rest, None),
    };
    if cell_id.trim().is_empty()
        || pack.trim().is_empty()
        || agent.is_some_and(|agent| agent.trim().is_empty())
    {
        return Err(format!("--cell {raw:?} has an empty id, pack or agent"));
    }
    Ok(CellArg {
        cell_id: cell_id.trim().to_owned(),
        pack: pack.trim().to_owned(),
        agent: agent.map(|agent| agent.trim().to_owned()),
    })
}

pub(crate) fn parse_assignment(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() && !value.trim().is_empty() => {
            Ok((key.trim().to_owned(), value.trim().to_owned()))
        }
        _ => Err(format!("{raw:?} must be <cell>=<profile_id>")),
    }
}

pub(crate) fn parse_run_purpose(raw: &str) -> Result<String, String> {
    let purpose = raw.trim();
    if purpose == gents::eval::scoring::PILOT_PURPOSE {
        return Err(format!(
            "{purpose:?} is recorded only by `gents eval init --pilot`; a pilot run is left out of exposure"
        ));
    }
    Ok(raw.to_owned())
}

pub(crate) fn parse_split(raw: &str) -> Result<gents::document_config::EvalSplit, String> {
    serde_json::from_value(serde_json::Value::String(raw.trim().to_owned()))
        .map_err(|_| format!("unknown split {raw:?}; expected train, validation or held_out"))
}

/// The shared ceiling counts complete trial attempts, not individual provider requests.
#[derive(clap::Args)]
pub(crate) struct EvalBatchArgs {
    /// JSON object with `runs: [{args: ["definition-id", "--cell", ...]}]`.
    /// Each entry takes `eval run` arguments and a unique explicit `--run-id`;
    /// use the batch's scope and JSON flags, not per-entry overrides.
    #[arg(long)]
    pub(crate) plan: PathBuf,
    /// Maximum active trials across all entries; each run's own cap still applies.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) concurrency: u32,
    /// Print every run's report and execution error as one JSON result.
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalRunArgs {
    pub(crate) definition_id: String,
    /// `<id>=<pack>[:<agent>]`, once per cell. `<pack>` is a pack name,
    /// resolved as `gents pack install` resolves one, or a pack directory
    /// written as a path (`./subject`, `/work/subject`).
    #[arg(long = "cell", value_parser = parse_cell, required = true)]
    pub(crate) cells: Vec<CellArg>,
    /// `<cell>=<inference_profile_id>`; a cell without one uses the home's
    /// default profile.
    #[arg(long = "profile", value_parser = parse_assignment)]
    pub(crate) profiles: Vec<(String, String)>,
    #[arg(long, value_parser = parse_split, default_value = "validation")]
    pub(crate) split: gents::document_config::EvalSplit,
    #[arg(long, default_value_t = 2)]
    pub(crate) trials: u32,
    #[arg(long, default_value_t = 1000)]
    pub(crate) seed_base: i64,
    #[arg(long, default_value_t = 1)]
    pub(crate) concurrency: u32,
    /// Why the run exists: `eval` by default, or `optimization:<job_id>`.
    /// `pilot` is refused; only `gents eval init --pilot` records a pilot.
    #[arg(long, default_value = "eval", value_parser = parse_run_purpose)]
    pub(crate) purpose: String,
    /// Defaults to `<definition_id>-<unix ms>-<4 random hex>`, fresh every
    /// time; name a run with `--run-id` to reuse it (the idempotent freeze).
    #[arg(long)]
    pub(crate) run_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub(crate) max_infra_retries: u32,
    /// The pack registry to fall back to for a pack the home's store does not
    /// hold; a download is stored in the home for the next run.
    #[arg(long)]
    pub(crate) registry: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

const EVAL_INIT_AFTER_HELP: &str = "Needs a terminal (this command is an interview) and a served home: start `gents server` first, or the command refuses before reading anything. An existing --out refuses unless --force replaces it, and --force replaces only a definition pack gents eval init wrote; an --out that is, lies inside, or contains the subject's directory, a Gents home, the user home or the working directory always refuses. --validation-min (default 6) is the floor the author drafts the validation split against; lower it when the operator wants fewer validation cases. --pilot runs the written pack once against the subject, one trial per case and one run per populated split (train, validation, held-out), and asks to spend that before it does, unless --yes; a decline leaves the pack written at --out but the command still exits 1. The session id printed at the end continues with `gents chat --session-id <id> --agent-id <the author's agent>`. A documents capture filter's only variable is \"$trial\", replaced with the trial's DID wherever it appears in a string value. Exit status: 0 when the pack was written and validated (piloted too, with --pilot) or the operator ended the interview with nothing written; 1 when refused (an existing --out without --force, an --out overlapping the subject, a non-terminal stdin, an unserved home, a declined pilot, or another failure) or when three drafts did not validate; 2 on a usage error.";

#[derive(clap::Args)]
pub(crate) struct EvalInitArgs {
    /// A pack name, resolved as `gents pack install` resolves one, or a pack
    /// directory written as a path (`./subject`, `/work/subject`).
    pub(crate) subject: String,
    /// The agent to draft cases for; implied when the pack has one.
    #[arg(long)]
    pub(crate) agent: Option<String>,
    /// The pack that authors the draft, resolved like `gents pack install`
    /// (or a directory path); it declares one inference slot with one
    /// agent.
    #[arg(long, default_value = "gents/eval_author")]
    pub(crate) author: String,
    /// Where the definition pack is written; refused when it exists, unless
    /// --force.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// The definition's id; the author's draft names one when absent.
    #[arg(long)]
    pub(crate) definition_id: Option<String>,
    /// The inference profile the author (and the pilot) runs on; the home's
    /// default when absent.
    #[arg(long)]
    pub(crate) profile: Option<String>,
    /// Run the written draft once, one trial per case on every split, and let
    /// the author revise it from the evidence.
    #[arg(long)]
    pub(crate) pilot: bool,
    /// Pilot without asking first.
    #[arg(long)]
    pub(crate) yes: bool,
    /// Replace an existing --out that gents eval init wrote.
    #[arg(long)]
    pub(crate) force: bool,
    /// The fewest validation cases a draft may have.
    #[arg(long, default_value_t = 6)]
    pub(crate) validation_min: usize,
    /// How long one author turn may go without progress.
    #[arg(long, default_value_t = 86_400)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
    /// The pack registry to fall back to for a subject or author the home's
    /// store does not hold.
    #[arg(long)]
    pub(crate) registry: Option<String>,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalResumeArgs {
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubjectArg {
    pub(crate) pack: String,
    pub(crate) agent: Option<String>,
}

pub(crate) fn parse_subject(raw: &str) -> Result<SubjectArg, String> {
    let (pack, agent) = match raw.split_once(':') {
        Some((pack, agent)) => (pack, Some(agent)),
        None => (raw, None),
    };
    if pack.trim().is_empty() || agent.is_some_and(|agent| agent.trim().is_empty()) {
        return Err(format!("--subject {raw:?} must be <pack>[:<agent>]"));
    }
    Ok(SubjectArg {
        pack: pack.trim().to_owned(),
        agent: agent.map(|agent| agent.trim().to_owned()),
    })
}

pub(crate) fn parse_target(raw: &str) -> Result<JobTarget, String> {
    match raw.trim() {
        "context" => Ok(JobTarget::Context),
        other => match other.strip_prefix("task:").map(str::trim) {
            Some(task_id) if !task_id.is_empty() => Ok(JobTarget::Task(task_id.to_owned())),
            _ => Err(format!(
                "unknown target {raw:?}; pass --target context or --target task:<task_id>"
            )),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProposerArg {
    /// A script of proposals, one per round.
    Scripted(PathBuf),
    /// An agent of a pack asked once per round; without a
    /// agent, the pack's only inference-slot agent.
    Agent { pack: String, agent: Option<String> },
}

pub(crate) fn parse_proposer(raw: &str) -> Result<ProposerArg, String> {
    let usage = || {
        format!(
            "unknown proposer {raw:?}; pass --proposer scripted:<file> or --proposer agent:<pack>[:<agent>]"
        )
    };
    if let Some(path) = raw.strip_prefix("scripted:") {
        return match path.trim() {
            "" => Err(usage()),
            path => Ok(ProposerArg::Scripted(PathBuf::from(path))),
        };
    }
    let Some(target) = raw.strip_prefix("agent:") else {
        return Err(usage());
    };
    let SubjectArg { pack, agent } = parse_subject(target).map_err(|_| usage())?;
    Ok(ProposerArg::Agent { pack, agent })
}
