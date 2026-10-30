use super::*;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

struct ReadScope {
    rows: Vec<AgentRequestRow>,
    refuse_history: bool,
    history_reads: AtomicUsize,
}

#[async_trait::async_trait]
impl crate::config_client::ConfigRead for ReadScope {
    async fn execute_read(
        &self,
        query: &str,
    ) -> Result<Value, crate::config_client::ConfigClientError> {
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
        if self.refuse_history {
            return Err(crate::graphql::GraphqlResponseError::from(
                gents_protocol::database_error::GraphqlFailure::new(
                    "Goal",
                    json!([{"message": "retained Goal history is unavailable"}]),
                ),
            )
            .into());
        }
        Ok(json!({"data":{"Goal":[]}}))
    }
}

#[tokio::test]
async fn signed_scope_reads_use_the_executed_assignment_contract_without_history() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["goal_request_assignment_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
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
        )
        .await;
        if explicit && valid {
            let mut scopes = result.unwrap();
            let scope = scopes
                .scope_for_request(&doc, &row.request_id)
                .await
                .unwrap();
            let RequestAssignmentScope::Referenced(reference) = scope else {
                panic!("{}", case["name"])
            };
            let actual = json!({"kind":"referenced", "goal":reference.goal_id.strip_prefix("goal-").unwrap().parse::<u64>().unwrap(),
                "root":reference.root_request_doc_id.unwrap().strip_prefix("physical-").unwrap().parse::<u64>().unwrap()});
            assert_eq!(actual, case["expected_queue_scope"], "{}", case["name"]);
            assert!(scopes
                .scope_for_request(&doc, "another-logical-request")
                .await
                .is_err());
            assert!(scopes.scope("unobserved-physical-request").await.is_err());
            assert_eq!(reader.history_reads.load(Ordering::Relaxed), 0);
        } else {
            assert!(
                result.unwrap().scope(&doc).await.is_err(),
                "{}",
                case["name"]
            );
            assert_eq!(
                reader.history_reads.load(Ordering::Relaxed),
                usize::from(valid)
            );
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
    let row = continuation_row(&root, &identities[0], "physical-2").await;
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
        let result = async {
            load_request_assignment_scopes(&reader, identities[0].did(), "session-10")
                .await?
                .scope_for_request("physical-2", &row.request_id)
                .await
        }
        .await;
        if kind == "valid" {
            assert_eq!(result.unwrap(), expected);
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
    let mut scopes = load_request_assignment_scopes(&reader, identities[0].did(), "session-10")
        .await
        .unwrap();
    assert_eq!(
        scopes
            .scope_for_request("physical-1", &row.request_id)
            .await
            .unwrap(),
        expected
    );
    assert_eq!(reader.history_reads.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn signed_scope_is_lazy_and_does_not_read_unselected_history() {
    let home = tempfile::tempdir().unwrap();
    let identities = [KeyIdentity::load_or_create(home.path().join("one.key"), None).unwrap()];
    let signed = super::contract_tests::entry(
        &json!({"owner":1,"session":10,"doc":1,
        "reference":{"goal":1,"root":null},"signature_valid":true}),
        &identities,
    )
    .await;
    let untagged = super::contract_tests::entry(
        &json!({"owner":1,"session":10,"doc":2,
        "reference":null,"signature_valid":true}),
        &identities,
    )
    .await;
    let reader = ReadScope {
        rows: vec![signed, untagged],
        refuse_history: true,
        history_reads: AtomicUsize::new(0),
    };
    let mut scopes = load_request_assignment_scopes(&reader, identities[0].did(), "session-10")
        .await
        .unwrap();
    let expected = scopes.scope("physical-1").await.unwrap();
    assert_eq!(reader.history_reads.load(Ordering::Relaxed), 0);
    assert!(scopes.scope("physical-2").await.is_err());
    assert_eq!(scopes.scope("physical-1").await.unwrap(), expected);
}

async fn continuation_row(
    root: &AgentRequestRow,
    identity: &KeyIdentity,
    doc: &str,
) -> AgentRequestRow {
    let parent = crate::request::watcher::AgentRequest::try_from(root.clone()).unwrap();
    let mut child = crate::request::lifecycle::queue::prepare_goal_continuation(
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
    crate::sign_agent_request_create(identity, &mut child)
        .await
        .unwrap();
    let mut value = serde_json::to_value(&child).unwrap();
    value.as_object_mut().unwrap().remove("admission");
    value["_docID"] = json!(doc);
    value["admission_kind"] = json!(child.admission.kind.as_str());
    value["admission_signer_did"] = json!(child.admission.signer_did);
    value["runtime_issuer_did"] = json!(child.admission.runtime_issuer_did);
    value["runtime_source_request_id"] = json!(child.admission.runtime_source_request_id);
    value["runtime_source_kind"] = json!(child
        .admission
        .runtime_source_kind
        .map(|kind| kind.as_str()));
    value["admission_signature"] = json!(bs58::encode(child.admission.signature).into_string());
    let row: AgentRequestRow = serde_json::from_value(value).unwrap();
    verify_request_receipt_signature(&row).unwrap();
    row
}

#[tokio::test]
async fn generated_ancestry_scope_cases_use_the_lean_composition() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["goal_request_scope_cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    let home = tempfile::tempdir().unwrap();
    let identities = [KeyIdentity::load_or_create(home.path().join("one.key"), None).unwrap()];
    for case in cases {
        let reference = case["root_tagged"]
            .as_bool()
            .unwrap()
            .then(|| json!({"goal":1,"root":null}));
        let root = super::contract_tests::entry(
            &json!({"owner":1,"session":1,"doc":10,"reference":reference,"signature_valid":true}),
            &identities,
        )
        .await;
        let mut child = continuation_row(&root, &identities[0], "physical-20").await;
        if case["child_valid"] == false {
            child.content = Some("changed after signing".into());
        }
        let mut rows = vec![child];
        if case["missing_parent"] == false {
            rows.push(root);
        }
        let mut goals = Vec::new();
        for (field, root) in [("root_openings", 10), ("child_openings", 20)] {
            for goal in case[field].as_array().unwrap() {
                goals.push(super::contract_tests::goal(
                    &json!({"owner":1,"session":1,"goal":goal,"root":root}),
                    &identities,
                ));
            }
        }
        let reader = ReadScope {
            rows: rows.clone(),
            refuse_history: true,
            history_reads: AtomicUsize::new(0),
        };
        let mut scopes = RequestAssignmentScopes {
            access: &reader,
            requests: request_assignments(identities[0].did(), "session-1", rows, goals).unwrap(),
            history_loaded: true,
            scopes: Default::default(),
        };
        let actual = match scopes.scope("physical-20").await {
            Ok(RequestAssignmentScope::Referenced(reference)) => json!({
                "kind":"referenced", "goal": reference.goal_id.strip_prefix("goal-").unwrap().parse::<u64>().unwrap(),
                "root": reference.root_request_doc_id.unwrap().strip_prefix("physical-").unwrap().parse::<u64>().unwrap(),
            }),
            Ok(RequestAssignmentScope::Unassigned) => json!({"kind":"unassigned"}),
            Ok(RequestAssignmentScope::Unresolved {
                entry_doc_id: entry,
            }) => {
                json!({"kind":"unresolved","entry":entry.strip_prefix("physical-").unwrap().parse::<u64>().unwrap()})
            }
            Err(_) => Value::Null,
        };
        assert_eq!(actual, case["expected"], "{}", case["name"]);
    }
}
