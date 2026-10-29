use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use super::{ACCOUNT_PROVIDERS, ScopeArgs};
use crate::cli::output_format::OutputFormat;

#[derive(Subcommand)]
pub(crate) enum JobCommand {
    #[command(about = "List plugin jobs, newest first")]
    List(JobListArgs),
    #[command(about = "Show one plugin job: its state, outcome, phases, children and next calls")]
    Show(JobShowArgs),
    #[command(
        about = "Follow one plugin job until it ends, is paused or awaits an answer; needs a running server"
    )]
    Watch(JobWatchArgs),
    #[command(about = "Stop one plugin job and interrupt its children; local only")]
    Stop(JobControlArgs),
    #[command(about = "Pause one plugin job and interrupt its children; local only")]
    Pause(JobControlArgs),
    #[command(
        about = "Turn a finished dynamic workflow run into an installed tool, wired into an agent"
    )]
    Promote(JobPromoteArgs),
}

#[derive(Clone, Copy, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
pub(crate) enum JobStateArg {
    Running,
    Paused,
    AwaitingUser,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    All,
}

#[derive(clap::Args)]
pub(crate) struct JobListArgs {
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(long, value_name = "SESSION_ID", help = "Only jobs of this session")]
    pub(crate) session: Option<String>,
    #[arg(
        long,
        value_enum,
        value_name = "STATE",
        help = "Only jobs in this state; repeatable, or `all`. Defaults to running, paused and awaiting_user"
    )]
    pub(crate) state: Vec<JobStateArg>,
    #[arg(long, value_name = "NS/NAME", help = "Only jobs of this plugin")]
    pub(crate) plugin: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct JobShowArgs {
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(help = "A job address (SESSION_ID:TOOL_CALL_ID), a tool call id or a handle")]
    pub(crate) selector: String,
    #[arg(
        long,
        value_name = "SESSION_ID",
        help = "Resolve a handle within this session"
    )]
    pub(crate) session: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct JobControlArgs {
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(help = "A job address (SESSION_ID:TOOL_CALL_ID), a tool call id or a handle")]
    pub(crate) selector: String,
    #[arg(
        long,
        value_name = "SESSION_ID",
        help = "Resolve a handle within this session"
    )]
    pub(crate) session: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct JobPromoteArgs {
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(help = "A dynamic job address (SESSION_ID:TOOL_CALL_ID) or a tool call id")]
    pub(crate) selector: String,
    #[arg(
        long,
        help = "The tool's snake_case name, installed as gents/<name>; defaults to the run's handle when it is one"
    )]
    pub(crate) name: Option<String>,
    #[arg(
        long,
        help = "Agent to wire the tool into; defaults to the agent that ran the workflow"
    )]
    pub(crate) agent: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct JobWatchArgs {
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(help = "A job address (SESSION_ID:TOOL_CALL_ID), a tool call id or a handle")]
    pub(crate) selector: String,
    #[arg(
        long,
        value_name = "SESSION_ID",
        help = "Resolve a handle within this session"
    )]
    pub(crate) session: Option<String>,
    #[arg(
        long,
        help = "Print nothing until the job ends, then one JSON line; exit 0 completed, 3 paused or awaiting_user, 1 failed, cancelled or timed_out, 4 not observable"
    )]
    pub(crate) until_terminal: bool,
    #[arg(long, default_value_t = 1_000)]
    pub(crate) interval_ms: u64,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output: OutputFormat,
}

#[derive(Subcommand)]
pub(crate) enum GoalCommand {
    #[command(about = "Show the durable goal for a session")]
    Show(GoalShowArgs),
    #[command(about = "Create or update the durable goal for a session")]
    Set(GoalSetArgs),
    #[command(about = "Resume a goal and atomically enqueue its continuation")]
    ResumeRequest(GoalResumeArgs),
    #[command(
        about = "Move the profile a usage limit stopped to another account, then resume the goal"
    )]
    ResumeOn(GoalResumeOnArgs),
    #[command(about = "Delete the durable goal for a session")]
    Clear(GoalShowArgs),
}

#[derive(clap::Args)]
pub(crate) struct GoalScopeArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for a running runtime")]
    pub(crate) graphql: Option<String>,
    #[arg(
        long,
        help = "Override the goal owner DID (defaults to local identity)"
    )]
    pub(crate) node_did: Option<String>,
    #[arg(long, value_name = "SESSION_ID")]
    pub(crate) session: String,
}

#[derive(clap::Args)]
pub(crate) struct GoalShowArgs {
    #[command(flatten)]
    pub(crate) scope: GoalScopeArgs,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct GoalResumeArgs {
    #[command(flatten)]
    pub(crate) scope: GoalScopeArgs,
    #[arg(
        long,
        value_name = "REQUEST_ID",
        help = "Terminal predecessor; reuse this ID when retrying the same resume"
    )]
    pub(crate) from: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct GoalResumeOnArgs {
    #[arg(
        help = "The account to move to: its label, credential_id or account_ref, or an API-key \
                backend's id or name"
    )]
    pub(crate) account: String,
    #[command(flatten)]
    pub(crate) scope: GoalScopeArgs,
    #[arg(
        long,
        value_name = "REQUEST_ID",
        help = "The request the usage limit stopped; reuse this ID when retrying"
    )]
    pub(crate) from: String,
    #[arg(long, value_parser = ACCOUNT_PROVIDERS, help = "Narrows the account to this sign-in provider")]
    pub(crate) provider: Option<String>,
    /// Also move the other profiles of the profile's agents on the same account.
    #[arg(long)]
    pub(crate) with_compaction: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum GoalStatusArg {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(clap::Args)]
pub(crate) struct GoalSetArgs {
    #[command(flatten)]
    pub(crate) scope: GoalScopeArgs,
    #[arg(long, help = "Goal objective; required when creating a goal")]
    pub(crate) objective: Option<String>,
    #[arg(long, value_enum)]
    pub(crate) status: Option<GoalStatusArg>,
    #[arg(long, value_name = "TOKENS", help = "Positive charged-token budget")]
    pub(crate) token_budget: Option<i64>,
    #[arg(
        long,
        conflicts_with = "token_budget",
        help = "Remove the charged-token budget"
    )]
    pub(crate) clear_token_budget: bool,
    #[arg(
        long,
        value_name = "on|off",
        value_parser = clap::builder::BoolishValueParser::new(),
        help = "Resume a usage-limited goal by itself at the reset the provider reported"
    )]
    pub(crate) auto_resume: Option<bool>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}

#[derive(Subcommand)]
pub(crate) enum ChainCommand {
    #[command(about = "Create, list, show, and revoke chain keys")]
    Key {
        #[command(subcommand)]
        command: ChainKeyCommand,
    },
    #[command(about = "Run an allowlisted eth_* JSON-RPC read against an EthTool endpoint")]
    Query(ChainQueryArgs),
}

#[derive(clap::Args)]
pub(crate) struct ChainQueryArgs {
    #[arg(long, help = "EthTool.tool_id")]
    pub(crate) tool_id: String,
    #[arg(help = "JSON-RPC method, e.g. eth_blockNumber")]
    pub(crate) method: String,
    #[arg(
        value_name = "PARAMS_JSON",
        help = "JSON array of RPC params. Default: []"
    )]
    pub(crate) params: Option<String>,
    #[command(flatten)]
    pub(crate) access: ChainKeyAccessArgs,
}

#[derive(Subcommand)]
pub(crate) enum ChainKeyCommand {
    #[command(about = "Generate a secp256k1 chain key in the OS keyring")]
    Generate(ChainKeyGenerateArgs),
    #[command(about = "List chain-key bindings for the local node")]
    List(ChainKeyAccessArgs),
    #[command(about = "Show one chain-key binding (never prints key material)")]
    Show(ChainKeyShowArgs),
    #[command(about = "Revoke a binding and delete the keyring secret")]
    Revoke(ChainKeyShowArgs),
}

#[derive(clap::Args)]
pub(crate) struct ChainKeyAccessArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for a running runtime")]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ChainKeyGenerateArgs {
    #[command(flatten)]
    pub(crate) access: ChainKeyAccessArgs,
    #[arg(long, help = "Stable binding id. Default: a UUID.")]
    pub(crate) name: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ChainKeyShowArgs {
    pub(crate) binding_id: String,
    #[command(flatten)]
    pub(crate) access: ChainKeyAccessArgs,
}

#[derive(Subcommand)]
pub(crate) enum MailboxCommand {
    #[command(about = "List open mailbox items for the local node")]
    List(MailboxListArgs),
    #[command(about = "Show one mailbox item")]
    Show(MailboxItemArgs),
    #[command(about = "Dismiss one open mailbox item as its owner")]
    Dismiss(MailboxItemArgs),
    #[command(about = "Submit a signed reply using a mailbox item's existing route")]
    Reply(MailboxReplyArgs),
}

#[derive(clap::Args)]
pub(crate) struct MailboxReplyArgs {
    #[command(flatten)]
    pub(crate) item: MailboxItemArgs,
    #[arg(value_name = "MESSAGE")]
    pub(crate) message: String,
}

#[derive(clap::Args)]
pub(crate) struct MailboxAccessArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for a running runtime")]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct MailboxListArgs {
    #[command(flatten)]
    pub(crate) access: MailboxAccessArgs,
    #[arg(long, help = "Include terminal mailbox history")]
    pub(crate) all: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct MailboxItemArgs {
    #[command(flatten)]
    pub(crate) access: MailboxAccessArgs,
    #[arg(value_name = "DOC_ID")]
    pub(crate) doc_id: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}
