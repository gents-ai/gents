use std::io::Write as _;
use std::path::{Path, PathBuf};

use gents_session::log_rate::{RateLimitConfig, RateLimitFilter};
use tracing_subscriber::{prelude::*, EnvFilter};

fn log_rate_ceiling() -> RateLimitFilter {
    RateLimitFilter::new(RateLimitConfig::default())
}

pub fn init_tracing_with_config(config: crate::config::TracingConfig) {
    let (env_filter, explicit_filter) = match config.filter {
        Some(ref filter) => (
            EnvFilter::try_new(filter).unwrap_or_else(|_| default_env_filter()),
            true,
        ),
        None => match EnvFilter::try_from_default_env() {
            Ok(filter) => (filter, true),
            Err(_) => (default_env_filter(), false),
        },
    };
    let native = gents::runtime::native_logging::layer("desktop");
    let console = config.console || explicit_filter || native.is_none();
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(native.map(|layer| layer.with_filter(log_rate_ceiling())))
        .with(console.then(|| {
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(true)
                .with_filter(log_rate_ceiling())
        }))
        .try_init();

    tracing::info!(
        diagnostics = gents::runtime::native_logging::diagnostics_hint(),
        "desktop logs initialized"
    );
}

pub fn runtime_error_log(desktop_root: &Path) -> PathBuf {
    desktop_root.join("logs").join("runtime-errors.log")
}

pub fn desktop_error_log(desktop_root: &Path) -> PathBuf {
    desktop_root.join("logs").join("desktop-errors.log")
}

pub fn append_desktop_error(desktop_root: &Path, operation: &str, detail: &str) {
    let path = desktop_error_log(desktop_root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(&path) {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let single_line = detail.replace(['\r', '\n'], " ");
        let _ = writeln!(file, "{at} {operation} failed: {single_line}");
    }
}

pub fn diagnostics_hint(user_home: &Path) -> String {
    format!(
        "{}. Runtime startup errors: {}",
        gents::runtime::native_logging::diagnostics_hint(),
        gents_service::native_service::runtime_error_log(user_home).display()
    )
}

fn default_env_filter() -> EnvFilter {
    with_default_transport_noise_filters(EnvFilter::new(
        "warn,\
             gents_desktop_core=info,\
             gents_desktop_bridge=info,\
             gents_desktop_tauri=info,\
             gents=info,\
             defra_node=info",
    ))
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
