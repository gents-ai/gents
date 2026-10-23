use super::*;
use crate::config_client::ConfigAccess;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::{json, Value};

async fn parent(
    db: &TestDb,
    session: &str,
    model: &Value,
    documents: &mut RapidHashMap<u64, AgentRequest>,
) -> AgentRequest {
    let id = model["doc"].as_u64().unwrap();
    if let Some(request) = documents.get(&id) {
        return request.clone();
    }
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("scope-parent-{id}"),
        db.node_did(),
        db.node_did(),
        TEST_AGENT_ID,
        session,
        "work",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(db.node_did()),
    );
    create.input.goal_assignment = model["reference"].as_object().map(|_| GoalAssignmentInput {
        goal_id: format!("goal-{}", model["reference"]["goal"]),
        root_request_doc_id: model["reference"]["root"]
            .as_u64()
            .map(|root| documents[&root].doc_id.clone()),
    });
    crate::sign_agent_request_create(db.identity.as_ref(), &mut create)
        .await
        .unwrap();
    ConfigAccess::write_local(
        &db.node,
        "test.signed_scope_parent",
        &create.graphql_mutation().unwrap(),
    )
    .await
    .unwrap();
    let request = crate::request_binding::load_agent_request(&db.node, &create.request_id)
        .await
        .unwrap()
        .unwrap();
    documents.insert(id, request.clone());
    request
}

fn queue(parent: &AgentRequest, key: u64) -> RequestQueue {
    RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background-scope:{key}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        delivery: Default::default(),
        position: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

#[tokio::test]
async fn generated_goal_wake_scopes_preserve_atomic_notification_bindings() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().goal_wake_coalescing_cases;
    assert_eq!(cases.len(), 10);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let db = test_db(name).await;
        let mut documents = RapidHashMap::new();
        let first = parent(&db, name, &case["first"], &mut documents).await;
        let second = parent(&db, name, &case["second"], &mut documents).await;
        let mut goals: RapidHashMap<u64, String> = RapidHashMap::new();
        let mut first_goal = None;
        for assignment in case["observed"].as_array().unwrap() {
            let goal_id = assignment["goal"].as_u64().unwrap();
            let value = json!({
                "goal_id":format!("goal-{goal_id}"), "node_did":db.node_did(), "session_id":name,
                "objective":"Keep each wake with its original work", "status":"active",
                "assignment_root_request_doc_id":assignment["root"].as_u64().map(|root| documents[&root].doc_id.clone()),
                "created_at":"2026-10-21T00:00:00Z", "updated_at":"2026-10-21T00:00:00Z"
            });
            let doc = goals.get(&goal_id).cloned();
            let result = ConfigAccess::transact_local_write(&db.node, None, "test.retained_wake_assignment", |txn| {
                let doc=doc.clone();let value=value.clone();
                Box::pin(async move {
                    if let Some(doc) = doc {
                        txn.execute_with_variables("mutation($doc: ID!, $input: GoalMutationInputArg!) {update_Goal(docID: $doc, input: $input) {_docID}}", &json!({"doc":doc,"input":value})).await?;
                        Ok(doc)
                    } else {
                        let result = txn.execute_with_variables("mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}", &json!({"input":value})).await?;
                        crate::graphql::created_doc_id(&result,"Goal")
                    }
                })
            }).await.unwrap();
            first_goal.get_or_insert_with(|| result.clone());
            goals.insert(goal_id, result);
        }
        if case["retire_first"] == true {
            ConfigAccess::write_local(
                &db.node,
                "test.retire_wake_assignment",
                &format!(
                    "mutation {{delete_Goal(docID: \"{}\") {{_docID}}}}",
                    escape_graphql_string(first_goal.as_ref().unwrap())
                ),
            )
            .await
            .unwrap();
        }
        let first_notice = "background-completion-notification:scope-first:tool";
        let second_notice = "background-completion-notification:scope-second:tool";
        let publish = |parent: AgentRequest, key: u64, notice: &'static str| {
            let node = db.node.clone();
            async move {
                persist_background_completion_with_message(
                    &node,
                    &parent,
                    notice,
                    notice,
                    "review notifications",
                    queue(&parent, key),
                    None,
                )
                .await
            }
        };
        let first_wake = publish(
            first.clone(),
            case["first_key"].as_u64().unwrap(),
            first_notice,
        )
        .await
        .unwrap();
        let second_wake = publish(
            second.clone(),
            case["second_key"].as_u64().unwrap(),
            second_notice,
        )
        .await
        .unwrap();
        assert_eq!(
            first_wake.created_request,
            case["expected_wake"].as_bool().unwrap(),
            "{name}: independent delivery"
        );
        let first_wake = first_wake.request.unwrap();
        let second_wake = second_wake.request.unwrap();
        assert_eq!(
            first_wake.doc_id == second_wake.doc_id,
            case["expected_coalesced"].as_bool().unwrap(),
            "{name}: assignment coalescing"
        );
        for (wake, field) in [(&first_wake, "first_scope"), (&second_wake, "second_scope")] {
            let row = crate::request_binding::load_agent_request(&db.node, &wake.request_id)
                .await
                .unwrap()
                .unwrap();
            let scope = &case[field];
            let reference = (scope["kind"] == "referenced").then(|| GoalAssignmentInput {
                goal_id: format!("goal-{}", scope["goal"]),
                root_request_doc_id: Some(
                    documents[&scope["root"].as_u64().unwrap()].doc_id.clone(),
                ),
            });
            assert_eq!(
                row.input.goal_assignment, reference,
                "{name}: signed wake attribution"
            );
        }
        let replay = publish(first, case["first_key"].as_u64().unwrap(), first_notice)
            .await
            .unwrap()
            .request
            .unwrap();
        assert_eq!(
            replay.doc_id, first_wake.doc_id,
            "{name}: immutable replay binding"
        );
        let response = db
            .node
            .execute("{AgentMessage {message_key request_doc_id}}")
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let messages = response.data.as_ref().unwrap()["AgentMessage"]
            .as_array()
            .unwrap();
        assert_eq!(
            messages.len(),
            2,
            "{name}: exactly one receipt per notification"
        );
        for (key, wake) in [(first_notice, &first_wake), (second_notice, &second_wake)] {
            let message = messages
                .iter()
                .find(|message| message["message_key"] == key)
                .unwrap();
            assert_eq!(
                message["request_doc_id"], wake.doc_id,
                "{name}: retained notification journal"
            );
        }
    }
}
