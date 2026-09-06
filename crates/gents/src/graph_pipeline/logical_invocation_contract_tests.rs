//! Real persisted graph invocations; request signatures use the actual target key.
use super::*;
use crate::goal::{set_goal, GoalStatus};
use crate::graph_pipeline::runtime::graph_test_owner;
use crate::identity::{AgentIdentity, KeyIdentity};
use crate::request_admission::SIGNED_REQUEST_FIELDS;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};

pub(super) async fn execute(node: &defra_node::EmbeddedNode, query: &str) -> serde_json::Value {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()
}

pub(super) async fn signed_invocation_fixture(
    max_invocations: u32,
) -> (
    std::sync::Arc<defra_node::EmbeddedNode>,
    GraphRunReceipt,
    crate::goal::GoalDocument,
    KeyIdentity,
    tempfile::TempDir,
) {
    let (node, run, trigger) = runtime::attribution_test_fixture(max_invocations).await;
    let temp = tempfile::tempdir().unwrap();
    let identity = runtime::graph_test_identity();
    let goal = set_goal(
        &node,
        identity.did(),
        "logical-session",
        Some("Finish the report"),
        Some(GoalStatus::Active),
        Some(Some(1000)),
    )
    .await
    .unwrap();
    let mut root = AgentRequestCreate::base(
        "graph-logical-root",
        identity.did(),
        identity.did(),
        "test-behavior",
        "logical-session",
        "Produce the report",
        "scheduled",
        "2026-08-25T00:00:00Z",
        AgentRequestAdmissionRecord::runtime_automated_trigger(identity.did(), &trigger),
    );
    let trigger_rows = execute(
        &node,
        &format!(
            r#"{{ EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&trigger)
        ),
    )
    .await;
    root.caused_by_trigger_doc_id = Some(
        trigger_rows["EventTrigger"][0]["_docID"]
            .as_str()
            .unwrap()
            .into(),
    );
    root.caused_by_trigger_id = Some(trigger);
    root.caused_by_trigger_kind = Some("event".into());
    root.caused_by_correlation = Some(run.correlation.clone());
    root.caused_by_source_doc_id = Some(run.seed_doc_id.clone());
    crate::sign_agent_request_create(&identity, &mut root)
        .await
        .unwrap();
    execute(&node, &root.graphql_mutation().unwrap()).await;
    execute(&node, r#"mutation { update_AgentRequest(filter: { request_id: { _eq: "graph-logical-root" } }, input: { lifecycle_state: "failed", failure_reason: "provider turn budget exhausted" }) { _docID } }"#).await;

    let persisted = persisted_requests(&node).await;
    assert_eq!(persisted.len(), 1);
    crate::request_admission::verify_request_receipt_signature(&persisted[0]).unwrap();
    assert_eq!(
        persisted[0].caused_by_trigger_doc_id,
        root.caused_by_trigger_doc_id
    );
    (node, run, goal, identity, temp)
}

#[tokio::test]
async fn failed_graph_root_with_active_goal_remains_running_without_failure_latch() {
    let (node, run, goal, _identity, _temp) = signed_invocation_fixture(3).await;
    let view = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(
        view.status, "running",
        "an active Goal keeps its logical invocation outstanding"
    );
    assert!(
        view.error.is_none(),
        "a physical predecessor failure is not a terminal logical failure"
    );
    let durable = execute(&node, &format!("{{ GraphRun {{ status error }} Goal {{ goal_id status }} AgentRequest {{ {SIGNED_REQUEST_FIELDS} }} }}")).await;
    assert_eq!(durable["GraphRun"][0]["status"], "running");
    assert!(durable["GraphRun"][0]["error"].is_null());
    assert_eq!(durable["Goal"][0]["goal_id"], goal.goal_id);
    assert_eq!(durable["Goal"][0]["status"], "active");
    assert_eq!(durable["AgentRequest"][0]["lifecycle_state"], "failed");
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(durable["AgentRequest"][0].clone()).unwrap();
    crate::request_admission::verify_request_receipt_signature(&row).unwrap();
    node.shutdown().await;
}

async fn persisted_requests(
    node: &defra_node::EmbeddedNode,
) -> Vec<gents_protocol::row::AgentRequestRow> {
    let value = execute(
        node,
        &format!("{{ AgentRequest {{ {SIGNED_REQUEST_FIELDS} }} }}"),
    )
    .await;
    serde_json::from_value(value["AgentRequest"].clone()).unwrap()
}

pub(super) async fn prepare_signed_child(
    node: &defra_node::EmbeddedNode,
    identity: &KeyIdentity,
    goal: &crate::goal::GoalDocument,
    variant: &str,
) -> AgentRequestCreate {
    let root = persisted_requests(node)
        .await
        .into_iter()
        .find(|row| row.request_id == "graph-logical-root")
        .unwrap();
    crate::request_admission::verify_request_receipt_signature(&root).unwrap();
    let mut parent = crate::watcher::AgentRequest::try_from(root).unwrap();
    let foreign_temp = tempfile::tempdir().unwrap();
    let foreign =
        KeyIdentity::load_or_create(foreign_temp.path().join("foreign.key"), None).unwrap();
    let signer = if variant == "wrong_child_owner" {
        parent.agent_did = foreign.did().to_owned();
        &foreign
    } else {
        identity
    };
    let mut child = crate::lifecycle::queue::prepare_goal_continuation(
        &parent,
        "test-behavior".into(),
        &goal.goal_id,
        "Finish the report after the prior attempt",
        1,
        false,
        "2026-08-25T00:00:00Z",
    )
    .unwrap();
    if variant == "wrong_physical_parent_edge" {
        child.caused_by_parent_request_doc_id = Some("unrelated-physical-document".into());
    } else if variant == "unrelated_same_correlation" {
        child = AgentRequestCreate::base(
            "unrelated-request",
            identity.did(),
            identity.did(),
            "test-behavior",
            "unrelated-session",
            "Independent work",
            "interactive",
            "2026-08-25T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        child.caused_by_correlation = parent.caused_by_correlation.clone();
    }
    crate::sign_agent_request_create(signer, &mut child)
        .await
        .unwrap();
    child
}

async fn signed_child(
    node: &defra_node::EmbeddedNode,
    identity: &KeyIdentity,
    goal: &crate::goal::GoalDocument,
    variant: &str,
    terminal: Option<&str>,
) -> String {
    let child = prepare_signed_child(node, identity, goal, variant).await;
    execute(node, &child.graphql_mutation().unwrap()).await;
    if let Some(state) = terminal {
        execute(node, &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{state}", failure_reason: "physical tip failed" }}) {{ _docID }} }}"#, child.request_id)).await;
    }
    let row = persisted_requests(node)
        .await
        .into_iter()
        .find(|row| row.request_id == child.request_id)
        .unwrap();
    crate::request_admission::verify_request_receipt_signature(&row).unwrap();
    child.request_id
}

#[tokio::test]
async fn successful_signed_goal_child_completes_graph_invocation() {
    let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
    let child = signed_child(&node, &identity, &goal, "valid", Some("completed")).await;
    execute(&node, &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "complete" }}) {{ _docID }} create_PipelineResult(input: {{ graph_run_id: "{}", report: "finished" }}) {{ _docID }} }}"#, goal.doc_id, run.correlation)).await;
    let view = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(view.status, "succeeded");
    assert!(view.error.is_none());
    assert_eq!(view.outstanding_invocation_count, 0);
    assert!(view.terminal_stages_completed);
    assert_eq!(view.stages[0].failed, 1);
    assert_eq!(view.stages[0].succeeded, 1);
    assert_eq!(
        view.requests.len(),
        2,
        "both physical attempts remain observable"
    );
    assert!(view
        .requests
        .iter()
        .any(|request| request.request_id == child));
    node.shutdown().await;
}

#[tokio::test]
async fn paused_goal_failure_uses_authenticated_tip_cause() {
    let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
    let child = signed_child(&node, &identity, &goal, "valid", Some("failed")).await;
    execute(&node, &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "paused" }}) {{ _docID }} }}"#, goal.doc_id)).await;
    let view = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(view.status, "failed");
    assert_eq!(view.error.as_ref().unwrap()["request_id"], child);
    assert_eq!(
        view.error.as_ref().unwrap()["root_request_id"],
        "graph-logical-root"
    );
    let repeated = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(repeated.error, view.error);
    node.shutdown().await;
}

#[derive(serde::Deserialize)]
struct Contracts {
    graph_logical_invocation_cases: Vec<InvocationCase>,
}
#[derive(serde::Deserialize)]
struct InvocationCase {
    name: String,
    rows: Vec<CaseRow>,
    edges: Vec<CaseEdge>,
    root: u64,
    goal: Option<serde_json::Value>,
    result_satisfied: bool,
    max_invocations: u32,
    expected: serde_json::Value,
}
#[derive(serde::Deserialize)]
struct CaseEdge {
    parent: u64,
    child: u64,
    authenticated: bool,
}
#[derive(serde::Deserialize)]
struct CaseRow {
    doc: u64,
    pinned_root: bool,
    terminal: Option<String>,
}

#[tokio::test]
async fn generated_graph_logical_invocations_drive_persisted_run_projection() {
    let snapshot: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(snapshot.graph_logical_invocation_cases.len(), 15);
    for case in snapshot.graph_logical_invocation_cases {
        assert_eq!(case.root, 10, "{}", case.name);
        let (node, run, goal, identity, _temp) =
            signed_invocation_fixture(case.max_invocations).await;
        let mut request_ids =
            std::collections::BTreeMap::from([(10, "graph-logical-root".to_owned())]);
        for row in &case.rows {
            if row.pinned_root {
                assert_eq!(row.doc, 10);
                assert_eq!(row.terminal.as_deref(), Some("failed"));
                continue;
            }
            let mut original_goal = goal.clone();
            if row.doc == 30 || case.name == "historical_head_canonical_goal_mismatch" {
                // Historical replacement Goal identities produce different durable
                // retry keys. Each original signed physical edge remains causal.
                original_goal.goal_id = "historical-other-goal".into();
            }
            let child = signed_child(
                &node,
                &identity,
                &original_goal,
                &case.name,
                row.terminal.as_deref(),
            )
            .await;
            request_ids.insert(row.doc, child);
        }
        if case.name == "same_second_child_before_root"
            || case.name == "historical_head_canonical_goal_mismatch"
        {
            let ordered = execute(&node, "{ AgentRequest(order: [{ created_at: ASC }, { request_id: ASC }]) { request_id created_at } }").await;
            assert_eq!(ordered["AgentRequest"][0]["request_id"], request_ids[&20]);
            assert_eq!(ordered["AgentRequest"][1]["request_id"], request_ids[&10]);
            assert_eq!(
                ordered["AgentRequest"][0]["created_at"],
                ordered["AgentRequest"][1]["created_at"]
            );
            assert_eq!(
                case.rows.iter().map(|row| row.doc).collect::<Vec<_>>(),
                vec![20, 10]
            );
        }
        let persisted = persisted_requests(&node).await;
        assert_eq!(
            persisted.len(),
            case.rows.len(),
            "{} raw fixture rows",
            case.name
        );
        for edge in &case.edges {
            let parent = persisted
                .iter()
                .find(|row| row.request_id == request_ids[&edge.parent])
                .unwrap();
            let child = persisted
                .iter()
                .find(|row| row.request_id == request_ids[&edge.child])
                .unwrap();
            let original_goal =
                if edge.child == 30 || case.name == "historical_head_canonical_goal_mismatch" {
                    "historical-other-goal"
                } else {
                    &goal.goal_id
                };
            assert_eq!(
                crate::goal::verify_goal_continuation_edge(
                    identity.did(),
                    "logical-session",
                    original_goal,
                    parent,
                    child
                )
                .is_ok(),
                edge.authenticated,
                "{} signed edge",
                case.name
            );
            if edge.authenticated {
                assert_eq!(
                    child.caused_by_parent_request_doc_id.as_deref(),
                    parent.doc_id.as_deref()
                );
            }
        }
        if let Some(goal_case) = &case.goal {
            let status = goal_case["status"].as_str().unwrap();
            let claimed = goal_case["phase"] == "claimed";
            let watermark = if claimed {
                "\"graph-logical-root\""
            } else {
                "null"
            };
            execute(&node, &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "{status}", continuation_sequence: {}, last_continued_from_request_id: {watermark}, wrapup_requested: {}, wrapup_completed: {} }}) {{ _docID }} }}"#, goal.doc_id, i64::from(claimed), goal_case["wrapup_requested"], goal_case["wrapup_completed"])).await;
        } else {
            execute(&node, &format!(r#"mutation {{ delete_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#, goal.doc_id)).await;
        }
        if case.result_satisfied {
            execute(&node, &format!(r#"mutation {{ create_PipelineResult(input: {{ graph_run_id: "{}", report: "finished" }}) {{ _docID }} }}"#, run.correlation)).await;
        }
        let view = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        assert_eq!(
            view.requests.len(),
            case.expected["physical_count"].as_u64().unwrap() as usize,
            "{} physical membership",
            case.name
        );
        let expected = case.expected["outcome"].as_str().unwrap();
        assert_eq!(
            view.outstanding_invocation_count,
            usize::from(expected == "outstanding"),
            "{} logical outstanding",
            case.name
        );
        assert_eq!(
            view.stages.iter().map(|stage| stage.total).sum::<usize>(),
            view.requests.len(),
            "{} physical stage accounting",
            case.name
        );
        match expected {
            "outstanding" => {
                assert!(
                    view.failure_evidence.is_none(),
                    "{}: {:?}",
                    case.name,
                    view.failure_evidence
                );
            }
            "succeeded" => assert!(
                view.failure_evidence.is_none(),
                "{}: {:?}",
                case.name,
                view.failure_evidence
            ),
            "failed" => {
                let failure = view.failure_evidence.as_ref().expect(&case.name);
                if case.name == "successful_tip_missing_result" {
                    assert_eq!(
                        failure["code"], "result_contract_unsatisfied",
                        "{}",
                        case.name
                    );
                } else {
                    let tip = case.expected["tip"].as_u64().unwrap();
                    assert_eq!(failure["request_id"], request_ids[&tip], "{}", case.name);
                }
            }
            "invalid" => assert_eq!(
                view.failure_evidence.as_ref().expect(&case.name)["code"],
                "contract_drift",
                "{}",
                case.name
            ),
            "limit_exceeded" => assert_eq!(
                view.failure_evidence.as_ref().expect(&case.name)["code"],
                "invocation_limit_exceeded",
                "{}",
                case.name
            ),
            other => panic!("unknown outcome {other}"),
        }
        let terminal = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        let status = match expected {
            "outstanding" => "running",
            "succeeded" => "succeeded",
            _ => "failed",
        };
        assert_eq!(terminal.status, status, "{}", case.name);
        node.shutdown().await;
    }
}

async fn publication_state(node: &defra_node::EmbeddedNode) -> serde_json::Value {
    execute(node, "{ GraphRun { status error update_generation cancel_requested_at } Goal { _docID status continuation_sequence last_continued_from_request_id updated_at } AgentRequest { _docID request_id lifecycle_state } }").await
}

#[tokio::test]
async fn typed_resume_obeys_cancelled_or_failed_graph_fence() {
    for cancel in [true, false] {
        let (node, run, _goal, identity, _temp) = signed_invocation_fixture(3).await;
        set_goal(
            &node,
            identity.did(),
            "logical-session",
            None,
            Some(GoalStatus::Paused),
            None,
        )
        .await
        .unwrap();
        let fenced = if cancel {
            request_graph_run_cancellation(
                &node,
                None,
                graph_test_owner(),
                &run.run_id,
                Some("operator cancelled"),
            )
            .await
            .unwrap()
        } else {
            let closed = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
                .await
                .unwrap();
            assert_eq!(closed.status, "failed");
            assert_eq!(
                closed.error.as_ref().unwrap()["request_id"],
                "graph-logical-root"
            );
            closed
        };
        if cancel {
            assert!(fenced.cancellation_requested_at.is_some());
        }
        let before = publication_state(&node).await;
        let result = crate::goal::resume_goal_request(
            &crate::ConfigAccess::Local(node.clone()),
            &identity,
            identity.did(),
            "logical-session",
            "graph-logical-root",
        )
        .await;
        let error = result.expect_err("a fenced graph must reject typed child publication");
        assert!(
            error.to_string().contains("graph run"),
            "unexpected pre-fence rejection: {error:#}"
        );
        assert_eq!(
            publication_state(&node).await,
            before,
            "resume must roll back Goal and child together"
        );
        assert_eq!(before["Goal"][0]["status"], "paused");
        assert_eq!(before["AgentRequest"].as_array().unwrap().len(), 1);
        node.shutdown().await;
    }
}

#[tokio::test]
async fn typed_resume_advances_graph_generation_with_signed_child() {
    let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
    set_goal(
        &node,
        identity.did(),
        "logical-session",
        None,
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let before = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let receipt = crate::goal::resume_goal_request(
        &crate::ConfigAccess::Local(node.clone()),
        &identity,
        identity.did(),
        "logical-session",
        "graph-logical-root",
    )
    .await
    .unwrap();
    assert!(receipt.created);
    let after = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(after.update_generation, before.update_generation + 1);
    assert_eq!(after.status, "running");
    assert!(after.error.is_none());
    let rows = persisted_requests(&node).await;
    assert_eq!(rows.len(), 2);
    let parent = rows
        .iter()
        .find(|row| row.request_id == "graph-logical-root")
        .unwrap();
    let child = rows
        .iter()
        .find(|row| row.request_id == receipt.request_id)
        .unwrap();
    crate::goal::verify_goal_continuation_edge(
        identity.did(),
        "logical-session",
        &goal.goal_id,
        parent,
        child,
    )
    .unwrap();
    let resumed = crate::goal::load_goal_by_id(&node, identity.did(), &goal.goal_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(resumed.continuation_sequence(), 1);
    // Acknowledgment retry must not acquire another graph generation.
    let repeated = crate::goal::resume_goal_request(
        &crate::ConfigAccess::Local(node.clone()),
        &identity,
        identity.did(),
        "logical-session",
        "graph-logical-root",
    )
    .await
    .unwrap();
    assert!(!repeated.created);
    assert_eq!(repeated.request_id, receipt.request_id);
    assert_eq!(
        load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap()
            .update_generation,
        after.update_generation
    );
    node.shutdown().await;
}

#[tokio::test]
async fn automatic_claimed_publication_obeys_graph_fence() {
    for cancel in [false, true] {
        let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
        // A persisted successful claim, before automatic child publication.
        execute(&node, &format!(r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ continuation_sequence: 1, last_continued_from_request_id: "graph-logical-root" }}) {{ _docID }} }}"#, goal.doc_id)).await;
        let observed = crate::goal::load_goal_by_id(&node, identity.did(), &goal.goal_id)
            .await
            .unwrap()
            .unwrap();
        if cancel {
            request_graph_run_cancellation(
                &node,
                None,
                graph_test_owner(),
                &run.run_id,
                Some("stop before publication"),
            )
            .await
            .unwrap();
        }
        let before = publication_state(&node).await;
        let result = crate::goal::publish_claimed_continuation(
            &node,
            &observed,
            "graph-logical-root",
            "Finish the report",
            false,
        )
        .await;
        if cancel {
            let error = result.expect_err("cancelled graph must reject automatic publication");
            assert!(
                error.to_string().contains("graph run"),
                "unexpected pre-fence rejection: {error:#}"
            );
            assert_eq!(publication_state(&node).await, before);
        } else {
            let receipt = result.unwrap().expect("live claimed publication");
            assert!(receipt.created);
            let after = publication_state(&node).await;
            assert_eq!(
                after["GraphRun"][0]["update_generation"].as_i64().unwrap(),
                before["GraphRun"][0]["update_generation"].as_i64().unwrap() + 1
            );
            assert_eq!(after["AgentRequest"].as_array().unwrap().len(), 2);
            assert_eq!(after["Goal"][0]["continuation_sequence"], 1);
            let rows = persisted_requests(&node).await;
            let parent = rows
                .iter()
                .find(|row| row.request_id == "graph-logical-root")
                .unwrap();
            let child = rows
                .iter()
                .find(|row| row.request_id == receipt.request_id)
                .unwrap();
            crate::goal::verify_goal_continuation_edge(
                identity.did(),
                "logical-session",
                &goal.goal_id,
                parent,
                child,
            )
            .unwrap();
        }
        node.shutdown().await;
    }
}

#[tokio::test]
async fn fresh_goal_on_closed_rooted_session_rolls_back() {
    let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
    set_goal(
        &node,
        identity.did(),
        "logical-session",
        None,
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let closed = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(closed.status, "failed");
    execute(
        &node,
        &format!(
            r#"mutation {{ delete_Goal(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            goal.doc_id
        ),
    )
    .await;
    let before = publication_state(&node).await;
    assert!(before["Goal"].as_array().unwrap().is_empty());
    let error = set_goal(
        &node,
        identity.did(),
        "logical-session",
        Some("Finish the report"),
        Some(GoalStatus::Active),
        Some(Some(1000)),
    )
    .await
    .expect_err("closed graph cannot acquire a fresh continuation obligation");
    assert!(
        error.to_string().contains("graph run"),
        "unexpected pre-fence rejection: {error:#}"
    );
    assert_eq!(publication_state(&node).await, before);
    node.shutdown().await;
}

async fn root_trigger_lineage(
    node: &defra_node::EmbeddedNode,
) -> (crate::lifecycle::TriggerLineage, String) {
    let rows = persisted_requests(node).await;
    let root = rows
        .iter()
        .find(|row| row.request_id == "graph-logical-root")
        .unwrap();
    (
        crate::lifecycle::TriggerLineage {
            trigger_id: root.caused_by_trigger_id.clone(),
            trigger_kind: root.caused_by_trigger_kind.clone(),
            source_doc_id: root.caused_by_source_doc_id.clone(),
            correlation: root.caused_by_correlation.clone(),
            trigger_context: root.caused_by_trigger_context.clone(),
        },
        root.caused_by_trigger_doc_id.clone().unwrap(),
    )
}

#[tokio::test]
async fn plain_graph_root_factory_fences_publication_and_cancellation() {
    let (node, run, _goal, identity, _temp) = signed_invocation_fixture(4).await;
    assert_eq!(
        run.run_id, run.correlation,
        "actual graph producer correlation identifies its run"
    );
    let (lineage, trigger_doc) = root_trigger_lineage(&node).await;
    let before = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let receipt = crate::lifecycle::materialize::write_pending_agent_request_with_lineage_workspace_and_conversation_title(
        &node, identity.did(), "test-behavior", "Plain graph root", crate::lifecycle::ExecutionOrigin::Scheduled,
        lineage.clone(), None, None, Some("plain-factory-root"), None, Some(&trigger_doc),
    ).await.unwrap();
    let rows = persisted_requests(&node).await;
    assert_eq!(rows.len(), 2);
    let row = rows
        .iter()
        .find(|row| row.request_id == receipt.request_id)
        .unwrap();
    crate::request_admission::verify_request_receipt_signature(row).unwrap();
    assert_eq!(row.doc_id.as_deref(), Some(receipt.doc_id.as_str()));
    assert_eq!(
        row.caused_by_correlation.as_deref(),
        Some(run.run_id.as_str())
    );
    assert_eq!(
        row.caused_by_trigger_doc_id.as_deref(),
        Some(trigger_doc.as_str())
    );
    assert_eq!(
        row.caused_by_source_doc_id.as_deref(),
        Some(run.seed_doc_id.as_str())
    );
    let published = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(published.update_generation, before.update_generation + 1);
    assert_eq!(
        published.requests.len(),
        2,
        "factory row is an authenticated graph root"
    );
    // Emulate a durable executor observation before cancellation, without a
    // provider or synthetic lease owner in this publication-boundary test.
    execute(&node, r#"mutation { update_AgentRequest(filter: { request_id: { _eq: "plain-factory-root" } }, input: { lifecycle_state: "completed" }) { _docID } }"#).await;
    request_graph_run_cancellation(
        &node,
        None,
        graph_test_owner(),
        &run.run_id,
        Some("stop new roots"),
    )
    .await
    .unwrap();
    let closed = publication_state(&node).await;
    let error = crate::lifecycle::materialize::write_pending_agent_request_with_lineage_workspace_and_conversation_title(
        &node, identity.did(), "test-behavior", "Too late", crate::lifecycle::ExecutionOrigin::Scheduled,
        lineage, None, None, Some("denied-plain-factory-root"), None, Some(&trigger_doc),
    ).await.expect_err("cancelled graph rejects the actual root factory");
    assert!(
        error.to_string().contains("graph run"),
        "unexpected factory failure: {error:#}"
    );
    assert_eq!(publication_state(&node).await, closed);
    node.shutdown().await;
}

#[tokio::test]
async fn goal_backed_graph_root_factory_rolls_back_after_cancellation() {
    let (node, run, _goal, identity, _temp) = signed_invocation_fixture(4).await;
    assert_eq!(run.run_id, run.correlation);
    let (lineage, trigger_doc) = root_trigger_lineage(&node).await;
    let before = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let create = crate::lifecycle::materialize::build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
        identity.did(), "test-behavior", "Goal-backed graph root", crate::lifecycle::ExecutionOrigin::Scheduled,
        lineage.clone(), None, None, "goal-backed-factory-root", "goal-backed-factory-session", Some("goal-backed-factory-key"), None, Some(&trigger_doc),
    ).await.unwrap();
    let receipt = crate::goal::submit_goal_backed_request_local(
        &node,
        identity.did(),
        &create.session_id,
        "Finish the extra stage",
        Some(1000),
        &create,
    )
    .await
    .unwrap();
    let rows = persisted_requests(&node).await;
    let row = rows
        .iter()
        .find(|row| row.request_id == receipt.request_id)
        .unwrap();
    crate::request_admission::verify_request_receipt_signature(row).unwrap();
    assert_eq!(
        row.caused_by_correlation.as_deref(),
        Some(run.run_id.as_str())
    );
    let published = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(published.update_generation, before.update_generation + 1);
    assert_eq!(published.requests.len(), 2);
    let obligations = execute(
        &node,
        "{ Goal { session_id } GoalCreationClaim { session_id } }",
    )
    .await;
    assert!(obligations["Goal"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["session_id"] == create.session_id));
    assert!(obligations["GoalCreationClaim"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["session_id"] == create.session_id));
    execute(&node, r#"mutation { update_AgentRequest(filter: { request_id: { _eq: "goal-backed-factory-root" } }, input: { lifecycle_state: "completed" }) { _docID } }"#).await;
    request_graph_run_cancellation(
        &node,
        None,
        graph_test_owner(),
        &run.run_id,
        Some("close before next goal"),
    )
    .await
    .unwrap();
    let closed = publication_state(&node).await;
    let claims = execute(&node, "{ GoalCreationClaim { _docID session_id } }").await;
    let denied = crate::lifecycle::materialize::build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
        identity.did(), "test-behavior", "Too late goal", crate::lifecycle::ExecutionOrigin::Scheduled,
        lineage, None, None, "denied-goal-backed-root", "denied-goal-backed-session", Some("denied-goal-backed-key"), None, Some(&trigger_doc),
    ).await.unwrap();
    let error = crate::goal::submit_goal_backed_request_local(
        &node,
        identity.did(),
        &denied.session_id,
        "Do not create this obligation",
        Some(1000),
        &denied,
    )
    .await
    .expect_err("closed graph rejects atomic root and Goal publication");
    assert!(
        error.to_string().contains("graph run"),
        "unexpected submission failure: {error:#}"
    );
    assert_eq!(publication_state(&node).await, closed);
    assert_eq!(
        execute(&node, "{ GoalCreationClaim { _docID session_id } }").await,
        claims
    );
    node.shutdown().await;
}

#[tokio::test]
async fn signed_foreign_graph_roots_cannot_change_invocation_or_failure() {
    let (node, run, _goal, _identity, _temp) = signed_invocation_fixture(1).await;
    let before = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    let root = persisted_requests(&node).await.remove(0);
    let temp = tempfile::tempdir().unwrap();
    let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap();
    for (name, signer, behavior, state) in [
        ("foreign-completed", &foreign, "test-behavior", "completed"),
        ("foreign-failed", &foreign, "test-behavior", "failed"),
    ] {
        let trigger = root.caused_by_trigger_id.as_deref().unwrap();
        let mut create = AgentRequestCreate::base(
            name,
            signer.did(),
            signer.did(),
            behavior,
            "foreign-session",
            "Copied graph route",
            "scheduled",
            "2026-08-26T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(signer.did(), trigger),
        );
        create.caused_by_trigger_id = root.caused_by_trigger_id.clone();
        create.caused_by_trigger_doc_id = root.caused_by_trigger_doc_id.clone();
        create.caused_by_trigger_kind = root.caused_by_trigger_kind.clone();
        create.caused_by_correlation = root.caused_by_correlation.clone();
        create.caused_by_source_doc_id = root.caused_by_source_doc_id.clone();
        crate::sign_agent_request_create(signer, &mut create)
            .await
            .unwrap();
        // The actual publication fence rejects the correctly signed foreign
        // target; direct DB seeding below models a replicated untrusted row.
        let txn = crate::config_client::ConfigApplyTxn::begin_local(&node, None)
            .await
            .unwrap();
        assert!(runtime::fence_graph_root_request_in_txn(&txn, &create)
            .await
            .is_err());
        txn.discard().await.unwrap();
        execute(&node, &create.graphql_mutation().unwrap()).await;
        execute(&node, &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{name}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#)).await;
    }
    for row in persisted_requests(&node)
        .await
        .into_iter()
        .filter(|row| row.request_id != "graph-logical-root")
    {
        crate::request_admission::verify_request_receipt_signature(&row).unwrap();
        let txn = crate::config_client::ConfigApplyTxn::begin_local(&node, None)
            .await
            .unwrap();
        assert!(
            graph_binding_for_request_in_txn(&txn, row.doc_id.as_deref().unwrap())
                .await
                .unwrap()
                .is_none()
        );
        txn.discard().await.unwrap();
    }
    let after = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(after.requests, before.requests);
    assert_eq!(after.outstanding_invocation_count, 1);
    assert_eq!(after.status, "running");
    assert!(after.error.is_none());
    assert!(after.failure_evidence.is_none());
    assert_eq!(after.update_generation, before.update_generation);
}

#[tokio::test]
async fn unrelated_interactive_head_preserves_graph_goal_obligation() {
    let (node, run, _goal, identity, _temp) = signed_invocation_fixture(3).await;
    let mut interactive = AgentRequestCreate::base(
        "later-interactive",
        identity.did(),
        identity.did(),
        "test-behavior",
        "logical-session",
        "Unrelated question",
        "interactive",
        "2026-08-27T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    crate::sign_agent_request_create(&identity, &mut interactive)
        .await
        .unwrap();
    execute(&node, &interactive.graphql_mutation().unwrap()).await;
    execute(&node, r#"mutation { update_AgentRequest(filter: { request_id: { _eq: "later-interactive" } }, input: { lifecycle_state: "completed" }) { _docID } }"#).await;
    let view = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(view.outstanding_invocation_count, 1);
    assert_eq!(view.status, "running");
    assert!(view.error.is_none());
    assert!(view.failure_evidence.is_none());
}

#[tokio::test]
async fn malformed_reserved_graph_trigger_cannot_publish() {
    let (node, run, _goal, identity, _temp) = signed_invocation_fixture(3).await;
    let mut create = AgentRequestCreate::base(
        "malformed-route",
        identity.did(),
        identity.did(),
        "test-behavior",
        "logical-session",
        "Malformed reserved route",
        "scheduled",
        "2026-08-27T00:00:00Z",
        AgentRequestAdmissionRecord::runtime_automated_trigger(
            identity.did(),
            "graph-trigger-malformed",
        ),
    );
    create.caused_by_trigger_id = Some("graph-trigger-malformed".into());
    create.caused_by_correlation = Some(run.correlation.clone());
    let txn = crate::config_client::ConfigApplyTxn::begin_local(&node, None)
        .await
        .unwrap();
    assert!(runtime::fence_graph_root_request_in_txn(&txn, &create)
        .await
        .unwrap_err()
        .to_string()
        .contains("malformed reserved"));
    txn.discard().await.unwrap();
    assert!(derive_graph_workspace(
        node.as_ref(),
        "graph-trigger-malformed",
        Some(&run.correlation),
        graph_test_owner(),
        Some(&run.seed_doc_id),
        &crate::lifecycle::WorkspaceLineage::default(),
    )
    .await
    .err()
    .expect("malformed reserved preflight must fail")
    .to_string()
    .contains("malformed reserved"));
}

#[tokio::test]
async fn bundled_package_root_binding_survives_task_metadata_changes() {
    use crate::config_client::{ConfigAccess, ConfigApplyTxn};
    use crate::graph_package::{install_bundled_graph_package, GraphPackageInstallBindings};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let identity = runtime::graph_test_identity();
    crate::document_config::ensure_agent_principal(&node, identity.did())
        .await
        .unwrap();
    for mutation in [
        r#"mutation { create_HostDeployment(input: { deployment_id: "graph-test-host", display_name: "Graph test" }) { _docID } }"#,
        r#"mutation { create_InferenceBackend(input: { backend_id: "graph-test-backend", name: "Graph test", provider_kind: "OpenAiCompatible", endpoint: "http://127.0.0.1:1/v1", max_concurrent: 4, enabled: true, models: ["test-model"] }) { _docID } }"#,
        r#"mutation { create_InferenceProfile(input: { profile_id: "graph-test-profile", display_name: "Graph test", max_turns: 8 }) { _docID } }"#,
    ] {
        execute(&node, mutation).await;
    }
    let role = PackageRoleBinding {
        principal_did: identity.did().into(),
        deployment_id: "graph-test-host".into(),
        backend_id: Some("graph-test-backend".into()),
        profile_id: Some("graph-test-profile".into()),
        model_name: Some("test-model".into()),
    };
    let bindings = GraphPackageInstallBindings {
        owner_did: identity.did().into(),
        roles: BTreeMap::from([
            ("coordinator".into(), role.clone()),
            ("reviewer".into(), role),
        ]),
    };
    let access = ConfigAccess::Local(node.clone());
    let installed =
        install_bundled_graph_package(&access, identity.did(), "code_review", &bindings)
            .await
            .unwrap();
    activate_graph_revision(
        &node,
        None,
        identity.did(),
        &installed.graph_id,
        &installed.revision_digest,
        None,
    )
    .await
    .unwrap();
    let run = start_graph_run(&node, None, identity.did(), &installed.graph_id, None, "review",
        serde_json::json!({"repository_path": "/tmp/repo", "base_ref": "base-sha", "head_ref": "head-sha", "lens_count": "4", "lens_min": "4", "lens_max": "4", "focus": "authorization"})).await.unwrap();
    let trigger =
        runtime::graph_trigger_id(&installed.revision_digest, "entry:review:recon:job").unwrap();
    let route = execute(
        &node,
        &format!(
            r#"{{ EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}) {{ _docID task_id }} }}"#,
            crate::graphql::escape_graphql_string(&trigger)
        ),
    )
    .await;
    let task_id = route["EventTrigger"][0]["task_id"].as_str().unwrap();
    let task = execute(
        &node,
        &format!(
            r#"{{ Task(filter: {{ task_id: {{ _eq: "{}" }} }}) {{ behavior_id }} }}"#,
            crate::graphql::escape_graphql_string(task_id)
        ),
    )
    .await;
    let behavior = task["Task"][0]["behavior_id"].as_str().unwrap();
    let mut request = AgentRequestCreate::base(
        "bundled-root",
        identity.did(),
        identity.did(),
        behavior,
        "bundled-session",
        "Inspect repository",
        "scheduled",
        "2026-08-27T00:00:00Z",
        AgentRequestAdmissionRecord::runtime_automated_trigger(identity.did(), &trigger),
    );
    request.caused_by_trigger_id = Some(trigger.clone());
    request.caused_by_trigger_doc_id =
        Some(route["EventTrigger"][0]["_docID"].as_str().unwrap().into());
    request.caused_by_trigger_kind = Some("event".into());
    request.caused_by_correlation = Some(run.correlation.clone());
    request.caused_by_source_doc_id = Some(run.seed_doc_id.clone());
    crate::sign_agent_request_create(&identity, &mut request)
        .await
        .unwrap();
    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    runtime::fence_graph_root_request_in_txn(&txn, &request)
        .await
        .unwrap();
    txn.execute(&request.graphql_mutation().unwrap())
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let admitted = load_graph_run_view(&node, identity.did(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(admitted.requests.len(), 1);
    assert_eq!(admitted.requests[0].node_id.as_deref(), Some("recon"));
    assert!(admitted.failure_evidence.is_none());
    execute(&node, &format!(r#"mutation {{ update_Task(filter: {{ task_id: {{ _eq: "{}" }} }}, input: {{ enabled: false }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(task_id))).await;
    let disabled = load_graph_run_view(&node, identity.did(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(disabled.requests, admitted.requests);
    assert!(
        disabled.failure_evidence.is_none(),
        "disable cannot erase a historical root's identity"
    );
    execute(&node, &format!(r#"mutation {{ update_Task(filter: {{ task_id: {{ _eq: "{}" }} }}, input: {{ behavior_id: "unapproved-task-target" }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(task_id))).await;
    let changed = load_graph_run_view(&node, identity.did(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(
        changed.requests, admitted.requests,
        "mutable Task metadata cannot erase owner-signed history"
    );
    assert!(changed.failure_evidence.is_none());
    let cancelling = request_graph_run_cancellation(
        &node,
        None,
        identity.did(),
        &run.run_id,
        Some("operator can still cancel after config changes"),
    )
    .await
    .unwrap();
    assert!(cancelling.cancellation_requested_at.is_some());
    drop(access);
    Arc::try_unwrap(node)
        .unwrap_or_else(|_| panic!("retained fixture node"))
        .shutdown()
        .await;
}

#[tokio::test]
async fn replacement_goal_on_other_authenticated_chain_does_not_attach_to_old_root() {
    let (node, run, goal, identity, _temp) = signed_invocation_fixture(3).await;
    let mut parent = AgentRequestCreate::base(
        "other-goal-parent",
        identity.did(),
        identity.did(),
        "test-behavior",
        "logical-session",
        "A different goal",
        "interactive",
        "2026-08-28T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    crate::sign_agent_request_create(&identity, &mut parent)
        .await
        .unwrap();
    execute(&node, &parent.graphql_mutation().unwrap()).await;
    execute(&node, r#"mutation { update_AgentRequest(filter: { request_id: { _eq: "other-goal-parent" } }, input: { lifecycle_state: "completed" }) { _docID } }"#).await;
    let parent_row = persisted_requests(&node)
        .await
        .into_iter()
        .find(|row| row.request_id == "other-goal-parent")
        .unwrap();
    let parent = crate::watcher::AgentRequest::try_from(parent_row).unwrap();
    let mut child = crate::lifecycle::queue::prepare_goal_continuation(
        &parent,
        "test-behavior".into(),
        "replacement-canonical-goal",
        "Continue the unrelated goal",
        1,
        false,
        "2026-08-28T00:00:00Z",
    )
    .unwrap();
    crate::sign_agent_request_create(&identity, &mut child)
        .await
        .unwrap();
    execute(&node, &child.graphql_mutation().unwrap()).await;
    execute(&node, &format!(r#"mutation {{ update_Goal(filter: {{ goal_id: {{ _eq: "{}" }} }}, input: {{ goal_id: "replacement-canonical-goal", status: "active" }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(&goal.goal_id))).await;
    let view = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(
        view.outstanding_invocation_count, 0,
        "the current Goal belongs to the other authenticated chain"
    );
    assert_eq!(
        view.failure_evidence.as_ref().unwrap()["request_id"],
        "graph-logical-root"
    );
}

#[tokio::test]
async fn generic_graph_foreign_roots_remain_ignored_after_reassignment_or_missing_task() {
    for missing_task in [false, true] {
        let (node, run, trigger) = runtime::attribution_test_fixture(1).await;
        let before = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let foreign = KeyIdentity::load_or_create(dir.path().join("foreign.key"), None).unwrap();
        let routes = execute(&node, &format!(r#"{{ EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}) {{ _docID task_id }} }}"#, crate::graphql::escape_graphql_string(&trigger))).await;
        if missing_task {
            let task = routes["EventTrigger"][0]["task_id"].as_str().unwrap();
            execute(&node, &format!(r#"mutation {{ delete_Task(filter: {{ task_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(task))).await;
        } else {
            execute(&node, &format!(r#"mutation {{ update_AgentBehavior(filter: {{ behavior_id: {{ _eq: "test-behavior" }} }}, input: {{ agent_did: "{}" }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(foreign.did()))).await;
        }
        let mut request = AgentRequestCreate::base(
            "foreign-root",
            foreign.did(),
            foreign.did(),
            "test-behavior",
            "foreign-session",
            "Copied owner route",
            "scheduled",
            "2026-08-28T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(foreign.did(), &trigger),
        );
        request.caused_by_trigger_id = Some(trigger.clone());
        request.caused_by_trigger_doc_id =
            Some(routes["EventTrigger"][0]["_docID"].as_str().unwrap().into());
        request.caused_by_trigger_kind = Some("event".into());
        request.caused_by_correlation = Some(run.correlation.clone());
        request.caused_by_source_doc_id = Some(run.seed_doc_id.clone());
        crate::sign_agent_request_create(&foreign, &mut request)
            .await
            .unwrap();
        let txn = crate::config_client::ConfigApplyTxn::begin_local(&node, None)
            .await
            .unwrap();
        let denial = runtime::fence_graph_root_request_in_txn(&txn, &request)
            .await
            .unwrap_err();
        assert!(
            denial
                .to_string()
                .contains("principal is not its pinned owner"),
            "{denial:#}"
        );
        txn.discard().await.unwrap();
        execute(&node, &request.graphql_mutation().unwrap()).await;
        execute(&node, r#"mutation { update_AgentRequest(filter: { request_id: { _eq: "foreign-root" } }, input: { lifecycle_state: "failed", failure_reason: "must not poison owner graph" }) { _docID } }"#).await;
        let rows = persisted_requests(&node).await;
        assert_eq!(rows.len(), 1);
        crate::request_admission::verify_request_receipt_signature(&rows[0]).unwrap();
        let after = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        assert_eq!(
            after.requests, before.requests,
            "foreign rows must be rejected before invalid-target fallback"
        );
        assert_eq!(after.status, "running");
        assert_eq!(after.update_generation, before.update_generation);
        assert!(after.error.is_none());
        assert!(after.failure_evidence.is_none());
    }
}

#[tokio::test]
async fn generic_task_behavior_changes_cannot_erase_active_owner_signed_root() {
    let (node, run, trigger) = runtime::attribution_test_fixture(2).await;
    runtime::seed_signed_graph_request(&node, &run, &trigger, "active-root", "processing", "")
        .await;
    let before = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(before.active_request_count, 1);
    execute(&node, &format!(r#"mutation {{ create_AgentBehavior(input: {{ behavior_id: "other-owner-behavior", agent_did: "{}", enabled: true }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(graph_test_owner()))).await;
    execute(&node, r#"mutation { update_Task(filter: { behavior_id: { _eq: "test-behavior" } }, input: { behavior_id: "other-owner-behavior", enabled: false, name: "renamed task" }) { _docID } }"#).await;
    let changed = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(changed.requests, before.requests);
    assert_eq!(changed.active_request_count, 1);
    assert_eq!(changed.status, "running");
    assert_eq!(changed.update_generation, before.update_generation);
    assert!(changed.error.is_none());
    assert!(changed.failure_evidence.is_none());
    let cancelling = request_graph_run_cancellation(
        &node,
        None,
        graph_test_owner(),
        &run.run_id,
        Some("still drains actual historical work"),
    )
    .await
    .unwrap();
    assert_eq!(cancelling.requests.len(), 1);
    assert!(cancelling.cancellation_requested_at.is_some());
    let rows = execute(&node, r#"{ AgentRequest(filter: { request_id: { _eq: "active-root" } }) { interrupt_requested_at } }"#).await;
    assert!(rows["AgentRequest"][0]["interrupt_requested_at"].is_string());
}
