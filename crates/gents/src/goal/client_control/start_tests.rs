use super::contract_tests::{persist_command, receipt};
use super::*;
use crate::agent::p2p_reconcile::session_goal_control::process_command;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::identity::KeyIdentity;
use gents_protocol::goal_control::{GoalRevision, SESSION_GOAL_CONTROL_VERSION};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::{QueueDelivery, RequestInput};
use gents_protocol::session::SessionTitle;
use serde_json::{json, Value};
use std::sync::Arc;

#[path = "replacement_tests.rs"]
mod replacement_tests;

const SESSION: &str = "start-session";

#[derive(Deserialize)]
struct Contracts {
    goal_client_start_cases: Vec<Value>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    session: Option<Value>,
    requested: Value,
    existing_goal: bool,
    objective: String,
    budget: Option<i64>,
    signature_valid: bool,
    first_request_valid: bool,
    first_goal: Option<String>,
    first_root: Option<String>,
    now: i64,
    expires_at: i64,
    busy: bool,
    expected_session_document: Option<String>,
    commit: bool,
    published: bool,
    claim: Option<Value>,
}

fn time(value: u64) -> String {
    format!("2026-01-01T00:00:{value:02}Z")
}
fn agent(value: &Value) -> String {
    format!("agent-{}", value["agent"])
}

#[tokio::test]
async fn generated_goal_starts_commit_the_session_goal_and_first_request_together() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_client_start_cases.len(), 46);
    for value in contracts.goal_client_start_cases {
        if value.get("replacement").is_some() {
            replacement_tests::run(&value).await;
            continue;
        }
        let case: Case = serde_json::from_value(value).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap());
        let foreign = KeyIdentity::load_or_create(temp.path().join("other.key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        crate::test_support::install_test_agent(&node, identity.did(), &agent(&case.requested))
            .await;
        let access = ConfigAccess::local(node.clone());
        if let Some(session) = &case.session {
            let title = session["title"]
                .as_object()
                .map(|_| serde_json::from_value::<SessionTitle>(session["title"].clone()).unwrap());
            let requester = if session["scope"]["requester"] == 1 {
                identity.did()
            } else {
                foreign.did()
            };
            access.transact_write("test.seed_goal_session", |txn| {
                let title = title.clone();
                let identity = identity.as_ref();
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(txn, SESSION, identity.did(), &agent(session),
                        Some(requester), title, None, &time(session["created_at"].as_u64().unwrap())).await?;
                    let tags = if session["tags"].as_array().unwrap().is_empty() { Value::Null } else { session["tags"].clone() };
                    let closed = session["closed_at"].as_u64().map(time);
                    txn.execute_with_variables("mutation($input: AgentSessionMutationInputArg!) { update_AgentSession(filter: {session_id: {_eq: \"start-session\"}}, input: $input) { _docID } }",
                        &json!({"input":{"closed_at":closed,"tags":tags}})).await?;
                    Ok::<_, anyhow::Error>(())
                })
            }).await.unwrap();
        }
        let session_before = access
            .transact_readonly("test.session_before", |txn| {
                Box::pin(async {
                    crate::session::load_agent_session_owner_in_txn(txn, identity.did(), SESSION)
                        .await
                })
            })
            .await
            .unwrap();
        if case.existing_goal || case.claim.is_some() {
            let objective = case
                .claim
                .as_ref()
                .map(|claim| claim["objective"].as_str().unwrap())
                .unwrap_or("existing");
            let budget = case
                .claim
                .as_ref()
                .and_then(|claim| claim["token_budget"].as_i64())
                .or(Some(1000));
            let existing =
                create_goal_for_session(&node, identity.did(), SESSION, objective, budget)
                    .await
                    .unwrap();
            if !case.existing_goal {
                delete_goal(&node, existing.goal()).await.unwrap();
            }
        }
        if case.busy {
            let mut busy = AgentRequestCreate::base(
                RequestPurpose::Normal,
                "busy",
                identity.did(),
                identity.did(),
                agent(&case.requested),
                SESSION,
                "Earlier work",
                "interactive",
                time(2),
                AgentRequestAdmissionRecord::local_self(identity.did()),
            );
            crate::sign_agent_request_create(identity.as_ref(), &mut busy)
                .await
                .unwrap();
            access
                .write("test.busy_goal_session", &busy.graphql_mutation().unwrap())
                .await
                .unwrap();
        }
        let command_id = uuid::Uuid::new_v4().to_string();
        let mut initial = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "first",
            identity.did(),
            identity.did(),
            agent(&case.requested),
            SESSION,
            "Work on this goal",
            "interactive",
            time(case.requested["created_at"].as_u64().unwrap()),
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        initial.input = crate::request::lifecycle::prepare_user_message_input(
            &access,
            identity.did(),
            SESSION,
            RequestInput::default(),
            QueueDelivery::Queue,
        )
        .await
        .unwrap();
        initial.input.goal_assignment = case.first_goal.as_ref().map(|goal| {
            gents_protocol::request_input::GoalAssignmentInput {
                goal_id: if goal == "initial-assignment" {
                    deterministic_goal_id(identity.did(), SESSION)
                } else {
                    goal.clone()
                },
                root_request_doc_id: case.first_root.clone(),
            }
        });
        initial.retry_key = Some(format!("session-goal:{command_id}"));
        crate::sign_agent_request_create(identity.as_ref(), &mut initial)
            .await
            .unwrap();
        if !case.first_request_valid {
            initial.admission.signature[0] ^= 1;
        }
        let now = Utc::now();
        let expired = case.now >= case.expires_at;
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id,
            requester_did: identity.did().into(),
            requester_peer_id: None,
            node_did: identity.did().into(),
            session_id: SESSION.into(),
            session_doc_id: case.expected_session_document.as_deref().map(|document| {
                if document == "session-document" {
                    session_before.as_ref().unwrap().doc_id.clone()
                } else {
                    document.into()
                }
            }),
            issued_at: (now - chrono::Duration::seconds(20)).to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(if expired { -10 } else { 300 }))
                .to_rfc3339(),
            expected: None::<GoalRevision>,
            operation: GoalOperation::Create {
                objective: case.objective,
                token_budget: case.budget,
            },
            initial_request: Some(Box::new(initial)),
            signature: Vec::new(),
        };
        command.signature = identity
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        if !case.signature_valid {
            command.signature[0] ^= 1;
        }
        let doc_id = persist_command(&access, &command).await;
        if case.commit {
            assert_eq!(
                process_command(&node, &identity, &doc_id).await.unwrap(),
                None,
                "{}",
                case.name
            );
            let receipt = receipt(&access, &doc_id).await.unwrap();
            receipt.validate_for(&command).unwrap();
            assert!(identity
                .verify(
                    identity.did(),
                    &receipt.signing_payload().unwrap(),
                    &receipt.signature
                )
                .await
                .unwrap());
            assert_eq!(
                matches!(receipt.outcome, GoalControlOutcome::Applied { .. }),
                case.published,
                "{}: {:?}",
                case.name,
                receipt.outcome
            );
            if case.signature_valid {
                let query = "{ AgentSessionGoalControl { _docID intent_json receipt_json } AgentRequest { _docID request_id admission_signature } Goal { _docID status objective } }";
                let before = access.execute(query).await.unwrap();
                assert_eq!(
                    crate::goal::recover_session_goal_control(&access, identity.as_ref(), &command)
                        .await
                        .unwrap(),
                    Some(receipt.clone()),
                    "{}: recover the exact signed creation result",
                    case.name
                );
                assert_eq!(
                    crate::goal::submit_prepared_session_goal_control(
                        &access,
                        identity.as_ref(),
                        &command,
                    )
                    .await
                    .unwrap(),
                    receipt,
                    "{}: resumption retains the original first request",
                    case.name
                );
                assert_eq!(
                    access.execute(query).await.unwrap(),
                    before,
                    "{}",
                    case.name
                );
            }
        } else {
            let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
            let prepared = prepare_goal_control(&txn, &command).await.unwrap().unwrap();
            stage_goal_control(&txn, identity.as_ref(), prepared)
                .await
                .unwrap();
            assert!(load_canonical_goal_in_txn(&txn, identity.did(), SESSION)
                .await
                .unwrap()
                .is_some());
            assert!(
                crate::session::load_agent_session_owner_in_txn(&txn, identity.did(), SESSION)
                    .await
                    .unwrap()
                    .is_some()
            );
            txn.discard().await.unwrap();
            assert!(receipt(&access, &doc_id).await.is_none());
        }
        let requests = access
            .execute(&format!(
                "{{ AgentRequest(filter: {{request_id: {{_eq: \"first\"}}}}) {{ {} }} }}",
                crate::SIGNED_REQUEST_FIELDS,
            ))
            .await
            .unwrap();
        assert_eq!(
            !requests["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            case.published,
            "{} first request",
            case.name
        );
        for value in requests["data"]["AgentRequest"].as_array().unwrap() {
            let request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                super::super::load_applied_opening_goal_id(&access, &request)
                    .await
                    .unwrap(),
                Some(deterministic_goal_id(identity.did(), SESSION)),
                "{} applied opening provenance",
                case.name,
            );
        }
        let goal = load_canonical_goal(&node, identity.did(), SESSION)
            .await
            .unwrap();
        assert_eq!(
            goal.is_some(),
            case.published || case.existing_goal,
            "{} goal",
            case.name
        );
        let after = access
            .transact_readonly("test.session_after", |txn| {
                Box::pin(async {
                    crate::session::load_agent_session_owner_in_txn(txn, identity.did(), SESSION)
                        .await
                })
            })
            .await
            .unwrap();
        if let Some(before) = session_before {
            assert_eq!(
                after.unwrap().session,
                before.session,
                "{} preserves existing session",
                case.name
            );
        } else {
            assert_eq!(after.is_some(), case.published, "{} session", case.name);
        }
        node.shutdown().await;
    }
}
