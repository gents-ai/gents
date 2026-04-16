use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use defra_agent_desktop::app::DesktopApp;
use defra_agent_desktop::client::DesktopPaths;
use defra_agent_desktop::local_runtime::{
    default_agent_home, init_standard_local_runtime, render_human_summary, DesktopInitOptions,
};
use defra_agent_desktop::telemetry::global_log_layer;
use eframe::egui;
use tracing_subscriber::{prelude::*, EnvFilter};

#[derive(Debug, Parser)]
#[command(
    name = "defra-agent-desktop",
    about = "Native desktop dashboard for local and peered defra-agent runtimes"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Discover and save the standard local defra-agent runtime")]
    Init(InitArgs),
}

#[derive(Debug, clap::Args)]
struct InitArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    agent_home: Option<PathBuf>,
    #[arg(
        long,
        help = "Desktop data directory. Defaults to the platform-local desktop data dir"
    )]
    desktop_home: Option<PathBuf>,
    #[arg(long, default_value = "Local Agent", help = "Saved deployment label")]
    label: String,
    #[arg(long, help = "Print machine-readable JSON instead of human output")]
    json: bool,
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    if let Some(command) = cli.command {
        return run_command(command);
    }

    launch_desktop()
}

fn run_command(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Init(args) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| anyhow::anyhow!(error))?;
            let agent_home = args.agent_home.unwrap_or(default_agent_home()?);
            let desktop_paths = match args.desktop_home {
                Some(root) => DesktopPaths::from_root(root),
                None => DesktopPaths::discover()?,
            };
            let summary = runtime.block_on(init_standard_local_runtime(DesktopInitOptions {
                agent_home,
                desktop_paths,
                label: args.label,
            }))?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print!("{}", render_human_summary(&summary));
            }
            Ok(())
        }
    }
}

fn launch_desktop() -> anyhow::Result<()> {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("desktop-client")
            .build()
            .map_err(|error| anyhow::anyhow!(error))?,
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("defra-agent desktop")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1180.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "defra-agent desktop",
        options,
        Box::new(move |cc| Ok(Box::new(DesktopApp::new(cc, Arc::clone(&runtime))))),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

fn init_tracing() {
    let env_filter = with_default_transport_noise_filters(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(
                "warn,\
                 defra_agent_desktop=trace,\
                 defra_agent=info,\
                 defra_node=info,\
                 p2p=warn,\
                 iroh=warn,\
                 iroh_net=warn,\
                 iroh_relay=warn,\
                 iroh_gossip=warn,\
                 iroh_blobs=warn,\
                 iroh_quinn=warn,\
                 iroh_quinn_proto=warn,\
                 netwatch=warn,\
                 wgpu=warn,\
                 winit=warn,\
                 eframe=warn,\
                 egui=warn,\
                 naga=warn",
            )
        }),
    );

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact()
                .without_time(),
        )
        .with(global_log_layer())
        .try_init();

    tracing::info!("launching defra-agent-desktop");
}

fn with_default_transport_noise_filters(filter: EnvFilter) -> EnvFilter {
    filter
        .add_directive(
            "iroh_quinn_proto::connection=error"
                .parse()
                .expect("valid tracing directive"),
        )
        .add_directive(
            "noq_proto::connection=error"
                .parse()
                .expect("valid tracing directive"),
        )
}
