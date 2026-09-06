//! The GraphRun publication fence composes with signed request staging in one
//! transaction. Goal claim eligibility has its separate production consumer.
use super::*;
use crate::graph_pipeline::logical_invocation_contract_tests::{
    execute, prepare_signed_child, signed_invocation_fixture,
};
use serde::Deserialize;

fn assert_created_request(response: &Value) {
    let data = response.get("data").unwrap_or(response);
    let result = data
        .get("create_AgentRequest")
        .or_else(|| data.get("add_AgentRequest"))
        .unwrap_or_else(|| panic!("missing created request: {response:?}"));
    let row = if let Some(rows) = result.as_array() {
        assert_eq!(rows.len(), 1);
        &rows[0]
    } else {
        result
    };
    assert!(
        row["_docID"].as_str().is_some(),
        "missing created document ID: {response:?}"
    );
}

#[derive(Deserialize)]
struct Contracts {
    graph_invocation_publication_cases: Vec<Trace>,
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
    cause: Option<u64>,
}
#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Observation {
    status: String,
    cancellation_requested: bool,
    generation: i64,
    primary: Option<u64>,
    may_interrupt_for_failure: bool,
    children: usize,
}
async fn observe(node: &EmbeddedNode, run_id: &str, initial_generation: i64) -> Observation {
    let view = load_graph_run_view(node, "did:key:owner", run_id)
        .await
        .unwrap();
    let primary = view.error.as_ref().map(|error| {
        assert_eq!(error["request_id"], "graph-logical-root");
        90
    });
    let value = execute(node, "{ AgentRequest { request_id } }").await;
    let children = value["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["request_id"] != "graph-logical-root")
        .count();
    Observation {
        may_interrupt_for_failure: view.status == "running"
            && view.cancellation_requested_at.is_none()
            && primary.is_some(),
        status: view.status,
        cancellation_requested: view.cancellation_requested_at.is_some(),
        generation: view.update_generation - initial_generation,
        primary,
        children,
    }
}

#[tokio::test]
async fn generated_graph_invocation_publication_traces_drive_real_transactions() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.graph_invocation_publication_cases.len(), 5);
    for trace in contracts.graph_invocation_publication_cases {
        let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
        execute(&node, &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "paused" }}) {{ _docID }} }}"#, goal.doc_id)).await;
        let cause = load_graph_run_view(&node, "did:key:owner", &run.run_id)
            .await
            .unwrap();
        assert_eq!(
            cause.failure_evidence.as_ref().unwrap()["request_id"],
            "graph-logical-root"
        );
        let initial_generation = cause.update_generation;
        let child = prepare_signed_child(&node, &identity, &goal, "valid").await;
        assert_eq!(
            observe(&node, &run.run_id, initial_generation).await,
            trace.initial,
            "{} initial",
            trace.name
        );
        assert_eq!(trace.events.len(), trace.expected.len());
        for (event, expected) in trace.events.iter().zip(trace.expected.iter()) {
            let before = load_graph_run_view(&node, "did:key:owner", &run.run_id)
                .await
                .unwrap();
            let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
            match event.kind.as_str() {
                "publish" => {
                    assert_eq!(
                        event.expected_generation.unwrap() + initial_generation,
                        before.update_generation
                    );
                    let result = crate::graph_pipeline::fence_graph_publication_in_txn(
                        &txn,
                        &run.run_id,
                        &run.revision_digest,
                    )
                    .await;
                    if result.is_ok() {
                        let response = txn
                            .execute(&child.graphql_mutation().unwrap())
                            .await
                            .unwrap();
                        assert_created_request(&response);
                        txn.commit().await.unwrap();
                    } else {
                        assert!(
                            before.status != "running"
                                || before.cancellation_requested_at.is_some()
                                || before.error.is_some(),
                            "unexpected fence error: {result:?}"
                        );
                        txn.discard().await.unwrap();
                    }
                }
                "capture" | "finish" => {
                    assert_eq!(event.cause, Some(90));
                    // Preserve the real earlier observation through publication;
                    // changing only its generation would not test stale evidence.
                    let mut observed = cause.clone();
                    observed.update_generation =
                        initial_generation + event.expected_generation.unwrap();
                    let result = if event.kind == "capture" {
                        capture_failure_txn(txn, &observed).await.map(|_| ())
                    } else {
                        commit_terminal_txn(txn, &observed, "failed")
                            .await
                            .map(|_| ())
                    };
                    if let Err(error) = result {
                        assert_ne!(
                            before.update_generation, observed.update_generation,
                            "unexpected owner error: {error:#}"
                        );
                    }
                }
                "cancel" => {
                    persist_cancellation_intent(
                        txn,
                        "did:key:owner",
                        &run.run_id,
                        Some("generated publication cancellation"),
                    )
                    .await
                    .unwrap();
                }
                kind => panic!("unknown event {kind}"),
            }
            assert_eq!(
                &observe(&node, &run.run_id, initial_generation).await,
                expected,
                "{} {}",
                trace.name,
                event.kind
            );
        }
        node.shutdown().await;
    }
}

#[tokio::test]
async fn discarding_graph_publication_rolls_back_generation_and_signed_child() {
    let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
    let before = load_graph_run_view(&node, "did:key:owner", &run.run_id)
        .await
        .unwrap();
    let child = prepare_signed_child(&node, &identity, &goal, "valid").await;
    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    crate::graph_pipeline::fence_graph_publication_in_txn(&txn, &run.run_id, &run.revision_digest)
        .await
        .unwrap();
    txn.execute(&child.graphql_mutation().unwrap())
        .await
        .unwrap();
    let staged = txn
        .execute("{ GraphRun { update_generation } AgentRequest { request_id } }")
        .await
        .unwrap();
    assert_eq!(
        staged["data"]["GraphRun"][0]["update_generation"],
        before.update_generation + 1
    );
    assert_eq!(staged["data"]["AgentRequest"].as_array().unwrap().len(), 2);
    txn.discard().await.unwrap();
    assert_eq!(
        observe(&node, &run.run_id, before.update_generation).await,
        Observation {
            status: "running".into(),
            cancellation_requested: false,
            generation: 0,
            primary: None,
            may_interrupt_for_failure: false,
            children: 0,
        }
    );
    node.shutdown().await;
}

#[tokio::test]
async fn overlapping_native_graph_publication_and_closure_conflict_atomically() {
    async fn native(node: &EmbeddedNode, handle: &query::TransactionHandle, query: &str) -> Value {
        let response = node
            .execute_request_in_txn(defra_node::QueryRequest::new(query), handle)
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()
    }
    for publication_first in [false, true] {
        let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
        execute(&node, &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "paused" }}) {{ _docID }} }}"#, goal.doc_id)).await;
        let before = load_graph_run_view(&node, "did:key:owner", &run.run_id)
            .await
            .unwrap();
        let child = prepare_signed_child(&node, &identity, &goal, "valid").await;
        // Intentionally use two actual native handles: ConfigApplyTxn's local
        // mutex would serialize begin and never exercise storage conflict checks.
        let publication = node.runner().begin_txn(false).await.unwrap();
        let closure = node.runner().begin_txn(false).await.unwrap();
        let read = format!(
            r#"{{ GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}) {{ _docID status update_generation }} }}"#,
            run.run_id
        );
        let left = native(&node, &publication, &read).await;
        let right = native(&node, &closure, &read).await;
        assert_eq!(
            left, right,
            "both native handles must observe the same snapshot"
        );
        assert_eq!(left["GraphRun"][0]["status"], "running");
        let next = before.update_generation.checked_add(1).unwrap();
        let filter = format!(
            r#"{{ run_id: {{ _eq: "{}" }}, status: {{ _eq: "running" }}, update_generation: {{ _eq: {} }} }}"#,
            run.run_id, before.update_generation
        );
        let touch = format!(
            r#"mutation {{ update_GraphRun(filter: {filter}, input: {{ update_generation: {next} }}) {{ _docID }} }}"#
        );
        assert_eq!(
            native(&node, &publication, &touch).await["update_GraphRun"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let staged_child = native(&node, &publication, &child.graphql_mutation().unwrap()).await;
        assert_created_request(&staged_child);
        let terminal = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "status": "failed",
            "update_generation": next,
            "error": before.failure_evidence.as_ref().unwrap().to_string(),
            "completed_at": chrono::Utc::now().to_rfc3339(),
        }))
        .unwrap();
        let finish = format!(
            "mutation {{ update_GraphRun(filter: {filter}, input: {terminal}) {{ _docID }} }}"
        );
        assert_eq!(
            native(&node, &closure, &finish).await["update_GraphRun"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // Both conditional mutations already matched. Only the overlapping
        // native commit boundary can reject the second writer now.
        let (winner, loser) = if publication_first {
            (&publication, &closure)
        } else {
            (&closure, &publication)
        };
        node.runner().commit_txn(winner).await.unwrap();
        let conflict = node
            .runner()
            .commit_txn(loser)
            .await
            .expect_err("overlapping native writes must not both commit");
        assert!(
            conflict.to_string().contains("transaction conflict"),
            "expected storage conflict, got: {conflict}"
        );
        let durable = execute(
            &node,
            "{ GraphRun { status update_generation } AgentRequest { request_id } }",
        )
        .await;
        assert_eq!(durable["GraphRun"][0]["update_generation"], next);
        assert_eq!(
            durable["GraphRun"][0]["status"],
            if publication_first {
                "running"
            } else {
                "failed"
            }
        );
        let rows = durable["AgentRequest"].as_array().unwrap();
        assert_eq!(rows.len(), if publication_first { 2 } else { 1 });
        assert_eq!(
            rows.iter().any(|row| row["request_id"] == child.request_id),
            publication_first
        );
        node.shutdown().await;
    }
}
