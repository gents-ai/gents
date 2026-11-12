use super::contract_tests::{entry, goal};
use super::*;
use gents_lean_vocab::families::goal_request_assignment_contracts_history as history;
use gents_lean_vocab::families::goal_request_assignment_contracts_persisted_history as persisted;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::{json, Value};
use std::sync::Arc;

fn assignment_value(value: &history::Version<String>) -> Value {
    json!({
        "goal": value.assignment.goal,
        "owner": value.assignment.owner,
        "root": value.assignment.root,
        "session": value.assignment.session,
    })
}

fn versions(
    values: &[history::Version<String>],
    identities: &[KeyIdentity],
) -> Vec<super::super::GoalAssignmentVersion> {
    values
        .iter()
        .map(|value| {
            let mut goal = goal(&assignment_value(value), identities);
            goal.doc_id = format!("goal-document-{}", value.document);
            goal.status = value.status.clone();
            super::super::GoalAssignmentVersion::from_snapshot(
                goal,
                format!("commit-{}", value.commit),
                value.height as i64,
                value
                    .parents
                    .iter()
                    .map(|id| format!("commit-{id}"))
                    .collect(),
            )
        })
        .collect()
}

fn entry_value(case: &history::HistoryCase<String>) -> Value {
    json!({
        "owner": case.entry.owner,
        "session": case.entry.session,
        "doc": case.entry.doc,
        "reference": case.entry.reference.as_ref().map(|reference| json!({
            "goal": reference.goal,
            "root": reference.root,
        })),
        "signature_valid": case.entry.signature_valid,
    })
}

fn observation(
    value: anyhow::Result<RequestAssignmentObservation>,
    roots: Option<&RapidHashMap<u64, String>>,
) -> history::ObservationWire<String> {
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
            history::ObservationWire {
                kind: "assigned".into(),
                goal: Some(
                    goal.goal_id
                        .strip_prefix("goal-")
                        .unwrap()
                        .parse::<u64>()
                        .unwrap(),
                ),
                root: Some(root),
                status: Some(goal.status.clone()),
            }
        }
        Ok(RequestAssignmentObservation::None) => history::ObservationWire {
            kind: "unassigned".into(),
            goal: None,
            root: None,
            status: None,
        },
        _ => history::ObservationWire {
            kind: "unknown".into(),
            goal: None,
            root: None,
            status: None,
        },
    }
}

#[tokio::test]
async fn generated_assignment_histories_drive_goal_version_selection() {
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("one.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("two.key"), None).unwrap(),
    ];
    for case in
        gents_lean_vocab::families::goal_request_assignment_contracts_history::cases::<String>()
    {
        let entry = entry(&entry_value(&case.input), &identities).await;
        let actual = latest_assignment_goals(&versions(&case.input.versions, &identities))
            .map(|goals| observe_request_assignment(&goals, &entry));
        case.check(observation(actual, None));
    }
}

#[tokio::test]
async fn persisted_histories_drive_native_assignment_and_handoff() {
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("one.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("two.key"), None).unwrap(),
    ];
    for case in
        gents_lean_vocab::families::goal_request_assignment_contracts_persisted_history::cases::<
            String,
        >()
    {
        let input: history::HistoryCase<String> = gents_lean_vocab::family::recode(&case.input);
        case.check(persisted_history(&input, &identities).await);
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
    gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

async fn persisted_history(
    case: &history::HistoryCase<String>,
    identities: &[KeyIdentity],
) -> persisted::PersistedObservation<String> {
    let identity = &identities[0];
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access =
        ConfigAccess::local_as(node.clone(), ::identity::Did::new(identity.did()).unwrap());
    let mut roots = RapidHashMap::new();
    for version in &case.versions {
        let Some(root) = version.assignment.root else {
            continue;
        };
        if roots.contains_key(&root) {
            continue;
        }
        let reference = GoalAssignmentInput {
            goal_id: format!("goal-{}", version.assignment.goal),
            root_request_doc_id: None,
        };
        roots.insert(
            root,
            persist_request(&access, identity, root, Some(reference)).await,
        );
    }
    let reference = case
        .entry
        .reference
        .as_ref()
        .map(|reference| GoalAssignmentInput {
            goal_id: format!("goal-{}", reference.goal),
            root_request_doc_id: reference.root.map(|root| roots[&root].clone()),
        });
    let entry_id = case.entry.doc;
    let request_doc = if let Some(doc) = roots.get(&entry_id) {
        doc.clone()
    } else {
        persist_request(&access, identity, entry_id, reference).await
    };
    let mut documents: RapidHashMap<u64, String> = RapidHashMap::new();
    for version in &case.versions {
        let model_doc = version.document;
        let mut value = serde_json::to_value(goal(&assignment_value(version), identities)).unwrap();
        value.as_object_mut().unwrap().remove("_docID");
        value["status"] = json!(version.status);
        value["objective"] = json!("Keep each assignment tied to its own work");
        value["created_at"] = json!("2026-10-21T00:00:00Z");
        value["updated_at"] = json!(format!("2026-10-21T00:00:{:02}Z", version.height));
        value["assignment_root_request_doc_id"] = version
            .assignment
            .root
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
                    gents_store::graphql::created_doc_id(&response,"Goal")
                }
            })
        }).await.unwrap();
        documents.insert(model_doc, response);
    }
    if case.name == "later_complete_goal_cannot_complete_older_active_assignment" {
        let first_document = case.versions[0].document;
        let doc = gents_store::graphql::escape_graphql_string(&documents[&first_document]);
        for index in 0..65 {
            let objective = gents_store::graphql::escape_graphql_string(&format!(
                "History observation {index}"
            ));
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
    for doc in &case.retire_documents {
        access
            .write(
                "test.retire_goal_history",
                &format!(
                    "mutation {{delete_Goal(docID: \"{}\") {{_docID}}}}",
                    gents_store::graphql::escape_graphql_string(&documents[doc])
                ),
            )
            .await
            .unwrap();
    }
    let response = access
        .execute(&format!(
            "{{AgentRequest(docID: \"{}\") {{{}}}}}",
            gents_store::graphql::escape_graphql_string(&request_doc),
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    let actual = super::super::load_request_assignment(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            ::identity::Did::new(identity.did()).unwrap(),
        ),
        &rows[0],
    )
    .await;
    let retained = observation(actual, Some(&roots));
    let request = &rows[0];
    let within_transaction = access
        .transact_readonly("test.assignment_history_in_transaction", |txn| {
            Box::pin(super::load_request_assignment_in_txn(txn, request))
        })
        .await;
    let within_transaction = observation(within_transaction, Some(&roots));
    assert_eq!(
        serde_json::to_value(&within_transaction).unwrap(),
        serde_json::to_value(&retained).unwrap(),
        "{}: retained history in the caller's transaction",
        case.name
    );
    let handoff = gents_toolcalls::background::completion::turn_assignment_handoff(
        &crate::goal::session_message::SessionGoals,
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            ::identity::Did::new(identity.did()).unwrap(),
        ),
        &rows[0],
        &gents_toolcalls::call::lifecycle::CausedRequestTerminal::Completed {
            output: "turn finished".into(),
        },
    )
    .await;
    let current = super::super::load_canonical_goal(&node, identity.did(), "session-10")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        current.status,
        case.versions.last().unwrap().status,
        "{}: distinct current assignment",
        case.name
    );
    node.shutdown().await.expect("node shutdown");
    persisted::PersistedObservation {
        observation: gents_lean_vocab::family::recode(&retained),
        handoff_reason: handoff.reason().map(str::to_owned),
    }
}
