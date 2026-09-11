//! Generated attribution traces consumed by the actual GraphRun transaction owners.
use super::*;
use crate::graph_pipeline::runtime::graph_test_owner;
use serde::Deserialize;

#[derive(Deserialize)]
struct Snapshot {
    graph_failure_attribution_traces: Vec<Trace>,
}
#[derive(Deserialize)]
struct Trace {
    name: String,
    initial: Observation,
    events: Vec<Event>,
    expected: Vec<ExpectedObservation>,
}
#[derive(Deserialize)]
struct ExpectedObservation {
    #[serde(flatten)]
    durable: Observation,
    may_interrupt_for_failure: bool,
}
#[derive(Deserialize)]
struct Event {
    kind: String,
    expected_generation: Option<i64>,
    witness: Option<u64>,
    #[serde(default)]
    all_terminal: bool,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Observation {
    status: String,
    cancellation_requested: bool,
    generation: i64,
    primary: Option<u64>,
}
fn cause_id(cause: u64) -> &'static str {
    match cause {
        10 => "a-cause",
        90 => "z-cause",
        _ => panic!("unknown cause {cause}"),
    }
}
fn observe(view: &GraphRunView, initial_generation: i64) -> Observation {
    let primary = view.error.as_ref().map(|error| {
        assert_eq!(error["code"], "required_request_failed");
        match error["request_id"].as_str().unwrap() {
            "a-cause" => 10,
            "z-cause" => 90,
            other => panic!("unexpected cause {other}"),
        }
    });
    Observation {
        status: view.status.clone(),
        cancellation_requested: view.cancellation_requested_at.is_some(),
        generation: view.update_generation - initial_generation,
        primary,
    }
}
async fn execute_fixture(node: &EmbeddedNode, query: String) {
    let result = node.execute(&query).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
}

#[tokio::test]
async fn generated_graph_failure_attribution_traces_drive_real_transactions() {
    let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(snapshot.graph_failure_attribution_traces.len(), 10);
    for trace in snapshot.graph_failure_attribution_traces {
        let (node, run, trigger) = super::super::runtime::attribution_test_fixture(3).await;
        let initial = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        let initial_generation = initial.update_generation;
        assert_eq!(
            observe(&initial, initial_generation),
            trace.initial,
            "{} initial",
            trace.name
        );
        let empty_fixture = trace.events.iter().all(|event| event.witness.is_none());
        if !empty_fixture {
            // A third live row allows the lower lexical failed sibling to be
            // observed while real active work still prevents terminal commit.
            for id in ["a-cause", "z-cause", "m-drain-sentinel"] {
                super::super::runtime::seed_signed_graph_request(
                    &node,
                    &run,
                    &trigger,
                    id,
                    "processing",
                    "",
                )
                .await;
            }
        }
        assert_eq!(trace.events.len(), trace.expected.len());
        for (index, (event, expected)) in trace.events.iter().zip(&trace.expected).enumerate() {
            let before = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
                .await
                .unwrap();
            if !before.is_terminal() {
                if let Some(witness) = event.witness {
                    let state = if trace.name
                        == "earlier_interrupted_sibling_does_not_replace_cause"
                        && witness == 10
                    {
                        "interrupted"
                    } else {
                        "failed"
                    };
                    // Never change an already terminal cause to manufacture a
                    // later abstract witness. The filter fences fixture legality.
                    execute_fixture(&node, format!(r#"mutation {{ update_AgentRequest(
                        filter: {{ request_id: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "processing" }} }},
                        input: {{ lifecycle_state: "{state}", failure_reason: "cause-{witness}" }}
                    ) {{ _docID }} }}"#, cause_id(witness))).await;
                }
                if event.kind == "finish" && event.all_terminal {
                    execute_fixture(&node, format!(r#"mutation {{ update_AgentRequest(
                        filter: {{ caused_by_correlation: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "processing" }} }},
                        input: {{ lifecycle_state: "completed" }}
                    ) {{ _docID }} }}"#, escape_graphql_string(&run.correlation))).await;
                }
            }
            let mut observed = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
                .await
                .unwrap();
            // Check emitted witness against durable facts independently of the
            // latched error, which correctly stops changing after capture.
            if let Some(witness) = event.witness {
                let candidate = if event.kind == "observed_failure" {
                    observed.requests.iter().find(|request| {
                        request.request_id == cause_id(witness)
                            && request.terminal
                            && !request.succeeded
                    })
                } else {
                    observed
                        .requests
                        .iter()
                        .filter(|request| request.terminal && !request.succeeded)
                        .min_by_key(|request| request.request_id.as_str())
                }
                .expect("the emitted failure must be a real durable terminal request");
                assert_eq!(
                    candidate.request_id,
                    cause_id(witness),
                    "{} event{index}: unreachable emitted witness",
                    trace.name
                );
            }
            if event.kind == "finish" {
                assert_eq!(
                    observed.active_request_count == 0,
                    event.all_terminal,
                    "{} event{index}",
                    trace.name
                );
            }
            let actual_generation = observed.update_generation;
            if let Some(generation) = event.expected_generation {
                observed.update_generation = initial_generation + generation;
            }
            match event.kind.as_str() {
                "observed_failure" => {} // The durable fixture transition above is the observation.
                "capture" => {
                    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
                    let result = capture_failure_txn(txn, &observed).await;
                    if result.is_err() {
                        assert!(
                            observed.is_terminal()
                                || observed.update_generation != actual_generation,
                            "{} event{index}: unexpected capture error {result:?}",
                            trace.name
                        );
                    }
                    // Use the emitted decision to drive and observe the real interrupt owner.
                    let after_capture = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
                        .await
                        .unwrap();
                    if expected.may_interrupt_for_failure {
                        assert!(
                            after_capture.active_request_count > 0,
                            "capture fixture must retain active work"
                        );
                        let reconciled =
                            reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
                                .await
                                .unwrap();
                        assert_eq!(
                            reconciled.update_generation, after_capture.update_generation,
                            "{} event{index}: interrupt pass must not rewrite the run",
                            trace.name
                        );
                        for request in &reconciled.requests {
                            if request.terminal {
                                continue;
                            }
                            let row = node
                                .execute(&format!(
                                    r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ interrupt_requested_at }} }}"#,
                                    escape_graphql_string(&request.request_id)
                                ))
                                .await;
                            assert!(!row.has_errors(), "{:?}", row.errors);
                            assert!(
                                row.data.unwrap()["AgentRequest"][0]["interrupt_requested_at"]
                                    .is_string(),
                                "{} event{index}: active {} must carry the owner's interrupt latch",
                                trace.name,
                                request.request_id
                            );
                        }
                    }
                }
                "cancel" => {
                    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
                    persist_cancellation_intent(
                        txn,
                        graph_test_owner(),
                        &run.run_id,
                        Some("generated cancellation"),
                    )
                    .await
                    .unwrap();
                }
                "finish" => {
                    let eligible = terminal_projection(&observed).is_some();
                    let status = if observed.cancellation_requested_at.is_some() {
                        "cancelled"
                    } else {
                        "failed"
                    };
                    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
                    let result = commit_terminal_txn(txn, &observed, status).await;
                    if result.is_err() {
                        assert!(
                            !eligible
                                || observed.is_terminal()
                                || observed.update_generation != actual_generation,
                            "{} event{index}: unexpected terminal error {result:?}",
                            trace.name
                        );
                    }
                }
                other => panic!("unknown generated action {other}"),
            }
            let durable = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
                .await
                .unwrap();
            assert_eq!(
                &observe(&durable, initial_generation),
                &expected.durable,
                "{} event{index}",
                trace.name
            );
        }
        node.shutdown().await;
    }
}

// FailureAttribution.capture_loser_observes_winner: two real observers load the
// same generation; a cancellation committed between them must remain visible.
#[tokio::test]
async fn failure_capture_loser_reloads_cancellation_winner() {
    let (node, run, trigger) = super::super::runtime::attribution_test_fixture(2).await;
    super::super::runtime::seed_signed_graph_request(
        &node,
        &run,
        &trigger,
        "failed",
        "failed",
        "first cause",
    )
    .await;
    super::super::runtime::seed_signed_graph_request(
        &node,
        &run,
        &trigger,
        "live",
        "processing",
        "",
    )
    .await;
    let stale = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    execute_fixture(&node, format!(r#"mutation {{ update_GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}, input: {{ cancel_requested_at: "2026-09-05T00:00:00Z", update_generation: {} }}) {{ _docID }} }}"#, escape_graphql_string(&run.run_id), stale.update_generation + 1)).await;
    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    capture_failure_txn(txn, &stale).await.unwrap();
    let fresh = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert!(fresh.cancellation_requested_at.is_some());
    assert!(fresh.error.is_none());
    assert_eq!(fresh.update_generation, stale.update_generation + 1);
}

#[tokio::test]
async fn future_failure_code_remains_observable_and_cancellable() {
    let (node, run, _) = super::super::runtime::attribution_test_fixture(2).await;
    let opaque = json!({"version": 1, "code": "future_owner_failure", "message": "Opaque diagnostic", "future_details": {"counter": 9}});
    execute_fixture(&node, format!(r#"mutation {{ update_GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}, input: {{ error: "{}" }}) {{ _docID }} }}"#, escape_graphql_string(&run.run_id), escape_graphql_string(&opaque.to_string()))).await;
    let observed = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(observed.error, Some(opaque));
    let cancelled = request_graph_run_cancellation(
        &node,
        None,
        graph_test_owner(),
        &run.run_id,
        Some("operator cancellation"),
    )
    .await
    .unwrap();
    assert_eq!(cancelled.status, "cancelled");
}

#[tokio::test]
async fn oversized_cancellation_reason_is_rejected_before_any_write() {
    let (node, run, _) = super::super::runtime::attribution_test_fixture(2).await;
    let oversized = "x".repeat(1_025);
    let error = request_graph_run_cancellation(
        &node,
        None,
        graph_test_owner(),
        &run.run_id,
        Some(&oversized),
    )
    .await
    .expect_err("the owner must reject a reason beyond its byte ceiling");
    assert!(
        error.to_string().contains("cancellation reason exceeds"),
        "unexpected rejection: {error:#}"
    );
    let durable = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(durable.status, "running");
    assert!(durable.cancellation_requested_at.is_none());
    assert_eq!(durable.update_generation, 0);
    node.shutdown().await;
}

#[tokio::test]
async fn expired_run_deadline_becomes_durable_failure_evidence() {
    let (node, run, _) = super::super::runtime::attribution_test_fixture(2).await;
    // Expire the plan's pinned runtime limit without waiting for the clock.
    execute_fixture(
        &node,
        format!(
            r#"mutation {{ update_GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}, input: {{ started_at: "2020-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
            escape_graphql_string(&run.run_id)
        ),
    )
    .await;
    let observed = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let evidence = observed
        .failure_evidence
        .as_ref()
        .expect("an exceeded run deadline is failure evidence");
    assert_eq!(evidence["code"], "run_deadline_exceeded");
    assert_eq!(evidence["max_runtime_secs"].as_u64(), Some(60));
    assert!(evidence["deadline_at"].is_string());
    // Reconciliation must persist the derived deadline evidence.
    let terminal = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(terminal.status, "failed");
    assert_eq!(
        terminal.error.as_ref().unwrap()["code"],
        "run_deadline_exceeded"
    );
    assert!(terminal.completed_at.is_some());
    let again = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(again.error, terminal.error);
    assert_eq!(again.update_generation, terminal.update_generation);
    node.shutdown().await;
}

#[tokio::test]
async fn quiesced_pinned_route_group_becomes_durable_failure_evidence() {
    let (node, run, trigger) = super::super::runtime::attribution_test_fixture(2).await;
    // Match the pinned trigger and run correlation so load_groups admits it.
    execute_fixture(
        &node,
        format!(
            r#"mutation {{ create_EventGroupState(input: {{
                group_key: "pinned-entry-group", agent_did: "{}", consumer: {{ kind: "trigger", trigger_id: "{}" }},
                correlation: "{}", consumer_config_key: "pinned-entry-group-v1",
                first_seen_at: "2026-09-05T00:00:00Z",
                quiesced_at: "2026-09-05T00:01:00Z",
                quiesced_reason: "operator timeout"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(graph_test_owner()),
            escape_graphql_string(&trigger),
            escape_graphql_string(&run.correlation),
        ),
    )
    .await;
    for (key, owner, consumer) in [
        (
            "foreign-owner",
            "did:key:foreign-owner",
            format!(
                r#"{{kind: "trigger", trigger_id: "{}"}}"#,
                escape_graphql_string(&trigger)
            ),
        ),
        (
            "callback",
            graph_test_owner(),
            r#"{kind: "callback_binding", binding_id: "other"}"#.to_string(),
        ),
    ] {
        execute_fixture(
            &node,
            format!(
                r#"mutation {{ create_EventGroupState(input: {{
            group_key: "{key}", agent_did: "{}", consumer: {consumer}, correlation: "{}",
            consumer_config_key: "unrelated", first_seen_at: "2026-09-04T00:00:00Z",
            quiesced_at: "2026-09-04T00:01:00Z", quiesced_reason: "foreign failure"
        }}) {{_docID}} }}"#,
                escape_graphql_string(owner),
                escape_graphql_string(&run.correlation)
            ),
        )
        .await;
    }
    let observed = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(observed.groups.len(), 1);
    assert_eq!(
        observed.groups[0].quiesced_reason.as_deref(),
        Some("operator timeout")
    );
    let evidence = observed
        .failure_evidence
        .as_ref()
        .expect("a quiesced pinned group is failure evidence");
    assert_eq!(evidence["code"], "group_quiesced");
    assert_eq!(evidence["group_key"].as_str(), Some("pinned-entry-group"));
    assert_eq!(evidence["trigger_id"].as_str(), Some(trigger.as_str()));
    let terminal = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(terminal.status, "failed");
    assert_eq!(terminal.error.as_ref().unwrap()["code"], "group_quiesced");
    node.shutdown().await;
}

#[tokio::test]
async fn competing_failure_observers_reload_single_committed_cause() {
    let (node, run, trigger) = super::super::runtime::attribution_test_fixture(2).await;
    super::super::runtime::seed_signed_graph_request(
        &node,
        &run,
        &trigger,
        "cause",
        "failed",
        "durable cause",
    )
    .await;
    super::super::runtime::seed_signed_graph_request(
        &node,
        &run,
        &trigger,
        "live",
        "processing",
        "",
    )
    .await;
    let first = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let second = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let first_txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    capture_failure_txn(first_txn, &first).await.unwrap();
    let second_txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    capture_failure_txn(second_txn, &second).await.unwrap();
    let access = ConfigAccess::Local(Arc::clone(&node));
    let fresh = reconcile_graph_run_with_access(&access, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(fresh.update_generation, first.update_generation + 1);
    assert_eq!(fresh.error.as_ref().unwrap()["request_id"], "cause");
    let persisted = node.execute(r#"{ AgentRequest(filter: { request_id: { _eq: "live" } }) { interrupt_requested_at } }"#).await;
    assert!(!persisted.has_errors());
    assert!(persisted.data.unwrap()["AgentRequest"][0]["interrupt_requested_at"].is_string());
}

#[tokio::test]
async fn native_failure_capture_conflict_reloads_cancellation_winner() {
    let (node, run, trigger) = super::super::runtime::attribution_test_fixture(2).await;
    super::super::runtime::seed_signed_graph_request(
        &node,
        &run,
        &trigger,
        "cause",
        "failed",
        "durable cause",
    )
    .await;
    super::super::runtime::seed_signed_graph_request(
        &node,
        &run,
        &trigger,
        "live",
        "processing",
        "",
    )
    .await;
    let stale = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    let initial = load_graph_run_view_with(&txn, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(initial.update_generation, stale.update_generation);
    // The direct node operation is an independent native transaction, deliberately
    // bypassing the local ConfigApplyTxn serialization mutex for this storage race.
    execute_fixture(&node, format!(r#"mutation {{ update_GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}, input: {{ cancel_requested_at: "2026-09-05T00:00:00Z", update_generation: {} }}) {{ _docID }} }}"#, escape_graphql_string(&run.run_id), stale.update_generation + 1)).await;
    let snapshot = load_graph_run_view_with(&txn, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert!(
        snapshot.cancellation_requested_at.is_none(),
        "must exercise an overlapping native snapshot, not merely sequential CAS loss"
    );
    capture_failure_txn(txn, &stale).await.unwrap();
    let fresh = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert!(fresh.cancellation_requested_at.is_some());
    assert!(fresh.error.is_none());
    assert_eq!(fresh.update_generation, stale.update_generation + 1);
}
