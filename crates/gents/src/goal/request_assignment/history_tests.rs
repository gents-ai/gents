use super::contract_tests::{entry, goal};
use super::*;
use crate::config_client::ConfigAccess;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::{json, Value};
use std::sync::Arc;

fn versions(
    values: &Value,
    identities: &[KeyIdentity],
) -> Vec<super::super::GoalAssignmentVersion> {
    values
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let mut goal = goal(&value["assignment"], identities);
            goal.doc_id = format!("goal-document-{}", value["document"]);
            goal.status = value["status"].as_str().unwrap().into();
            super::super::GoalAssignmentVersion::from_snapshot(
                goal,
                format!("commit-{}", value["commit"]),
                value["height"].as_i64().unwrap(),
                value["parents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| format!("commit-{id}"))
                    .collect(),
            )
        })
        .collect()
}

fn observation(
    value: anyhow::Result<RequestAssignmentObservation>,
    roots: Option<&RapidHashMap<u64, String>>,
) -> Value {
    match value {
        Ok(RequestAssignmentObservation::Assigned(goal)) => {
            let root = goal.assignment_root_request_doc_id.as_deref().unwrap();
            let root = roots
                .map(|roots| {
                    *roots
                        .iter()
                        .find(|(_, doc)| doc.as_str() == root)
                        .unwrap()
                        .0
                })
                .unwrap_or_else(|| {
                    root.strip_prefix("physical-")
                        .unwrap()
                        .parse::<u64>()
                        .unwrap()
                });
            json!({"kind":"assigned","goal":goal.goal_id.strip_prefix("goal-").unwrap().parse::<u64>().unwrap(),
                "root":root,"status":goal.status})
        }
        Ok(RequestAssignmentObservation::None) => json!({"kind":"unassigned"}),
        _ => json!({"kind":"unknown"}),
    }
}

#[tokio::test]
async fn generated_assignment_histories_drive_goal_version_selection() {
    #[derive(serde::Deserialize)]
    struct Contracts {
        goal_assignment_history_cases: Vec<Value>,
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_assignment_history_cases.len(), 21);
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("one.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("two.key"), None).unwrap(),
    ];
    for case in contracts.goal_assignment_history_cases {
        let entry = entry(&case["entry"], &identities).await;
        let actual = latest_assignment_goals(&versions(&case["versions"], &identities))
            .map(|goals| observe_request_assignment(&goals, &entry));
        assert_eq!(
            observation(actual, None),
            case["expected"],
            "{}",
            case["name"]
        );
        if matches!(
            case["name"].as_str(),
            Some(
                "retired_goal_keeps_its_status"
                    | "reused_goal_id_keeps_old_assignment"
                    | "later_complete_goal_cannot_complete_older_active_assignment"
            )
        ) {
            persisted_history(&case, &identities).await;
        }
    }
}

async fn persist_request(
    access: &ConfigAccess,
    identity: &KeyIdentity,
    id: u64,
    reference: Option<GoalAssignmentInput>,
) -> String {
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("history-request-{id}"),
        identity.did(),
        identity.did(),
        "history-agent",
        "session-10",
        "assignment input",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = reference;
    request.admission.signature = identity.sign(&request.signing_payload()).await.unwrap();
    let response = access
        .write(
            "test.signed_history_request",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    crate::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

async fn persisted_history(case: &Value, identities: &[KeyIdentity]) {
    let identity = &identities[0];
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let mut roots = RapidHashMap::new();
    for version in case["versions"].as_array().unwrap() {
        let Some(root) = version["assignment"]["root"].as_u64() else {
            continue;
        };
        if roots.contains_key(&root) {
            continue;
        }
        let reference = GoalAssignmentInput {
            goal_id: format!("goal-{}", version["assignment"]["goal"]),
            root_request_doc_id: None,
        };
        roots.insert(
            root,
            persist_request(&access, identity, root, Some(reference)).await,
        );
    }
    let reference = &case["entry"]["reference"];
    let reference = reference.as_object().map(|_| GoalAssignmentInput {
        goal_id: format!("goal-{}", reference["goal"]),
        root_request_doc_id: reference["root"].as_u64().map(|root| roots[&root].clone()),
    });
    let entry_id = case["entry"]["doc"].as_u64().unwrap();
    let request_doc = if let Some(doc) = roots.get(&entry_id) {
        doc.clone()
    } else {
        persist_request(&access, identity, entry_id, reference).await
    };
    let mut documents: RapidHashMap<u64, String> = RapidHashMap::new();
    for version in case["versions"].as_array().unwrap() {
        let model_doc = version["document"].as_u64().unwrap();
        let mut value = serde_json::to_value(goal(&version["assignment"], identities)).unwrap();
        value.as_object_mut().unwrap().remove("_docID");
        value["status"] = version["status"].clone();
        value["objective"] = json!("Keep each assignment tied to its own work");
        value["created_at"] = json!("2026-10-21T00:00:00Z");
        value["updated_at"] = json!(format!(
            "2026-10-21T00:00:{:02}Z",
            version["height"].as_u64().unwrap()
        ));
        value["assignment_root_request_doc_id"] = version["assignment"]["root"]
            .as_u64()
            .map(|root| json!(roots[&root]))
            .unwrap_or(Value::Null);
        let doc = documents.get(&model_doc).cloned();
        let response = access.transact_write("test.goal_history_version",|txn| {
            let value=value.clone();let doc=doc.clone();
            Box::pin(async move {
                if let Some(doc) = doc {
                    txn.execute_with_variables("mutation($doc: ID!, $input: GoalMutationInputArg!) {update_Goal(docID: $doc, input: $input) {_docID}}",
                        &json!({"doc":doc,"input":value})).await?;
                    Ok(doc)
                } else {
                    let response = txn.execute_with_variables("mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                        &json!({"input":value})).await?;
                    crate::graphql::created_doc_id(&response,"Goal")
                }
            })
        }).await.unwrap();
        documents.insert(model_doc, response);
    }
    if case["name"] == "later_complete_goal_cannot_complete_older_active_assignment" {
        let first_document = case["versions"][0]["document"].as_u64().unwrap();
        let doc = crate::graphql::escape_graphql_string(&documents[&first_document]);
        for index in 0..65 {
            let objective =
                crate::graphql::escape_graphql_string(&format!("History observation {index}"));
            access
                .write(
                    "test.goal_history_many_revisions",
                    &format!(
                        r#"mutation {{ update_Goal(docID: "{doc}", input: {{objective: "{objective}"}}) {{_docID}} }}"#
                    ),
                )
                .await
                .unwrap();
        }
    }
    for doc in case["retire_documents"].as_array().unwrap() {
        access
            .write(
                "test.retire_goal_history",
                &format!(
                    "mutation {{delete_Goal(docID: \"{}\") {{_docID}}}}",
                    crate::graphql::escape_graphql_string(&documents[&doc.as_u64().unwrap()])
                ),
            )
            .await
            .unwrap();
    }
    let response = access
        .execute(&format!(
            "{{AgentRequest(docID: \"{}\") {{{}}}}}",
            crate::graphql::escape_graphql_string(&request_doc),
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    let actual = super::super::load_request_assignment(&node, &rows[0]).await;
    assert_eq!(
        observation(actual, Some(&roots)),
        case["expected"],
        "{}: retained native history",
        case["name"]
    );
    let request = &rows[0];
    let within_transaction = access
        .transact_readonly("test.assignment_history_in_transaction", |txn| {
            Box::pin(super::super::load_request_assignment_in_txn(txn, request))
        })
        .await;
    assert_eq!(
        observation(within_transaction, Some(&roots)),
        case["expected"],
        "{}: retained history in the caller's transaction",
        case["name"]
    );
    let handoff = crate::background_completion::turn_assignment_handoff(
        &node,
        &rows[0],
        &crate::tool_call_lifecycle::CausedRequestTerminal::Completed {
            output: "turn finished".into(),
        },
    )
    .await;
    assert_eq!(
        json!(handoff.reason()),
        case["expected_handoff_reason"],
        "{}: actual completion handoff",
        case["name"]
    );
    let current = super::super::load_canonical_goal(&node, identity.did(), "session-10")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        current.status,
        case["versions"].as_array().unwrap().last().unwrap()["status"],
        "{}: distinct current assignment",
        case["name"]
    );
    node.shutdown().await;
}
