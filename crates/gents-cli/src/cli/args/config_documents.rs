use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use super::{ConfigListArgs, ConfigShowArgs};

#[derive(Subcommand)]
pub(crate) enum ConfigTriggerCommand {
    #[command(name = "list", about = "List Trigger documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show a Trigger document")]
    Show(ConfigShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum ConfigScheduleCommand {
    #[command(name = "list", about = "List Schedule documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show a Schedule document")]
    Show(ConfigShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum ConfigMcpCommand {
    #[command(name = "list", about = "List ToolServiceRegistry documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show a ToolServiceRegistry document")]
    Show(ConfigShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum TaskCommand {
    #[command(name = "list", about = "List configured Task documents")]
    List(TaskListArgs),
    #[command(name = "show", about = "Show a configured Task document")]
    Show(TaskShowArgs),
    #[command(name = "run", about = "Run a configured Task once, now")]
    Run(ConfigTaskRunArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct TaskListArgs {
    /// GraphQL endpoint of the running node's DefraDB. Defaults to local.
    #[arg(long)]
    pub(crate) graphql: Option<String>,

    /// Path to the node home. Used to resolve GraphQL endpoint when
    /// `--graphql` is not set.
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct TaskShowArgs {
    /// The task_id of the task to show.
    #[arg(long = "task-id")]
    pub(crate) task_id_flag: Option<String>,

    /// The task_id of the task to show.
    #[arg(value_name = "TASK_ID")]
    pub(crate) task_id: Option<String>,

    /// GraphQL endpoint of the running node's DefraDB. Defaults to local.
    #[arg(long)]
    pub(crate) graphql: Option<String>,

    /// Path to the node home. Used to resolve GraphQL endpoint when
    /// `--graphql` is not set.
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct ConfigTaskRunArgs {
    /// The task_id of the task to run.
    #[arg(long = "task-id")]
    pub(crate) task_id_flag: Option<String>,

    /// The task_id of the task to run.
    #[arg(value_name = "TASK_ID")]
    pub(crate) task_id: Option<String>,

    /// JSON object of arguments bound as the `args.*` template scope.
    /// Example: `--args '{"name": "Amy"}'`.
    #[arg(long, default_value = "{}")]
    pub(crate) args: String,

    /// Stable invocation key for a durable-goal Task run. Required when the
    /// Task declares goal_objective_template; the output session_id is derived
    /// deterministically from this key and the Task identity. For ordinary
    /// Tasks, this retains its existing meaning as a session label.
    #[arg(long)]
    pub(crate) session_id: Option<String>,

    /// Existing session to receive this Task. Queues behind its current request.
    /// This is separate from --session-id, the stable invocation key.
    #[arg(long)]
    pub(crate) continue_session: Option<String>,

    /// GraphQL endpoint of the running node's DefraDB. Defaults to local.
    #[arg(long)]
    pub(crate) graphql: Option<String>,

    /// Path to the node home. Used to resolve GraphQL endpoint when
    /// `--graphql` is not set.
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,

    /// Wait for the created AgentRequest to reach a terminal response.
    #[arg(long, default_value_t = false)]
    pub(crate) wait: bool,

    /// Idle timeout while waiting for a terminal response.
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,

    /// Poll interval while waiting for a terminal response.
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
}

#[derive(clap::Args)]
pub(crate) struct ConfigExportArgs {
    #[arg(
        long,
        value_name = "ROOT",
        help = "Directory to write the manifest root into (author format for `config validate`, `diff`, and `apply`)"
    )]
    pub(crate) root: PathBuf,
    #[arg(
        long,
        default_value_t = false,
        help = "Overwrite the root dir if it is non-empty"
    )]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
    #[arg(long = "bind-node-did", value_enum)]
    pub(crate) bind_node_did: Option<ManifestNodeDidBindingArg>,
}

#[derive(clap::Args)]
pub(crate) struct ConfigValidateArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "bind-node-did", value_enum)]
    pub(crate) bind_node_did: Option<ManifestNodeDidBindingArg>,
    #[arg(long, default_value_t = false)]
    pub(crate) force_rebind_concrete_did: bool,
}

#[derive(clap::Args)]
pub(crate) struct ConfigDiffArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "bind-node-did", value_enum)]
    pub(crate) bind_node_did: Option<ManifestNodeDidBindingArg>,
    #[arg(long, default_value_t = false)]
    pub(crate) force_rebind_concrete_did: bool,
}

#[derive(clap::Args)]
pub(crate) struct ConfigApplyArgs {
    #[arg(
        long,
        value_name = "ROOT",
        help = "Desired-state pack root. If ROOT/schemas/ exists, SDL/patches there are applied first (pack-scoped), then agent config (surfaces, Tools, triggers, …)"
    )]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "bind-node-did", value_enum)]
    pub(crate) bind_node_did: Option<ManifestNodeDidBindingArg>,
    #[arg(long, default_value_t = false)]
    pub(crate) force_rebind_concrete_did: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete live-only desired-state documents absent from the manifest, routed through the proven ApplyReconcile delete-safety model"
    )]
    pub(crate) prune: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ManifestNodeDidBindingArg {
    Home,
    Live,
}

#[derive(Subcommand)]
pub(crate) enum SchemaCommand {
    #[command(about = "Apply SDL and JSON Patch schema files")]
    Apply(SchemaApplyArgs),
}

#[derive(clap::Args)]
pub(crate) struct SchemaApplyArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "GraphQL endpoint to apply to instead of local home state"
    )]
    pub(crate) graphql: Option<String>,
    #[arg(
        value_name = "PATH",
        help = "Schema file or directory. Directories apply *.graphql/*.gql files, then *.patch.json/*.json-patch files"
    )]
    pub(crate) path: PathBuf,
    #[arg(
        long = "patch",
        value_name = "PATCH",
        help = "Extra JSON Patch file to apply after SDL files. May be repeated"
    )]
    pub(crate) patches: Vec<PathBuf>,
}
