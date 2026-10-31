use std::net::IpAddr;
use std::path::PathBuf;

use clap::Subcommand;

use super::{P2pDiscoveryArg, P2pRelayModeArg};

#[derive(Subcommand)]
pub(crate) enum ServiceCommand {
    #[command(
        about = "Hand a verified update to an independent native supervisor; return acceptance, then finish your turn"
    )]
    Update(ServiceUpdateArgs),
    #[command(about = "Read the durable outcome of a managed service update")]
    UpdateStatus {
        #[arg(long)]
        home: Option<PathBuf>,
        operation_id: uuid::Uuid,
    },
    #[command(hide = true)]
    UpdateRun {
        #[arg(long)]
        home: PathBuf,
        operation_id: uuid::Uuid,
    },
    #[command(hide = true)]
    UpdateCheck {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    #[command(
        about = "Write the native user-service definition without starting it",
        long_about = "Write the native user-service definition without starting it. The definition runs `gents server --home <HOME>`; network flags persist into <HOME>/network.json, which `gents server` reads for every flag it is not given, so the settings survive reinstalls and desktop-managed updates and apply at the next start"
    )]
    Install {
        #[command(flatten)]
        target: ServiceTargetArgs,
        #[command(flatten)]
        network: ServiceNetworkArgs,
        #[arg(
            long,
            value_name = "JSON",
            help = "Replace custom non-secret service environment from a JSON string dictionary; {} clears it. Omit to preserve entries. PATH and GENTS_SYSTEM_LOG remain managed"
        )]
        environment_file: Option<PathBuf>,
    },
    #[command(about = "Start the native service")]
    Start {
        #[command(flatten)]
        target: ServiceTargetArgs,
        #[arg(long, help = "Also enable the service at login")]
        enable: bool,
    },
    #[command(about = "Stop and disable the native service")]
    Stop {
        #[command(flatten)]
        target: ServiceTargetArgs,
        #[arg(long, help = "Leave login-time enablement unchanged")]
        keep_enabled: bool,
    },
    Restart(ServiceTargetArgs),
    Enable(ServiceTargetArgs),
    Disable(ServiceTargetArgs),
    #[command(about = "Show native supervisor state (not runtime health)")]
    Status(ServiceTargetArgs),
    #[command(about = "Remove the service definition while preserving runtime data")]
    Uninstall(ServiceTargetArgs),
}

#[derive(clap::Args)]
pub(crate) struct ServiceUpdateArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "Local verified Gents executable to stage; omit for configuration-only updates"
    )]
    pub(crate) executable: Option<PathBuf>,
    #[arg(
        long,
        requires = "executable",
        help = "Expected SHA-256 of --executable, obtained from a trusted source"
    )]
    pub(crate) sha256: Option<String>,
    #[arg(
        long,
        help = "Idempotency key; reuse the same UUID only for the same update"
    )]
    pub(crate) operation_id: Option<uuid::Uuid>,
    #[command(flatten)]
    pub(crate) network: ServiceNetworkArgs,
    #[arg(long)]
    pub(crate) environment_file: Option<PathBuf>,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub(crate) drain_timeout_secs: u64,
}

#[derive(clap::Args)]
pub(crate) struct ServiceTargetArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "Absolute path to the gents runtime executable")]
    pub(crate) executable: Option<PathBuf>,
}

#[derive(clap::Args, Default)]
pub(crate) struct ServiceNetworkArgs {
    #[arg(
        long,
        help = "HTTP listen address: loopback or one interface, such as this host's tailnet address. Requests still need a DID-signed bearer the node's access control admits"
    )]
    pub(crate) http_addr: Option<IpAddr>,
    #[arg(long, help = "HTTP listen port")]
    pub(crate) http_port: Option<u16>,
    #[arg(long, help = "P2P listen address")]
    pub(crate) p2p_bind_addr: Option<IpAddr>,
    #[arg(long, help = "P2P listen port; 0 picks a random port at each start")]
    pub(crate) p2p_port: Option<u16>,
    #[arg(long, value_enum)]
    pub(crate) p2p_relay_mode: Option<P2pRelayModeArg>,
    #[arg(long, value_enum)]
    pub(crate) p2p_discovery: Option<P2pDiscoveryArg>,
    #[arg(
        long,
        help = "Clear the persisted network settings before applying the other network flags"
    )]
    pub(crate) reset_network: bool,
}

pub(crate) enum ServiceAction {
    Install {
        network: ServiceNetworkArgs,
        environment_file: Option<PathBuf>,
    },
    Start {
        enable: bool,
    },
    Stop {
        keep_enabled: bool,
    },
    Restart,
    Enable,
    Disable,
    Status,
    Uninstall,
}

impl ServiceCommand {
    pub(crate) fn into_parts(self) -> (Option<PathBuf>, Option<PathBuf>, ServiceAction) {
        let (target, action) = match self {
            Self::Update(_)
            | Self::UpdateStatus { .. }
            | Self::UpdateRun { .. }
            | Self::UpdateCheck { .. } => {
                unreachable!("managed update commands are routed asynchronously")
            }
            Self::Install {
                target,
                network,
                environment_file,
            } => (
                target,
                ServiceAction::Install {
                    network,
                    environment_file,
                },
            ),
            Self::Start { target, enable } => (target, ServiceAction::Start { enable }),
            Self::Stop {
                target,
                keep_enabled,
            } => (target, ServiceAction::Stop { keep_enabled }),
            Self::Restart(target) => (target, ServiceAction::Restart),
            Self::Enable(target) => (target, ServiceAction::Enable),
            Self::Disable(target) => (target, ServiceAction::Disable),
            Self::Status(target) => (target, ServiceAction::Status),
            Self::Uninstall(target) => (target, ServiceAction::Uninstall),
        };
        (target.home, target.executable, action)
    }
}
