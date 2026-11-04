use clap::Subcommand;

use super::{
    AccountsCommand, BackgroundCommand, ClaudeGatewayLoginArgs, ClaudeLoginArgs,
    CodexAuthProbeArgs, CodexLoginArgs, DocumentCommand, ExportArgs, FleetCommand,
    GrokAuthProbeArgs, GrokLoginArgs, QueryArgs, RequestCommand, ResponseCommand, SchemaCommand,
    TraceCommand, WriteArgs,
};
use crate::{
    BACKGROUND_AFTER_HELP, FLEET_AFTER_HELP, REQUEST_AFTER_HELP, RESPONSE_AFTER_HELP,
    SCHEMA_AFTER_HELP, TRACE_AFTER_HELP,
};

#[derive(Subcommand)]
pub(crate) enum AuthCommand {
    #[command(about = "Sign in to a provider and store its credentials in DefraDB")]
    Login {
        #[command(subcommand)]
        command: AuthLoginCommand,
    },
    #[command(about = "Check a stored OAuth credential")]
    Probe {
        #[command(subcommand)]
        command: AuthProbeCommand,
    },
    #[command(
        about = "List, label, disable or remove the subscription accounts signed in on this node"
    )]
    Accounts {
        #[command(subcommand)]
        command: AccountsCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum AuthLoginCommand {
    #[command(about = "Sign in with ChatGPT and store OAuth credentials in DefraDB")]
    Codex(CodexLoginArgs),
    #[command(
        about = "Sign in with Grok / xAI subscription OAuth and store credentials in DefraDB"
    )]
    Grok(GrokLoginArgs),
    #[command(
        about = "Sign in with the Claude subscription (OAuth) and store credentials in DefraDB",
        after_help = "Default: opens the browser and listens on a localhost callback. Use --manual on hosts without a browser: open the printed URL anywhere, then paste the code shown on Anthropic's page."
    )]
    Claude(ClaudeLoginArgs),
    #[command(about = "Sign in to a Claude Code cloud gateway and store the account in DefraDB")]
    ClaudeGateway(ClaudeGatewayLoginArgs),
}

#[derive(Subcommand)]
pub(crate) enum AuthProbeCommand {
    #[command(about = "Probe a DefraDB-backed ChatGPT OAuth credential")]
    Codex(CodexAuthProbeArgs),
    #[command(about = "Probe a DefraDB-backed Grok / xAI OAuth credential (read-only)")]
    Grok(GrokAuthProbeArgs),
}

#[derive(Subcommand)]
pub(crate) enum DataCommand {
    #[command(about = "Run a read-only structured query against a DefraDB collection")]
    Query(QueryArgs),
    #[command(about = "Preview and apply scoped application document mutations")]
    Write(WriteArgs),
    #[command(about = "Export full-fidelity application records from the home node")]
    Export(ExportArgs),
    #[command(about = "Create documents as the home node")]
    Document {
        #[command(subcommand)]
        command: DocumentCommand,
    },
    #[command(about = "Apply app-specific collection schemas", after_help = SCHEMA_AFTER_HELP)]
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum DebugCommand {
    #[command(about = "Low-level request submission and inspection", after_help = REQUEST_AFTER_HELP)]
    Request {
        #[command(subcommand)]
        command: RequestCommand,
    },
    #[command(about = "Low-level response inspection", after_help = RESPONSE_AFTER_HELP)]
    Response {
        #[command(subcommand)]
        command: ResponseCommand,
    },
    #[command(
        about = "Export persisted tool-call traces for measurement",
        after_help = TRACE_AFTER_HELP
    )]
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
    },
    #[command(
        about = "Inspect backgrounded tool calls",
        after_help = BACKGROUND_AFTER_HELP
    )]
    Background {
        #[command(subcommand)]
        command: BackgroundCommand,
    },
    #[command(about = "Inspect fleet admission slot accounting", after_help = FLEET_AFTER_HELP)]
    Fleet {
        #[command(subcommand)]
        command: FleetCommand,
    },
}
