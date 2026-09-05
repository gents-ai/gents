use super::*;
#[path = "support.rs"]
mod support;
use serde::Deserialize;
use serde_json::json;
use support::*;

#[derive(Deserialize)]
struct Contracts {
    pub goal_operator_resume_cases: Vec<ResumeCase>,
    pub goal_config_reactivation_cases: Vec<ConfigCase>,
}

#[derive(Deserialize)]
struct ConfigCase {
    pub current: String,
    pub target: String,
    pub allowed: bool,
}

#[tokio::test]
async fn generated_goal_operator_resume_cases_drive_real_transactions() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_operator_resume_cases.len(), 11);
    let mut seen = std::collections::BTreeSet::new();
    for case in contracts.goal_operator_resume_cases {
        assert!(seen.insert(case.name.clone()), "duplicate generated case");
        let f = Fixture::new(&case.before).await;
        let conflict = case.name == "foreign_fingerprint_conflicts";
        if !case.before["children"].as_array().unwrap().is_empty() {
            f.seed_child(conflict).await;
        }
        if case.before["latest_request"] == 30 {
            f.other_request("later-request", "2030-01-01T00:00:00Z", "completed")
                .await;
        }
        match case.name.as_str() {
            "nonterminal_parent_cannot_publish" => execute(&f.node, &format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{PARENT}" }} }}, input: {{ lifecycle_state: "processing" }}) {{ _docID }} }}"#)).await,
            "busy_session_cannot_publish" => f.other_request("older-active", "2019-01-01T00:00:00Z", "processing").await,
            "atomic_publication" | "staging_failure_rolls_back" | "lost_ack_returns_same_child"
            | "retry_after_later_progress" | "unauthorized_cannot_publish" | "foreign_parent_cannot_recover"
            | "non_latest_parent_cannot_publish" | "foreign_fingerprint_conflicts" | "budget_limited_cannot_resume" => {},
            name => panic!("unmapped generated resume case {name}"),
        }
        assert_eq!(f.observe().await, case.before, "{} initial", case.name);
        let access = crate::ConfigAccess::Local(f.node.clone());
        if !case.commit {
            assert_eq!(case.name, "staging_failure_rolls_back");
            let txn = ConfigApplyTxn::begin_local(&f.node, None).await.unwrap();
            let receipt =
                stage_resume(&txn, f.identity.as_ref(), f.identity.did(), SESSION, PARENT)
                    .await
                    .unwrap();
            assert!(receipt.created);
            let staged_goal = load_canonical_goal_in_txn(&txn, f.identity.did(), SESSION)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(staged_goal.parsed_status(), Some(GoalStatus::Active));
            assert_eq!(staged_goal.continuation_sequence(), 1);
            let staged = txn
                .execute(&format!(
                    "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
                    escape_graphql_string(&receipt.request_id)
                ))
                .await
                .unwrap();
            assert_eq!(staged["data"]["AgentRequest"].as_array().unwrap().len(), 1);
            txn.discard().await.unwrap();
            assert_eq!(case.outcome, "rolled_back");
        } else {
            let foreign_temp = tempfile::tempdir().unwrap();
            let foreign = crate::identity::KeyIdentity::load_or_create(
                foreign_temp.path().join("foreign.key"),
                None,
            )
            .unwrap();
            let identity: &dyn AgentIdentity = if case.request["authorized"] == false {
                &foreign
            } else {
                f.identity.as_ref()
            };
            let predecessor = if case.request["parent_belongs_to_goal"] == false {
                "foreign-parent"
            } else {
                PARENT
            };
            let result =
                resume_goal_request(&access, identity, f.identity.did(), SESSION, predecessor)
                    .await;
            match case.outcome.as_str() {
                "created" | "recovered" => {
                    let receipt = result.unwrap();
                    assert_eq!(receipt.created, case.outcome == "created");
                    assert_eq!(
                        Some(receipt.goal_status.as_str()),
                        case.goal_status.as_deref()
                    );
                    if case.name == "retry_after_later_progress" {
                        assert_eq!(receipt.goal_status, GoalStatus::Paused);
                        assert!(!receipt.created);
                    }
                    let expected = goal_continuation_identity(&f.goal.goal_id, PARENT, 1).unwrap();
                    assert_eq!(receipt.request_id, expected.request_id);
                    assert_eq!(receipt.goal_id, f.goal.goal_id);
                    let rows = request_rows(&f.node).await;
                    assert_eq!(
                        rows.iter()
                            .filter(|r| r.doc_id.as_deref() == Some(receipt.doc_id.as_str()))
                            .count(),
                        1
                    );
                    if case.name == "atomic_publication" {
                        let row = rows
                            .into_iter()
                            .find(|r| r.request_id == receipt.request_id)
                            .unwrap();
                        let request = crate::watcher::AgentRequest::try_from(row).unwrap();
                        let (_authority_owner, authority) =
                            crate::agent::p2p_reconcile::enrollment_authority_channel();
                        let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
                            f.node.clone(),
                            f.identity.clone(),
                            authority,
                        );
                        verifier
                            .verify_fresh(&request, "contract-behavior")
                            .await
                            .expect("published continuation must pass actual execution admission");
                    }
                }
                "denied" | "illegal" | "conflict" => {
                    let error = format!("{:#}", result.unwrap_err());
                    let expected_reason = match case.name.as_str() {
                        "unauthorized_cannot_publish" => "target principal",
                        "foreign_parent_cannot_recover" => "predecessor must uniquely belong",
                        "non_latest_parent_cannot_publish" => "no longer the latest",
                        "nonterminal_parent_cannot_publish" => "predecessor must be terminal",
                        "busy_session_cannot_publish" => "still has unfinished requests",
                        "foreign_fingerprint_conflicts" => "physical predecessor binding",
                        "budget_limited_cannot_resume" => "does not allow operator resume",
                        name => panic!("unmapped rejection {name}"),
                    };
                    assert!(error.contains(expected_reason), "{}: {error}", case.name);
                }
                outcome => panic!("unmapped outcome {outcome}"),
            }
        }
        assert_eq!(
            f.observe().await,
            case.expected,
            "{} durable result",
            case.name
        );
        f.node.shutdown().await;
    }
}

#[tokio::test]
async fn generated_goal_config_reactivation_cases_drive_transactional_setter() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_config_reactivation_cases.len(), 7);
    let mut seen = std::collections::BTreeSet::new();
    for case in contracts.goal_config_reactivation_cases {
        assert!(seen.insert((case.current.clone(), case.target.clone())));
        let before = json!({"status":case.current,"blocked_audits":2,"wrapup_requested":false,
            "wrapup_completed":false,"sequence":0,"last_continued_from":null,"latest_request":10,
            "children":[],"tokens_used":37,"token_budget":1000});
        let f = Fixture::new(&before).await;
        assert_eq!(f.observe().await, before);
        let access = crate::ConfigAccess::Local(f.node.clone());
        let result = set_goal_from_access(
            &access,
            f.identity.did(),
            SESSION,
            None,
            Some(GoalStatus::parse(&case.target).unwrap()),
            None,
        )
        .await;
        assert_eq!(
            result.is_ok(),
            case.allowed,
            "{} -> {}: {result:?}",
            case.current,
            case.target
        );
        if let Err(error) = result {
            assert!(format!("{error:#}").contains("requires goal resume-request"));
        }
        let mut expected = before;
        if case.allowed {
            expected["status"] = json!(case.target);
        }
        assert_eq!(f.observe().await, expected);
        f.node.shutdown().await;
    }
}

#[tokio::test]
async fn waiting_requests_keep_resume_idle_guard_closed_without_duplicate_children() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    let initial = &contracts
        .goal_operator_resume_cases
        .iter()
        .find(|case| case.name == "atomic_publication")
        .unwrap()
        .before;
    for state in [
        RequestLifecycleState::InputRequired,
        RequestLifecycleState::WorkspaceBindingPending,
    ] {
        assert!(!state.is_terminal());
        let f = Fixture::new(initial).await;
        f.other_request("waiting-existing", "2019-01-01T00:00:00Z", state.as_str())
            .await;
        let before = f.observe().await;
        let rows_before = serde_json::to_value(request_rows(&f.node).await).unwrap();
        let error = resume_goal_request(
            &crate::ConfigAccess::Local(f.node.clone()),
            f.identity.as_ref(),
            f.identity.did(),
            SESSION,
            PARENT,
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("unfinished requests"),
            "{state:?}: {error}"
        );
        assert_eq!(f.observe().await, before);
        assert_eq!(
            serde_json::to_value(request_rows(&f.node).await).unwrap(),
            rows_before,
            "{state:?}: resume duplicated existing work"
        );
        f.node.shutdown().await;
    }
}

#[tokio::test]
async fn overlapping_resume_and_native_goal_write_conflict_atomically() {
    async fn native(
        node: &EmbeddedNode,
        handle: &query::TransactionHandle,
        query: &str,
    ) -> serde_json::Value {
        let result = node
            .execute_request_in_txn(defra_node::QueryRequest::new(query), handle)
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        result.data.unwrap()
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    let initial = &contracts
        .goal_operator_resume_cases
        .iter()
        .find(|case| case.name == "atomic_publication")
        .unwrap()
        .before;
    for resume_first in [false, true] {
        let f = Fixture::new(initial).await;
        // Remote/native writes do not participate in ConfigApplyTxn's local
        // mutation mutex. Keep both storage snapshots open deliberately.
        let remote = f.node.runner().begin_txn(false).await.unwrap();
        let goal_id = escape_graphql_string(&f.goal.goal_id);
        let query = format!(
            r#"{{ Goal(filter: {{ goal_id: {{ _eq: "{goal_id}" }} }}) {{ _docID status continuation_sequence updated_at }} }}"#
        );
        let before = native(&f.node, &remote, &query).await;
        assert_eq!(before["Goal"][0]["status"], "paused");
        let local = ConfigApplyTxn::begin_local(&f.node, None).await.unwrap();
        let receipt = stage_resume(
            &local,
            f.identity.as_ref(),
            f.identity.did(),
            SESSION,
            PARENT,
        )
        .await
        .unwrap();
        assert!(receipt.created);
        let update = format!(
            r#"mutation {{ update_Goal(filter: {{ goal_id: {{ _eq: "{goal_id}" }}, status: {{ _eq: "paused" }}, continuation_sequence: {{ _eq: 0 }} }}, input: {{ status: "paused", updated_at: "2031-01-01T00:00:00Z" }}) {{ _docID }} }}"#
        );
        assert_eq!(
            native(&f.node, &remote, &update).await["update_Goal"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let conflict = if resume_first {
            local.commit().await.unwrap();
            f.node
                .runner()
                .commit_txn(&remote)
                .await
                .unwrap_err()
                .to_string()
        } else {
            f.node.runner().commit_txn(&remote).await.unwrap();
            local.commit().await.unwrap_err().to_string()
        };
        assert!(conflict.contains("transaction conflict"), "{conflict}");
        let _ = f.node.runner().rollback_txn(&remote).await;
        let observed = f.observe().await;
        assert_eq!(
            observed["status"],
            if resume_first { "active" } else { "paused" }
        );
        assert_eq!(observed["sequence"], if resume_first { 1 } else { 0 });
        assert_eq!(
            request_rows(&f.node)
                .await
                .iter()
                .filter(|row| row.request_id == receipt.request_id)
                .count(),
            usize::from(resume_first)
        );
        f.node.shutdown().await;
    }
}
