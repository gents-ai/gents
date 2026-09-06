//! Generated cases exercise the actual native transaction and publication owner.
use super::*;
use crate::identity::AgentIdentity;
use crate::lifecycle::materialize::{sign_request, RequestSigner};
use crate::lifecycle::queue::{goal_continuation_identity, prepare_goal_continuation};
use crate::request_admission::SIGNED_REQUEST_FIELDS;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::json;

// Reuse the existing real-DB Goal fixture and independent signed-field checks.
#[path = "../operator_resume/support.rs"]
mod support;
use support::*;

#[derive(Deserialize)]
struct PublicationContracts {
    goal_claimed_publication_cases: Vec<ResumeCase>,
}

#[tokio::test]
async fn generated_goal_claimed_publication_cases_drive_real_transactions() {
    let contracts: PublicationContracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_claimed_publication_cases.len(), 8);
    let mut seen = std::collections::BTreeSet::new();
    for case in contracts.goal_claimed_publication_cases {
        assert!(seen.insert(case.name.clone()), "duplicate generated case");
        let fixture = Fixture::new(&case.before).await;
        if !case.before["children"].as_array().unwrap().is_empty() {
            fixture.seed_child(case.outcome == "conflict").await;
        }
        if case.before["latest_request"] == 40 {
            fixture
                .other_request("claimed-later-request", "2030-01-01T00:00:00Z", "completed")
                .await;
        }
        assert_eq!(
            fixture.observe().await,
            case.before,
            "{} initial",
            case.name
        );
        let mut observed = load_canonical_goal(&fixture.node, fixture.identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        observed.status = case.request["expected_status"].as_str().unwrap().into();
        observed.continuation_sequence = case.request["expected_sequence"].as_i64();
        observed.last_continued_from_request_id =
            match case.request["expected_last_continued_from"].as_u64() {
                Some(10) => Some(PARENT.into()),
                None => None,
                value => panic!("unmapped observed watermark {value:?}"),
            };
        let wrapup = observed.parsed_status() == Some(GoalStatus::BudgetLimited);
        let txn = ConfigApplyTxn::begin_local(&fixture.node, None)
            .await
            .unwrap();
        let result = stage_claimed_continuation(
            &txn,
            fixture.identity.as_ref(),
            &observed,
            PARENT,
            "Original signed continuation",
            wrapup,
        )
        .await;
        match case.outcome.as_str() {
            "created" | "rolled_back" => {
                let receipt = result.unwrap().expect("current claim must stage a child");
                assert!(receipt.created);
                let staged_goal = load_canonical_goal_in_txn(&txn, fixture.identity.did(), SESSION)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(staged_goal.status, case.before["status"].as_str().unwrap());
                assert_eq!(
                    staged_goal.continuation_sequence(),
                    case.before["sequence"].as_i64().unwrap()
                );
                assert_eq!(
                    staged_goal.last_continued_from_request_id.as_deref(),
                    Some(PARENT)
                );
                let staged = txn.execute(&format!(
                    r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {SIGNED_REQUEST_FIELDS} }} }}"#,
                    escape_graphql_string(&receipt.request_id),
                )).await.unwrap();
                let rows: Vec<AgentRequestRow> =
                    serde_json::from_value(staged["data"]["AgentRequest"].clone()).unwrap();
                assert_eq!(rows.len(), 1);
                crate::request_admission::verify_runtime_local_control_receipt(
                    &rows[0],
                    fixture.identity.did(),
                    PARENT,
                )
                .unwrap();
                assert_eq!(rows[0].doc_id.as_deref(), Some(receipt.doc_id.as_str()));
                if case.commit {
                    txn.commit().await.unwrap();
                } else {
                    txn.discard().await.unwrap();
                }
            }
            "recovered" => {
                let receipt = result.unwrap().expect("existing child must recover");
                assert!(!receipt.created);
                assert_eq!(
                    receipt.request_id,
                    goal_continuation_identity(&fixture.goal.goal_id, PARENT, 1)
                        .unwrap()
                        .request_id
                );
                txn.discard().await.unwrap();
            }
            "stale" => {
                assert!(
                    result.unwrap().is_none(),
                    "{} must not publish from stale observation",
                    case.name
                );
                txn.discard().await.unwrap();
            }
            "conflict" => {
                let error = format!("{:#}", result.unwrap_err());
                assert!(
                    error.contains("binding")
                        || error.contains("receipt")
                        || error.contains("immutable"),
                    "{}: {error}",
                    case.name
                );
                txn.discard().await.unwrap();
            }
            outcome => panic!("unmapped generated publication outcome {outcome}"),
        }
        assert_eq!(
            fixture.observe().await,
            case.expected,
            "{} durable result",
            case.name
        );
        fixture.node.shutdown().await;
    }
}

#[tokio::test]
async fn queued_claimed_publication_observes_pause_before_creating_child() {
    let before = json!({"status":"active","blocked_audits":2,"wrapup_requested":false,
        "wrapup_completed":false,"sequence":1,"last_continued_from":10,"latest_request":10,
        "children":[],"tokens_used":37,"token_budget":1000});
    let fixture = Fixture::new(&before).await;
    let observed = load_canonical_goal(&fixture.node, fixture.identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let txn = ConfigApplyTxn::begin_local(&fixture.node, None)
        .await
        .unwrap();
    let publication = publish_claimed_continuation(
        &fixture.node,
        &observed,
        PARENT,
        "Original signed continuation",
        false,
    );
    tokio::pin!(publication);
    assert!(
        matches!(
            futures::poll!(publication.as_mut()),
            std::task::Poll::Pending
        ),
        "publisher must wait behind the held native transaction gate"
    );
    txn.execute(&format!(
        r#"mutation {{ update_Goal(docID: "{}", input: {{ status: "paused", active_started_at: null }}) {{ _docID }} }}"#,
        escape_graphql_string(&fixture.goal.doc_id),
    )).await.unwrap();
    txn.commit().await.unwrap();
    assert!(
        publication.await.unwrap().is_none(),
        "paused claim must not publish a child"
    );
    let mut expected = before;
    expected["status"] = json!("paused");
    assert_eq!(fixture.observe().await, expected);
    fixture.node.shutdown().await;
}
