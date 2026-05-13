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
            let liveness_degraded = data.liveness.expired_processing_count > 0;
            let ok = runtime_ready;
            let status = if !runtime_ready {
                "unhealthy"
            } else if runtime_degraded || backend_degraded || liveness_degraded {
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
            let liveness_status = if liveness_degraded { "degraded" } else { "ok" };

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
                    "liveness": {
                        "status": liveness_status,
                        "active_request_count": data.liveness.active_request_ids.len(),
                        "active_tool_call_count": data.liveness.active_tool_calls.len(),
                        "expired_processing_count": data.liveness.expired_processing_count,
                    },
                },
                "runtimes": data.agent_runtimes,
                "backends": data.inference_backends,
                "liveness": data.liveness,
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::http::liveness::{ActiveRequest, ActiveToolCall, RuntimeLivenessSnapshot};
    use crate::http::prometheus::{MetricsBackendRow, MetricsQueryData, MetricsRuntimeRow};

    fn state() -> RuntimeHttpState {
        RuntimeHttpState {
            graphql: "http://localhost:9181/api/v0/graphql".to_string(),
            agent_name: "test-agent".to_string(),
            agent_did: "did:defra-agent:test".to_string(),
            started_at: "2026-05-13T12:00:00Z".to_string(),
            started_instant: Instant::now(),
        }
    }

    fn ready_runtime() -> MetricsRuntimeRow {
        MetricsRuntimeRow {
            agent_did: "did:defra-agent:test".to_string(),
            process_state: defra_agent::ProcessLifecycleState::Ready
                .as_str()
                .to_string(),
            reconcile_phase: "idle".to_string(),
            active_generation: 1,
            router_generation: 1,
            runnable_behavior_count: 1,
            unavailable_behavior_count: 0,
            last_reconcile_result: "applied".to_string(),
            last_reconcile_completed_at: "2026-05-13T11:59:00Z".to_string(),
        }
    }

    fn healthy_backend() -> MetricsBackendRow {
        MetricsBackendRow {
            backend_id: "backend-1".to_string(),
            enabled: true,
            max_concurrent: 1,
            max_queue_depth: 4,
            probe_status: "healthy".to_string(),
            last_probe: Some("2026-05-13T11:59:30Z".to_string()),
        }
    }

    #[test]
    fn healthz_reports_degraded_when_expired_processing_count_positive() {
        let data = MetricsQueryData {
            agent_runtimes: vec![ready_runtime()],
            inference_backends: vec![healthy_backend()],
            liveness: RuntimeLivenessSnapshot {
                active_request_ids: vec!["req-stuck".to_string()],
                expired_processing_count: 1,
                requests: vec![ActiveRequest {
                    request_id: "req-stuck".to_string(),
                    claimed_at: Some("2026-05-13T11:55:00Z".to_string()),
                    deadline: Some("2026-05-13T11:59:00Z".to_string()),
                    deadline_expired: true,
                    deadline_age_ms: Some(60_000),
                    last_progress_age_ms: 300_000,
                    subagent_depth: 0,
                    caused_by_parent_request_id: None,
                    caused_by_trigger_kind: None,
                }],
                active_tool_calls: vec![ActiveToolCall {
                    request_id: "req-stuck".to_string(),
                    tool_call_id: "tc-1".to_string(),
                    tool_name: "glob".to_string(),
                    started_at: Some("2026-05-13T11:55:30Z".to_string()),
                    deadline_at: Some("2026-05-13T11:58:00Z".to_string()),
                    await_mode: None,
                    running_age_ms: 270_000,
                    deadline_expired: true,
                }],
            },
        };

        let payload = render_healthz_payload(&state(), Some(&data), None);

        assert_eq!(
            payload.get("status").and_then(|v| v.as_str()),
            Some("degraded"),
            "expired processing must downgrade /healthz to degraded; payload was {payload}"
        );
        assert_eq!(
            payload.get("ok").and_then(|v| v.as_bool()),
            Some(true),
            "degraded must keep ok=true so /healthz stays 200 OK"
        );
        let liveness_check = payload
            .pointer("/checks/liveness")
            .expect("checks.liveness must be present");
        assert_eq!(
            liveness_check.get("status").and_then(|v| v.as_str()),
            Some("degraded")
        );
        assert_eq!(
            liveness_check
                .get("expired_processing_count")
                .and_then(|v| v.as_i64()),
            Some(1)
        );
    }

    #[test]
    fn healthz_stays_ok_when_no_expired_processing() {
        let data = MetricsQueryData {
            agent_runtimes: vec![ready_runtime()],
            inference_backends: vec![healthy_backend()],
            liveness: RuntimeLivenessSnapshot::default(),
        };

        let payload = render_healthz_payload(&state(), Some(&data), None);

        assert_eq!(payload.get("status").and_then(|v| v.as_str()), Some("ok"));
    }
}
