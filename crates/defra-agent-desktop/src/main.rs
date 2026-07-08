use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::Duration;
use std::{fs::OpenOptions, path::Path};

use clap::{Parser, Subcommand};
use defra_agent_desktop_core::client::DesktopPaths;
use defra_agent_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, default_agent_home, init_standard_local_runtime,
    init_status_endpoint_runtime, render_human_summary, reset_desktop_runtime_state,
    DesktopInitOptions, StatusEndpointInitOptions,
};
use tracing_subscriber::{prelude::*, EnvFilter};

#[derive(Debug, Parser)]
#[command(
    name = "defra-agent-desktop",
    about = "Tauri desktop launcher for local and peered defra-agent runtimes"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Discover and save a local or status-endpoint defra-agent runtime")]
    Init(InitArgs),
}

#[derive(Debug, clap::Args)]
struct InitArgs {
    #[arg(long, help = "Agent home directory. Defaults to ~/.defra-agent")]
    agent_home: Option<PathBuf>,
    #[arg(
        long,
        visible_aliases = ["status-url", "graphql", "graphql-endpoint"],
        value_name = "URL",
        help = "Remote defra-agent /status or GraphQL endpoint to seed as the initial desktop deployment"
    )]
    status_endpoint: Option<String>,
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
            let desktop_paths = match args.desktop_home {
                Some(root) => DesktopPaths::from_root(root),
                None => DesktopPaths::discover()?,
            };
            if args.dangerously_overwrite {
                dangerously_overwrite_desktop_home(desktop_paths.root())?;
            } else if args.reset {
                let _ = reset_desktop_runtime_state(&desktop_paths)?;
            }
            let summary = if let Some(status_endpoint) = args.status_endpoint {
                runtime.block_on(init_status_endpoint_runtime(StatusEndpointInitOptions {
                    desktop_paths,
                    status_endpoint,
                    label: args.label,
                }))?
            } else {
                let agent_home = args.agent_home.unwrap_or(default_agent_home()?);
                runtime.block_on(init_standard_local_runtime(DesktopInitOptions {
                    agent_home,
                    desktop_paths,
                    label: args.label.unwrap_or_else(|| "Local Agent".to_string()),
                }))?
            };
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

    // This check allows for a helpful error message to be shown to the user on the
    // command line, as opposed to opening a blank Tauri window with a more generic
    // "Connection refused" message.
    if is_debug_build(&tauri_binary) && !dev_server_reachable() {
        anyhow::bail!(
            "{} is a debug build, which loads its UI from the Vite dev server \
             (http://localhost:1420) instead of bundled assets, but nothing is \
             listening there right now.\n\n\
             You can fix this in one of the following ways:\n  \
             - Start the dev server first: `npm --prefix apps/desktop-tauri run dev`, \
             then run `defra-agent-desktop` again.\n  \
             - Launch via `make desktop-native-dev`, which starts both for you.\n  \
             - Build a standalone binary with `make desktop-native-build` (release \
             mode; no dev server needed).",
            tauri_binary.display()
        );
    }

    if desktop_console_log_enabled() {
        ProcessCommand::new(&tauri_binary)
            .spawn()
            .map_err(|error| {
                anyhow::anyhow!("failed to launch {}: {error}", tauri_binary.display())
            })?;
        return Ok(());
    }

    let log_path = DesktopPaths::discover()
        .map(|paths| paths.log_file_path())
        .unwrap_or_else(|_| std::env::temp_dir().join("defra-agent-desktop.log"));
    let stderr = open_log_writer(&log_path)?;
    let stdout = stderr.try_clone().map_err(|error| {
        anyhow::anyhow!(
            "failed to clone desktop log writer {}: {error}",
            log_path.display()
        )
    })?;

    ProcessCommand::new(&tauri_binary)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to launch {}: {error}", tauri_binary.display()))?;
    Ok(())
}

fn desktop_console_log_enabled() -> bool {
    std::env::var("DEFRA_AGENT_DESKTOP_CONSOLE_LOG")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn open_log_writer(path: &Path) -> anyhow::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| anyhow::anyhow!("failed to open desktop log {}: {error}", path.display()))
}

/// True if `binary`'s path runs through a `debug` build directory (e.g.
/// `target/debug/...`), the standard cargo layout for a non-release profile.
fn is_debug_build(binary: &Path) -> bool {
    binary
        .components()
        .any(|component| component.as_os_str() == "debug")
}

/// True if something is listening on the Vite dev server port
/// (`tauri.conf.json`'s `build.devUrl`, `http://localhost:1420`).
fn dev_server_reachable() -> bool {
    TcpStream::connect_timeout(
        &"127.0.0.1:1420".parse().expect("valid socket address"),
        Duration::from_millis(300),
    )
    .is_ok()
}

fn resolve_tauri_binary() -> anyhow::Result<PathBuf> {
    if let Ok(explicit) = std::env::var("DEFRA_AGENT_DESKTOP_TAURI_BIN") {
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
        "could not find the Tauri desktop binary `{}`. Install or build `defra-agent-desktop-tauri`, or set DEFRA_AGENT_DESKTOP_TAURI_BIN.",
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
        "defra-agent-desktop-tauri.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "defra-agent-desktop-tauri"
    }
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        with_default_transport_noise_filters(EnvFilter::new(
            "warn,\
                 defra_agent_desktop_core=trace,\
                 defra_agent=info,\
                 defra_node=info",
        ))
    });

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                // Diagnostics go to stderr so stdout stays clean for `init --json`.
                .with_writer(std::io::stderr)
                .with_target(false)
                .compact()
                .without_time()
                // Per-callsite log-rate ceiling: no code path may flood the
                // host journal, however hot its failure loop (#588).
                .with_filter(defra_agent::log_rate::RateLimitFilter::new(
                    defra_agent::log_rate::RateLimitConfig::default(),
                )),
        )
        .try_init();

    tracing::info!("launching defra-agent desktop launcher");
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
