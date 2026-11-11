use rapidhash::{HashMapExt, RapidHashMap};

use opentelemetry::global;
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub fn current_trace_context_headers() -> RapidHashMap<String, String> {
    let context = tracing::Span::current().context();
    let mut headers = RapidHashMap::new();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut headers);
    });
    headers.retain(|name, value| !name.trim().is_empty() && !value.trim().is_empty());
    headers
}
