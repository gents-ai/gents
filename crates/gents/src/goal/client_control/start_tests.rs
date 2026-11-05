use super::contract_tests::{persist_command, receipt, try_persist_command};
use super::*;
use crate::runtime::agent::p2p_reconcile::session_goal_control::process_command;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
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
    selected_agent: u64,
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
    value["agent_did"]
        .as_str()
        .expect("fixture agent DID")
        .to_owned()
}

async fn seed_agents(
    node: &Arc<EmbeddedNode>,
    runtime: &dyn NodeIdentity,
    user: &dyn NodeIdentity,
    bindings: &[gents_protocol::agent_identity::AgentIdentityBinding],
) {
    let access = ConfigAccess::local_as(node.clone(), ::identity::Did::new(user.did()).unwrap());
    let (node_doc, agent_docs) = access.transact_write("test.goal_start.agents", |txn| Box::pin(async move {
        let node_doc = txn.execute_with_variables(
            "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
            &json!({"input": {"node_did": runtime.did()}}),
        ).await?;
        let node_doc = crate::store::graphql::created_doc_id(&node_doc, "Node")?;
        let mut agent_docs = Vec::new();
        for binding in bindings {
            let agent_doc = txn.execute_with_variables(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &json!({"input": {"node_did": runtime.did(), "agent_did": binding.agent_did,
                    "creation_nonce": binding.creation_nonce}}),
            ).await?;
            let agent_doc = crate::store::graphql::created_doc_id(&agent_doc, "Agent")?;
            agent_docs.push((agent_doc, binding.agent_did.clone()));
        }
        Ok::<_, anyhow::Error>((node_doc, agent_docs))
    })).await.unwrap();
    access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &::identity::Did::new(runtime.did()).unwrap(),
        )
        .await
        .unwrap();
    for (agent_doc, agent_did) in agent_docs {
        access
            .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
            .await
            .unwrap();
        access
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "executor",
                &::identity::Did::new(&agent_did).unwrap(),
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn generated_goal_starts_commit_the_session_goal_and_first_request_together() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_client_start_cases.len(), 53);
    for value in contracts.goal_client_start_cases {
        if value.get("replacement").is_some() {
            replacement_tests::run(&value).await;
            continue;
        }
        let mut case: Case = serde_json::from_value(value).unwrap();
        if matches!(
            case.name.as_str(),
            "requester_revoked"
                | "concurrent_revocation"
                | "invocation_revoked"
                | "concurrent_invocation_revocation"
        ) {
            // ACP1 does not expose transaction-bound permission observations;
            // these modeled decisions are outside the trial's guarantees.
            assert!(!case.published);
            continue;
        }
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap());
        let user = KeyIdentity::load_or_create(temp.path().join("user.key"), None).unwrap();
        let foreign = KeyIdentity::load_or_create(temp.path().join("other.key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let binding = crate::store::identity::provision_agent_identity(identity.as_ref()).unwrap();
        let other_binding =
            crate::store::identity::provision_agent_identity(identity.as_ref()).unwrap();
        let _agent =
            crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
        let _other_agent =
            crate::store::identity::resolve_agent_identity(identity.as_ref(), &other_binding)
                .unwrap();
        seed_agents(
            &node,
            identity.as_ref(),
            &user,
            &[binding.clone(), other_binding.clone()],
        )
        .await;
        case.requested["agent_did"] = json!(if case.requested["agent"] == 4 {
            &other_binding.agent_did
        } else {
            &binding.agent_did
        });
        if let Some(session) = &mut case.session {
            session["agent_did"] = json!(if session["agent"] == 4 {
                &other_binding.agent_did
            } else {
                &binding.agent_did
            });
        }
        let access =
            ConfigAccess::local_as(node.clone(), ::identity::Did::new(user.did()).unwrap());
        if let Some(session) = &case.session {
            let title = session["title"]
                .as_object()
                .map(|_| serde_json::from_value::<SessionTitle>(session["title"].clone()).unwrap());
            let requester = if session["scope"]["requester"] == 1 {
                user.did()
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
        let signed_session_doc_id = if let Some(session) = session_before
            .as_ref()
            .filter(|session| session.session.requester_did.as_deref() == Some(user.did()))
        {
            session.doc_id.clone()
        } else {
            access
                .transact_write("test.goal_signing_session", |txn| {
                    let selected_agent = agent(&case.requested);
                    let identity = identity.as_ref();
                    let user = &user;
                    Box::pin(async move {
                        crate::session::ensure_session_in_txn(
                            txn,
                            "separate-signing-session",
                            identity.did(),
                            &selected_agent,
                            Some(user.did()),
                            None,
                            None,
                            "2026-01-01T00:00:00Z",
                        )
                        .await?;
                        let session = crate::session::load_agent_session_owner_in_txn(
                            txn,
                            identity.did(),
                            "separate-signing-session",
                        )
                        .await?
                        .expect("created signing session");
                        Ok::<_, anyhow::Error>(session.doc_id)
                    })
                })
                .await
                .unwrap()
        };
        let signed_agent_doc_id =
            super::agent_document_for_test(&access, identity.did(), &agent(&case.requested)).await;
        if let Some(session) = &session_before {
            let session_agent_doc =
                super::agent_document_for_test(&access, identity.did(), &session.session.agent_did)
                    .await;
            crate::session::grant_created_session_relationships(
                &access,
                &session.doc_id,
                &session_agent_doc,
                &session.session.agent_did,
            )
            .await
            .unwrap();
        }
        if session_before
            .as_ref()
            .is_none_or(|session| session.doc_id != signed_session_doc_id)
        {
            crate::session::grant_created_session_relationships(
                &access,
                &signed_session_doc_id,
                &signed_agent_doc_id,
                &agent(&case.requested),
            )
            .await
            .unwrap();
        }
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
            let actor = case
                .session
                .as_ref()
                .map(agent)
                .unwrap_or_else(|| binding.agent_did.clone());
            let agent_access =
                ConfigAccess::local_as(node.clone(), ::identity::Did::new(&actor).unwrap());
            let existing = set_goal_from_access(
                &agent_access,
                identity.did(),
                SESSION,
                Some(objective),
                Some(GoalStatus::Active),
                Some(budget),
                None,
            )
            .await
            .unwrap();
            let creation_key = deterministic_goal_creation_key(identity.did(), SESSION);
            agent_access.transact_write("test.seed_goal_claim", |txn| Box::pin(async {
                txn.execute_with_variables(
                    "mutation($input: GoalCreationClaimMutationInputArg!) { create_GoalCreationClaim(input: $input) { _docID } }",
                    &json!({"input": {"creation_key": creation_key, "goal_id": existing.goal_id,
                        "node_did": identity.did(), "session_id": SESSION,
                        "session_doc_id": session_before.as_ref().unwrap().doc_id, "agent_did": actor,
                        "objective": objective.trim(), "token_budget": budget,
                        "created_at": "2026-01-01T00:00:00Z"}}),
                ).await?;
                Ok::<_, anyhow::Error>(())
            })).await.unwrap();
            if !case.existing_goal {
                agent_access
                    .write(
                        "test.remove_seed_goal",
                        &format!(
                            "mutation {{ delete_Goal(docID: \"{}\") {{ _docID }} }}",
                            escape_graphql_string(&existing.doc_id),
                        ),
                    )
                    .await
                    .unwrap();
            }
        }
        if case.busy {
            let mut busy = AgentRequestCreate::base(
                RequestPurpose::Normal,
                "busy",
                identity.did(),
                user.did(),
                agent(&case.requested),
                SESSION,
                "Earlier work",
                "interactive",
                time(2),
                AgentRequestAdmissionRecord::client(user.did()),
            );
            busy.session_doc_id = Some(signed_session_doc_id.clone());
            busy.agent_doc_id = Some(signed_agent_doc_id.clone());
            crate::sign_agent_request_create(&user, &mut busy)
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
            user.did(),
            agent(&case.requested),
            SESSION,
            "Work on this goal",
            "interactive",
            time(case.requested["created_at"].as_u64().unwrap()),
            AgentRequestAdmissionRecord::client(user.did()),
        );
        initial.session_doc_id = Some(signed_session_doc_id);
        initial.agent_doc_id = Some(signed_agent_doc_id);
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
        crate::sign_agent_request_create(&user, &mut initial)
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
            requester_did: user.did().into(),
            requester_peer_id: None,
            node_did: identity.did().into(),
            agent_did: if case.selected_agent == 4 {
                other_binding.agent_did.clone()
            } else {
                binding.agent_did.clone()
            },
            agent_doc_id: super::agent_document_for_test(
                &access,
                identity.did(),
                if case.selected_agent == 4 {
                    &other_binding.agent_did
                } else {
                    &binding.agent_did
                },
            )
            .await,
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
        command.signature = user
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        if !case.signature_valid {
            command.signature[0] ^= 1;
        }
        if matches!(
            case.name.as_str(),
            "unreserved_session"
                | "wrong_signed_agent"
                | "foreign_agent"
                | "recreated_session"
                | "unexpected_session"
        ) {
            assert!(!case.published);
            if command.session_doc_id.is_none() {
                assert!(
                    try_persist_command(&access, &command).await.is_err(),
                    "{}",
                    case.name
                );
            } else {
                let doc_id = persist_command(&access, &command).await;
                process_command(
                    &node,
                    &identity,
                    &doc_id,
                    &::identity::Did::new(&command.agent_did).unwrap(),
                )
                .await
                .unwrap();
                assert!(
                    matches!(
                        receipt(&access, &doc_id).await.unwrap().outcome,
                        GoalControlOutcome::Rejected { .. }
                    ),
                    "{}",
                    case.name
                );
            }
            node.shutdown().await.expect("node shutdown");
            continue;
        }
        let doc_id = persist_command(&access, &command).await;
        if case.name == "authority_unavailable" {
            let actor = ::identity::Did::new(&command.agent_did).unwrap();
            let result = ConfigApplyTxn::with_read_storage_failure(
                "AgentSessionGoalControl",
                process_command(&node, &identity, &doc_id, &actor),
            )
            .await;
            assert!(result.is_err());
            assert!(receipt(&access, &doc_id).await.is_none());
            node.shutdown().await.expect("node shutdown");
            continue;
        }
        if case.commit {
            assert_eq!(
                process_command(
                    &node,
                    &identity,
                    &doc_id,
                    &::identity::Did::new(&command.agent_did).unwrap()
                )
                .await
                .unwrap(),
                None,
                "{}",
                case.name
            );
            let receipt = receipt(&access, &doc_id).await.unwrap();
            receipt.validate_for(&command).unwrap();
            assert!(identity
                .verify(
                    &receipt.agent_did,
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
                let query = "{ AgentSessionGoalControl { _docID intent_json } AgentSessionGoalReceipt { _docID receipt_json } AgentRequest { _docID request_id admission_signature } Goal { _docID status objective } }";
                let before = access.execute(query).await.unwrap();
                assert_eq!(
                    crate::goal::recover_session_goal_control(&access, &user, &command)
                        .await
                        .unwrap(),
                    Some(receipt.clone()),
                    "{}: recover the exact signed creation result",
                    case.name
                );
                assert_eq!(
                    crate::goal::submit_prepared_session_goal_control(&access, &user, &command,)
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
            let txn = ConfigApplyTxn::begin_local(
                &node,
                Some(::identity::Did::new(&command.agent_did).unwrap()),
            )
            .await
            .unwrap();
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
        node.shutdown().await.expect("node shutdown");
    }
}
