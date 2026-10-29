use std::path::PathBuf;

use clap::Subcommand;

use super::{ConfigListArgs, ConfigShowArgs};
use crate::cli::output_format::OutputFormat;

#[derive(Subcommand)]
pub(crate) enum RequestCommand {
    #[command(
        about = "Send a message at the next safe step and optionally wait for its canonical terminal output"
    )]
    Submit(RequestSubmitArgs),
    #[command(about = "Show a stored AgentRequest document")]
    Show(RequestShowArgs),
    #[command(about = "Signal interrupt on an in-flight request (idempotent latch)")]
    Interrupt(RequestInterruptArgs),
    #[command(about = "Inspect pending user messages and their physical queue identities")]
    Pending(RequestPendingArgs),
    #[command(about = "Replace a pending message with newly signed text")]
    EditPending(RequestEditPendingArgs),
    #[command(about = "Remove adjacent pending messages")]
    RemovePending(RequestRemovePendingArgs),
    #[command(about = "Reorder adjacent pending messages through signed replacements")]
    ReorderPending(RequestReorderPendingArgs),
    #[command(about = "Resend a stale-terminal request with a fresh TTL")]
    Resend(RequestResendArgs),
}

#[derive(clap::Args)]
pub(crate) struct RequestPendingArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
    #[arg(long)]
    pub(crate) session_id: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct RequestPendingMutationArgs {
    #[command(flatten)]
    pub(crate) scope: RequestPendingArgs,
    #[arg(long = "expected-request-doc-id", required = true, action = clap::ArgAction::Append,
        help = "Every physical ID from request pending, in its observed order; repeat for each entry")]
    pub(crate) expected_request_doc_ids: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct RequestEditPendingArgs {
    #[command(flatten)]
    pub(crate) queue: RequestPendingMutationArgs,
    #[arg(long)]
    pub(crate) request_doc_id: String,
    #[arg(
        long,
        required_unless_present = "content_file",
        conflicts_with = "content_file"
    )]
    pub(crate) content: Option<String>,
    #[arg(long, required_unless_present = "content", conflicts_with = "content")]
    pub(crate) content_file: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct RequestRemovePendingArgs {
    #[command(flatten)]
    pub(crate) queue: RequestPendingMutationArgs,
    #[arg(long = "request-doc-id", required = true, action = clap::ArgAction::Append)]
    pub(crate) request_doc_ids: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct RequestReorderPendingArgs {
    #[command(flatten)]
    pub(crate) queue: RequestPendingMutationArgs,
    #[arg(long = "request-doc-id", required = true, action = clap::ArgAction::Append,
        help = "Selected adjacent physical IDs in the desired order; repeat for each entry")]
    pub(crate) request_doc_ids: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct RequestSubmitArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
    #[arg(long)]
    pub(crate) content: Option<String>,
    #[arg(long = "content-file")]
    pub(crate) content_file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) session_id: Option<String>,
    #[arg(long)]
    pub(crate) agent_id: Option<String>,
    /// Canonical request input JSON (skills, cwd, title, and queue settings).
    #[arg(long)]
    pub(crate) input: Option<String>,
    #[arg(
        long = "valid-until",
        help = "TTL for this request (e.g. 30s, 5m, 2h, 1d). Default: 5m. Use \"none\" or 0 to disable."
    )]
    pub(crate) valid_until: Option<String>,
    #[arg(long = "output-file")]
    pub(crate) output_file: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub(crate) no_wait: bool,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}

#[derive(clap::Args)]
pub(crate) struct RequestInterruptArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) wait: bool,
    #[arg(
        long,
        value_name = "DURATION",
        default_value = "30s",
        help = "Maximum time to wait for a terminal request state when --wait is set"
    )]
    pub(crate) timeout: String,
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format; use json for scripts"
    )]
    pub(crate) output: OutputFormat,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct RequestResendArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
    #[arg(long = "output-file")]
    pub(crate) output_file: Option<PathBuf>,
    #[arg(long, default_value_t = true)]
    pub(crate) no_wait: bool,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}

#[derive(clap::Args)]
pub(crate) struct RequestShowArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "output", value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output: OutputFormat,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum SessionCommand {
    #[command(about = "List AgentSession documents")]
    List(ConfigListArgs),
    #[command(about = "Show an AgentSession document")]
    Show(ConfigShowArgs),
    #[command(about = "Fork an existing session at a user-turn boundary")]
    Fork(SessionForkArgs),
}

#[derive(clap::Args)]
pub(crate) struct SessionForkArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "GraphQL endpoint for forking through a running runtime instead of opening local state"
    )]
    pub(crate) graphql: Option<String>,
    #[arg(
        long,
        help = "Override the caller node DID (defaults to local identity)"
    )]
    pub(crate) node_did: Option<String>,
    #[arg(
        long,
        help = "Exact source requester DID; omit for a session with no requester"
    )]
    pub(crate) requester_did: Option<String>,
    #[arg(long, value_name = "SOURCE_SESSION_ID")]
    pub(crate) from: String,
    #[arg(
        long,
        value_name = "N",
        help = "0-based user-turn index; fork cuts before this user message"
    )]
    pub(crate) at_user_turn: u32,
    #[arg(
        long,
        help = "Target agent_id for the child; omit to inherit the parent's agent"
    )]
    pub(crate) agent: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum ResponseCommand {
    #[command(about = "Show the selected canonical terminal output for a request")]
    Show(ResponseShowArgs),
    #[command(about = "Wait until a request reaches terminal state with canonical terminal output")]
    Wait(ResponseWaitArgs),
}

#[derive(clap::Args)]
pub(crate) struct ResponseShowArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ResponseWaitArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id")]
    pub(crate) request_id_flag: Option<String>,
    #[arg(value_name = "REQUEST_ID")]
    pub(crate) request_id: Option<String>,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}
