use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{
    BACKGROUND_AFTER_HELP, CHAT_AFTER_HELP, CLI_AFTER_HELP, CLOUD_AFTER_HELP, CODEX_AFTER_HELP,
    CONFIG_AFTER_HELP, DIAGNOSE_AFTER_HELP, FLEET_AFTER_HELP, INIT_AFTER_HELP, MCP_AFTER_HELP,
    P2P_AFTER_HELP, PROVISION_AFTER_HELP, REQUEST_AFTER_HELP, RESET_AFTER_HELP,
    RESPONSE_AFTER_HELP, SCHEMA_AFTER_HELP, SERVER_AFTER_HELP, SESSION_AFTER_HELP,
    STATUS_AFTER_HELP, TASK_AFTER_HELP, TOOLS_AFTER_HELP, TRACE_AFTER_HELP,
};

mod accounts;
mod backend;
mod config;
mod config_documents;
mod eval;
mod optimization;
mod p2p;
mod pack;
mod pack_registry;
mod plugin;
mod request;
mod runtime;
mod serve;
mod service;
mod setup;
mod trace;
mod work;

pub(crate) use accounts::*;
pub(crate) use backend::*;
pub use config::ToolCeilingArg;
pub(crate) use config::*;
pub(crate) use config_documents::*;
pub(crate) use eval::*;
pub(crate) use optimization::*;
pub(crate) use p2p::*;
pub(crate) use pack::*;
pub(crate) use pack_registry::*;
pub(crate) use plugin::*;
pub(crate) use request::*;
pub(crate) use runtime::*;
pub(crate) use serve::*;
pub(crate) use service::*;
pub(crate) use setup::*;
pub(crate) use trace::*;
pub(crate) use work::*;

fn parse_env_var_name(value: &str) -> Result<String, String> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err("environment variable name cannot be empty".to_string());
    };
    if !(first == '_' || first.is_ascii_alphabetic())
        || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(format!("invalid environment variable name {value:?}"));
    }
    Ok(value.to_string())
}

#[derive(Parser)]
#[command(
    name = "gents",
    about = "Local-first CLI for bootstrapping, running, and inspecting a gents runtime",
    after_help = CLI_AFTER_HELP
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "Print build and release metadata")]
    Version,
    #[command(about = "Manage local desktop state")]
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
    #[command(about = "Initialize a local node home directory", after_help = INIT_AFTER_HELP)]
    Init(InitArgs),
    #[command(
        about = "Provision a local node home from a portable manifest root",
        after_help = PROVISION_AFTER_HELP
    )]
    Provision(ProvisionArgs),
    #[command(about = "Clear persisted local runtime state", after_help = RESET_AFTER_HELP)]
    Reset(ResetArgs),
    #[command(
        name = "server",
        about = "Run the local gents runtime from an initialized home",
        after_help = SERVER_AFTER_HELP
    )]
    Server(ServeArgs),
    #[command(about = "Prepare an initialized local runtime and serve until shutdown")]
    PrepareRuntime(ServeArgs),
    #[command(about = "Install and control the per-user native Gents runtime service")]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    #[command(
        about = "Chat with the selected or default agent in the terminal",
        after_help = CHAT_AFTER_HELP
    )]
    Chat(ChatArgs),
    #[command(
        about = "Open the Codex terminal UI against the local Node runtime",
        after_help = CODEX_AFTER_HELP
    )]
    Codex(CodexArgs),
    #[command(about = "Sign in with ChatGPT and store OAuth credentials in DefraDB")]
    CodexLogin(CodexLoginArgs),
    #[command(about = "Probe a DefraDB-backed ChatGPT OAuth credential")]
    CodexAuthProbe(CodexAuthProbeArgs),
    #[command(
        about = "List, label, disable or remove the subscription accounts signed in on this node"
    )]
    Accounts {
        #[command(subcommand)]
        command: AccountsCommand,
    },
    #[command(
        name = "grok-login",
        about = "Sign in with Grok / xAI subscription OAuth and store credentials in DefraDB"
    )]
    GrokLogin(GrokLoginArgs),
    #[command(
        name = "grok-auth-probe",
        about = "Probe a DefraDB-backed Grok / xAI OAuth credential (read-only)"
    )]
    GrokAuthProbe(GrokAuthProbeArgs),
    #[command(
        name = "claude-login",
        about = "Sign in with the Claude subscription (OAuth) and store credentials in DefraDB",
        after_help = "Default: opens the browser and listens on a localhost callback. Use --manual on hosts without a browser: open the printed URL anywhere, then paste the code shown on Anthropic's page."
    )]
    ClaudeLogin(ClaudeLoginArgs),
    #[command(about = "Sign in to and work with a gents cloud", after_help = CLOUD_AFTER_HELP)]
    Cloud {
        #[command(subcommand)]
        command: CloudCommand,
    },
    #[command(name = "__native-fs-runner", hide = true)]
    NativeFsRunner(NativeFsRunnerArgs),
    #[command(about = "Inspect and control live P2P runtime connectivity", after_help = P2P_AFTER_HELP)]
    P2p {
        #[command(subcommand)]
        command: P2pCommand,
    },
    #[command(about = "Apply app-specific collection schemas", after_help = SCHEMA_AFTER_HELP)]
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    #[command(
        about = "Export persisted tool-call traces for measurement",
        after_help = TRACE_AFTER_HELP
    )]
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
    },
    #[command(about = "Show the current local runtime status", after_help = STATUS_AFTER_HELP)]
    Status(StatusArgs),
    #[command(about = "Run a read-only structured query against a DefraDB collection")]
    Query(QueryArgs),
    #[command(about = "Preview and apply scoped application document mutations")]
    Write(WriteArgs),
    #[command(about = "Create documents as the home node")]
    Document {
        #[command(subcommand)]
        command: DocumentCommand,
    },
    #[command(
        about = "Inspect backgrounded tool calls",
        after_help = BACKGROUND_AFTER_HELP
    )]
    Background {
        #[command(subcommand)]
        command: BackgroundCommand,
    },
    #[command(about = "Probe registered MCP service health", after_help = MCP_AFTER_HELP)]
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    #[command(about = "Inspect fleet admission slot accounting", after_help = FLEET_AFTER_HELP)]
    Fleet {
        #[command(subcommand)]
        command: FleetCommand,
    },
    #[command(
        about = "Trigger configured Task documents on demand",
        after_help = TASK_AFTER_HELP
    )]
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    #[command(about = "List, inspect, build, publish, install and exercise packs")]
    Pack {
        #[command(subcommand)]
        command: PackCommand,
    },
    #[command(about = "Build, publish, install, and run Afterburner plugins")]
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    #[command(about = "List and inspect the UX plugins the desktop loads from this home")]
    Ux {
        #[command(subcommand)]
        command: UxCommand,
    },
    #[command(about = "Run local configuration and runtime diagnostics", after_help = DIAGNOSE_AFTER_HELP)]
    Diagnose(DiagnoseArgs),
    #[command(about = "Explain resolved agent tool surfaces", after_help = TOOLS_AFTER_HELP)]
    Tools {
        #[command(subcommand)]
        command: ToolsCommand,
    },
    #[command(about = "Inspect and write runtime configuration documents", after_help = CONFIG_AFTER_HELP)]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
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
    #[command(about = "Manage and fork sessions", after_help = SESSION_AFTER_HELP)]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    #[command(about = "Inspect and control durable session goals")]
    Goal {
        #[command(subcommand)]
        command: GoalCommand,
    },
    #[command(about = "Inspect and control plugin jobs")]
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    #[command(about = "Manage EVM chain keys, endpoints, and queries")]
    Chain {
        #[command(subcommand)]
        command: ChainCommand,
    },
    #[command(about = "List and resolve human-attention mailbox items")]
    Mailbox {
        #[command(subcommand)]
        command: MailboxCommand,
    },
    #[command(about = "Run, inspect, compare and clean up evals")]
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
    #[command(about = "Run, inspect, promote and revert configuration optimization jobs")]
    Optimization {
        #[command(subcommand)]
        command: OptimizationCommand,
    },
}

#[derive(clap::Args, Clone)]
pub(crate) struct ScopeArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum DesktopCommand {
    #[command(
        about = "Preview or reset the managed home, desktop state, service and browser caches on this Mac; quit Gents first"
    )]
    Reset {
        /// Managed home; defaults to GENTS_HOME, the desktop's reviewed home, then ~/.gents.
        #[arg(long)]
        home: Option<PathBuf>,
        /// Desktop client state; defaults to GENTS_DESKTOP_HOME, then the platform's app data folder.
        #[arg(long)]
        desktop_home: Option<PathBuf>,
        /// Delete node/client/service data instead of backing it up; browser data is always cleared.
        #[arg(long)]
        delete: bool,
        /// Exact confirmation returned by the preview for the chosen action.
        #[arg(long)]
        confirm: Option<String>,
        #[arg(long, hide = true, requires = "confirm")]
        wait_for_desktop: bool,
        #[arg(long, hide = true, requires = "wait_for_desktop")]
        desktop_pid: Option<u32>,
        #[arg(long, hide = true, requires = "wait_for_desktop")]
        desktop_executable: Option<PathBuf>,
    },
}

#[cfg(test)]
mod tests;
