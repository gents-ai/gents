use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use super::{
    BackendDiscoverModelsArgs, BackendSetArgs, ConfigApplyArgs, ConfigDiffArgs, ConfigExportArgs,
    ConfigMcpCommand, ConfigScheduleCommand, ConfigTriggerCommand, ConfigValidateArgs,
    InferenceProfileCommand,
};
use crate::CONFIG_EXPORT_AFTER_HELP;
use crate::cli::output_format::OutputFormat;

#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    #[command(about = "Validate desired-state manifests under a repository root")]
    Validate(ConfigValidateArgs),
    #[command(about = "Diff desired-state manifests against live configuration")]
    Diff(ConfigDiffArgs),
    #[command(about = "Apply desired-state manifests to live configuration")]
    Apply(ConfigApplyArgs),
    #[command(about = "Write an InferenceBackend document")]
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
    },
    #[command(about = "Write an Agent document")]
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    #[command(about = "Write a Tools document")]
    Tools {
        #[command(subcommand)]
        command: ToolsConfigCommand,
    },
    #[command(about = "Write an InferenceProfile document")]
    Profile {
        #[command(subcommand)]
        command: InferenceProfileCommand,
    },
    #[command(about = "Inspect Trigger documents")]
    Trigger {
        #[command(subcommand)]
        command: ConfigTriggerCommand,
    },
    #[command(about = "Inspect Schedule documents")]
    Schedule {
        #[command(subcommand)]
        command: ConfigScheduleCommand,
    },
    #[command(about = "Inspect MCP service registry documents")]
    Mcp {
        #[command(subcommand)]
        command: ConfigMcpCommand,
    },
    #[command(about = "Create, list, show, delete, enable, or disable Skill documents")]
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    #[command(about = "Write a WorkspaceRoot document")]
    WorkspaceRoot {
        #[command(subcommand)]
        command: WorkspaceRootCommand,
    },
    #[command(about = "Export desired configuration documents", after_help = CONFIG_EXPORT_AFTER_HELP)]
    Export(ConfigExportArgs),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
pub enum ToolCeilingArg {
    MetaOnly,
    Readonly,
    Readwrite,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
pub(crate) enum ToolPackageArg {
    Minimal,
    Introspection,
    Readonly,
    Write,
    Yolo,
}

#[derive(Subcommand)]
pub(crate) enum BackendCommand {
    #[command(name = "set")]
    Set(BackendSetArgs),
    #[command(name = "discover-models")]
    DiscoverModels(BackendDiscoverModelsArgs),
    #[command(name = "list", about = "List InferenceBackend documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show an InferenceBackend document")]
    Show(ConfigShowArgs),
    #[command(name = "rm", about = "Delete an InferenceBackend document")]
    Rm(ConfigShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum AgentCommand {
    #[command(name = "set")]
    Set(AgentUpsertArgs),
    #[command(
        name = "create",
        about = "Create an Agent through the shared agent materializer"
    )]
    Create(AgentCreateArgs),
    #[command(
        name = "clone",
        about = "Clone an existing agent's context and tools into a new Agent"
    )]
    Clone(AgentCloneArgs),
    #[command(
        name = "disable",
        about = "Disable an Agent through the shared agent materializer"
    )]
    Disable(AgentDisableArgs),
    #[command(name = "list", about = "List Agent documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show an Agent document")]
    Show(ConfigShowArgs),
    #[command(name = "rm", about = "Delete an Agent document")]
    Rm(ConfigShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum WorkspaceRootCommand {
    #[command(name = "set")]
    Set(WorkspaceRootUpsertArgs),
    #[command(name = "list", about = "List WorkspaceRoot documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show a WorkspaceRoot document")]
    Show(ConfigShowArgs),
    #[command(name = "rm", about = "Delete a WorkspaceRoot document")]
    Rm(ConfigShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum ToolsConfigCommand {
    #[command(name = "set")]
    Set(ToolsSetArgs),
    #[command(name = "list", about = "List Tools documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show a Tools document")]
    Show(ConfigShowArgs),
    #[command(name = "rm", about = "Delete a Tools document")]
    Rm(ConfigShowArgs),
    #[command(
        name = "agent-target-entry",
        about = "Emit a canonical AgentTarget document"
    )]
    AgentTargetEntry(AgentTargetEntryArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct AgentTargetEntryArgs {
    #[arg(long, help = "Logical ID referenced by Tools.agents.target_ids")]
    pub(crate) target_id: String,
    #[arg(long, help = "Principal that owns this target configuration")]
    pub(crate) node_did: String,
    #[arg(long, help = "Principal that owns the destination agent")]
    pub(crate) target_node_did: String,
    #[arg(long, help = "Model-facing target name")]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) agent_id: String,
    #[arg(long)]
    pub(crate) description: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum SkillCommand {
    #[command(name = "add", about = "Create or update a Skill document")]
    Add(SkillAddArgs),
    #[command(
        name = "import",
        about = "Import a directory tree of Codex-format SKILL.md files as Skill documents"
    )]
    Import(SkillImportArgs),
    #[command(
        name = "export",
        about = "Export an agent's Skill documents as a SKILL.md directory tree"
    )]
    Export(SkillExportArgs),
    #[command(name = "list", about = "List Skill documents for an agent")]
    List(SkillListArgs),
    #[command(name = "show", about = "Show a single Skill document")]
    Show(SkillShowArgs),
    #[command(name = "rm", about = "Delete a Skill document")]
    Rm(SkillRefArgs),
    #[command(name = "enable", about = "Enable a Skill document")]
    Enable(SkillRefArgs),
    #[command(name = "disable", about = "Disable a Skill document")]
    Disable(SkillRefArgs),
}

#[derive(Subcommand)]
pub(crate) enum ToolsCommand {
    #[command(
        name = "explain",
        about = "Explain final model-callable tools per agent"
    )]
    Explain(ToolExplainArgs),
}

#[derive(clap::Args)]
pub(crate) struct ToolExplainArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
    #[arg(long, help = "Only explain one agent_id")]
    pub(crate) agent_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct SkillAddArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) node_did: String,
    #[arg(long)]
    pub(crate) skill_id: String,
    #[arg(long)]
    pub(crate) name: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    /// Inline skill instructions (the body composed into the prompt).
    #[arg(long)]
    pub(crate) instructions: Option<String>,
    /// Read instructions from a file (takes precedence over --instructions).
    #[arg(long)]
    pub(crate) instructions_file: Option<PathBuf>,
    /// Declared tool dependency (repeatable). Intersected with the agent
    /// tool ceiling at activation; never grants a tool.
    #[arg(long = "tool-ref")]
    pub(crate) tool_refs: Vec<String>,
    #[arg(long)]
    pub(crate) display_name: Option<String>,
    #[arg(long, default_value_t = true)]
    pub(crate) enabled: bool,
}

#[derive(clap::Args)]
pub(crate) struct SkillImportArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) node_did: String,
    /// A SKILL.md file, skill directory, or directory tree to scan. Each
    /// skill may include optional agents/openai.yaml metadata.
    #[arg(value_name = "PATH")]
    pub(crate) dir: PathBuf,
    /// Import skills as disabled.
    #[arg(long)]
    pub(crate) disabled: bool,
    /// Parse and report what would be imported without writing.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(clap::Args)]
pub(crate) struct SkillExportArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) node_did: String,
    /// Output directory. Each skill is written to `<dir>/<skill_id>/SKILL.md`
    /// (plus `agents/openai.yaml` when it has tool_refs or a display name).
    #[arg(value_name = "DIR")]
    pub(crate) dir: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SkillListArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) node_did: String,
}

#[derive(clap::Args)]
pub(crate) struct SkillShowArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) skill_id: String,
}

#[derive(clap::Args)]
pub(crate) struct SkillRefArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) skill_id: String,
}

#[derive(clap::Args)]
pub(crate) struct ConfigListArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct ConfigShowArgs {
    #[arg(long = "id", value_name = "ID")]
    pub(crate) id_flag: Option<String>,
    #[arg(value_name = "ID")]
    pub(crate) id: Option<String>,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct WorkspaceRootUpsertArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long, help = "Absolute path to register as a workspace root")]
    pub(crate) path: PathBuf,
    #[arg(long)]
    pub(crate) display_name: Option<String>,
    #[arg(
        long,
        help = "Register the root disabled; excluded from allowed_roots until re-enabled"
    )]
    pub(crate) disabled: bool,
}

#[derive(clap::Args)]
pub(crate) struct AgentUpsertArgs {
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) node_did: String,
    #[arg(long)]
    pub(crate) agent_id: Option<String>,
    #[arg(long)]
    pub(crate) display_name: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(
        long,
        help = "Optional AgentContext reference owned by this agent (literal prompt, skills, tools, compaction)"
    )]
    pub(crate) context_id: Option<String>,
    #[arg(
        long,
        help = "Required InferenceProfile reference; Task -> Agent -> Profile is the only model-selection path"
    )]
    pub(crate) inference_profile_id: String,
    #[arg(long = "tag", help = "Optional UI/discovery label (repeatable)")]
    pub(crate) tags: Vec<String>,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub(crate) enabled: bool,
}

#[derive(clap::Args)]
pub(crate) struct AgentCreateArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(long)]
    pub(crate) node_did: String,
    #[arg(long, help = "Agent display name")]
    pub(crate) display_name: String,
    #[arg(long, help = "Concise purpose for the created agent")]
    pub(crate) description: Option<String>,
    #[arg(
        long,
        help = "Complete agent instructions; required with --preset and optional as a clone override"
    )]
    pub(crate) system_prompt: Option<String>,
    #[arg(
        long,
        help = "Built-in permission preset (readonly|write); mutually exclusive with --clone-from"
    )]
    pub(crate) preset: Option<String>,
    #[arg(long, help = "Optional host cwd from the node's published choices")]
    pub(crate) root: Option<String>,
    #[arg(
        long,
        help = "agent_id of an existing enabled agent to clone the context/tools from; mutually exclusive with --preset"
    )]
    pub(crate) clone_from: Option<String>,
    #[arg(
        long,
        help = "Required published InferenceProfile id under the target agent's scope"
    )]
    pub(crate) profile_id: String,
}

#[derive(clap::Args)]
pub(crate) struct AgentCloneArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(value_name = "SOURCE_AGENT_ID")]
    pub(crate) source_agent_id: String,
    #[arg(long, help = "Display name for the cloned agent")]
    pub(crate) display_name: String,
    #[arg(long, help = "Override the source agent/context description")]
    pub(crate) description: Option<String>,
    #[arg(long, help = "Override the source context system prompt")]
    pub(crate) system_prompt: Option<String>,
    #[arg(long, help = "Override the source's workspace root scope")]
    pub(crate) root: Option<String>,
    #[arg(
        long,
        help = "Required published InferenceProfile id for the clone; no implicit source fallback"
    )]
    pub(crate) profile_id: String,
}

#[derive(clap::Args)]
pub(crate) struct AgentDisableArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: String,
    #[arg(value_name = "AGENT_ID")]
    pub(crate) agent_id: String,
}

#[derive(Debug, clap::Args)]
pub(crate) struct ToolsSetArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Canonical Tools JSON document to create or replace")]
    pub(crate) file: PathBuf,
}
