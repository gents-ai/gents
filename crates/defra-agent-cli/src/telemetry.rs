use std::env;

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_sdk::trace::{SdkTracerProvider, SpanExporter};
use opentelemetry_sdk::Resource;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const DEFAULT_SERVICE_NAME: &str = "defra-agent";
const DEFAULT_HOSTNAME: &str = "unknown";
const TRACER_NAME: &str = "defra-agent";

pub(crate) struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
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

impl TelemetryGuard {
    pub(crate) fn shutdown(self) {
        let Some(provider) = self.tracer_provider else {
            return;
        };

        if let Err(error) = provider.force_flush() {
            eprintln!("force flushing OTLP tracer provider failed: {error}");
        }
        if let Err(error) = provider.shutdown() {
            eprintln!("shutting down OTLP tracer provider failed: {error}");
        }
    }
}

pub(crate) fn init(default_log_filter: &str) -> Result<TelemetryGuard> {
    let env_filter = with_default_transport_noise_filters(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_log_filter)),
    );

    if !otlp_enabled_from_env() {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer())
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
        .with(fmt::layer())
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

#[cfg(test)]
fn build_tracer_provider_with_simple_exporter<T>(
    service_name: &str,
    exporter: T,
) -> SdkTracerProvider
where
    T: SpanExporter + 'static,
{
    SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use opentelemetry::{Key, Value};
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::SpanData;

    #[derive(Clone, Debug, Default)]
    struct TestSpanExporter {
        spans: Arc<Mutex<Vec<SpanData>>>,
    }

    impl TestSpanExporter {
        fn spans(&self) -> Vec<SpanData> {
            self.spans
                .lock()
                .expect("test span store should lock")
                .clone()
        }
    }

    impl SpanExporter for TestSpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.spans
                .lock()
                .expect("test span store should lock")
                .extend(batch);
            Ok(())
        }

        fn shutdown_with_timeout(&mut self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }
    }

    #[test]
    fn build_resource_sets_service_metadata() {
        let resource = build_resource("telemetry-test");

        assert_eq!(
            resource.get(&Key::new("service.name")),
            Some(Value::from("telemetry-test"))
        );
        assert_eq!(
            resource.get(&Key::new("service.namespace")),
            Some(Value::from("sourcenetwork"))
        );
        assert_eq!(
            resource.get(&Key::new("service.version")),
            Some(Value::from(env!("CARGO_PKG_VERSION")))
        );
        assert!(resource.get(&Key::new("service.instance.id")).is_some());
    }

    #[test]
    fn simple_provider_exports_request_spans() {
        let exporter = TestSpanExporter::default();
        let provider =
            build_tracer_provider_with_simple_exporter("telemetry-test", exporter.clone());
        let tracer = provider.tracer("telemetry-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                "agent.request",
                request_id = "request-123",
                session_id = "session-456"
            );
            let _guard = span.enter();
            tracing::info!("processing request");
        });

        provider.force_flush().expect("force flush should succeed");
        let spans = exporter.spans();
        let span = spans
            .iter()
            .find(|span| span.name == "agent.request")
            .expect("request span should be exported");

        assert!(
            span.attributes.iter().any(|attr| {
                attr.key.as_str() == "request_id" && attr.value == Value::from("request-123")
            }),
            "request_id attribute missing"
        );
        assert!(
            span.attributes.iter().any(|attr| {
                attr.key.as_str() == "session_id" && attr.value == Value::from("session-456")
            }),
            "session_id attribute missing"
        );
    }
}
