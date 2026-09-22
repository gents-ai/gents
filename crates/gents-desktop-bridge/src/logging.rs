use gents::log_rate::{RateLimitConfig, RateLimitFilter};
use tracing_subscriber::{prelude::*, EnvFilter};

fn log_rate_ceiling() -> RateLimitFilter {
    RateLimitFilter::new(RateLimitConfig::default())
}

pub fn init_tracing_with_config(config: crate::config::TracingConfig) {
    let env_filter = match config.filter {
        Some(ref filter) => EnvFilter::try_new(filter).unwrap_or_else(|_| default_env_filter()),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| default_env_filter()),
    };
    let native = gents::native_logging::layer("desktop");
    let console = config.console || native.is_none();
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
        diagnostics = gents::native_logging::diagnostics_hint(),
        "desktop logs initialized"
    );
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
