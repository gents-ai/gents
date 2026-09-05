use gents_protocol::row::{
    project_behavior_readiness_summary, BehaviorReadinessUnavailableReason,
    ProjectedBehaviorReadinessSummary,
};
use serde_json::{json, Value};

use crate::http::prometheus::MetricsQueryData;
use crate::http::router::RuntimeHttpState;
use crate::http::version::version_response;

const SERVICE_NAME: &str = "gents";

pub(crate) fn render_healthz_payload(
    state: &RuntimeHttpState,
    data: Option<&MetricsQueryData>,
    error: Option<String>,
) -> Value {
    render_healthz_payload_at(state, data, error, chrono::Utc::now())
}

fn render_healthz_payload_at(
    state: &RuntimeHttpState,
    data: Option<&MetricsQueryData>,
    error: Option<String>,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> Value {
    let version = version_response();
    let uptime_seconds = state.started_instant.elapsed().as_secs();

    match data {
        Some(data) => {
            let local_runtime = data
                .agent_runtimes
                .iter()
                .find(|runtime| runtime.agent_did == state.agent_did);
            let local_readiness_row = data
                .behavior_readiness
                .iter()
                .find(|row| row.agent_did == state.agent_did);
            let readiness = project_behavior_readiness_summary(
                local_readiness_row,
                &state.agent_did,
                observed_at,
            );
            let runtime_ready =
                matches!(&readiness, ProjectedBehaviorReadinessSummary::Observed(_));
            let runtime_degraded = match &readiness {
                ProjectedBehaviorReadinessSummary::Observed(summary) => {
                    !summary.unavailable_behaviors.is_empty()
                }
                ProjectedBehaviorReadinessSummary::Unknown(_) => true,
            };
            // Measured backend health is never persisted to
            // `InferenceBackend` (#640; see `backend_health.rs`), so this
            // out-of-process reader can't compute degradation from that
            // document's `enabled`/`probe_status` fields — only from the
            // readiness projection this runtime already published above.
            let backend_degraded = match &readiness {
                ProjectedBehaviorReadinessSummary::Observed(summary) => {
                    summary.unavailable_behaviors.values().any(|reason| {
                        matches!(
                            reason,
                            BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable
                                | BehaviorReadinessUnavailableReason::BackendDisabled
                        )
                    })
                }
                ProjectedBehaviorReadinessSummary::Unknown(_) => false,
            };
            let liveness_degraded = data.liveness.expired_processing_count > 0;
            let codex_shim = state
                .codex_shim_health
                .as_ref()
                .and_then(|handle| handle.read().ok().map(|health| health.clone()));
            let codex_shim_degraded = codex_shim
                .as_ref()
                .is_some_and(|health| health.is_degraded());
            let ok = runtime_ready;
            let status = if !runtime_ready {
                "unhealthy"
            } else if runtime_degraded
                || backend_degraded
                || liveness_degraded
                || codex_shim_degraded
            {
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

            let mut checks = json!({
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
                        "count": usize::from(local_readiness_row.is_some()),
                    },
                    "backends": {
                        "status": backend_status,
                        "count": data.inference_backends.len(),
                    },
                    "liveness": {
                        "status": liveness_status,
                        "active_request_count": data.liveness.active_request_ids.len(),
                        "active_tool_call_count": data.liveness.active_tool_calls.len(),
                        "active_native_executors_available": data.liveness.active_native_executors_available,
                        "active_native_executor_count": data.liveness.active_native_executors.len(),
                        "expired_processing_count": data.liveness.expired_processing_count,
                        "ignored_foreign_processing_count": data.liveness.ignored_foreign_processing_count,
                        "ignored_foreign_tool_call_count": data.liveness.ignored_foreign_tool_call_count,
                    },
            });
            if let Some(health) = codex_shim.as_ref() {
                checks["codex_shim"] = health.to_json();
            }

            json!({
                "status": status,
                "ok": ok,
                "service": SERVICE_NAME,
                "version": version.version,
                "started_at": state.started_at,
                "uptime_seconds": uptime_seconds,
                "checks": checks,
                "runtimes": local_runtime.into_iter().collect::<Vec<_>>(),
                "behavior_readiness": local_readiness_row.into_iter().collect::<Vec<_>>(),
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
            "behavior_readiness": [],
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
    use gents_protocol::row::{
        AgentBehaviorReadinessRow, BehaviorReadinessEntry, BehaviorReadinessProcessState,
        BehaviorReadinessSnapshot, BehaviorReadinessState, BEHAVIOR_READINESS_FORMAT_VERSION,
    };

    fn state() -> RuntimeHttpState {
        RuntimeHttpState {
            graphql: "http://localhost:9181/api/v0/graphql".to_string(),
            agent_name: "test-agent".to_string(),
            agent_did: "did:test:test".to_string(),
            started_at: "2026-05-13T12:00:00Z".to_string(),
            started_instant: Instant::now(),
            backend_health: None,
            p2p_admission: None,
            p2p_metrics_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            p2p_http_client: reqwest::Client::new(),
            codex_shim_health: None,
            enrollment_offer_issuer: crate::http::enrollment::empty_issuer_handle(),
            enrollment_decisions: crate::http::enrollment::empty_decision_service_handle(),
        }
    }

    fn ready_runtime() -> MetricsRuntimeRow {
        MetricsRuntimeRow {
            agent_did: "did:test:test".to_string(),
            reconcile_phase: "idle".to_string(),
            last_reconcile_result: "applied".to_string(),
            last_reconcile_completed_at: "2026-05-13T11:59:00Z".to_string(),
        }
    }

    fn ready_readiness() -> AgentBehaviorReadinessRow {
        AgentBehaviorReadinessRow {
            agent_did: "did:test:test".to_string(),
            snapshot_json: serde_json::to_string(&BehaviorReadinessSnapshot {
                format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
                process_state: BehaviorReadinessProcessState::Ready,
                active_generation: 1,
                router_generation: 1,
                default_behavior_id: "default".to_string(),
                behaviors: vec![BehaviorReadinessEntry {
                    behavior_id: "default".to_string(),
                    state: BehaviorReadinessState::Ready,
                    reason: None,
                }],
            })
            .unwrap(),
            updated_at: "2026-05-13T11:59:30Z".to_string(),
        }
    }

    fn observed_at() -> chrono::DateTime<chrono::Utc> {
        "2026-05-13T12:00:00Z"
            .parse()
            .expect("fixed health observation timestamp must parse")
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
            behavior_readiness: vec![ready_readiness()],
            inference_backends: vec![healthy_backend()],
            liveness: RuntimeLivenessSnapshot {
                active_request_ids: vec!["req-stuck".to_string()],
                expired_processing_count: 1,
                ignored_foreign_processing_count: 0,
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
                ignored_foreign_tool_call_count: 0,
                active_native_executors_available: true,
                active_native_executors: Vec::new(),
            },
        };

        let payload = render_healthz_payload_at(&state(), Some(&data), None, observed_at());

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
    fn healthz_reports_backend_degraded_from_readiness_not_backend_document() {
        // The `InferenceBackend` row itself is healthy — this runtime's
        // *measured* health (#640, never persisted to that document) is
        // what vetoed the behavior, and only the readiness projection
        // carries that signal.
        let vetoed_readiness = AgentBehaviorReadinessRow {
            agent_did: "did:test:test".to_string(),
            snapshot_json: serde_json::to_string(&BehaviorReadinessSnapshot {
                format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
                process_state: BehaviorReadinessProcessState::Ready,
                active_generation: 1,
                router_generation: 1,
                default_behavior_id: "default".to_string(),
                behaviors: vec![BehaviorReadinessEntry {
                    behavior_id: "default".to_string(),
                    state: BehaviorReadinessState::Unavailable,
                    reason: Some(
                        gents_protocol::row::BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable,
                    ),
                }],
            })
            .unwrap(),
            updated_at: "2026-05-13T11:59:30Z".to_string(),
        };
        let data = MetricsQueryData {
            agent_runtimes: vec![ready_runtime()],
            behavior_readiness: vec![vetoed_readiness],
            inference_backends: vec![healthy_backend()],
            liveness: RuntimeLivenessSnapshot::default(),
        };

        let payload = render_healthz_payload_at(&state(), Some(&data), None, observed_at());

        assert_eq!(
            payload
                .pointer("/checks/backends/status")
                .and_then(Value::as_str),
            Some("degraded"),
            "a readiness-reported backend veto must degrade the backends check even though \
             the InferenceBackend document itself reports healthy: {payload}"
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some("degraded")
        );
    }

    #[test]
    fn healthz_stays_ok_when_no_expired_processing() {
        let data = MetricsQueryData {
            agent_runtimes: vec![ready_runtime()],
            behavior_readiness: vec![ready_readiness()],
            inference_backends: vec![healthy_backend()],
            liveness: RuntimeLivenessSnapshot::default(),
        };

        let payload = render_healthz_payload_at(&state(), Some(&data), None, observed_at());

        assert_eq!(payload.get("status").and_then(|v| v.as_str()), Some("ok"));
    }

    #[test]
    fn healthz_fails_closed_when_readiness_is_missing_or_malformed() {
        let mut missing = healthy_data();
        missing.behavior_readiness.clear();
        let payload = render_healthz_payload_at(&state(), Some(&missing), None, observed_at());
        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some("unhealthy")
        );
        assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(
            payload
                .pointer("/checks/runtime/count")
                .and_then(Value::as_u64),
            Some(0),
            "diagnostic AgentRuntime rows must not create readiness inventory"
        );

        let mut malformed = healthy_data();
        malformed.behavior_readiness[0].snapshot_json = "{}".to_string();
        let payload = render_healthz_payload_at(&state(), Some(&malformed), None, observed_at());
        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some("unhealthy")
        );
        assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn healthz_fails_closed_when_readiness_is_stale() {
        let mut data = healthy_data();
        data.behavior_readiness[0].updated_at = "2026-05-13T11:59:00Z".to_string();

        let payload = render_healthz_payload_at(&state(), Some(&data), None, observed_at());

        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some("unhealthy")
        );
        assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn readiness_is_runtime_inventory_when_diagnostics_are_absent() {
        let mut data = healthy_data();
        data.agent_runtimes.clear();

        let payload = render_healthz_payload_at(&state(), Some(&data), None, observed_at());

        assert_eq!(payload.get("status").and_then(Value::as_str), Some("ok"));
        assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            payload
                .pointer("/checks/runtime/count")
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            payload
                .get("runtimes")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0),
            "AgentRuntime remains optional diagnostics"
        );
    }

    #[test]
    fn unrelated_ready_agent_cannot_mask_missing_local_readiness() {
        let mut data = healthy_data();
        data.agent_runtimes[0].agent_did = "did:test:foreign".to_string();
        data.behavior_readiness[0].agent_did = "did:test:foreign".to_string();

        let payload = render_healthz_payload_at(&state(), Some(&data), None, observed_at());

        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some("unhealthy")
        );
        assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(
            payload
                .pointer("/checks/runtime/count")
                .and_then(Value::as_u64),
            Some(0)
        );
        assert_eq!(
            payload
                .get("runtimes")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        assert_eq!(
            payload
                .get("behavior_readiness")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    fn state_with_shim(health: crate::shared::CodexShimHealth) -> RuntimeHttpState {
        let mut state = state();
        state.codex_shim_health = Some(std::sync::Arc::new(std::sync::RwLock::new(health)));
        state
    }

    fn healthy_data() -> MetricsQueryData {
        MetricsQueryData {
            agent_runtimes: vec![ready_runtime()],
            behavior_readiness: vec![ready_readiness()],
            inference_backends: vec![healthy_backend()],
            liveness: RuntimeLivenessSnapshot::default(),
        }
    }

    #[test]
    fn healthz_reports_a_pending_shim_as_degraded() {
        let state = state_with_shim(crate::shared::CodexShimHealth::Pending {
            bound_behavior_id: "default".to_string(),
            reason: "no AgentBehavior document with that behavior_id exists".to_string(),
        });
        let payload = render_healthz_payload_at(&state, Some(&healthy_data()), None, observed_at());

        assert_eq!(
            payload
                .pointer("/checks/codex_shim/status")
                .and_then(Value::as_str),
            Some("pending"),
            "a shim waiting for its behavior must be visible in /healthz; payload was {payload}"
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some("degraded"),
            "a closed shim port must not read as fully healthy; payload was {payload}"
        );
        assert_eq!(
            payload.get("ok").and_then(Value::as_bool),
            Some(true),
            "the runtime is still serving, so /healthz stays 200 OK"
        );
    }

    #[test]
    fn healthz_reports_a_listening_shim_as_ok() {
        let state = state_with_shim(crate::shared::CodexShimHealth::Listening {
            websocket: "ws://127.0.0.1:9292/".to_string(),
            auth_required: true,
            bound_agent_did: "did:key:agent".to_string(),
            bound_behavior_id: "did:key:agent:default".to_string(),
        });
        let payload = render_healthz_payload_at(&state, Some(&healthy_data()), None, observed_at());

        assert_eq!(
            payload
                .pointer("/checks/codex_shim/status")
                .and_then(Value::as_str),
            Some("ok")
        );
        assert_eq!(
            payload
                .pointer("/checks/codex_shim/auth_required")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            payload
                .pointer("/checks/codex_shim/bound_agent_did")
                .and_then(Value::as_str),
            Some("did:key:agent")
        );
        assert_eq!(payload.get("status").and_then(Value::as_str), Some("ok"));
    }

    #[test]
    fn healthz_does_not_degrade_when_the_shim_is_switched_off() {
        let state = state_with_shim(crate::shared::CodexShimHealth::Off);
        let payload = render_healthz_payload_at(&state, Some(&healthy_data()), None, observed_at());

        assert_eq!(
            payload
                .pointer("/checks/codex_shim/status")
                .and_then(Value::as_str),
            Some("off")
        );
        assert_eq!(payload.get("status").and_then(Value::as_str), Some("ok"));
    }
}
