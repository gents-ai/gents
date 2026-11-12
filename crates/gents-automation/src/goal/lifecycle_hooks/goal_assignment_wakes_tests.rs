use gents_lean_vocab::families::goal_wake_coalescing_contracts_assignment_wakes as wakes;
use gents_protocol::goal::RequestAssignmentScope;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{GoalAssignmentInput, QueuePolicy, QueueSource, RequestQueue};
use gents_protocol::row::AgentRequest;
use gents_session::request::lifecycle::queue::persist_background_completion_with_message_waking_with_access;
use gents_session::request::lifecycle::test_support::{test_db_with, TestDb};
use gents_store::graphql::escape_graphql_string;
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::json;

async fn parent(
    db: &TestDb,
    session: &str,
    model: &wakes::Entry,
    documents: &mut RapidHashMap<u64, AgentRequest>,
) -> AgentRequest {
    let id = model.doc;
    if let Some(request) = documents.get(&id) {
        return request.clone();
    }
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("scope-parent-{id}"),
        db.node_did(),
        db.agent_did(),
        db.agent_did(),
        session,
        "work",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(db.agent_did()),
    );
    create.input.goal_assignment = model.reference.as_ref().map(|reference| GoalAssignmentInput {
        goal_id: format!("goal-{}", reference.goal),
        root_request_doc_id: reference.root.map(|root| documents[&root].doc_id.clone()),
    });
    gents_session::request::admission::sign_agent_request_create(
        db.agent_identity.as_ref(),
        &mut create,
    )
    .await
    .unwrap();
    let mutation = create.graphql_mutation().unwrap();
    let node_did = db.node_did().to_owned();
    let agent_did = db.agent_did().to_owned();
    let now = chrono::Utc::now().to_rfc3339();
    // The parent's session and request are both Agent-owned rows, written as the Agent.
    db.agent_access
        .transact_write("test.signed_scope_parent", |txn| {
            let mutation = &mutation;
            let (node_did, agent_did, now) = (node_did.as_str(), agent_did.as_str(), now.as_str());
            Box::pin(async move {
                gents_session::session::ensure_session_in_txn(
                    txn,
                    session,
                    node_did,
                    agent_did,
                    Some(agent_did),
                    None,
                    None,
                    now,
                )
                .await?;
                Ok::<_, anyhow::Error>(txn.execute(mutation).await?)
            })
        })
        .await
        .unwrap();
    let request = gents_session::request::binding::load_agent_request_with_access(
        &db.agent_access,
        &create.request_id,
    )
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

fn scope_wire(
    scope: anyhow::Result<gents_session::request::hooks::queue_scope::Partition>,
    numbers: &RapidHashMap<String, u64>,
) -> Option<wakes::ScopeWire> {
    let partition = scope.ok()?;
    let key = match partition.keys() {
        [] => return None,
        [(_, key)] => key,
        keys => panic!("one plugin answers a wake's queue scope: {keys:?}"),
    };
    Some(match key {
        RequestAssignmentScope::Unassigned => wakes::ScopeWire {
            kind: "unassigned".into(),
            goal: None,
            root: None,
            entry: None,
        },
        RequestAssignmentScope::Unresolved { entry_doc_id } => wakes::ScopeWire {
            kind: "unresolved".into(),
            goal: None,
            root: None,
            entry: Some(numbers[entry_doc_id]),
        },
        RequestAssignmentScope::Referenced(reference) => wakes::ScopeWire {
            kind: "referenced".into(),
            goal: Some(
                reference
                    .goal_id
                    .strip_prefix("goal-")
                    .unwrap()
                    .parse()
                    .unwrap(),
            ),
            root: reference
                .root_request_doc_id
                .as_ref()
                .map(|root| numbers[root]),
            entry: None,
        },
    })
}

#[tokio::test]
async fn generated_goal_wake_scopes_preserve_atomic_notification_bindings() {
    for case in wakes::cases() {
        let name = case.input.name.as_str();
        let db = test_db_with(name, crate::lifecycle_hooks::composition()).await;
        let mut documents = RapidHashMap::new();
        let first = parent(&db, name, &case.input.first, &mut documents).await;
        let second = parent(&db, name, &case.input.second, &mut documents).await;
        let mut goals: RapidHashMap<u64, String> = RapidHashMap::new();
        let mut first_goal = None;
        for assignment in &case.input.observed {
            let goal_id = assignment.goal;
            let value = json!({
                "goal_id":format!("goal-{goal_id}"), "node_did":db.node_did(), "session_id":name,
                "objective":"Keep each wake with its original work", "status":"active",
                "assignment_root_request_doc_id":assignment.root.map(|root| documents[&root].doc_id.clone()),
                "created_at":"2026-10-21T00:00:00Z", "updated_at":"2026-10-21T00:00:00Z"
            });
            let doc = goals.get(&goal_id).cloned();
            let agent = ::identity::Did::new(db.agent_did()).unwrap();
            let root = ::identity::Did::new(db.node_did()).unwrap();
            let result = gents_store::config_client::ConfigAccess::transact_local_write(&db.node, Some(root), "test.retained_wake_assignment", |txn| {
                let doc=doc.clone();let value=value.clone();let agent=agent.clone();
                Box::pin(async move {
                    if let Some(doc) = doc {
                        txn.execute_with_variables("mutation($doc: ID!, $input: GoalMutationInputArg!) {update_Goal(docID: $doc, input: $input) {_docID}}", &json!({"doc":doc,"input":value})).await?;
                        Ok::<_, anyhow::Error>(doc)
                    } else {
                        let result = txn.execute_with_variables("mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}", &json!({"input":value})).await?;
                        let doc = gents_store::graphql::created_doc_id(&result,"Goal")?;
                        txn.add_actor_relationship("Goal", &doc, "reader", &agent).await?;
                        Ok(doc)
                    }
                })
            }).await.unwrap();
            first_goal.get_or_insert_with(|| result.clone());
            goals.insert(goal_id, result);
        }
        if case.input.retire_first {
            gents_store::config_client::ConfigAccess::local_as(
                db.node.clone(),
                ::identity::Did::new(db.node_did()).unwrap(),
            )
            .write(
                "test.retire_wake_assignment",
                &format!(
                    "mutation {{delete_Goal(docID: \"{}\") {{_docID}}}}",
                    escape_graphql_string(first_goal.as_ref().unwrap())
                ),
            )
            .await
            .unwrap();
        }
        let numbers: RapidHashMap<String, u64> = documents
            .iter()
            .map(|(number, request)| (request.doc_id.clone(), *number))
            .collect();
        let node_did = db.node_did().to_owned();
        let parents = [first.doc_id.clone(), second.doc_id.clone()];
        let partitions = db
            .agent_access
            .transact_readonly("test.observe_wake_scopes", |txn| {
                let (node_did, parents) = (node_did.as_str(), &parents);
                Box::pin(async move {
                    let scopes = gents_session::request::hooks::points::queue_scope(
                        txn,
                        node_did,
                        name,
                        parents.to_vec(),
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(
                        parents
                            .iter()
                            .map(|doc| scopes.scope(doc))
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .await
            .unwrap();
        let referenced: Vec<Option<GoalAssignmentInput>> = partitions
            .iter()
            .map(|scope| {
                scope
                    .as_ref()
                    .ok()
                    .and_then(|partition| partition.referenced().cloned())
            })
            .collect();
        let mut scopes = partitions
            .into_iter()
            .map(|scope| scope_wire(scope, &numbers));
        let (first_scope, second_scope) = (scopes.next().unwrap(), scopes.next().unwrap());
        let first_notice = "background-completion-notification:scope-first:tool";
        let second_notice = "background-completion-notification:scope-second:tool";
        let publish = |parent: AgentRequest, key: u64, notice: &'static str| {
            let node = db.node.clone();
            let access = db.agent_access.clone();
            async move {
                persist_background_completion_with_message_waking_with_access(
                    &access,
                    &node,
                    &parent,
                    notice,
                    notice,
                    "review notifications",
                    queue(&parent, key),
                    None,
                    gents_protocol::request_lifecycle::RequestHopCause::Continuation,
                )
                .await
            }
        };
        let first_wake = publish(first.clone(), case.input.first_key, first_notice)
            .await
            .unwrap();
        let second_wake = publish(second.clone(), case.input.second_key, second_notice)
            .await
            .unwrap();
        let wake = first_wake.created_request;
        let first_wake = first_wake.request.unwrap();
        let second_wake = second_wake.request.unwrap();
        let coalesced = first_wake.doc_id == second_wake.doc_id;
        for (wake, reference) in [(&first_wake, &referenced[0]), (&second_wake, &referenced[1])] {
            let row = gents_session::request::binding::load_agent_request_with_access(
                &db.agent_access,
                &wake.request_id,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                &row.input.goal_assignment, reference,
                "{name}: signed wake attribution"
            );
        }
        let replay = publish(first, case.input.first_key, first_notice)
            .await
            .unwrap()
            .request
            .unwrap();
        assert_eq!(
            replay.doc_id, first_wake.doc_id,
            "{name}: immutable replay binding"
        );
        let response = db
            .agent_access
            .execute("{AgentMessage {message_key request_doc_id}}")
            .await
            .unwrap();
        let messages = response["data"]["AgentMessage"].as_array().unwrap();
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
        case.check(wakes::Output {
            coalesced,
            wake,
            first_scope,
            second_scope,
        });
    }
}
