use super::contract_tests::{persist_command, receipt, try_persist_command};
use super::*;
use crate::runtime::agent::p2p_reconcile::session_goal_control::process_command;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
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
        let (db, user, access, binding) =
            crate::test_support::signed_test_node("generated-goal-start").await;
        let node = db.node.clone();
        let identity = db.node_identity.clone();
        let foreign = tokio::task::spawn_blocking(|| {
            crate::support::fixtures::test_identity("goal-start-foreign")
        })
        .await
        .unwrap();
        let foreign_access = db.client_access(&foreign);
        let user_did = ::identity::Did::new(user.did()).unwrap();
        let other_binding = crate::store::identity::provision_agent_identity_authorized(
            &node,
            identity.as_ref(),
            &user_did,
        )
        .await
        .unwrap();
        crate::test_support::install_test_agent_binding_with_access(
            &node,
            identity.did(),
            "second goal agent",
            &other_binding,
            &access,
        )
        .await;
        let agent_identity =
            crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
        let other_agent_identity =
            crate::store::identity::resolve_agent_identity(identity.as_ref(), &other_binding)
                .unwrap();
        let admin =
            ConfigAccess::local_as(node.clone(), ::identity::Did::new(identity.did()).unwrap());
        for principal in [agent_identity.did(), other_agent_identity.did()] {
            let principal = ::identity::Did::new(principal).unwrap();
            for permission in [
                crate::store::config_client::NodePermission::CollectionGet,
                crate::store::config_client::NodePermission::DocumentRead,
                crate::store::config_client::NodePermission::DocumentUpdate,
                crate::store::config_client::NodePermission::DocumentDelete,
                crate::store::config_client::NodePermission::DacRelationAdd,
                crate::store::config_client::NodePermission::DacRelationDelete,
            ] {
                admin
                    .grant_node_permission(&principal, permission)
                    .await
                    .unwrap();
            }
        }
        let foreign_did = ::identity::Did::new(foreign.did()).unwrap();
        for permission in [
            crate::store::config_client::NodePermission::CollectionGet,
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::DocumentUpdate,
            crate::store::config_client::NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&foreign_did, permission)
                .await
                .unwrap();
        }
        for agent_did in [&binding.agent_did, &other_binding.agent_did] {
            let doc = super::agent_document_for_test(&access, identity.did(), agent_did).await;
            access
                .add_actor_relationship(
                    "Agent",
                    &doc,
                    "session_writer",
                    &::identity::Did::new(foreign.did()).unwrap(),
                )
                .await
                .unwrap();
        }
        let (user, access, foreign, foreign_access) = if case.name == "foreign_requester" {
            (foreign, foreign_access, user, access)
        } else {
            (user, access, foreign, foreign_access)
        };
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
        if let Some(session) = &case.session {
            let title = session["title"]
                .as_object()
                .map(|_| serde_json::from_value::<SessionTitle>(session["title"].clone()).unwrap());
            let foreign_owner = session["scope"]["requester"] != 1;
            let (requester, session_access) = if foreign_owner {
                (foreign.did(), &foreign_access)
            } else {
                (user.did(), &access)
            };
            let session_doc = session_access.transact_write("test.seed_goal_session", |txn| {
                let title = title.clone();
                let identity = identity.as_ref();
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(txn, SESSION, identity.did(), &agent(session),
                        Some(requester), title, None, &time(session["created_at"].as_u64().unwrap())).await?;
                    let record = crate::session::load_agent_session_owner_in_txn(txn, identity.did(), SESSION)
                        .await?.expect("created modeled session");
                    let tags = if session["tags"].as_array().unwrap().is_empty() { Value::Null } else { session["tags"].clone() };
                    let closed = session["closed_at"].as_u64().map(time);
                    if !tags.is_null() || closed.is_some() {
                        let mutation = format!("mutation($input: AgentSessionMutationInputArg!) {{ update_AgentSession(docID: \"{}\", input: $input) {{ _docID }} }}", escape_graphql_string(&record.doc_id));
                        txn.execute_client_signed_update(&mutation, &json!({"input":{"closed_at":closed,"tags":tags}}), gents_protocol::schemas::AGENT_SESSION).await?;
                    }
                    Ok::<_, anyhow::Error>(record.doc_id)
                })
            }).await.unwrap();
            let agent_doc =
                super::agent_document_for_test(&access, identity.did(), &agent(session)).await;
            crate::session::grant_created_session_relationships(
                session_access,
                &session_doc,
                &agent_doc,
                &agent(session),
            )
            .await
            .unwrap();
            if foreign_owner {
                session_access
                    .add_actor_relationship(
                        "AgentSession",
                        &session_doc,
                        "reader",
                        &::identity::Did::new(user.did()).unwrap(),
                    )
                    .await
                    .unwrap();
            }
            if case.name == "wrong_signed_agent" {
                session_access
                    .add_actor_relationship(
                        "AgentSession",
                        &session_doc,
                        "reader",
                        &::identity::Did::new(&other_binding.agent_did).unwrap(),
                    )
                    .await
                    .unwrap();
            }
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
        if case.name == "foreign_requester" {
            let session = session_before.as_ref().unwrap();
            let owners = access
                .transact_readonly("test.foreign_session_owner", |txn| {
                    Box::pin(
                        txn.document_owners("AgentSession", std::slice::from_ref(&session.doc_id)),
                    )
                })
                .await
                .unwrap();
            assert_eq!(owners.len(), 1);
            assert_eq!(
                owners[0].owner.as_ref().map(::identity::Did::as_str),
                Some(foreign.did())
            );
            assert_ne!(foreign.did(), user.did());
        }
        let signed_session_doc_id = if let Some(session) =
            session_before.as_ref().filter(|session| {
                session.session.requester_did.as_deref() == Some(user.did())
                    || case.name == "foreign_requester"
            }) {
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
        let signed_agent = if case.name == "wrong_signed_agent" {
            other_binding.agent_did.clone()
        } else {
            agent(&case.requested)
        };
        let signed_agent_doc_id =
            super::agent_document_for_test(&access, identity.did(), &signed_agent).await;
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
            let selected = if actor == binding.agent_did {
                &agent_identity
            } else {
                &other_agent_identity
            };
            let agent_access = ConfigAccess::local_managed_agent(
                node.clone(),
                ::identity::Did::new(&actor).unwrap(),
                selected.as_ref(),
            )
            .unwrap();
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
            crate::request::lifecycle::submit_client_request(&access, &busy)
                .await
                .unwrap();
        }
        let command_id = uuid::Uuid::new_v4().to_string();
        let mut initial = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "first",
            identity.did(),
            session_before
                .as_ref()
                .and_then(|session| session.session.requester_did.as_deref())
                .unwrap_or(user.did()),
            &signed_agent,
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
                } else if document == "old-header" {
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
            native_evidence: None,
            signature: Vec::new(),
        };
        let initial = command.initial_request.as_deref().unwrap();
        command.native_evidence = Some(gents_protocol::goal_control::GoalNativeEvidence {
            request: crate::store::config_client::prepare_request_evidence(
                &user,
                crate::request::lifecycle::client_request_input(initial).unwrap(),
            )
            .await
            .unwrap(),
            replacement_delete: None,
        });
        command.signature = user
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        let selected_identity = if command.agent_did == binding.agent_did {
            agent_identity.as_ref()
        } else {
            other_agent_identity.as_ref()
        };
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
                if case.name == "recreated_session" {
                    let old_session = session_before.as_ref().unwrap().doc_id.clone();
                    assert_eq!(
                        command.session_doc_id.as_deref(),
                        Some(old_session.as_str())
                    );
                    let initial = command.initial_request.as_deref().unwrap();
                    assert_eq!(
                        initial.session_doc_id.as_deref(),
                        Some(old_session.as_str())
                    );
                    let signed_session = &session_before.as_ref().unwrap().session;
                    assert_eq!(initial.node_did, signed_session.node_did);
                    assert_eq!(initial.session_id, signed_session.session_id);
                    assert_eq!(initial.agent_did, signed_session.agent_did);
                    assert_eq!(
                        Some(initial.requester_did.as_str()),
                        signed_session.requester_did.as_deref()
                    );
                    assert!(user
                        .verify(
                            user.did(),
                            &initial.signing_payload(),
                            &initial.admission.signature
                        )
                        .await
                        .unwrap());
                    assert!(user
                        .verify(
                            user.did(),
                            &command.signing_payload().unwrap(),
                            &command.signature,
                        )
                        .await
                        .unwrap());
                    access
                        .write(
                            "test.goal.delete_original_session",
                            &format!(
                                r#"mutation {{ delete_AgentSession(docID: "{}") {{ _docID }} }}"#,
                                escape_graphql_string(&old_session),
                            ),
                        )
                        .await
                        .unwrap();
                    let recreated = access
                        .transact_write("test.goal.recreate_session", |txn| {
                            Box::pin(async {
                                crate::session::ensure_session_in_txn(
                                    txn,
                                    SESSION,
                                    identity.did(),
                                    &command.agent_did,
                                    Some(user.did()),
                                    None,
                                    None,
                                    &time(3),
                                )
                                .await?;
                                crate::session::load_agent_session_owner_in_txn(
                                    txn,
                                    identity.did(),
                                    SESSION,
                                )
                                .await?
                                .context("recreated session is missing")
                            })
                        })
                        .await
                        .unwrap();
                    assert_ne!(recreated.session.created_at, signed_session.created_at);
                    assert_ne!(recreated.doc_id, old_session);
                    crate::session::grant_created_session_relationships(
                        &access,
                        &recreated.doc_id,
                        &command.agent_doc_id,
                        &command.agent_did,
                    )
                    .await
                    .unwrap();
                    let agent_access = ConfigAccess::local_managed_agent(
                        node.clone(),
                        ::identity::Did::new(&command.agent_did).unwrap(),
                        selected_identity,
                    )
                    .unwrap();
                    let before = agent_access
                        .execute("{ Goal { _docID } AgentRequest { _docID } GoalCreationClaim { _docID } }")
                        .await
                        .unwrap();
                    process_command(&node, &identity, &doc_id, selected_identity)
                        .await
                        .unwrap();
                    assert!(receipt(&access, &doc_id).await.is_none());
                    assert_eq!(
                        agent_access.execute("{ Goal { _docID } AgentRequest { _docID } GoalCreationClaim { _docID } }").await.unwrap(),
                        before,
                        "stale target must not publish any goal or request"
                    );
                    let after = access
                        .transact_readonly("test.goal.recreated_session_after", |txn| {
                            Box::pin(crate::session::load_agent_session_owner_in_txn(
                                txn,
                                identity.did(),
                                SESSION,
                            ))
                        })
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(after.doc_id, recreated.doc_id);
                    assert_eq!(after.session, recreated.session);
                    node.shutdown().await.expect("node shutdown");
                    continue;
                }
                process_command(&node, &identity, &doc_id, selected_identity)
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
            let result = ConfigApplyTxn::with_read_storage_failure(
                "AgentSessionGoalControl",
                process_command(&node, &identity, &doc_id, selected_identity),
            )
            .await;
            assert!(result.is_err());
            assert!(receipt(&access, &doc_id).await.is_none());
            node.shutdown().await.expect("node shutdown");
            continue;
        }
        if case.commit {
            assert_eq!(
                process_command(&node, &identity, &doc_id, selected_identity)
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
            let txn = ConfigApplyTxn::begin_local_with_signer(
                &node,
                ::identity::Did::new(&command.agent_did).unwrap(),
                selected_identity.managed_agent_signer().unwrap(),
            )
            .await
            .unwrap();
            let mut prepared = prepare_goal_control(&txn, &command).await.unwrap().unwrap();
            attach_client_goal_graphs(&txn, &command, &mut prepared)
                .await
                .unwrap()
                .unwrap();
            stage_goal_control(&txn, selected_identity, prepared)
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
            let initial = command.initial_request.as_deref().unwrap();
            assert_eq!(
                value["admission_signature"],
                bs58::encode(&initial.admission.signature).into_string()
            );
            let expected_graph = crate::store::config_client::verify_request_evidence(
                crate::request::lifecycle::client_request_input(initial).unwrap(),
                command.native_evidence.as_ref().unwrap().request.clone(),
            )
            .await
            .unwrap();
            let physical = value["_docID"].as_str().unwrap().to_owned();
            assert_eq!(physical, expected_graph.doc_id());
            let owners = access
                .transact_readonly("test.goal_first_native_owner", |txn| {
                    Box::pin(txn.document_owners("AgentRequest", std::slice::from_ref(&physical)))
                })
                .await
                .unwrap();
            assert_eq!(
                owners[0].owner.as_ref().map(|owner| owner.as_str()),
                Some(user.did())
            );
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
        let goal = access
            .transact_readonly("test.goal_start.result", |txn| {
                Box::pin(load_canonical_goal_in_txn(txn, identity.did(), SESSION))
            })
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

#[test]
fn modeled_client_goal_actor_binding_requires_the_transaction_signer() {
    let model = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("identity conformance case");
    for case in &model.client_goal_transaction_actor_cases {
        assert_eq!(
            super::client_goal_transaction_actor_matches(
                case["actor"].as_str(),
                case["requester"].as_str().unwrap(),
            ),
            case["allowed"].as_bool().unwrap(),
            "{case}",
        );
    }
}

#[tokio::test]
async fn client_goal_control_cannot_publish_user_graph_as_the_agent() {
    use crate::request::lifecycle::{
        build_signed_client_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
    };
    let modeled = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .client_goal_transaction_actor_cases
        .iter()
        .find(|case| case["actor"] == "agent" && case["requester"] == "user")
        .unwrap();
    for replacement in [false, true] {
        let (db, user, client, binding) =
            crate::test_support::signed_test_node("goal-actor-fence").await;
        let node = db.node.clone();
        let runtime = db.node_identity.clone();
        let agent =
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let admin =
            ConfigAccess::local_as(node.clone(), ::identity::Did::new(runtime.did()).unwrap());
        let actor = ::identity::Did::new(agent.did()).unwrap();
        for permission in [
            crate::store::config_client::NodePermission::CollectionGet,
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::DocumentUpdate,
            crate::store::config_client::NodePermission::DocumentDelete,
            crate::store::config_client::NodePermission::DacRelationAdd,
            crate::store::config_client::NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&actor, permission)
                .await
                .unwrap();
        }
        let agent_access =
            ConfigAccess::local_managed_agent(node.clone(), actor, agent.as_ref()).unwrap();
        let agent_doc = super::agent_document_for_test(&client, runtime.did(), agent.did()).await;
        client
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "session_writer",
                &::identity::Did::new(user.did()).unwrap(),
            )
            .await
            .unwrap();
        let mut spec = RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                node_did: runtime.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: agent.did().into(),
                session_id: SESSION.into(),
                request_id: "user-signed-goal-first".into(),
                content: "User work".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: "2026-11-04T00:00:00Z".into(),
            },
            AgentRequestAdmissionRecord::client(user.did()),
        );
        spec.retry_key = Some("goal-control-actor-fence".into());
        let request = build_signed_client_request(&client, spec, RequestSigner::Identity(&user))
            .await
            .unwrap();
        let old = if replacement {
            let goal = crate::goal::set_goal_from_access(
                &agent_access,
                runtime.did(),
                SESSION,
                Some("old terminal work"),
                Some(GoalStatus::Complete),
                None,
                None,
            )
            .await
            .unwrap();
            let owners = agent_access
                .transact_readonly("test.control_fence.goal_owner", |txn| {
                    Box::pin(txn.document_owners("Goal", std::slice::from_ref(&goal.doc_id)))
                })
                .await
                .unwrap();
            assert_eq!(
                owners[0].owner.as_ref().map(|did| did.as_str()),
                Some(agent.did())
            );
            Some(goal)
        } else {
            None
        };
        let before = agent_access
            .execute("{Goal {_docID status objective} AgentRequest {_docID}}")
            .await
            .unwrap();
        let result = agent_access
            .transact_write("test.control_fence", |txn| {
                let runtime = &runtime;
                let requested = PreparedGoalControl::Create {
                    objective: "new User work".into(),
                    token_budget: None,
                    request: Box::new(request.clone()),
                    replacement: old.clone().map(|goal| {
                        (
                            goal,
                            replacement_goal_identity(runtime.did(), SESSION, "actor-fence"),
                        )
                    }),
                    client_graphs: None,
                };
                Box::pin(async move { stage_goal_control(txn, runtime.as_ref(), requested).await })
            })
            .await;
        assert_eq!(result.is_ok(), modeled["allowed"].as_bool().unwrap());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("requester's native transaction signer"));
        let after = agent_access
            .execute("{Goal {_docID status objective} AgentRequest {_docID}}")
            .await
            .unwrap();
        assert_eq!(
            after, before,
            "both fresh and replacement writes must remain absent"
        );

        let cases = &crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap()
            .client_goal_graph_handoff_cases;
        assert_eq!(cases.len(), 7);
        let foreign =
            tokio::task::spawn_blocking(|| crate::support::fixtures::test_identity("goal-foreign"))
                .await
                .unwrap();
        for case in cases.iter().filter(|case| case["replacing"] == replacement) {
            let requester: &dyn NodeIdentity = if case["authorized"] == false {
                &foreign
            } else {
                &user
            };
            let mut initial = request.clone();
            initial.admission.signer_did = requester.did().into();
            crate::sign_agent_request_create(requester, &mut initial)
                .await
                .unwrap();
            let request_graph = crate::store::config_client::prepare_request_evidence(
                requester,
                crate::request::lifecycle::client_request_input(&initial).unwrap(),
            )
            .await
            .unwrap();
            let deletion = if let Some(goal) = &old {
                Some(
                    crate::store::config_client::prepare_goal_delete_evidence(
                        &client,
                        &user,
                        &goal.doc_id,
                    )
                    .await
                    .unwrap(),
                )
            } else {
                None
            };
            let mut command = SessionGoalControl {
                version: SESSION_GOAL_CONTROL_VERSION,
                command_id: "graph-handoff".into(),
                requester_did: requester.did().into(),
                requester_peer_id: None,
                node_did: runtime.did().into(),
                agent_did: agent.did().into(),
                agent_doc_id: initial.agent_doc_id.clone().unwrap(),
                session_id: SESSION.into(),
                session_doc_id: initial.session_doc_id.clone(),
                issued_at: "2026-11-04T00:00:00Z".into(),
                expires_at: "2026-11-04T00:05:00Z".into(),
                expected: old.as_ref().map(|goal| GoalRevision {
                    document: goal.doc_id.clone(),
                    composite: "observed".into(),
                }),
                operation: GoalOperation::Create {
                    objective: "new User work".into(),
                    token_budget: None,
                },
                initial_request: Some(Box::new(initial.clone())),
                native_evidence: Some(gents_protocol::goal_control::GoalNativeEvidence {
                    request: request_graph,
                    replacement_delete: deletion,
                }),
                signature: Vec::new(),
            };
            if case["command"] == false {
                command.agent_doc_id = "different-agent".into();
            }
            if case["create"] == false {
                command.native_evidence.as_mut().unwrap().request["composite_signature"][0] =
                    json!(256);
            }
            if case["delete"] == false && replacement {
                command
                    .native_evidence
                    .as_mut()
                    .unwrap()
                    .replacement_delete
                    .as_mut()
                    .unwrap()["composite_signature"][0] = json!(256);
            }
            let txn = if case["actor"] == true {
                ConfigApplyTxn::begin_local_with_signer(
                    &node,
                    ::identity::Did::new(agent.did()).unwrap(),
                    agent.managed_agent_signer().unwrap(),
                )
                .await
                .unwrap()
            } else {
                ConfigApplyTxn::begin_local(&node, Some(::identity::Did::new(user.did()).unwrap()))
                    .await
                    .unwrap()
            };
            let mut prepared = PreparedGoalControl::Create {
                objective: "new User work".into(),
                token_budget: None,
                request: Box::new(initial),
                replacement: old.clone().map(|goal| {
                    (
                        goal,
                        replacement_goal_identity(runtime.did(), SESSION, "graph-handoff"),
                    )
                }),
                client_graphs: None,
            };
            assert_eq!(
                attach_client_goal_graphs(&txn, &command, &mut prepared)
                    .await
                    .unwrap()
                    .is_ok(),
                case["allowed"].as_bool().unwrap(),
                "{case}"
            );
            txn.discard().await.unwrap();
        }
        if !replacement {
            let mut opening = request.clone();
            opening.retry_key = Some("session-goal:http-goal-control".into());
            opening.input.goal_assignment =
                Some(gents_protocol::request_input::GoalAssignmentInput {
                    goal_id: deterministic_goal_id(runtime.did(), SESSION),
                    root_request_doc_id: None,
                });
            crate::sign_agent_request_create(&user, &mut opening)
                .await
                .unwrap();
            let now = Utc::now();
            let command = SessionGoalControl {
                version: SESSION_GOAL_CONTROL_VERSION,
                command_id: "http-goal-control".into(),
                requester_did: user.did().into(),
                requester_peer_id: None,
                node_did: runtime.did().into(),
                agent_did: agent.did().into(),
                agent_doc_id: opening.agent_doc_id.clone().unwrap(),
                session_id: SESSION.into(),
                session_doc_id: opening.session_doc_id.clone(),
                issued_at: now.to_rfc3339(),
                expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
                expected: None,
                operation: GoalOperation::Create {
                    objective: "new User work".into(),
                    token_budget: None,
                },
                initial_request: Some(Box::new(opening)),
                native_evidence: None,
                signature: Vec::new(),
            };
            let command = crate::goal::prepare_session_goal_control(&client, &user, command)
                .await
                .unwrap();
            let command_doc = crate::goal::client_submission::publish(&client, &command)
                .await
                .unwrap();
            crate::goal::client_submission::grant_command_relationships(
                &client,
                &command,
                &command_doc,
            )
            .await
            .unwrap();
            let persisted = client
                .execute(&format!(
                    r#"{{ AgentSessionGoalControl(docID: "{}") {{ intent_json }} }}"#,
                    escape_graphql_string(&command_doc)
                ))
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_str::<SessionGoalControl>(
                    persisted["data"]["AgentSessionGoalControl"][0]["intent_json"]
                        .as_str()
                        .unwrap()
                )
                .unwrap(),
                command
            );
            let owners = client
                .transact_readonly("test.goal_command_user_owner", |txn| {
                    Box::pin(txn.document_owners(
                        "AgentSessionGoalControl",
                        std::slice::from_ref(&command_doc),
                    ))
                })
                .await
                .unwrap();
            assert_eq!(
                owners[0].owner.as_ref().map(|owner| owner.as_str()),
                Some(user.did())
            );
            let input = crate::request::lifecycle::client_request_input(&request).unwrap();
            let evidence =
                crate::store::config_client::prepare_request_evidence(&user, input.clone())
                    .await
                    .unwrap();
            let graph = Arc::new(
                crate::store::config_client::verify_request_evidence(input.clone(), evidence)
                    .await
                    .unwrap(),
            );
            let foreign_doc = agent_access.transact_write("test.foreign_goal_request", |txn| {
                let input = input.clone();
                Box::pin(async move {
                    let result = txn.execute_client_signed_create(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &json!({"input": input}), gents_protocol::schemas::AGENT_REQUEST,
                    ).await?;
                    crate::store::graphql::created_doc_id(&result, "AgentRequest")
                })
            }).await.unwrap();
            let txn = ConfigApplyTxn::begin_local_with_signer(
                &node,
                ::identity::Did::new(agent.did()).unwrap(),
                agent.managed_agent_signer().unwrap(),
            )
            .await
            .unwrap();
            let owners = txn
                .document_owners("AgentRequest", std::slice::from_ref(&foreign_doc))
                .await
                .unwrap();
            assert_eq!(
                owners[0].owner.as_ref().map(|owner| owner.as_str()),
                Some(agent.did())
            );
            let result = stage_goal_backed_request_with_identity_in_txn(
                &txn,
                runtime.did(),
                SESSION,
                "new User work",
                None,
                &request,
                None,
                Some(&graph),
            )
            .await;
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("signed native graph or owner"));
            txn.discard().await.unwrap();
        }
        if let Some(goal) = &old {
            let evidence = crate::store::config_client::prepare_goal_delete_evidence(
                &client,
                &user,
                &goal.doc_id,
            )
            .await
            .unwrap();
            agent_access.write("test.goal_graph_stale", &format!(
                r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "changed after signing" }}) {{ _docID }} }}"#,
                escape_graphql_string(&goal.doc_id),
            )).await.unwrap();
            let txn = ConfigApplyTxn::begin_local_with_signer(
                &node,
                ::identity::Did::new(agent.did()).unwrap(),
                agent.managed_agent_signer().unwrap(),
            )
            .await
            .unwrap();
            assert!(matches!(
                crate::store::config_client::verify_goal_delete_evidence(
                    &txn,
                    &goal.doc_id,
                    evidence
                )
                .await,
                Err(crate::store::config_client::ConfigClientError::ClientVerifiedMutationGraph(_))
            ));
            txn.discard().await.unwrap();
        }
        node.shutdown().await.unwrap();
    }
}
