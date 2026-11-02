use std::path::PathBuf;

use super::ScopeArgs;
use crate::cli::output_format::OutputFormat;

#[derive(clap::Args)]
pub(crate) struct PackFetchArgs {
    #[arg(
        help = "Pack to download, as `name` or `namespace/name`; with --store, also a directory or .pack file"
    )]
    pub(crate) package: String,
    #[arg(long, help = "Version to download; defaults to the latest published")]
    pub(crate) version: Option<String>,
    #[arg(
        long,
        conflicts_with = "out",
        help = "Admit the pack into the home's pack store and name index, instead of writing a file"
    )]
    pub(crate) store: bool,
    #[arg(long, help = "Home whose pack store --store fills")]
    pub(crate) home: Option<std::path::PathBuf>,
    #[arg(
        long,
        conflicts_with = "store",
        help = "Where to write the .pack; defaults to <namespace>.<name>-<version>.pack here"
    )]
    pub(crate) out: Option<std::path::PathBuf>,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct PackBuildArgs {
    #[arg(help = "Pack directory to compile and pack; defaults to the current directory")]
    pub(crate) dir: Option<PathBuf>,
    #[arg(
        long,
        conflicts_with_all = ["dir", "out"],
        help = "Build every pack under packs/ into a .pack beside it, instead of one directory"
    )]
    pub(crate) all: bool,
    #[arg(
        long,
        help = "Where to write the .pack; defaults to <dir>/../<namespace>.<name>-<version>.pack"
    )]
    pub(crate) out: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PackSearchArgs {
    #[arg(help = "Search terms; omit to list every published pack")]
    pub(crate) query: Option<String>,
    #[arg(long, default_value_t = 1, help = "Result page, 50 packs each")]
    pub(crate) page: u32,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct PackPublishArgs {
    #[arg(help = "Path to the .pack file to publish")]
    pub(crate) file: PathBuf,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
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
pub(crate) struct PackLoginArgs {
    #[arg(long, help = "Account name; the password is read from standard input")]
    pub(crate) username: Option<String>,
    #[arg(
        long,
        requires = "username",
        help = "Read the password from standard input"
    )]
    pub(crate) password_stdin: bool,
    #[arg(
        long,
        conflicts_with = "username",
        help = "A token from your registry dashboard"
    )]
    pub(crate) token: Option<String>,
    #[arg(
        long = "token-stdin",
        conflicts_with_all = ["token", "username"],
        help = "Read the registry token from standard input, instead of --token"
    )]
    pub(crate) token_stdin: bool,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
    #[arg(long, help = "Home to save the login in")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PackAccountArgs {
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
    #[arg(long, help = "Home holding your saved registry login")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PackInfoArgs {
    #[arg(help = "The package, as name or namespace/name")]
    pub(crate) package: String,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct PackOwnerArgs {
    #[arg(help = "The package, as name or namespace/name")]
    pub(crate) package: String,
    #[arg(
        long,
        value_name = "USERNAME",
        help = "Give the package to this account"
    )]
    pub(crate) transfer: Option<String>,
    #[arg(
        long,
        requires = "transfer",
        help = "Confirm the transfer; it cannot be undone"
    )]
    pub(crate) yes: bool,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
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
pub(crate) struct PackYankArgs {
    #[arg(help = "The version, as namespace/name@version")]
    pub(crate) package: String,
    #[arg(long, help = "Restore a yanked version")]
    pub(crate) undo: bool,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
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
pub(crate) struct PackOutdatedArgs {
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct PackUpdateArgs {
    #[arg(help = "The installed pack to update; every outdated pack when omitted")]
    pub(crate) package: Option<String>,
    #[arg(
        long = "inference-slot",
        value_name = "NAME=PROFILE_ID",
        action = clap::ArgAction::Append,
        requires = "package",
        help = "Bind a declared inference slot to an existing node-owned profile, as for install"
    )]
    pub(crate) inference_slots: Vec<String>,
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(
        long,
        help = "Pack registry base URL. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
    #[command(flatten)]
    pub(crate) drift: PackDriftArgs,
}

#[derive(clap::Args)]
pub(crate) struct PackPruneArgs {
    pub(crate) package: String,
    #[arg(long, help = "Home containing the local pack asset cache")]
    pub(crate) home: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct PackWireArgs {
    #[arg(help = "An installed plugin, as <namespace>/<name>")]
    pub(crate) plugin: String,
    #[arg(long, help = "Agent whose Tools and Context to change")]
    pub(crate) agent: String,
    #[arg(
        long = "targets-from",
        value_name = "PACK",
        action = clap::ArgAction::Append,
        help = "Wire an installed pack's AgentTargets into the job block and trust the pack; repeat for each pack"
    )]
    pub(crate) targets_from: Vec<String>,
    #[arg(
        long,
        help = "Take the wiring back out, leaving what other plugins still use"
    )]
    pub(crate) remove: bool,
    #[arg(long, help = "Report what wiring would write, writing nothing")]
    pub(crate) preview: bool,
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct PackInstallArgs {
    #[arg(help = "A local path/sha256:<hex>, an installed coordinate, or a registry name")]
    pub(crate) package: String,
    #[arg(
        long = "inference-slot",
        value_name = "NAME=PROFILE_ID",
        action = clap::ArgAction::Append,
        help = "Bind a declared inference slot to an existing node-owned profile; repeat for every slot"
    )]
    pub(crate) inference_slots: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Inspect slots, usable profiles, and effective bindings without writing"
    )]
    pub(crate) preview: bool,
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json,
        help = "Report format; document/asset installs require json")]
    pub(crate) output: OutputFormat,
    #[arg(
        long,
        default_value_t = false,
        help = "Explicitly rebind concrete identities in document packs to the target node"
    )]
    pub(crate) force_rebind_concrete_did: bool,
    #[arg(
        long,
        help = "Pack registry base URL, used when the pack is not found locally or already installed. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
    #[command(flatten)]
    pub(crate) drift: PackDriftArgs,
    #[arg(
        long,
        help = "Allow the files, network or environment the pack's plugins ask for"
    )]
    pub(crate) grant_authority: bool,
    #[arg(
        long,
        value_name = "AGENT",
        action = clap::ArgAction::Append,
        help = "After the install, wire the pack's plugins into this agent's Tools and Context; repeat for each agent"
    )]
    pub(crate) wire: Vec<String>,
    #[arg(
        long = "expect-digest",
        value_name = "sha256:HEX",
        help = "Refuse the install, writing nothing, unless the pack's content digest is this one (see gents pack show)"
    )]
    pub(crate) expect_digest: Option<String>,
    /// Whether this install was requested for its own sake, rather than only
    /// as a documents pack's dependency; never a CLI flag.
    #[arg(skip = true)]
    pub(crate) explicit: bool,
}

#[derive(clap::Args, Clone, Copy, Debug, Default)]
pub(crate) struct PackDriftArgs {
    #[arg(long, conflicts_with = "keep", help = "Replace edited pack documents")]
    pub(crate) overwrite: bool,
    #[arg(long, help = "Leave edited pack documents as they are")]
    pub(crate) keep: bool,
}

impl PackDriftArgs {
    pub(crate) fn policy(self) -> gents::pack::DriftPolicy {
        if self.overwrite {
            gents::pack::DriftPolicy::Overwrite
        } else if self.keep {
            gents::pack::DriftPolicy::Keep
        } else {
            gents::pack::DriftPolicy::Refuse
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct PackRemoveArgs {
    #[arg(
        help = "The installed pack, as name or namespace/name; asset and plugins packs need only --home"
    )]
    pub(crate) package: String,
    #[command(flatten)]
    pub(crate) scope: ScopeArgs,
    #[command(flatten)]
    pub(crate) drift: PackDriftArgs,
}

#[derive(clap::Args)]
pub(crate) struct PackRunArgs {
    #[arg(help = "Pack directory, or a name resolved as gents pack install resolves one")]
    pub(crate) pack: String,
    #[arg(long, help = "Reuse this home instead of a fresh one per run")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "Seed prompt. Defaults to the pack's default_prompt")]
    pub(crate) prompt: Option<String>,
    #[arg(long = "job-id", help = "Run id stamped on the seed document")]
    pub(crate) job_id: Option<String>,
    #[arg(long, default_value_t = 19191, help = "HTTP port for the pack node")]
    pub(crate) http_port: u16,
    #[arg(
        long,
        default_value_t = false,
        help = "Keep the generated home after the run (for debugging)"
    )]
    pub(crate) keep_home: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Allow a prepare-step or dependency plugin that declares standing authority (bind_dir alone needs none)"
    )]
    pub(crate) grant_authority: bool,
    #[arg(
        long = "with-pack",
        value_name = "DIR_OR_PACK",
        action = clap::ArgAction::Append,
        help = "Admit a directory or .pack file into the run's home store first, so a dependency resolves from it offline; repeatable"
    )]
    pub(crate) with_pack: Vec<PathBuf>,
    #[arg(
        long,
        help = "Pack registry base URL, used to resolve the pack and its dependencies. Defaults to GENTS_REGISTRY, then the public registry"
    )]
    pub(crate) registry: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct PackInitArgs {
    #[arg(help = "Pack directory, or a pack name resolved as `gents pack install` resolves one")]
    pub(crate) pack: String,
    #[arg(long, help = "Home directory to initialize")]
    pub(crate) home: PathBuf,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace an existing home instead of refusing"
    )]
    pub(crate) overwrite: bool,
}

#[derive(clap::Args)]
pub(crate) struct PackSeedArgs {
    #[arg(help = "Pack directory, or a pack name resolved as `gents pack install` resolves one")]
    pub(crate) pack: String,
    #[arg(long, help = "Seed prompt. Defaults to the pack's default_prompt")]
    pub(crate) prompt: Option<String>,
    #[arg(long = "job-id", help = "Run id stamped on the seed document")]
    pub(crate) job_id: Option<String>,
    #[arg(long, default_value_t = 19191, help = "HTTP port for the pack node")]
    pub(crate) http_port: u16,
    #[arg(long, help = "If set, print the talk-page URL with ?run=")]
    pub(crate) page_port: Option<u16>,
}
