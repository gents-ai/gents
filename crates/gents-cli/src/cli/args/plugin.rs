use std::path::PathBuf;

use super::ScopeArgs;

#[derive(clap::Subcommand)]
pub(crate) enum PluginCommand {
    /// Compile one Afterburner package directory into a standalone `.afb`.
    Build(PluginBuildArgs),
    /// Publish a built `.afb` to the plugin registry.
    Publish(PluginPublishArgs),
    /// Download and install a plugin into the home so gents can run it.
    Install(PluginInstallArgs),
    /// List the plugins installed under a home.
    List(PluginListArgs),
    /// Remove an installed plugin from a home.
    Remove(PluginRemoveArgs),
    /// Run an installed plugin once and print what it returned.
    Run(PluginRunArgs),
    /// Point an installed plugin's optional model slot at an inference profile.
    Bind(PluginBindArgs),
    /// Leave an installed plugin's model slot unbound again.
    Unbind(PluginUnbindArgs),
    /// Choose the folders plugins may read or write when an agent names a path.
    /// The session's working folder is readable without asking (never `/` or your home); add folders or single files beyond it here.
    Dirs {
        #[command(subcommand)]
        command: PluginDirsCommand,
    },
}

#[derive(clap::Subcommand)]
pub(crate) enum UxCommand {
    /// List every UX plugin on disk: the dev folder and installed packs.
    List(UxListArgs),
    /// Print one UX plugin's module as the desktop would receive it.
    Module(UxModuleArgs),
    /// Run the static lint on a plugin.js file, as `gents pack check` does.
    Lint(UxLintArgs),
}

#[derive(clap::Args)]
pub(crate) struct UxListArgs {
    #[arg(long, help = "Home to list from; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct UxModuleArgs {
    #[arg(help = "A listing id: <name> for a dev plugin, <ns>/<pack>/<name> for a pack one")]
    pub(crate) id: String,
    #[arg(long, help = "Home to read from; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct UxLintArgs {
    #[arg(help = "A plugin.js file")]
    pub(crate) file: PathBuf,
}

#[derive(clap::Subcommand)]
pub(crate) enum PluginDirsCommand {
    /// Show the folders you allowed.
    List(PluginDirsListArgs),
    /// Allow a folder or one file, read-only unless --access read_write is given.
    Add(PluginDirsAddArgs),
    /// Stop allowing a folder or file.
    Remove(PluginDirsRemoveArgs),
}

#[derive(clap::Args)]
pub(crate) struct PluginDirsListArgs {
    #[arg(long, help = "Home to read the list from; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginDirsAddArgs {
    #[arg(help = "Folder or file to allow")]
    pub(crate) path: PathBuf,
    #[arg(long, default_value = "read", help = "read or read_write")]
    pub(crate) access: gents::pack::BindAccess,
    #[arg(long, help = "Home to write the list to; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginDirsRemoveArgs {
    #[arg(help = "Folder or file to stop allowing")]
    pub(crate) path: PathBuf,
    #[arg(long, help = "Home to write the list to; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginBuildArgs {
    #[arg(help = "Directory of the Afterburner package to compile (its own afb.toml)")]
    pub(crate) dir: PathBuf,
    #[arg(
        long,
        help = "Where to write the .afb; defaults to <dir>/../<namespace>-<name>-<version>.afb"
    )]
    pub(crate) out: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginPublishArgs {
    #[arg(help = "Path to the .afb file to publish")]
    pub(crate) file: PathBuf,
    #[arg(
        long,
        help = "Plugin registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
    #[arg(
        long,
        help = "Registry token. Defaults to GENTS_REGISTRY_TOKEN, then your gents pack login"
    )]
    pub(crate) token: Option<String>,
    #[arg(
        long = "token-stdin",
        conflicts_with = "token",
        help = "Read the registry token from standard input, instead of --token"
    )]
    pub(crate) token_stdin: bool,
    #[arg(long, help = "Home holding your saved registry login")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginInstallArgs {
    #[arg(help = "Plugin to install, as `name` or `namespace/name`")]
    pub(crate) name: String,
    #[arg(long, help = "Version to install; defaults to the latest published")]
    pub(crate) version: Option<String>,
    #[arg(
        long,
        help = "Plugin registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
    #[arg(long, help = "Home to install into; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "Allow the files, network or environment the plugin asks for"
    )]
    pub(crate) grant_authority: bool,
}

#[derive(clap::Args)]
pub(crate) struct PluginListArgs {
    #[arg(
        long,
        help = "Home to list installed plugins from; defaults to ~/.gents"
    )]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginRemoveArgs {
    #[arg(help = "Installed plugin to remove, as `name` or `namespace/name`")]
    pub(crate) name: String,
    #[arg(long, help = "Home to remove the plugin from; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginRunArgs {
    #[arg(help = "Installed plugin to run, as `name` or `namespace/name`")]
    pub(crate) name: String,
    #[arg(
        long,
        help = "Canonical JSON argument delivered to the plugin on stdin; defaults to null"
    )]
    pub(crate) input: Option<String>,
    #[arg(long, help = "Home to run the plugin from; defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        value_name = "DIR",
        help = "Bind DIR read-only for this call; only a plugin that declares bind_dir may use it"
    )]
    pub(crate) bind_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PluginBindArgs {
    #[arg(help = "Installed plugin, as `name` or `namespace/name`")]
    pub(crate) name: String,
    #[arg(help = "Existing inference profile (see `gents profile list`) the plugin calls through")]
    pub(crate) profile: String,
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct PluginUnbindArgs {
    #[arg(help = "Installed plugin, as `name` or `namespace/name`")]
    pub(crate) name: String,
    #[arg(
        long,
        help = "Home the plugin is installed under; defaults to ~/.gents"
    )]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct NativeFsRunnerArgs {
    #[arg(long, value_name = "ROOT")]
    pub(crate) root: Option<PathBuf>,
    #[arg(long, value_name = "BASE")]
    pub(crate) base: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub(crate) self_test: bool,
}
