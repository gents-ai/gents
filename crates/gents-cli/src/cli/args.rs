use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{
    CODEX_AFTER_HELP, CONFIG_AFTER_HELP, DIAGNOSE_AFTER_HELP, INIT_AFTER_HELP, P2P_AFTER_HELP,
    PROVISION_AFTER_HELP, RESET_AFTER_HELP, ROOT_HELP_TEMPLATE, SERVER_AFTER_HELP,
    SESSION_AFTER_HELP, STATUS_AFTER_HELP, TASK_AFTER_HELP,
};

mod accounts;
mod backend;
mod config;
mod config_documents;
mod eval;
mod groups;
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
pub(crate) use groups::*;
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

fn parse_message(value: &str) -> Result<String, String> {
    crate::require_non_empty("message", value)
        .map(str::to_string)
        .map_err(|error| error.to_string())
}

/// Root commands from before the regrouping, and where each went. A bare `gents PROMPT`
/// sends the prompt to an agent that may run commands as the user, so one that is only a
/// former command's name, typed from habit, is refused instead of sent: `gents reset` must
/// not become a message saying "reset".
const FORMER_COMMANDS: &[(&str, &str)] = &[
    ("auth", "accounts"),
    ("cloud", "accounts login cloud"),
    ("codex-login", "accounts login codex"),
    ("grok-login", "accounts login grok"),
    ("claude-login", "accounts login claude"),
    ("claude-gateway-login", "accounts login claude-gateway"),
    ("codex-auth-probe", "accounts probe codex"),
    ("grok-auth-probe", "accounts probe grok"),
    ("session", "sessions"),
    ("goal", "automation goal"),
    ("task", "automation task"),
    ("job", "automation job"),
    ("mailbox", "automation mailbox"),
    ("tools", "config tools"),
    ("mcp", "config mcp"),
    ("pack", "config pack"),
    ("plugin", "config pack plugin"),
    ("ux", "config pack ux"),
    ("provision", "runtime provision"),
    ("reset", "runtime reset"),
    ("diagnose", "runtime diagnose"),
    ("p2p", "runtime p2p"),
    ("desktop", "runtime desktop"),
    ("version", "runtime version"),
    ("data", "utils data"),
    ("query", "utils data query"),
    ("write", "utils data write"),
    ("export", "utils data export"),
    ("document", "utils data document"),
    ("schema", "utils data schema"),
    ("debug", "utils debug"),
    ("request", "utils debug request"),
    ("response", "utils debug response"),
    ("trace", "utils debug trace"),
    ("background", "utils debug background"),
    ("fleet", "utils debug fleet"),
    ("eval", "utils eval"),
    ("optimization", "utils optimization"),
    ("chat", "-p MESSAGE"),
];

/// A command name reaches the prompt only when a root option comes before it, as in
/// `gents --home H status`, which meant `gents status --home H`.
fn parse_prompt(value: &str) -> Result<String, String> {
    use clap::CommandFactory as _;
    let prompt = parse_message(value)?;
    let word = prompt.trim();
    if Cli::command().find_subcommand(word).is_some() {
        return Err(format!(
            "`{word}` is a command; put options after it, as in `gents {word} --home HOME`"
        ));
    }
    match FORMER_COMMANDS.iter().find(|(former, _)| *former == word) {
        Some((former, now)) => Err(format!(
            "`gents {former}` is now `gents {now}`; a prompt that is only a former command's name is not sent"
        )),
        None => Ok(prompt),
    }
}

fn parse_session_id(value: &str) -> Result<String, String> {
    crate::require_non_empty("session", value)
        .map(str::to_string)
        .map_err(|error| error.to_string())
}

fn parse_agent_id(value: &str) -> Result<String, String> {
    crate::require_non_empty("agent", value)
        .map(str::to_string)
        .map_err(|error| error.to_string())
}

#[derive(Parser)]
#[command(
    name = "gents",
    about = "Local-first CLI for bootstrapping, running, and inspecting a gents runtime",
    help_template = ROOT_HELP_TEMPLATE,
    args_conflicts_with_subcommands = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
    /// Node home to set up or open. Defaults to GENTS_HOME, the folder chosen at setup, then ~/.gents.
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    /// Agent to open or message instead of the node's default. See `gents config agent list`.
    #[arg(long, value_name = "AGENT_ID", value_parser = parse_agent_id)]
    pub(crate) agent: Option<String>,
    /// Send MESSAGE to the agent, print its reply and exit, without opening an interface.
    #[arg(short = 'p', long = "print", value_name = "MESSAGE", value_parser = parse_message)]
    pub(crate) print: Option<String>,
    /// Use the yolo agent: a general-purpose agent with The Engineer's tools, made on first use.
    #[arg(long, conflicts_with = "agent")]
    pub(crate) yolo: bool,
    /// Pick SESSION_ID back up with the agent it had. See `gents sessions list`.
    #[arg(
        long = "continue",
        value_name = "SESSION_ID",
        value_parser = parse_session_id,
        conflicts_with_all = ["agent", "yolo"]
    )]
    pub(crate) continue_session: Option<String>,
    /// Open the interface with PROMPT as its first message. A PROMPT that names a command runs it.
    #[arg(value_name = "PROMPT", value_parser = parse_prompt, conflicts_with = "print")]
    pub(crate) prompt: Option<String>,
    /// As on `init`: tests pass `file` so they never write login-keychain items.
    #[arg(long, value_enum, hide = true)]
    pub(crate) store_key_custody: Option<StoreKeyCustodyArg>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "Show the current local runtime status", after_help = STATUS_AFTER_HELP)]
    Status(StatusArgs),
    #[command(about = "List, show and fork sessions", after_help = SESSION_AFTER_HELP)]
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
    #[command(about = "Goals, tasks, plugin jobs and the mailbox")]
    Automation {
        #[command(subcommand)]
        command: AutomationCommand,
    },
    #[command(about = "Sign in to providers and gents cloud, and manage accounts")]
    Accounts {
        #[command(subcommand)]
        command: AuthCommand,
    },
    #[command(
        about = "Agents, backends, tools, MCP services and packs",
        after_help = CONFIG_AFTER_HELP
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(about = "Run, install, diagnose and reset the local runtime")]
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    #[command(about = "Data, debugging, evals and optimization")]
    Utils {
        #[command(subcommand)]
        command: UtilsCommand,
    },
    #[command(
        about = "Initialize a local node home directory",
        after_help = INIT_AFTER_HELP,
        hide = true
    )]
    Init(InitArgs),
    #[command(
        about = "Prepare an initialized local runtime and serve until shutdown",
        hide = true
    )]
    PrepareRuntime(PrepareRuntimeArgs),
    #[command(
        about = "Open the Codex terminal UI against the local Node runtime",
        after_help = CODEX_AFTER_HELP,
        hide = true
    )]
    Codex(CodexArgs),
    /// Installed service definitions run `gents server --home`, so this name
    /// outlives the binary that wrote them.
    #[command(name = "server", hide = true)]
    Server(ServeArgs),
    /// A running updater of another version runs the candidate's
    /// `gents service update-check`, so this name must keep parsing.
    #[command(hide = true)]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    #[command(name = "__native-fs-runner", hide = true)]
    NativeFsRunner(NativeFsRunnerArgs),
}

#[derive(Subcommand)]
pub(crate) enum RuntimeCommand {
    #[command(
        name = "server",
        about = "Run the local gents runtime from an initialized home",
        after_help = SERVER_AFTER_HELP
    )]
    Server(Box<ServeArgs>),
    #[command(about = "Install and control the per-user native Gents runtime service")]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    #[command(
        about = "Provision a local node home from a portable manifest root",
        after_help = PROVISION_AFTER_HELP
    )]
    Provision(ProvisionArgs),
    #[command(about = "Clear persisted local runtime state", after_help = RESET_AFTER_HELP)]
    Reset(ResetArgs),
    #[command(about = "Run local configuration and runtime diagnostics", after_help = DIAGNOSE_AFTER_HELP)]
    Diagnose(DiagnoseArgs),
    #[command(about = "Inspect and control live P2P runtime connectivity", after_help = P2P_AFTER_HELP)]
    P2p {
        #[command(subcommand)]
        command: P2pCommand,
    },
    #[command(about = "Manage local desktop state")]
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
    #[command(about = "Print build and release metadata")]
    Version,
}

#[derive(Subcommand)]
pub(crate) enum AutomationCommand {
    #[command(about = "Inspect and control durable session goals")]
    Goal {
        #[command(subcommand)]
        command: GoalCommand,
    },
    #[command(
        about = "Trigger configured Task documents on demand",
        after_help = TASK_AFTER_HELP
    )]
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    #[command(about = "Inspect and control plugin jobs")]
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    #[command(about = "List and resolve human-attention mailbox items")]
    Mailbox {
        #[command(subcommand)]
        command: MailboxCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum UtilsCommand {
    #[command(about = "Query, write and export application documents and schemas")]
    Data {
        #[command(subcommand)]
        command: DataCommand,
    },
    #[command(about = "Inspect requests, responses, traces, background calls and slots")]
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
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
