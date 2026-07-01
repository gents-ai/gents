use std::fs::OpenOptions;
use std::path::Path;

use defra_agent::log_rate::{RateLimitConfig, RateLimitFilter};
use defra_agent_desktop_core::client::DesktopPaths;
use tracing_subscriber::{prelude::*, EnvFilter};

/// Per-callsite log-rate ceiling: no code path may flood the desktop log
/// file or the host journal, however hot its failure loop (#588).
fn log_rate_ceiling() -> RateLimitFilter {
    RateLimitFilter::new(RateLimitConfig::default())
}

pub(crate) fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        with_default_transport_noise_filters(EnvFilter::new(
            "warn,\
                 defra_agent_desktop_core=info,\
                 defra_agent_desktop_tauri=info,\
                 defra_agent=info,\
                 defra_node=info",
        ))
    });
    let log_path = DesktopPaths::discover()
        .map(|paths| paths.log_file_path())
        .unwrap_or_else(|_| std::env::temp_dir().join("defra-agent-desktop.log"));
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let writer_path = log_path.clone();

    if desktop_console_log_enabled() {
        let file_writer_path = writer_path.clone();
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(move || open_log_writer(&file_writer_path))
            .with_filter(log_rate_ceiling());
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .compact()
            .without_time()
            .with_filter(log_rate_ceiling());
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .with(file_layer)
            .try_init();
    } else {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(move || open_log_writer(&writer_path))
            .with_filter(log_rate_ceiling());
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .try_init();
    }

    tracing::info!(path = %log_path.display(), "desktop logs initialized");
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

fn open_log_writer(path: &Path) -> std::fs::File {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|_| {
            let fallback = std::env::temp_dir().join("defra-agent-desktop.log");
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&fallback)
                .expect("open fallback desktop log file")
        })
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
        "p2p::sync::replication::loop_runner=off",
    ]
    .into_iter()
    .fold(filter, |filter, directive| {
        filter.add_directive(directive.parse().expect("valid tracing directive"))
    })
}
