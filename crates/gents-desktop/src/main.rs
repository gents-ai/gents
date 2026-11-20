use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::time::Duration;

use clap::{Parser, Subcommand};
use gents::client_runtime::{
    dangerously_overwrite_desktop_home, default_node_home, init_standard_local_runtime,
    render_human_summary, reset_desktop_runtime_state, DesktopInitOptions,
};
use gents_store::client_journal::paths::DesktopPaths;
use tracing_subscriber::{prelude::*, EnvFilter};

#[derive(Debug, Parser)]
#[command(
    name = "gents-desktop",
    about = "Tauri desktop launcher for local and peered Gents runtimes"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Discover and save a local Gents runtime")]
    Init(InitArgs),
}

#[derive(Debug, clap::Args)]
struct InitArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    node_home: Option<PathBuf>,
    #[arg(
        long,
        help = "Desktop data directory. Defaults to the platform-local desktop data dir"
    )]
    desktop_home: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete the existing desktop data directory before re-initializing it"
    )]
    dangerously_overwrite: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Clear persisted desktop runtime state before re-initializing it"
    )]
    reset: bool,
    #[arg(long, help = "Saved deployment label")]
    label: Option<String>,
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
            let desktop_paths = DesktopPaths::resolve(args.desktop_home)?;
            if args.dangerously_overwrite {
                dangerously_overwrite_desktop_home(desktop_paths.root())?;
            } else if args.reset {
                let _ = reset_desktop_runtime_state(&desktop_paths)?;
            }
            let node_home = args.node_home.unwrap_or(default_node_home()?);
            let summary = runtime.block_on(init_standard_local_runtime(DesktopInitOptions {
                node_home,
                desktop_paths,
                label: args.label.unwrap_or_else(|| "Forge".to_string()),
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
    let tauri_binary = resolve_tauri_binary()?;
    tracing::info!(path = %tauri_binary.display(), "launching tauri desktop shell");

    if is_debug_build(&tauri_binary) && !dev_server_reachable() {
        anyhow::bail!(
            "{} is a debug build, which loads its UI from the Vite dev server \
             (http://localhost:1420) instead of bundled assets, but nothing is \
             listening there right now.\n\n\
             You can fix this in one of the following ways:\n  \
             - Start the dev server first: `npm --prefix apps/gents-desktop run dev`, \
             then run `gents-desktop` again.\n  \
             - Launch via `make desktop-native-dev`, which starts both for you.\n  \
             - Build a standalone binary with `make desktop-native-build` (release \
             mode; no dev server needed).",
            tauri_binary.display()
        );
    }

    // The frontend is not a runtime or logging supervisor. Native logging is
    // initialized inside the app; the OS service owns the agent's lifetime.
    ProcessCommand::new(&tauri_binary)
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to launch {}: {error}", tauri_binary.display()))?;
    Ok(())
}

fn is_debug_build(binary: &Path) -> bool {
    binary
        .components()
        .any(|component| component.as_os_str() == "debug")
}

/// The optional development server may not exist; bound the launcher's probe
/// before choosing whether a debug build can start.
fn dev_server_reachable() -> bool {
    TcpStream::connect_timeout(
        &"127.0.0.1:1420".parse().expect("valid socket address"),
        Duration::from_millis(300),
    )
    .is_ok()
}

fn resolve_tauri_binary() -> anyhow::Result<PathBuf> {
    if let Ok(explicit) = std::env::var("GENTS_DESKTOP_TAURI_BIN") {
        let explicit = PathBuf::from(explicit);
        if explicit.is_file() {
            return Ok(explicit);
        }
    }

    let current_exe = std::env::current_exe()?;
    let sibling = current_exe
        .parent()
        .map(|dir| dir.join(tauri_binary_name()))
        .ok_or_else(|| anyhow::anyhow!("failed to resolve launcher directory"))?;
    if sibling.is_file() {
        return Ok(sibling);
    }

    let path_candidate = PathBuf::from(tauri_binary_name());
    if which_in_path(&path_candidate).is_some() {
        return Ok(path_candidate);
    }

    Err(anyhow::anyhow!(
        "could not find the Tauri desktop binary `{}`. Install or build `gents-desktop-tauri`, or set GENTS_DESKTOP_TAURI_BIN.",
        tauri_binary_name()
    ))
}

fn which_in_path(binary: &PathBuf) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn tauri_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "gents-desktop-tauri.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "gents-desktop-tauri"
    }
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        with_default_transport_noise_filters(EnvFilter::new(
            "warn,\
                 gents_desktop_core=trace,\
                 gents=info,\
                 defra_node=info",
        ))
    });

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .compact()
                .without_time()
                .with_filter(gents_session::log_rate::RateLimitFilter::new(
                    gents_session::log_rate::RateLimitConfig::default(),
                )),
        )
        .try_init();

    tracing::info!("launching Gents desktop launcher");
}

fn with_default_transport_noise_filters(filter: EnvFilter) -> EnvFilter {
    [
        "iroh=error",
        "iroh_net=error",
        "iroh_relay=error",
        "iroh_gossip=error",
        "iroh_blobs=error",
        "iroh_quinn=error",
        "iroh_quinn_proto=error",
        "iroh_quinn_proto::connection=error",
        "quinn=error",
        "quinn_proto=error",
        "quinn_udp=error",
        "netwatch=error",
        "noq_proto::connection=error",
    ]
    .into_iter()
    .fold(filter, |filter, directive| {
        filter.add_directive(directive.parse().expect("valid tracing directive"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_launcher_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn init_reads_every_flag() {
        let cli = Cli::try_parse_from([
            "gents-desktop",
            "init",
            "--node-home",
            "node",
            "--desktop-home",
            "desktop",
            "--reset",
            "--label",
            "Studio",
            "--json",
        ])
        .expect("init flags parse");
        let Some(Command::Init(args)) = cli.command else {
            panic!("init is a subcommand");
        };
        assert_eq!(args.node_home, Some(PathBuf::from("node")));
        assert_eq!(args.desktop_home, Some(PathBuf::from("desktop")));
        assert!(args.reset && args.json && !args.dangerously_overwrite);
        assert_eq!(args.label.as_deref(), Some("Studio"));
    }

    #[test]
    fn no_subcommand_launches_the_desktop() {
        let cli = Cli::try_parse_from(["gents-desktop"]).expect("bare launch parses");
        assert!(cli.command.is_none());
    }

    #[test]
    fn a_debug_build_is_one_under_a_debug_profile_directory() {
        assert!(is_debug_build(Path::new(
            "target/debug/gents-desktop-tauri"
        )));
        assert!(!is_debug_build(Path::new(
            "target/release/gents-desktop-tauri"
        )));
        assert!(!is_debug_build(Path::new(
            "target/debugger/gents-desktop-tauri"
        )));
    }

    #[test]
    fn every_transport_noise_directive_parses() {
        with_default_transport_noise_filters(EnvFilter::new("warn"));
    }
}
