use std::path::PathBuf;

use clap::Subcommand;

use gents_service::args::{ServiceAction, ServiceNetworkArgs, ServiceUpdateArgs};

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
        long_about = "Write the native user-service definition without starting it. The definition runs `gents runtime server --home <HOME>`; network flags persist into <HOME>/network.json, which `gents runtime server` reads for every flag it is not given, so the settings survive reinstalls and desktop-managed updates and apply at the next start"
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
    #[command(
        about = "Move the background service to another home and start its node there",
        long_about = "Move the per-user background service to --home and start that home's node; the node the service served before stops. Other commands never move the service to another home that still exists. If the node does not start, the service is moved back"
    )]
    Repoint {
        #[arg(
            long,
            help = "Node home directory the service should serve. Defaults to ~/.gents"
        )]
        home: Option<PathBuf>,
    },
    #[command(about = "Show native supervisor state (not runtime health)")]
    Status(ServiceTargetArgs),
    #[command(about = "Remove the service definition while preserving runtime data")]
    Uninstall(ServiceTargetArgs),
}

#[derive(clap::Args)]
pub(crate) struct ServiceTargetArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "Absolute path to the gents runtime executable")]
    pub(crate) executable: Option<PathBuf>,
}

impl ServiceCommand {
    pub(crate) fn into_parts(self) -> (Option<PathBuf>, Option<PathBuf>, ServiceAction) {
        let (target, action) = match self {
            Self::Update(_)
            | Self::UpdateStatus { .. }
            | Self::UpdateRun { .. }
            | Self::UpdateCheck { .. }
            | Self::Repoint { .. } => {
                unreachable!("managed update and repoint commands are routed asynchronously")
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
