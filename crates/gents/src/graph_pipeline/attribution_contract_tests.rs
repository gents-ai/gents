//! Generated attribution traces consumed by the actual GraphRun transaction owners.
use super::*;
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
    expected: Vec<Observation>,
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
    may_interrupt_for_failure: bool,
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
        // This is an observation of durable owner output, not a transition or
        // a reference implementation deciding which cause should be selected.
        may_interrupt_for_failure: view.status == "running"
            && view.cancellation_requested_at.is_none()
            && primary.is_some(),
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
        let initial = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
                execute_fixture(
                    &node,
                    format!(
                        r#"mutation {{ create_AgentRequest(input: {{
                    request_id: "{id}", agent_did: "did:key:worker", requester_did: "did:key:owner",
                    behavior_id: "worker-v1", lifecycle_state: "processing",
                    caused_by_trigger_id: "{}", caused_by_correlation: "{}",
                    created_at: "2026-08-25T00:00:00Z"
                }}) {{ _docID }} }}"#,
                        escape_graphql_string(&trigger),
                        escape_graphql_string(&run.correlation)
                    ),
                )
                .await;
            }
        }
        assert_eq!(trace.events.len(), trace.expected.len());
        for (index, (event, expected)) in trace.events.iter().zip(&trace.expected).enumerate() {
            let before = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
            let mut observed = load_graph_run_view(&node, "did:key:owner", &run.run_id)
                .await
                .unwrap();
            // Check emitted witness against durable facts independently of the
            // latched error, which correctly stops changing after capture.
            if let Some(witness) = event.witness {
                let candidate = observed
                    .requests
                    .iter()
                    .filter(|request| request.terminal && !request.succeeded)
                    .min_by_key(|request| request.request_id.as_str())
                    .unwrap();
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
                }
                "cancel" => {
                    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
                    persist_cancellation_intent(
                        txn,
                        "did:key:owner",
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
            let durable = load_graph_run_view(&node, "did:key:owner", &run.run_id)
                .await
                .unwrap();
            assert_eq!(
                &observe(&durable, initial_generation),
                expected,
                "{} event{index}",
                trace.name
            );
        }
        node.shutdown().await;
    }
}
