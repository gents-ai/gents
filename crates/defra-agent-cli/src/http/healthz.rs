use defra_agent::ProcessLifecycleState;
use serde_json::{json, Value};

use crate::http::prometheus::MetricsQueryData;
use crate::http::router::RuntimeHttpState;
use crate::http::version::version_response;

const SERVICE_NAME: &str = "defra-agent";

pub(crate) fn render_healthz_payload(
    state: &RuntimeHttpState,
    data: Option<&MetricsQueryData>,
    error: Option<String>,
) -> Value {
    let version = version_response();
    let uptime_seconds = state.started_instant.elapsed().as_secs();

    match data {
        Some(data) => {
            let runtime_ready = data
                .agent_runtimes
                .iter()
                .any(|runtime| runtime.process_state == ProcessLifecycleState::Ready.as_str());
            let runtime_degraded = data
                .agent_runtimes
                .iter()
                .any(|runtime| runtime.unavailable_behavior_count > 0);
            let backend_degraded = data
                .inference_backends
                .iter()
                .any(|backend| backend.enabled && backend.probe_status != "healthy");
            let ok = runtime_ready;
            let status = if !runtime_ready {
                "unhealthy"
            } else if runtime_degraded || backend_degraded {
                "degraded"
            } else {
                "ok"
            };
            let runtime_status = if runtime_ready {
                if runtime_degraded {
                    "degraded"
                } else {
                    "ok"
                }
            } else {
                "unhealthy"
            };
            let backend_status = if backend_degraded { "degraded" } else { "ok" };

            json!({
                "status": status,
                "ok": ok,
                "service": SERVICE_NAME,
                "version": version.version,
                "started_at": state.started_at,
                "uptime_seconds": uptime_seconds,
                "checks": {
                    "http": {
                        "status": "ok",
                    },
                    "graphql": {
                        "status": "ok",
                        "endpoint": state.graphql,
                    },
                    "runtime": {
                        "status": runtime_status,
                        "ready": runtime_ready,
                        "count": data.agent_runtimes.len(),
                    },
                    "backends": {
                        "status": backend_status,
                        "count": data.inference_backends.len(),
                    },
                },
                "runtimes": data.agent_runtimes,
                "backends": data.inference_backends,
            })
        }
        None => json!({
            "status": "unhealthy",
            "ok": false,
            "service": SERVICE_NAME,
            "version": version.version,
            "started_at": state.started_at,
            "uptime_seconds": uptime_seconds,
            "checks": {
                "http": {
                    "status": "ok",
                },
                "graphql": {
                    "status": "unhealthy",
                    "endpoint": state.graphql,
                    "error": error.unwrap_or_else(|| "runtime GraphQL status unavailable".to_string()),
                },
                "runtime": {
                    "status": "unknown",
                    "ready": false,
                    "count": 0,
                },
                "backends": {
                    "status": "unknown",
                    "count": 0,
                },
            },
            "runtimes": [],
            "backends": [],
        }),
    }
}
