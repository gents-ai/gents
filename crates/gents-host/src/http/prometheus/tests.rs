use chrono::{DateTime, Duration, Utc};
use gents_protocol::row::NodeReadinessRow;
use gents_protocol::usage::TokenUsageColumns;
use rapidhash::RapidHashMap;

use super::backends::{push_backend_metric, push_backend_metrics};
use super::background::render_background_completion_metrics;
use super::exposition::{push_metric_sample, rfc3339_timestamp};
use super::inference::{
    build_inference_metric_families, render_inference_metrics, InferenceMetricsQueryData,
    InferenceRequestMetricKey, InferenceTokenMetricKey, INFERENCE_METRICS_WINDOW_SECS,
};
use super::inference_query::{
    inference_metrics_ended_lower_bound, inference_metrics_query, InferenceAgentRow,
    InferenceCallMetricRow, InferenceMetricsPageData, InferenceNodeRow,
};
use super::p2p::{render_p2p_metrics, P2pMetricsSnapshot};
use super::p2p_sync;
use super::render_command_policy_metrics;
use super::runtime::push_node_readiness_metrics_at;
use super::store::{
    load_metrics_query_data, MetricsBackendRow, MetricsQueryEnvelope, MetricsRuntimeRow,
};

#[test]
fn command_policy_metrics_expose_process_counts() {
    let mut lines = Vec::new();
    render_command_policy_metrics(
        &mut lines,
        gents_tools::native::command_observability::CommandPolicyMetrics {
            policy_rejections: 17,
            credential_inspection_failures: 5,
        },
    );
    let rendered = lines.join("\n");
    assert!(rendered.contains("# TYPE gents_command_policy_rejections_total counter"));
    assert!(rendered.contains("gents_command_policy_rejections_total 17"));
    assert!(rendered.contains("# TYPE gents_command_credential_inspection_failures_total counter"));
    assert!(rendered.contains("gents_command_credential_inspection_failures_total 5"));
}

#[test]
fn backend_capacity_metrics_resolve_nullable_config_through_backend_defaults() {
    use gents_documents::inference_defaults::{DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_QUEUE_DEPTH};

    for (limits, concurrency, queue_depth) in [
        (
            serde_json::json!({}),
            DEFAULT_MAX_CONCURRENT,
            DEFAULT_MAX_QUEUE_DEPTH,
        ),
        (
            serde_json::json!({"max_concurrent": null, "max_queue_depth": null}),
            DEFAULT_MAX_CONCURRENT,
            DEFAULT_MAX_QUEUE_DEPTH,
        ),
        (
            serde_json::json!({"max_concurrent": 7, "max_queue_depth": 0}),
            7,
            0,
        ),
    ] {
        let mut backend = serde_json::json!({
            "backend_id": "backend", "enabled": true, "probe_status": null
        });
        backend
            .as_object_mut()
            .unwrap()
            .extend(limits.as_object().unwrap().clone());
        let data: MetricsQueryEnvelope = serde_json::from_value(serde_json::json!({
            "InferenceBackendMetadata": [backend]
        }))
        .unwrap();
        let mut lines = Vec::new();
        push_backend_metrics(
            &mut lines,
            &data.inference_backends,
            &RapidHashMap::default(),
        );
        assert!(lines.contains(&format!(
            "gents_backend_max_concurrent{{backend_id=\"backend\"}} {concurrency}"
        )));
        assert!(lines.contains(&format!(
            "gents_backend_max_queue_depth{{backend_id=\"backend\"}} {queue_depth}"
        )));
    }
}

#[tokio::test]
async fn failed_liveness_activity_read_degrades_to_claimed_at() {
    let claimed_at = (Utc::now() - Duration::seconds(120)).to_rfc3339();
    let database = gents_store::test_support::wire_database::WireDatabase::spawn(
        serde_json::json!({
            "AgentRequest": [{
                "_docID": "doc-req-1",
                "request_id": "req-1",
                "node_did": "did:test:local",
                "lifecycle_state": "processing",
                "claimed_at": claimed_at,
            }],
        }),
        std::sync::Arc::new(|collection| {
            Box::pin(async move {
                if collection == "InferenceCall" {
                    Err(axum::http::StatusCode::BAD_REQUEST)
                } else {
                    Ok(())
                }
            })
        }),
    )
    .await;

    let data = load_metrics_query_data(&database.reader(), "did:test:local")
        .await
        .expect("an activity read failure must not fail the liveness owner");
    let request = &data.liveness.requests[0];
    assert_eq!(request.request_id, "req-1");
    assert!(
        (120_000..180_000).contains(&request.last_progress_age_ms),
        "progress falls back to claimed_at: {}",
        request.last_progress_age_ms
    );
}

use gents_protocol::row::{
    AgentReadinessEntry, AgentReadinessState, AgentReadinessUnavailableReason,
    NodeReadinessProcessState, NodeReadinessSnapshot, NODE_READINESS_FORMAT_VERSION,
};

fn metrics_runtime(node_did: &str) -> MetricsRuntimeRow {
    MetricsRuntimeRow {
        node_did: node_did.to_string(),
        reconcile_phase: "idle".to_string(),
        last_reconcile_result: "applied".to_string(),
        last_reconcile_completed_at: "2026-08-29T00:00:00Z".to_string(),
    }
}

fn metrics_readiness(node_did: &str) -> NodeReadinessRow {
    NodeReadinessRow {
        node_did: node_did.to_string(),
        snapshot_json: serde_json::to_string(&NodeReadinessSnapshot {
            format_version: NODE_READINESS_FORMAT_VERSION,
            process_state: NodeReadinessProcessState::Ready,
            active_generation: 4,
            router_generation: 4,
            default_agent_did: "default".to_string(),
            agents: vec![
                AgentReadinessEntry {
                    agent_did: "default".to_string(),
                    state: AgentReadinessState::Ready,
                    reason: None,
                    effective_inference: None,
                },
                AgentReadinessEntry {
                    agent_did: "offline".to_string(),
                    state: AgentReadinessState::Unavailable,
                    reason: Some(AgentReadinessUnavailableReason::BackendTemporarilyUnavailable),
                    effective_inference: None,
                },
            ],
        })
        .unwrap(),
        updated_at: "2026-08-29T00:00:00Z".to_string(),
    }
}

fn readiness_observed_at() -> chrono::DateTime<chrono::Utc> {
    "2026-08-29T00:00:30Z"
        .parse()
        .expect("fixed metrics observation timestamp must parse")
}

#[test]
fn node_readiness_metrics_distinguish_observed_from_unknown() {
    let runtime = metrics_runtime("did:test:metrics");
    let mut lines = Vec::new();
    push_node_readiness_metrics_at(
        &mut lines,
        &runtime.node_did,
        &[metrics_readiness("did:test:metrics")],
        readiness_observed_at(),
    );
    let rendered = lines.join("\n");
    assert!(rendered.contains(r#"gents_runtime_runnable_agents{node_did="did:test:metrics"} 1"#));
    assert!(rendered.contains(r#"gents_runtime_unavailable_agents{node_did="did:test:metrics"} 1"#));
    assert!(rendered
        .contains(r#"gents_runtime_node_readiness_observed{node_did="did:test:metrics"} 1"#));

    for rows in [
        Vec::new(),
        vec![NodeReadinessRow {
            snapshot_json: "{}".to_string(),
            ..metrics_readiness("did:test:metrics")
        }],
    ] {
        let mut lines = Vec::new();
        push_node_readiness_metrics_at(
            &mut lines,
            &runtime.node_did,
            &rows,
            readiness_observed_at(),
        );
        let rendered = lines.join("\n");
        assert!(
            rendered.contains(r#"gents_runtime_runnable_agents{node_did="did:test:metrics"} 0"#)
        );
        assert!(
            rendered.contains(r#"gents_runtime_unavailable_agents{node_did="did:test:metrics"} 0"#)
        );
        assert!(rendered
            .contains(r#"gents_runtime_node_readiness_observed{node_did="did:test:metrics"} 0"#));
    }
}

#[test]
fn backend_probe_status_metric_reflects_measured_health() {
    fn doc_row(
        backend_id: &str,
        probe_status: &str,
        last_probe: Option<&str>,
    ) -> MetricsBackendRow {
        MetricsBackendRow {
            backend_id: backend_id.to_string(),
            enabled: true,
            max_concurrent: Some(4),
            max_queue_depth: Some(100),
            probe_status: probe_status.to_string(),
            last_probe: last_probe.map(str::to_string),
        }
    }
    fn measured_entry(
        state: gents_inference::backends::health::BackendHealthState,
        last_probe_at: &str,
    ) -> (gents_inference::backends::health::BackendHealthState, i64) {
        (
            state,
            chrono::DateTime::parse_from_rfc3339(last_probe_at)
                .unwrap()
                .timestamp(),
        )
    }

    let backends = vec![
        doc_row("sparks-cluster", "healthy", None),
        doc_row("workstation-1", "healthy", Some("2026-07-01T00:00:00Z")),
        doc_row("spark-2", "healthy", None),
        doc_row("codex", "healthy", Some("2026-07-06T00:00:00Z")),
        doc_row("unprobed-unknown", "unknown", None),
    ];
    let measured = RapidHashMap::from_iter([
        (
            "sparks-cluster".to_string(),
            measured_entry(
                gents_inference::backends::health::BackendHealthState::Unhealthy,
                "2026-07-07T12:00:00Z",
            ),
        ),
        (
            "workstation-1".to_string(),
            measured_entry(
                gents_inference::backends::health::BackendHealthState::Healthy,
                "2026-07-07T12:00:00Z",
            ),
        ),
        (
            "spark-2".to_string(),
            measured_entry(
                gents_inference::backends::health::BackendHealthState::Degraded,
                "2026-07-07T12:00:00Z",
            ),
        ),
    ]);

    let mut lines = Vec::new();
    for backend in &backends {
        push_backend_metric(
            &mut lines,
            backend,
            measured.get(&backend.backend_id).copied(),
        );
    }
    let rendered = lines.join("\n");

    let measured_last_probe = chrono::DateTime::parse_from_rfc3339("2026-07-07T12:00:00Z")
        .unwrap()
        .timestamp();
    assert!(rendered.contains(
        r#"gents_backend_probe_status{backend_id="sparks-cluster",status="unhealthy"} 0"#
    ));
    assert!(rendered.contains(&format!(
        r#"gents_backend_last_probe_seconds{{backend_id="sparks-cluster"}} {measured_last_probe}"#
    )));
    assert!(rendered
        .contains(r#"gents_backend_probe_status{backend_id="workstation-1",status="healthy"} 1"#));
    assert!(rendered.contains(&format!(
        r#"gents_backend_last_probe_seconds{{backend_id="workstation-1"}} {measured_last_probe}"#
    )));
    assert!(rendered
        .contains(r#"gents_backend_probe_status{backend_id="spark-2",status="degraded"} 0"#));
    assert!(
        rendered.contains(r#"gents_backend_probe_status{backend_id="codex",status="healthy"} 1"#)
    );
    let doc_last_probe = chrono::DateTime::parse_from_rfc3339("2026-07-06T00:00:00Z")
        .unwrap()
        .timestamp();
    assert!(rendered.contains(&format!(
        r#"gents_backend_last_probe_seconds{{backend_id="codex"}} {doc_last_probe}"#
    )));
    assert!(rendered.contains(
        r#"gents_backend_probe_status{backend_id="unprobed-unknown",status="unknown"} 0"#
    ));
    assert!(
        !rendered.contains(r#"gents_backend_last_probe_seconds{backend_id="unprobed-unknown"}"#)
    );
}

#[test]
fn background_completion_metrics_surface_pending_and_stranded_delivery() {
    let mut lines = Vec::new();
    render_background_completion_metrics(
        &mut lines,
        "did:agent:amy",
        Some(
            &gents_toolcalls::background::diagnostics::BackgroundCompletionDiagnostics {
                scanned_wakes: 1,
                scanned_notifications: 10,
                scan_truncated: false,
                pending_notifications: 3,
                acknowledged_notifications: 7,
                stranded_notifications: 2,
                oldest_pending_age_seconds: Some(41),
                epochs: vec![
                    gents_toolcalls::background::diagnostics::BackgroundCompletionEpochDiagnostic {
                        root_request_id: "wake-1".to_string(),
                        active_request_id: "wake-1".to_string(),
                        session_id: "session-1".to_string(),
                        coalescing_key: "background_completion:parent".to_string(),
                        state: "exhausted".to_string(),
                        attempt_count: 4,
                        retry_count: 3,
                        max_retries: 3,
                        input_through_sequence: Some(12),
                        notification_count: 2,
                        acknowledged_notification_count: 0,
                        attempted_notification_keys: vec!["notification-1".to_string()],
                        acknowledged_notification_keys: Vec::new(),
                        pending_notification_keys: vec!["notification-1".to_string()],
                        pending_age_seconds: Some(41),
                        last_failure: Some("provider failed".to_string()),
                        next_retry_at: None,
                        created_at: None,
                        claimed_at: None,
                        terminalized_at: None,
                    },
                ],
            },
        ),
    );
    let rendered = lines.join("\n");
    assert!(rendered.contains(
        r#"gents_background_completion_pending_notifications{node_did="did:agent:amy"} 3"#
    ));
    assert!(rendered.contains(
        r#"gents_background_completion_stranded_notifications{node_did="did:agent:amy"} 2"#
    ));
    assert!(rendered.contains(
        r#"gents_background_completion_epochs{node_did="did:agent:amy",state="exhausted"} 1"#
    ));
}

#[test]
fn p2p_metrics_render_configured_admission_and_live_counts() {
    let mut lines = Vec::new();
    render_p2p_metrics(
        &mut lines,
        Some(&P2pMetricsSnapshot {
            enabled: true,
            connected_peers: 3,
            replicators: 2,
            admission: Some(gents_service::state::P2pAdmissionState {
                max_pending_dags: 1000,
                max_concurrent_push_tasks: 8,
                max_concurrent_dag_fetches: 4,
                rate_limit_burst: 500,
                rate_limit_rate: 50.0,
            }),
            sync_status: Some(gents_p2p::p2p::observability::P2pSyncStatusSnapshot {
                push_backlog: gents_p2p::p2p::observability::P2pPushBacklogSnapshot {
                    queue_item_capacity: 128,
                    queue_byte_capacity: 1_048_576,
                    per_peer_active_cap: 2,
                    worker_count: 8,
                    queued_items: 7,
                    queued_bytes: 4_096,
                    active_jobs: 3,
                    enqueued_total: 101,
                    coalesced_total: 11,
                    rejected_items_total: 5,
                    rejected_bytes_total: 2,
                    completed_total: 79,
                    failed_total: 4,
                    stale_head_retirements_total: 17,
                    peer_capacity_parks_total: 13,
                    per_peer: vec![gents_p2p::p2p::observability::P2pPeerBacklogSnapshot {
                        peer_id: "peer-a".to_string(),
                        queued_items: 4,
                        queued_bytes: 2_048,
                        active_jobs: 1,
                        consecutive_failures: 3,
                        cooldown_remaining_ms: 750,
                    }],
                    ..Default::default()
                },
                broadcast_coalesced_total: 41,
                push_updates_coalesced_total: 43,
                gossip_direction_filtered_total: 47,
                pending_dags: 13,
                pending_dag_capacity: 1_000,
                persisted_pending_dags: 17,
                persisted_pending_dag_capacity: 4_000,
                pending_resync_in_flight: true,
                retained_background_tasks: 6,
                missing_link_retries: 23,
                pending_dag_resolved: 29,
                pending_dag_expired: 31,
                single_flight_suppressed: 37,
                already_merged_fast_path: 53,
                pending_dag_capacity_shed: 59,
                pending_dag_retry_dispatched: 61,
                pending_dag_retry_suppressed: 67,
                pending_dag_fetch_deferred_unavailable: 69,
                pending_dag_fetch_deferred_contention: 68,
                pending_dag_fetch_deferred_busy: 76,
                next_pending_retry_in_ms: Some(71),
                pending_dag_terminal_quarantined: 73,
                quarantined_pending_dags: 79,
                ..Default::default()
            }),
            stale: false,
            peer_id: Some("1dd587be0123456789".to_string()),
            transport: Some("iroh".to_string()),
        }),
    );
    let rendered = lines.join("\n");
    assert!(rendered.contains(
        r#"gents_p2p_self_info{peer_id="1dd587be0123456789",peer="1dd587be",transport="iroh"} 1"#
    ));
    assert_valid_exposition(&rendered, false);
    assert!(rendered.contains("gents_p2p_enabled 1"));
    assert!(rendered.contains("gents_p2p_status_stale 0"));
    assert!(rendered.contains("gents_p2p_connected_peers 3"));
    assert!(rendered.contains("gents_p2p_replicators 2"));
    assert!(rendered.contains("gents_p2p_admission_max_pending_dags 1000"));
    assert!(rendered.contains("gents_p2p_admission_max_concurrent_push_tasks 8"));
    assert!(rendered.contains("gents_p2p_admission_max_concurrent_dag_fetches 4"));
    assert!(rendered.contains("gents_p2p_admission_rate_limit_burst 500"));
    assert!(rendered.contains("gents_p2p_admission_rate_limit_rate 50"));
    assert!(rendered.contains("gents_p2p_sync_status_available 1"));
    assert!(rendered.contains("gents_p2p_push_queue_items 7"));
    assert!(rendered.contains("gents_p2p_push_queue_bytes 4096"));
    assert!(rendered.contains("gents_p2p_push_active_jobs 3"));
    assert!(rendered.contains("gents_p2p_pending_dags 13"));
    assert!(rendered.contains("gents_p2p_persisted_pending_dags 17"));
    assert!(rendered.contains("gents_p2p_quarantined_pending_dags 79"));
    assert!(rendered.contains("gents_p2p_push_rejected_items_total 5"));
    assert!(rendered.contains("gents_p2p_push_stale_head_retirements_total 17"));
    assert!(rendered.contains("gents_p2p_push_peer_capacity_parks_total 13"));
    assert!(rendered.contains("gents_p2p_broadcast_coalesced_total 41"));
    assert!(rendered.contains("gents_p2p_push_updates_coalesced_total 43"));
    assert!(rendered.contains("gents_p2p_missing_link_retries_total 23"));
    assert!(rendered.contains("gents_p2p_gossip_direction_filtered_total 47"));
    assert!(rendered.contains("gents_p2p_pending_dag_capacity_shed_total 59"));
    assert!(rendered.contains("gents_p2p_pending_dag_fetch_deferred_unavailable_total 69"));
    assert!(rendered.contains("gents_p2p_pending_dag_fetch_deferred_contention_total 68"));
    assert!(rendered.contains("gents_p2p_pending_dag_fetch_deferred_busy_total 76"));
    assert!(rendered.contains("gents_p2p_single_flight_suppressed_total 37"));
    assert!(rendered.contains("gents_p2p_already_merged_fast_path_total 53"));
    assert!(rendered.contains("gents_p2p_pending_dag_terminal_quarantined_total 73"));
    assert!(rendered.contains(r#"gents_p2p_peer_consecutive_failures{peer_id="peer-a"} 3"#));
    assert!(rendered
        .contains(r#"gents_p2p_peer_cooldown_remaining_milliseconds{peer_id="peer-a"} 750"#));
}

/// Checks the text exposition format: comments are HELP or TYPE, every
/// sample's family is typed exactly once before it, values are numbers,
/// labels are well formed and no series repeats. `grouped` additionally
/// requires each family's samples to be contiguous after its TYPE line.
pub(super) fn assert_valid_exposition(text: &str, grouped: bool) {
    let mut typed = std::collections::BTreeMap::<String, String>::new();
    let mut series = std::collections::BTreeSet::<String>::new();
    let mut closed = std::collections::BTreeSet::<String>::new();
    let mut current: Option<String> = None;
    let valid_name = |name: &str| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    };
    for line in text.lines().filter(|line| !line.is_empty()) {
        if let Some(comment) = line.strip_prefix("# ") {
            let mut parts = comment.splitn(3, ' ');
            let (kind, name) = (parts.next().unwrap(), parts.next().unwrap_or_default());
            assert!(valid_name(name), "bad family name: {line}");
            match kind {
                "HELP" => {}
                "TYPE" => {
                    let metric_type = parts.next().unwrap_or_default();
                    assert!(
                        ["gauge", "counter", "histogram", "summary", "untyped"]
                            .contains(&metric_type),
                        "bad type: {line}"
                    );
                    assert!(
                        typed
                            .insert(name.to_string(), metric_type.to_string())
                            .is_none(),
                        "family typed twice: {name}"
                    );
                    if grouped {
                        if let Some(previous) = current.replace(name.to_string()) {
                            closed.insert(previous);
                        }
                    }
                }
                _ => panic!("unexpected comment: {line}"),
            }
            continue;
        }
        let (series_key, value) = line.rsplit_once(' ').expect("sample has a value");
        assert!(value.parse::<f64>().is_ok(), "non-numeric value: {line}");
        let name = series_key.split('{').next().unwrap();
        let metric_type = typed
            .get(name)
            .unwrap_or_else(|| panic!("sample before TYPE: {line}"));
        if metric_type == "counter" {
            assert!(name.ends_with("_total"), "counter without _total: {name}");
        }
        if grouped {
            assert_eq!(
                current.as_deref(),
                Some(name),
                "family not contiguous: {line}"
            );
            assert!(!closed.contains(name), "family reopened: {line}");
        }
        if let Some(labels) = series_key.strip_prefix(name) {
            if !labels.is_empty() {
                let inner = labels
                    .strip_prefix('{')
                    .and_then(|rest| rest.strip_suffix('}'))
                    .unwrap_or_else(|| panic!("bad label set: {line}"));
                let mut keys = std::collections::BTreeSet::new();
                for pair in inner.split("\",").filter(|pair| !pair.is_empty()) {
                    let (key, value) = pair
                        .split_once("=\"")
                        .unwrap_or_else(|| panic!("bad label: {line}"));
                    assert!(valid_name(key), "bad label name: {line}");
                    assert!(keys.insert(key), "duplicate label {key}: {line}");
                    assert!(!value.trim_end_matches('"').contains('\n'));
                }
            }
        }
        assert!(
            series.insert(series_key.to_string()),
            "duplicate series: {line}"
        );
    }
}

fn rich_sync_status() -> gents_p2p::p2p::observability::P2pSyncStatusSnapshot {
    gents_p2p::p2p::observability::P2pSyncStatusSnapshot {
        request_dispatch: gents_p2p::p2p::observability::P2pRequestDispatchSnapshot {
            request_capacity: 32,
            active_requests: 5,
            active_requests_high_water: 30,
            recovery_capacity: 8,
            active_recovery: 7,
            active_recovery_high_water: 8,
            rejection_capacity: 4,
            active_rejections: 1,
            active_rejections_high_water: 2,
            completion_capacity: 16,
            active_completions: 3,
            active_completions_high_water: 9,
            saturated_total: 1_611_200,
            admission_saturated_total: 29,
            recovery_saturated_total: 1_611_171,
            rejection_dropped_total: 12,
            gossip_refusals_dropped_total: 0,
        },
        push_backlog: gents_p2p::p2p::observability::P2pPushBacklogSnapshot {
            enqueued_total: 512_729,
            failed_total: 82_089,
            head_hints_enqueued_document: 10,
            head_hints_enqueued_collection: 11,
            head_hints_sent_document: 12,
            head_hints_sent_collection: 13,
            head_hints_acked_document: 14,
            head_hints_acked_collection: 15,
            head_hints_nacked_capacity: 16,
            head_hints_nacked_other: 17,
            head_hints_failed_transport: 47_037,
            head_hints_failed_local: 18,
            ..Default::default()
        },
        push_retry_markers: gents_p2p::p2p::observability::P2pPushRetryMarkerSnapshot {
            document_markers: 3,
            collection_markers: 4,
            scheduled_peers: 2,
            oldest_scheduled_retry_unix: Some(1_800_000_000),
        },
        pending_dag_high_water: 900,
        persisted_pending_dag_high_water: 950,
        inbound_budget: vec![gents_p2p::p2p::observability::P2pInboundClassSnapshot {
            class: "sync".to_string(),
            capacity_bytes: 1_048_576,
            held_bytes: 4_096,
            held_bytes_high_water: 65_536,
            busy_resets: 5,
            oversized_resets: 6,
        }],
        peer_fetches: vec![
            gents_p2p::p2p::observability::P2pPeerFetchSnapshot {
                peer: "1dd587be-hub-peer".to_string(),
                fetches: 82_928,
                bytes: 13_600_000_000,
                delivery_rate_bytes_per_sec: 2_048,
                min_rtt_micros: Some(400),
                success_ppm: 20_897,
                timeout_ppm: 970_000,
                partial_ppm: 9_103,
                busy_ppm: 1_200,
                in_flight: 2,
                in_flight_bytes: 1_024,
                success_latency_micros: Some(84_000),
                useful_ewma_ppm: Some(170_000),
                attempt_cost_micros: Some(490_000),
                expected_useful_micros: Some(2_882_352),
                ewma_age_millis: Some(1_500),
            },
            gents_p2p::p2p::observability::P2pPeerFetchSnapshot {
                peer: "abcdef0123-spoke".to_string(),
                fetches: 10,
                success_ppm: 1_000_000,
                min_rtt_micros: None,
                ..Default::default()
            },
        ],
        non_authoritative_broadcast_tasks: 3,
        non_authoritative_broadcast_high_water: 7,
        non_authoritative_broadcast_rejected_total: 9,
        car_requested_cids: 1_400_000_000,
        car_present_cids: 1_350_000_000,
        car_served_cids: 1_340_000_000,
        car_filtered_cids: 10,
        car_served_bytes: 0,
        car_received_already_held: 0,
        car_served_per_requested: 0.0,
        provider_rotations: 36_455,
        provider_busy_rotations: 412,
        provider_demotions_total: 37,
        provider_cooldown_skips_total: 205,
        car_busy_responses: 96,
        pending_dag_registered: 11_329,
        pending_dag_retry_dispatched: 20_852,
        pending_dag_retry_suppressed: 44,
        pending_dag_fetch_exhausted: 9_871,
        pending_dag_terminal_merged: 10_523,
        next_pending_retry_in_ms: Some(1_500),
        pending_dag_attempt_ewma_median_ms: Some(4_500),
        pending_dag_attempt_ewma_max_ms: Some(35_000),
        pending_dag_no_progress_median_ppm: Some(500_007),
        pending_dag_tie_reorders: 12,
        pending_dag_peer_occupancy: vec![
            gents_p2p::p2p::observability::P2pPendingPeerOccupancySnapshot {
                peer: "1dd587be-hub-peer".to_string(),
                held: 3,
                occupancy_q8: 640,
            },
        ],
        peers: vec![
            gents_p2p::p2p::observability::P2pPeerSummarySnapshot {
                peer_id: "1dd587be-hub-peer".to_string(),
                connected: true,
                last_seen_at: "2026-11-02T15:14:50Z".to_string(),
                last_block_received_at: Some("2026-11-02T15:14:00Z".to_string()),
                last_head_acked_at: Some("2026-11-02T15:10:00Z".to_string()),
            },
            gents_p2p::p2p::observability::P2pPeerSummarySnapshot {
                peer_id: "abcdef0123-spoke".to_string(),
                connected: false,
                last_seen_at: "not a timestamp".to_string(),
                last_block_received_at: None,
                last_head_acked_at: None,
            },
        ],
        ..Default::default()
    }
}

fn scrape_now() -> DateTime<Utc> {
    rfc3339_timestamp("2026-11-02T15:15:00Z").unwrap()
}

#[test]
fn sync_diagnostics_render_every_decoded_field_with_bounded_labels() {
    let mut lines = Vec::new();
    p2p_sync::render_sync_diagnostics(&mut lines, Some(&rich_sync_status()), scrape_now());
    let rendered = lines.join("\n");
    assert_valid_exposition(&rendered, true);
    let hub = r#"peer_id="1dd587be-hub-peer",peer="1dd587be""#;
    let spoke = r#"peer_id="abcdef0123-spoke",peer="abcdef01""#;
    for sample in [
        r#"gents_p2p_request_dispatch_capacity{lane="recovery"} 8"#.to_string(),
        r#"gents_p2p_request_dispatch_active{lane="recovery"} 7"#.to_string(),
        r#"gents_p2p_request_dispatch_high_water{lane="recovery"} 8"#.to_string(),
        r#"gents_p2p_request_dispatch_capacity{lane="request"} 32"#.to_string(),
        r#"gents_p2p_request_dispatch_active{lane="rejection"} 1"#.to_string(),
        r#"gents_p2p_request_dispatch_high_water{lane="completion"} 9"#.to_string(),
        r#"gents_p2p_request_dispatch_saturated_total{lane="any"} 1611200"#.to_string(),
        r#"gents_p2p_request_dispatch_saturated_total{lane="admission"} 29"#.to_string(),
        r#"gents_p2p_request_dispatch_saturated_total{lane="recovery"} 1611171"#.to_string(),
        "gents_p2p_request_dispatch_rejection_dropped_total 12".to_string(),
        "gents_p2p_pending_dag_registered_total 11329".to_string(),
        "gents_p2p_pending_dag_fetch_exhausted_total 9871".to_string(),
        "gents_p2p_pending_dag_terminal_merged_total 10523".to_string(),
        "gents_p2p_pending_dag_retry_dispatched_total 20852".to_string(),
        "gents_p2p_pending_dag_retry_suppressed_total 44".to_string(),
        "gents_p2p_provider_rotations_total 36455".to_string(),
        "gents_p2p_provider_busy_rotations_total 412".to_string(),
        "gents_p2p_provider_demotions_total 37".to_string(),
        "gents_p2p_provider_cooldown_skips_total 205".to_string(),
        "gents_p2p_car_busy_responses_total 96".to_string(),
        "gents_p2p_pending_dag_high_water 900".to_string(),
        "gents_p2p_persisted_pending_dag_high_water 950".to_string(),
        "gents_p2p_next_pending_retry_seconds 1.5".to_string(),
        r#"gents_p2p_car_cids_total{outcome="present"} 1350000000"#.to_string(),
        r#"gents_p2p_car_cids_total{outcome="served"} 1340000000"#.to_string(),
        r#"gents_p2p_head_hints_total{outcome="acked",scope="collection"} 15"#.to_string(),
        r#"gents_p2p_head_hints_total{outcome="failed_transport",scope="all"} 47037"#.to_string(),
        r#"gents_p2p_inbound_budget_held_bytes{class="sync"} 4096"#.to_string(),
        r#"gents_p2p_inbound_budget_capacity_bytes{class="sync"} 1048576"#.to_string(),
        r#"gents_p2p_inbound_budget_held_high_water_bytes{class="sync"} 65536"#.to_string(),
        r#"gents_p2p_inbound_budget_resets_total{class="sync",reason="oversized"} 6"#.to_string(),
        "gents_p2p_non_authoritative_broadcast_tasks 3".to_string(),
        "gents_p2p_non_authoritative_broadcast_rejected_total 9".to_string(),
        r#"gents_p2p_push_retry_markers{scope="collection"} 4"#.to_string(),
        "gents_p2p_push_retry_scheduled_peers 2".to_string(),
        "gents_p2p_push_retry_oldest_scheduled_timestamp_seconds 1800000000".to_string(),
        format!("gents_p2p_peer_fetch_total{{{hub}}} 82928"),
        format!("gents_p2p_peer_fetch_bytes_total{{{hub}}} 13600000000"),
        format!("gents_p2p_peer_fetch_success_ppm{{{hub}}} 20897"),
        format!("gents_p2p_peer_fetch_timeout_ppm{{{hub}}} 970000"),
        format!("gents_p2p_peer_fetch_partial_ppm{{{hub}}} 9103"),
        format!("gents_p2p_peer_fetch_busy_ppm{{{hub}}} 1200"),
        format!("gents_p2p_peer_fetch_min_rtt_seconds{{{hub}}} 0.0004"),
        format!("gents_p2p_peer_fetch_delivery_rate_bytes_per_second{{{hub}}} 2048"),
        format!("gents_p2p_peer_fetch_in_flight{{{hub}}} 2"),
        format!("gents_p2p_peer_fetch_in_flight_bytes{{{hub}}} 1024"),
        format!("gents_p2p_peer_fetch_success_ppm{{{spoke}}} 1000000"),
        format!("gents_p2p_peer_fetch_success_latency_seconds{{{hub}}} 0.084"),
        format!("gents_p2p_peer_fetch_useful_ewma_ppm{{{hub}}} 170000"),
        format!("gents_p2p_peer_fetch_attempt_cost_seconds{{{hub}}} 0.49"),
        format!("gents_p2p_peer_fetch_expected_useful_seconds{{{hub}}} 2.882352"),
        format!("gents_p2p_peer_fetch_ewma_age_seconds{{{hub}}} 1.5"),
        "gents_p2p_pending_dag_tie_reorders_total 12".to_string(),
        "gents_p2p_pending_dag_attempt_ewma_median_seconds 4.5".to_string(),
        "gents_p2p_pending_dag_attempt_ewma_max_seconds 35".to_string(),
        "gents_p2p_pending_dag_no_progress_median_ppm 500007".to_string(),
        format!("gents_p2p_pending_dag_peer_held{{{hub}}} 3"),
        format!("gents_p2p_pending_dag_peer_occupancy{{{hub}}} 2.5"),
        r#"gents_p2p_peer_series_dropped{family="pending_dag"} 0"#.to_string(),
        format!("gents_p2p_peer_connected{{{hub}}} 1"),
        format!("gents_p2p_peer_connected{{{spoke}}} 0"),
        format!("gents_p2p_peer_last_seen_age_seconds{{{hub}}} 10"),
        format!("gents_p2p_peer_last_block_received_age_seconds{{{hub}}} 60"),
        format!("gents_p2p_peer_last_head_acked_age_seconds{{{hub}}} 300"),
        r#"gents_p2p_peer_series_dropped{family="fetch"} 0"#.to_string(),
        r#"gents_p2p_peer_series_dropped{family="peer"} 0"#.to_string(),
    ] {
        assert!(
            rendered.lines().any(|line| line == sample),
            "missing {sample}"
        );
    }
    for absent in [
        format!("gents_p2p_peer_fetch_min_rtt_seconds{{{spoke}}}"),
        format!("gents_p2p_peer_fetch_success_latency_seconds{{{spoke}}}"),
        format!("gents_p2p_peer_fetch_expected_useful_seconds{{{spoke}}}"),
        format!("gents_p2p_peer_last_seen_age_seconds{{{spoke}}}"),
        format!("gents_p2p_peer_last_head_acked_age_seconds{{{spoke}}}"),
    ] {
        assert!(
            !rendered.contains(&absent),
            "unmeasured {absent} must be absent"
        );
    }
}

#[test]
fn unavailable_sync_status_declares_families_without_zero_samples() {
    let mut lines = Vec::new();
    p2p_sync::render_sync_diagnostics(&mut lines, None, scrape_now());
    let rendered = lines.join("\n");
    assert_valid_exposition(&rendered, true);
    assert!(rendered.contains("# TYPE gents_p2p_request_dispatch_saturated_total counter"));
    assert!(rendered.lines().all(|line| line.starts_with('#')));
}

#[test]
fn per_peer_series_are_capped_and_the_overflow_is_counted() {
    let status = gents_p2p::p2p::observability::P2pSyncStatusSnapshot {
        peer_fetches: (0..300)
            .map(
                |index| gents_p2p::p2p::observability::P2pPeerFetchSnapshot {
                    peer: format!("peer-{index:04}"),
                    ..Default::default()
                },
            )
            .collect(),
        ..Default::default()
    };
    let mut lines = Vec::new();
    p2p_sync::render_sync_diagnostics(&mut lines, Some(&status), scrape_now());
    let rendered = lines.join("\n");
    assert_valid_exposition(&rendered, true);
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with("gents_p2p_peer_fetch_total{"))
            .count(),
        256
    );
    assert!(
        rendered.contains(r#"gents_p2p_peer_fetch_total{peer_id="peer-0255",peer="peer-025"} 0"#)
    );
    assert!(!rendered.contains(r#"peer_id="peer-0256""#));
    assert!(rendered
        .lines()
        .any(|line| line == r#"gents_p2p_peer_series_dropped{family="fetch"} 44"#));
}

#[test]
fn short_peer_id_respects_char_boundaries() {
    assert_eq!(p2p_sync::short_peer_id("1dd587be0123"), "1dd587be");
    assert_eq!(p2p_sync::short_peer_id("abc"), "abc");
    assert_eq!(p2p_sync::short_peer_id("ééééééééé"), "éééééééé");
}

#[test]
fn metrics_query_envelope_treats_null_probe_status_as_unknown() {
    let envelope: MetricsQueryEnvelope = serde_json::from_value(serde_json::json!({
        "NodeRuntime": [],
        "NodeReadiness": [],
        "InferenceBackendMetadata": [{
            "backend_id": "workstation-1",
            "enabled": true,
            "max_concurrent": 2,
            "max_queue_depth": 8,
            "probe_status": null,
            "last_probe": null
        }],
        "AgentRequest": [],
        "AgentToolCall": []
    }))
    .expect("metrics query envelope should decode null probe_status");

    assert_eq!(envelope.inference_backends[0].probe_status, "unknown");
}

#[test]
fn nullable_recorded_model_keeps_inference_metrics_available() {
    for model in [
        serde_json::json!({}),
        serde_json::json!({"model_name": null}),
    ] {
        let mut call = serde_json::json!({"call_state":"completed", "call_kind":"inference"});
        call.as_object_mut()
            .unwrap()
            .extend(model.as_object().unwrap().clone());
        let page: InferenceMetricsPageData =
            serde_json::from_value(serde_json::json!({"InferenceCall":[call]})).unwrap();
        let families = build_inference_metric_families(&InferenceMetricsQueryData {
            nodes: vec![],
            agents: vec![],
            calls: page.calls,
            window_seconds: INFERENCE_METRICS_WINDOW_SECS,
        });
        assert_eq!(families.request_totals.len(), 1);
        assert_eq!(
            families.request_totals.keys().next().unwrap().model,
            "unknown"
        );
    }
}

#[test]
fn metric_labels_escape_backslash_quote_and_newline() {
    let mut lines = Vec::new();
    push_metric_sample(
        &mut lines,
        "gents_runtime_active_generation",
        &[("node_did", "did:key:zA\\b\"c\nd".to_string())],
        1,
    );
    assert_eq!(
        lines[0], r#"gents_runtime_active_generation{node_did="did:key:zA\\b\"c\nd"} 1"#,
        "backslash must be doubled and quote/newline escaped, in that order"
    );
}

#[test]
fn inference_metrics_group_by_agent_backend_model_and_status() {
    let data = InferenceMetricsQueryData {
        nodes: vec![InferenceNodeRow {
            node_did: "did:key:zAgent".to_string(),
            display_name: "observability-steward".to_string(),
        }],
        agents: vec![InferenceAgentRow {
            agent_did: "agent_config-1".to_string(),
            node_did: "did:key:zAgent".to_string(),
            display_name: "fallback-agent_config-name".to_string(),
        }],
        calls: vec![
            InferenceCallMetricRow {
                node_did: "did:key:zAgent".to_string(),
                agent_did: "agent_config-1".to_string(),
                backend_id: "backend-1".to_string(),
                call_state: "completed".to_string(),
                ended_at: "2026-06-13T10:00:00.000000000+00:00".to_string(),
                model_name: "fixture-model".to_string(),
                call_kind: "inference".to_string(),
                usage: TokenUsageColumns {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(4),
                    ..Default::default()
                },
            },
            InferenceCallMetricRow {
                node_did: "did:key:zAgent".to_string(),
                agent_did: "agent_config-1".to_string(),
                backend_id: "backend-1".to_string(),
                call_state: "completed".to_string(),
                ended_at: "2026-06-13T10:00:01.000000000+00:00".to_string(),
                model_name: "fixture-model".to_string(),
                call_kind: "inference".to_string(),
                usage: TokenUsageColumns {
                    prompt_tokens: Some(7),
                    completion_tokens: Some(3),
                    ..Default::default()
                },
            },
            InferenceCallMetricRow {
                node_did: "did:key:zAgent".to_string(),
                agent_did: "agent_config-1".to_string(),
                backend_id: "backend-1".to_string(),
                call_state: "failed".to_string(),
                ended_at: "2026-06-13T10:00:02.000000000+00:00".to_string(),
                model_name: "fixture-model".to_string(),
                call_kind: "inference".to_string(),
                usage: TokenUsageColumns {
                    prompt_tokens: Some(5),
                    completion_tokens: None,
                    ..Default::default()
                },
            },
            InferenceCallMetricRow {
                node_did: "did:key:zAgent".to_string(),
                agent_did: "agent_config-1".to_string(),
                backend_id: "backend-1".to_string(),
                call_state: "running".to_string(),
                ended_at: "2026-06-13T10:00:03.000000000+00:00".to_string(),
                model_name: "fixture-model".to_string(),
                call_kind: "inference".to_string(),
                usage: TokenUsageColumns {
                    prompt_tokens: Some(99),
                    completion_tokens: Some(99),
                    ..Default::default()
                },
            },
        ],
        window_seconds: INFERENCE_METRICS_WINDOW_SECS,
    };

    let families = build_inference_metric_families(&data);
    let completed_key = InferenceRequestMetricKey {
        agent: "observability-steward".to_string(),
        node_did: "did:key:zAgent".to_string(),
        backend_id: "backend-1".to_string(),
        model: "fixture-model".to_string(),
        status: "completed".to_string(),
        call_kind: "inference".to_string(),
    };
    let failed_key = InferenceRequestMetricKey {
        status: "failed".to_string(),
        ..completed_key.clone()
    };
    let token_key = InferenceTokenMetricKey {
        agent: "observability-steward".to_string(),
        node_did: "did:key:zAgent".to_string(),
        backend_id: "backend-1".to_string(),
        model: "fixture-model".to_string(),
        call_kind: "inference".to_string(),
    };

    assert_eq!(families.request_totals.get(&completed_key), Some(&2));
    assert_eq!(families.request_totals.get(&failed_key), Some(&1));
    let summary = families.usage[&token_key].unwrap();
    assert_eq!(summary.input.known, 22);
    assert_eq!(summary.output.known, 7);
    assert_eq!(summary.incomplete_calls, 3);
}

#[test]
fn inference_metrics_query_reads_only_the_scrape_window() {
    let start = DateTime::parse_from_rfc3339("2026-10-22T21:15:00.500+00:00")
        .unwrap()
        .with_timezone(&Utc);
    let bound = inference_metrics_ended_lower_bound(&start);
    assert_eq!(bound, "2026-10-22T21:15:00");
    let inside = (start + Duration::milliseconds(1)).to_rfc3339();
    assert!(bound.as_str() <= inside.as_str());
    assert!(bound.as_str() <= "2026-10-22T21:15:00+00:00");
    let query = inference_metrics_query(500, 0, false, &start);
    assert!(query.contains(r#"ended_at: { _ge: "2026-10-22T21:15:00" }"#));
}

#[test]
fn render_inference_metrics_emits_windowed_gauge_families() {
    let data = InferenceMetricsQueryData {
        nodes: vec![],
        agents: vec![InferenceAgentRow {
            agent_did: "agent-1".to_string(),
            node_did: "did:key:zAgent".to_string(),
            display_name: "agent \"friendly\"".to_string(),
        }],
        calls: vec![InferenceCallMetricRow {
            node_did: "did:key:zAgent".to_string(),
            agent_did: "agent-1".to_string(),
            backend_id: "backend-1".to_string(),
            call_state: "completed".to_string(),
            ended_at: "2026-06-13T10:00:00.000000000+00:00".to_string(),
            model_name: "model\none".to_string(),
            call_kind: "inference".to_string(),
            usage: TokenUsageColumns {
                prompt_tokens: Some(10),
                completion_tokens: Some(3),
                ..Default::default()
            },
        }],
        window_seconds: INFERENCE_METRICS_WINDOW_SECS,
    };

    let mut lines = Vec::new();
    render_inference_metrics(&mut lines, &data);
    let body = lines.join("\n");

    assert!(body.contains("# TYPE gents_inference_metrics_window_seconds gauge"));
    assert!(body.contains("gents_inference_metrics_window_seconds 300"));
    assert!(body.contains("# TYPE gents_inference_requests_window_count gauge"));
    assert!(body.contains(
        "gents_inference_requests_window_count{agent=\"agent \\\"friendly\\\"\",node_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",status=\"completed\",call_kind=\"inference\"} 1"
    ));
    assert!(body.contains(
        "gents_inference_prompt_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",node_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",call_kind=\"inference\"} 10"
    ));
    assert!(body.contains(
        "gents_inference_completion_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",node_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",call_kind=\"inference\"} 3"
    ));
}
