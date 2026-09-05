//! Real persisted graph invocations; request signatures use the actual target key.
use super::*;
use crate::goal::{set_goal, GoalStatus};
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
    let identity = KeyIdentity::load_or_create(temp.path().join("worker.key"), None).unwrap();
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
    let view = reconcile_graph_run(&node, None, "did:key:owner", &run.run_id)
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
    let view = reconcile_graph_run(&node, None, "did:key:owner", &run.run_id)
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
    let view = reconcile_graph_run(&node, None, "did:key:owner", &run.run_id)
        .await
        .unwrap();
    assert_eq!(view.status, "failed");
    assert_eq!(view.error.as_ref().unwrap()["request_id"], child);
    assert_eq!(
        view.error.as_ref().unwrap()["root_request_id"],
        "graph-logical-root"
    );
    let repeated = reconcile_graph_run(&node, None, "did:key:owner", &run.run_id)
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
        let view = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
        let terminal = reconcile_graph_run(&node, None, "did:key:owner", &run.run_id)
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
                "did:key:owner",
                &run.run_id,
                Some("operator cancelled"),
            )
            .await
            .unwrap()
        } else {
            let closed = reconcile_graph_run(&node, None, "did:key:owner", &run.run_id)
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
    let before = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
    let after = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
        load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
                "did:key:owner",
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
    let closed = reconcile_graph_run(&node, None, "did:key:owner", &run.run_id)
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
    let before = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
    let published = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
        "did:key:owner",
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
    let before = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
    let published = load_graph_run_view(&node, "did:key:owner", &run.run_id)
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
        "did:key:owner",
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
