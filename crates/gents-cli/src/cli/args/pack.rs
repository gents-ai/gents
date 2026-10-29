use std::path::PathBuf;

use super::{
    PackAccountArgs, PackBuildArgs, PackFetchArgs, PackInfoArgs, PackInitArgs, PackInstallArgs,
    PackLoginArgs, PackOutdatedArgs, PackOwnerArgs, PackPruneArgs, PackPublishArgs, PackRemoveArgs,
    PackRunArgs, PackSearchArgs, PackSeedArgs, PackUpdateArgs, PackWireArgs, PackYankArgs,
};

#[derive(clap::Args)]
pub(crate) struct PackShowArgs {
    #[arg(help = "A local path/sha256:<hex>, an installed coordinate, or a registry name")]
    pub(crate) package: String,
    #[arg(long, help = "Home whose pack store holds local packs")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
    #[arg(
        long,
        help = "Print {config, scenario} as an install would load them, instead of the manifest and files"
    )]
    pub(crate) config: bool,
}

#[derive(clap::Args)]
pub(crate) struct PackListArgs {
    #[arg(long, help = "Home whose pack store to list")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PackTemplate {
    /// A documents pack: one agent, context, tools and task.
    Minimal,
    /// The minimal pack plus a job schema, an event source and a trigger.
    Automation,
    /// A plugins pack with one plugin, ready to build.
    PluginTool,
    /// A pack of files with no configuration.
    Assets,
    /// A documents pack shipping one workflow: a Rust job plugin, the worker it spawns and the caller Tools template.
    Workflow,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackKindArg {
    Documents,
    Assets,
    Plugins,
}

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct PackScaffoldArgs {
    #[arg(
        long,
        value_enum,
        help = "Pack kind; picks the template when --template is absent"
    )]
    pub(crate) kind: Option<PackKindArg>,
    #[arg(
        long,
        default_value = "gents",
        help = "Registry namespace the pack publishes under"
    )]
    pub(crate) namespace: String,
    #[arg(long, value_enum)]
    pub(crate) template: Option<PackTemplate>,
    #[arg(
        long,
        help = "Plugin language; the plugin-tool template takes any language gents builds, the workflow template rust, js or ts; defaults to rust"
    )]
    pub(crate) language: Option<String>,
    #[arg(
        long,
        help = "Also scaffold a UX plugin (ux/<name>/plugin.js): a nav row, an agent section and a ::<name> directive"
    )]
    pub(crate) ux: bool,
}

#[derive(clap::Args)]
pub(crate) struct PackNewArgs {
    #[arg(help = "snake_case pack name; the pack is created in ./<name>")]
    pub(crate) name: String,
    #[command(flatten)]
    pub(crate) options: PackScaffoldArgs,
}

#[derive(clap::Args)]
pub(crate) struct PackAddArgs {
    #[command(subcommand)]
    pub(crate) command: PackAddCommand,
    #[arg(
        long,
        global = true,
        help = "Pack directory; defaults to the current directory"
    )]
    pub(crate) dir: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
pub(crate) enum PackAddCommand {
    /// An agent with its prompt, context and tools, in an inference slot.
    Agent {
        id: String,
        #[arg(long, help = "Inference slot; defaults to one named after the agent")]
        slot: Option<String>,
    },
    /// A task with its prompt, run by an existing agent.
    Task {
        id: String,
        #[arg(long)]
        agent: String,
    },
    /// An event source and a trigger that runs a task when a document is created.
    Trigger {
        id: String,
        #[arg(long)]
        task: String,
        #[arg(long, help = "Collection whose new documents fire the trigger")]
        on: String,
    },
    /// A plugin from source, or from an already compiled plugin file.
    Plugin {
        name: String,
        #[arg(long, help = "Source language; defaults to rust")]
        language: Option<String>,
        #[arg(
            long,
            conflicts_with = "language",
            help = "A compiled plugin file to ship as is"
        )]
        prebuilt: Option<PathBuf>,
    },
    /// A skill whose instructions live in skills/<id>/SKILL.md.
    Skill {
        id: String,
        #[arg(long, help = "An existing SKILL.md to copy")]
        from: Option<PathBuf>,
    },
    /// A collection schema under schemas/.
    Schema { collection: String },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum PackPart {
    Agent,
    Task,
    Trigger,
    Plugin,
    Skill,
    Schema,
}

#[derive(clap::Args)]
pub(crate) struct PackRemovePartArgs {
    #[arg(value_enum)]
    pub(crate) part: PackPart,
    pub(crate) id: String,
    #[arg(long, help = "Pack directory; defaults to the current directory")]
    pub(crate) dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PackFmtArgs {
    #[arg(help = "Pack directory; defaults to the current directory")]
    pub(crate) dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PackDiffArgs {
    #[arg(help = "A local path/sha256:<hex>, an installed coordinate, or a registry name")]
    pub(crate) a: String,
    #[arg(help = "The pack to compare it with, named the same ways")]
    pub(crate) b: String,
    #[arg(long, help = "Home whose pack store holds local packs")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct PackTestArgs {
    #[arg(help = "Pack directory; defaults to the current directory")]
    pub(crate) dir: Option<PathBuf>,
    #[arg(
        long,
        help = "Also run the pack's experiment.json scenario; needs a model endpoint"
    )]
    pub(crate) scenario: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackImportFrom {
    /// Agent Plugins: `plugin.json`, `skills/`, `mcp.json`.
    AgentPlugins,
    /// Claude Code: `.claude-plugin/plugin.json`.
    Claude,
    /// Codex: `.codex-plugin/plugin.json`.
    Codex,
    /// Hermes Agent: `plugin.yaml`.
    Hermes,
    /// An MCP registry `server.json`.
    Mcp,
    /// A single Agent Skill: `SKILL.md`.
    Skill,
}

#[derive(clap::Args)]
pub(crate) struct PackImportArgs {
    #[arg(help = "The plugin: a directory or a git URL")]
    pub(crate) source: String,
    #[arg(long, value_enum, help = "Its ecosystem; detected when omitted")]
    pub(crate) from: Option<PackImportFrom>,
    #[arg(long, help = "Where to write the pack; defaults to ./<name>")]
    pub(crate) out: Option<PathBuf>,
    #[arg(long, help = "snake_case pack name; defaults to the plugin's own name")]
    pub(crate) name: Option<String>,
    #[arg(
        long,
        default_value = "gents",
        help = "Registry namespace the pack publishes under"
    )]
    pub(crate) namespace: String,
}

#[derive(clap::Args)]
pub(crate) struct PackCheckArgs {
    #[arg(help = "Pack directories to check; defaults to the current directory")]
    pub(crate) dirs: Vec<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PackVerifyArgs {
    #[arg(help = "A .pack file, or sha256:<hex> for a pack in the home's store")]
    pub(crate) target: String,
    #[arg(long, help = "Home whose pack store to check")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
pub(crate) enum PackCommand {
    /// List the packs in the home's store.
    List(PackListArgs),
    /// Inspect a pack: its manifest, digest and every file, or (with
    /// `--config`) its loaded configuration and scenario.
    Show(PackShowArgs),
    /// Check a .pack file or a stored pack against its digest.
    Verify(PackVerifyArgs),
    /// Scaffold a new pack in ./<name> from a template.
    New(PackNewArgs),
    /// Scaffold a pack in the current directory, named after it.
    Init(PackScaffoldArgs),
    /// Convert a plugin from another ecosystem into a pack directory to review.
    Import(PackImportArgs),
    /// Add a part to a pack and record it in manifest.json.
    Add(PackAddArgs),
    /// Remove a part and every file it added.
    RemovePart(PackRemovePartArgs),
    /// Rewrite manifest.json and pack_config.json in canonical form.
    Fmt(PackFmtArgs),
    /// List the files and documents that differ between two packs.
    Diff(PackDiffArgs),
    /// Check and build a pack, then run every plugin's test cases.
    Test(PackTestArgs),
    /// Run every validation an install would on pack directories; writes nothing.
    Check(PackCheckArgs),
    /// Install a pack into an initialized node; never seed or prune.
    Install(PackInstallArgs),
    /// Wire an installed plugin into an agent's Tools and Context, or with
    /// `--remove` take it back out.
    Wire(PackWireArgs),
    /// Remove an installed pack of any kind, and whatever nothing else still uses.
    Remove(PackRemoveArgs),
    /// List installed packs with a newer version in the registry.
    Outdated(PackOutdatedArgs),
    /// Install the latest version of outdated packs.
    Update(PackUpdateArgs),
    /// Remove superseded generated asset-cache versions without run history.
    Prune(PackPruneArgs),
    /// Run, initialize or seed a pack's experiment.json scenario.
    #[command(subcommand)]
    Scenario(PackScenarioCommand),
    /// Compile a pack's plugins and pack the whole pack into one `.pack`.
    Build(PackBuildArgs),
    /// Search the pack registry.
    Search(PackSearchArgs),
    /// Show a package's registry page: versions, digests and owner.
    Info(PackInfoArgs),
    /// Sign in to the registry and save the token in the gents home.
    Login(PackLoginArgs),
    /// Remove the saved token from this machine; does not revoke it on the registry.
    Logout(PackAccountArgs),
    /// Show who the saved registry login is.
    Whoami(PackAccountArgs),
    /// Yank a published version so new installs skip it, or restore it.
    Yank(PackYankArgs),
    /// Show a package's owner, or give it to another account.
    Owner(PackOwnerArgs),
    /// Publish a built `.pack` to the pack registry.
    Publish(PackPublishArgs),
    /// Download a pack without installing it, or admit it into the home's
    /// pack store with `--store`.
    Fetch(PackFetchArgs),
}

#[derive(clap::Subcommand)]
pub(crate) enum PackScenarioCommand {
    /// Exercise a scenario in a dedicated home: apply, seed, await, report.
    Run(PackRunArgs),
    /// Initialize a dedicated scenario home.
    Init(PackInitArgs),
    /// Seed an installed scenario against an already-serving node.
    Seed(PackSeedArgs),
}
