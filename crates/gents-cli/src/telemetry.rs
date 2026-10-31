use std::env;

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{global, KeyValue};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SdkTracerProvider, SpanExporter};
use opentelemetry_sdk::Resource;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const DEFAULT_SERVICE_NAME: &str = "gents";
const DEFAULT_HOSTNAME: &str = "unknown";
const TRACER_NAME: &str = "gents";

pub(crate) struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
}

fn with_default_transport_noise_filters(filter: EnvFilter) -> EnvFilter {
    const NOISE_DIRECTIVES: &[&str] = &[
        "iroh_quinn_proto::connection=error",
        "noq_proto::connection=error",
        "p2p=warn",
        "iroh=warn",
        "iroh_net=warn",
        "iroh_relay=warn",
        "iroh_gossip=warn",
        "iroh_blobs=warn",
        "iroh_quinn=warn",
        "iroh_quinn_proto=warn",
        "netwatch=warn",
    ];
    // Equal-specificity directives added later replace earlier ones. Parse the
    // built-in noise floor first so explicit RUST_LOG directives remain the
    // final authority for any transport target they mention.
    EnvFilter::new(format!("{},{}", NOISE_DIRECTIVES.join(","), filter))
}

impl TelemetryGuard {
    pub(crate) fn shutdown(self) {
        let Some(provider) = self.tracer_provider else {
            return;
        };

        if let Err(error) = provider.force_flush() {
            tracing::warn!(%error, "force flushing OTLP tracer provider failed");
        }
        if let Err(error) = provider.shutdown() {
            tracing::warn!(%error, "shutting down OTLP tracer provider failed");
        }
    }
}

fn log_rate_ceiling() -> gents::observe::log_rate::RateLimitFilter {
    gents::observe::log_rate::RateLimitFilter::new(
        gents::observe::log_rate::RateLimitConfig::default(),
    )
}

pub(crate) fn init(default_log_filter: &str) -> Result<TelemetryGuard> {
    install_trace_context_propagator();

    let env_filter = with_default_transport_noise_filters(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_log_filter)),
    );
    let native = gents::runtime::native_logging::service_logging_enabled()
        .then(|| gents::runtime::native_logging::layer("runtime"))
        .flatten();
    let console = native.is_none();

    if !otlp_enabled_from_env() {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(native.map(|layer| layer.with_filter(log_rate_ceiling())))
            .with(console.then(|| {
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(log_rate_ceiling())
            }))
            .try_init()
            .context("initializing tracing subscriber")?;
        return Ok(TelemetryGuard {
            tracer_provider: None,
        });
    }

    let service_name = resolve_service_name();
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()
        .context("building OTLP span exporter")?;
    let provider = build_tracer_provider_with_batch_exporter(&service_name, exporter);
    let tracer = provider.tracer(TRACER_NAME);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(native.map(|layer| layer.with_filter(log_rate_ceiling())))
        .with(console.then(|| {
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(log_rate_ceiling())
        }))
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .try_init()
        .context("initializing tracing subscriber")?;

    tracing::info!(
        service_name = %service_name,
        otlp_endpoint = %configured_otlp_endpoint().unwrap_or_default(),
        "OTLP trace export enabled"
    );

    Ok(TelemetryGuard {
        tracer_provider: Some(provider),
    })
}

fn install_trace_context_propagator() {
    global::set_text_map_propagator(TraceContextPropagator::new());
}

fn build_tracer_provider_with_batch_exporter<T>(
    service_name: &str,
    exporter: T,
) -> SdkTracerProvider
where
    T: SpanExporter + 'static,
{
    SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(build_resource(service_name))
        .build()
}

fn build_resource(service_name: &str) -> Resource {
    Resource::builder_empty()
        .with_service_name(service_name.to_string())
        .with_attributes([
            KeyValue::new("service.namespace", "sourcenetwork"),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new(
                "service.instance.id",
                format!("{}:{}", resolve_hostname(), std::process::id()),
            ),
        ])
        .build()
}

fn otlp_enabled_from_env() -> bool {
    configured_otlp_endpoint().is_some()
}

fn configured_otlp_endpoint() -> Option<String> {
    env_var_nonempty("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .or_else(|| env_var_nonempty("OTEL_EXPORTER_OTLP_ENDPOINT"))
}

fn resolve_service_name() -> String {
    env_var_nonempty("OTEL_SERVICE_NAME").unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string())
}

fn env_var_nonempty(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_hostname() -> String {
    hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| DEFAULT_HOSTNAME.to_string())
}
