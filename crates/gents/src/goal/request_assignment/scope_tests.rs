use super::*;
use crate::identity::{KeyIdentity, NodeIdentity};
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

struct ReadScope {
    rows: Vec<AgentRequestRow>,
    refuse_history: bool,
    history_reads: AtomicUsize,
}

#[async_trait::async_trait]
impl crate::config_client::ConfigRead for ReadScope {
    async fn execute_read(&self, query: &str) -> Result<Value> {
        if query.contains("AgentRequest(") {
            let rows = self
                .rows
                .iter()
                .map(|row| {
                    let mut value = serde_json::to_value(row).unwrap();
                    value["_docID"] = json!(row.doc_id);
                    value
                })
                .collect::<Vec<_>>();
            return Ok(json!({"data":{"AgentRequest":rows}}));
        }
        self.history_reads.fetch_add(1, Ordering::Relaxed);
        anyhow::ensure!(!self.refuse_history, "retained Goal history is unavailable");
        Ok(json!({"data":{"Goal":[]}}))
    }
}

#[tokio::test]
async fn signed_scope_reads_use_the_executed_assignment_contract_without_history() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["goal_request_assignment_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 18);
    let home = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(home.path().join("one.key"), None).unwrap(),
        KeyIdentity::load_or_create(home.path().join("two.key"), None).unwrap(),
    ];
    for case in cases {
        let row = super::contract_tests::entry(&case["entry"], &identities).await;
        let explicit = row
            .input
            .as_ref()
            .is_some_and(|input| input.goal_assignment.is_some());
        let valid = verify_request_receipt_signature(&row).is_ok();
        let doc = row.doc_id.clone().unwrap();
        let reader = ReadScope {
            rows: vec![row.clone()],
            refuse_history: true,
            history_reads: AtomicUsize::new(0),
        };
        let result = load_request_assignment_scopes(
            &reader,
            row.node_did.as_deref().unwrap(),
            row.session_id.as_deref().unwrap(),
            &[&doc],
        )
        .await;
        if explicit && valid {
            let scopes = result.unwrap();
            let scope = scopes.scope_for_request(&doc, &row.request_id).unwrap();
            let RequestAssignmentScope::Referenced(reference) = scope else {
                panic!("{}", case["name"])
            };
            let actual = json!({"kind":"referenced", "goal":reference.goal_id.strip_prefix("goal-").unwrap().parse::<u64>().unwrap(),
                "root":reference.root_request_doc_id.unwrap().strip_prefix("physical-").unwrap().parse::<u64>().unwrap()});
            assert_eq!(actual, case["expected_queue_scope"], "{}", case["name"]);
            assert!(scopes
                .scope_for_request(&doc, "another-logical-request")
                .is_err());
            assert!(scopes.scope("unobserved-physical-request").is_err());
            assert_eq!(reader.history_reads.load(Ordering::Relaxed), 0);
        } else {
            assert!(result.is_err(), "{}", case["name"]);
            assert_eq!(reader.history_reads.load(Ordering::Relaxed), 1);
        }
    }
}

#[tokio::test]
async fn signed_scope_reads_authenticate_continuation_ancestry_before_skipping_history() {
    let home = tempfile::tempdir().unwrap();
    let identities = [KeyIdentity::load_or_create(home.path().join("one.key"), None).unwrap()];
    let root = super::contract_tests::entry(
        &json!({"owner":1,"session":10,"doc":1,
        "reference":{"goal":1,"root":null},"signature_valid":true}),
        &identities,
    )
    .await;
    let parent = crate::watcher::AgentRequest::try_from(root.clone()).unwrap();
    let mut child = crate::lifecycle::queue::prepare_goal_continuation(
        &parent,
        "agent".into(),
        "goal-1",
        "continue",
        1,
        false,
        "2026-10-21T00:01:00Z",
        parent.request_hop,
    )
    .unwrap();
    crate::sign_agent_request_create(&identities[0], &mut child)
        .await
        .unwrap();
    let mut value = serde_json::to_value(&child).unwrap();
    value.as_object_mut().unwrap().remove("admission");
    value["_docID"] = json!("physical-2");
    value["admission_kind"] = json!(child.admission.kind.as_str());
    value["admission_signer_did"] = json!(child.admission.signer_did);
    value["runtime_issuer_did"] = json!(child.admission.runtime_issuer_did);
    value["runtime_source_request_id"] = json!(child.admission.runtime_source_request_id);
    value["runtime_source_kind"] = json!(child.admission.runtime_source_kind.map(|kind| kind.as_str()));
    value["admission_signature"] = json!(bs58::encode(child.admission.signature).into_string());
    let row: AgentRequestRow = serde_json::from_value(value).unwrap();
    verify_request_receipt_signature(&row).unwrap();
    let expected = request_assignment_scope(&root, &RequestAssignmentObservation::Unknown).unwrap();
    for kind in [
        "valid",
        "missing-parent",
        "corrupt-signature",
        "foreign-session",
        "duplicate-physical",
    ] {
        let mut current = row.clone();
        if kind == "corrupt-signature" {
            current.content = Some("changed after signing".into());
        }
        if kind == "foreign-session" {
            current.session_id = Some("another-session".into());
        }
        let mut rows = vec![current];
        if kind != "missing-parent" {
            rows.push(root.clone());
        }
        if kind == "duplicate-physical" {
            rows.push(root.clone());
        }
        let reader = ReadScope {
            rows,
            refuse_history: kind == "valid",
            history_reads: AtomicUsize::new(0),
        };
        let result = load_request_assignment_scopes(
            &reader,
            identities[0].did(),
            "session-10",
            &["physical-2"],
        )
        .await;
        if kind == "valid" {
            assert_eq!(
                result
                    .unwrap()
                    .scope_for_request("physical-2", &row.request_id)
                    .unwrap(),
                expected
            );
            assert_eq!(reader.history_reads.load(Ordering::Relaxed), 0);
        } else {
            assert!(result.is_err(), "{kind}");
        }
    }
}

#[tokio::test]
async fn signed_scope_reads_preserve_the_historical_fallback_for_untagged_requests() {
    let home = tempfile::tempdir().unwrap();
    let identities = [KeyIdentity::load_or_create(home.path().join("one.key"), None).unwrap()];
    let row = super::contract_tests::entry(
        &json!({"owner":1,"session":10,"doc":1,
        "reference":null,"signature_valid":true}),
        &identities,
    )
    .await;
    let expected = request_assignment_scope(&row, &RequestAssignmentObservation::None).unwrap();
    let reader = ReadScope {
        rows: vec![row.clone()],
        refuse_history: false,
        history_reads: AtomicUsize::new(0),
    };
    let scopes =
        load_request_assignment_scopes(&reader, identities[0].did(), "session-10", &["physical-1"])
            .await
            .unwrap();
    assert_eq!(
        scopes
            .scope_for_request("physical-1", &row.request_id)
            .unwrap(),
        expected
    );
    assert_eq!(reader.history_reads.load(Ordering::Relaxed), 1);
}
