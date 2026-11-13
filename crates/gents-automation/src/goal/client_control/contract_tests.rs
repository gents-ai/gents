use super::*;
use crate::goal::operator_resume::support::{execute, FixtureNode};
use crate::goal::session_goal_control::process_command;
use gents_protocol::goal_control::{
    GoalRevision, SessionGoalControlReceipt, SESSION_GOAL_CONTROL_VERSION,
};
use std::sync::Arc;

pub(super) async fn persist_command(
    access: &gents_store::config_client::ConfigAccess,
    command: &SessionGoalControl,
) -> String {
    try_persist_command(access, command).await.unwrap()
}

pub(super) async fn try_persist_command(
    access: &gents_store::config_client::ConfigAccess,
    command: &SessionGoalControl,
) -> anyhow::Result<String> {
    let document = crate::goal::client_submission::publish(access, command).await?;
    crate::goal::client_submission::grant_command_relationships(access, command, &document).await?;
    Ok(document)
}

pub(super) async fn receipt(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) -> Option<SessionGoalControlReceipt> {
    let response = access.execute(&format!(r#"{{ AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(doc_id))).await.unwrap();
    response["data"]["AgentSessionGoalReceipt"][0]["receipt_json"]
        .as_str()
        .map(|raw| serde_json::from_str(raw).unwrap())
}

/// Fixtures sharing a node slow down as it accumulates their documents, so
/// the cases run on a small pool of nodes, a few at a time.
const CASES_PER_NODE: usize = 32;
const NODES_AT_ONCE: usize = 4;

#[tokio::test]
async fn generated_goal_client_controls_drive_signed_commands() {
    use futures::StreamExt;
    let cases =
        gents_lean_vocab::families::goal_client_control_contracts_control::cases::<String>();
    futures::stream::iter(cases.chunks(CASES_PER_NODE))
        .for_each_concurrent(NODES_AT_ONCE, |chunk| async move {
            let node = FixtureNode::start().await;
            for case in chunk {
                drive_signed_command(&node, case).await;
            }
            node.shutdown().await;
        })
        .await;
}

async fn drive_signed_command(
    node: &FixtureNode,
    modeled_case: &gents_lean_vocab::family::Case<
        gents_lean_vocab::families::goal_client_control_contracts_control::ControlInput<String>,
        Option<
            gents_lean_vocab::families::goal_client_control_contracts_control::GoalSnapshot<String>,
        >,
    >,
) {
    let case = &modeled_case.input;
    let before_execution = serde_json::to_value(&case.before.execution).unwrap();
    let terminal_parent = !case.facts.unfinished_request;
    let fixture = node
        .fixture_with_parent_state(&before_execution, terminal_parent)
        .await;
    execute(
        &fixture.agent_access(),
        &format!(
            r#"mutation {{ update_Goal(docID: "{}", input: {{ objective: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.goal.doc_id),
            escape_graphql_string(&case.before.objective)
        ),
    )
    .await;
    let owner_access = fixture.user_access();
    let session = owner_access
        .transact_readonly("test.goal_session", |txn| {
            Box::pin(async {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    fixture.identity.did(),
                    &fixture.session,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    let (requester, access) = if !case.facts.authorized {
        let requester = Arc::new(
            tokio::task::spawn_blocking(|| {
                gents_store::test_support::test_identity("goal-denied-requester")
            })
            .await
            .unwrap(),
        );
        let requester_did = ::identity::Did::new(requester.did()).unwrap();
        let admin = gents_store::config_client::ConfigAccess::local_as(
            fixture.node.clone(),
            ::identity::Did::new(fixture.identity.did()).unwrap(),
        );
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&requester_did, permission)
                .await
                .unwrap();
        }
        owner_access
            .add_actor_relationship("AgentSession", &session.doc_id, "reader", &requester_did)
            .await
            .unwrap();
        owner_access
            .add_actor_relationship(
                "Agent",
                &fixture.agent_doc_id,
                "session_writer",
                &requester_did,
            )
            .await
            .unwrap();
        let owner_endpoint = match &owner_access {
            gents_store::config_client::ConfigAccess::Graphql(endpoint) => endpoint.url(),
            gents_store::config_client::ConfigAccess::Local(_) => {
                panic!("fixture requires held client access")
            }
        };
        let access = gents_store::config_client::ConfigAccess::Graphql(
            gents_store::config_client::GraphqlEndpoint::as_principal(
                owner_endpoint,
                requester.did(),
            )
            .with_client_signing_capability(requester.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
        );
        let decisions = fixture
            .agent_access()
            .transact_readonly("test.goal_denied_session_authority", |txn| {
                let requester_did = &requester_did;
                let session_doc = &session.doc_id;
                Box::pin(async move {
                    txn.observe_document_access(
                        requester_did,
                        "AgentSession",
                        std::slice::from_ref(session_doc),
                        defra_node::DocumentAccessMode::Named {
                            permission: "update".into(),
                            node_permission: acp::nac::NodePermission::DocumentUpdate,
                        },
                    )
                    .await
                })
            })
            .await
            .unwrap();
        assert_eq!(decisions, [defra_node::DocumentAccessDecision::Denied]);
        (requester, access)
    } else {
        (fixture.user.clone(), owner_access)
    };
    let revision = fixture.goal_commit(&fixture.goal.doc_id).await.cid;
    let before = fixture.observe().await;
    let operation = match case.operation.kind.as_str() {
        "create" => GoalOperation::Create {
            objective: case.operation.objective.clone().unwrap(),
            token_budget: case.operation.budget,
        },
        "edit" => GoalOperation::Edit {
            objective: case.operation.objective.clone(),
            token_budget: case.operation.budget,
        },
        "pause" => GoalOperation::Pause {},
        "resume" => GoalOperation::Resume {},
        "stop" => GoalOperation::Stop {},
        other => panic!("unknown modeled goal operation {other}"),
    };
    let now = Utc::now();
    let mut command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: uuid::Uuid::new_v4().to_string(),
        requester_did: requester.did().into(),
        requester_peer_id: None,
        node_did: fixture.identity.did().into(),
        agent_did: session.session.agent_did.clone(),
        agent_doc_id: super::agent_document_for_test(
            &access,
            fixture.identity.did(),
            &session.session.agent_did,
        )
        .await,
        session_id: fixture.session.clone(),
        session_doc_id: Some(session.doc_id),
        issued_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        expected: Some(GoalRevision {
            document: if case.expected_revision.document == case.before.revision.document {
                fixture.goal.doc_id.clone()
            } else {
                "foreign-goal-document".into()
            },
            composite: if case.expected_revision.composite == case.before.revision.composite {
                revision.clone()
            } else {
                "stale-goal-commit".into()
            },
        }),
        operation,
        initial_request: None,
        native_evidence: None,
        signature: Vec::new(),
    };
    command.signature = requester
        .sign(&command.signing_payload().unwrap())
        .await
        .unwrap();
    let doc_id = persist_command(&access, &command).await;
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    assert_eq!(
        process_command(&fixture.node, &identity, &doc_id, fixture.agent.as_ref())
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
    let observed = match &receipt.outcome {
        GoalControlOutcome::Applied { .. } => {
            Some(serde_json::from_value(fixture.control_snapshot(&revision).await).unwrap())
        }
        GoalControlOutcome::Rejected { .. } => {
            assert_eq!(
                fixture.observe().await,
                before,
                "{} unchanged execution",
                case.name
            );
            assert_eq!(
                fixture.current_goal().await.objective,
                case.before.objective,
                "{} unchanged objective",
                case.name
            );
            assert_eq!(
                fixture.goal_commit(&fixture.goal.doc_id).await.cid,
                revision,
                "{} no goal write",
                case.name
            );
            None
        }
    };
    modeled_case.check(observed);
    let first_receipt = serde_json::to_value(receipt).unwrap();
    process_command(&fixture.node, &identity, &doc_id, fixture.agent.as_ref())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(self::receipt(&access, &doc_id).await.unwrap()).unwrap(),
        first_receipt,
        "{} receipt replay",
        case.name
    );
}

#[tokio::test]
async fn goal_command_signature_binds_modeled_physical_targets() {
    let directory = tempfile::tempdir().unwrap();
    let user =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user.key"), None)
            .unwrap();
    let cases = &gents_lean_vocab::lean_contract_snapshot().cross_actor_authorization_cases;
    let baseline = cases
        .iter()
        .find(|case| case.name == "committed_claim")
        .unwrap();
    let mut command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: baseline.intent.document.clone(),
        requester_did: user.did().into(),
        requester_peer_id: None,
        node_did: "node".into(),
        agent_did: baseline.intent.selected_agent.clone(),
        agent_doc_id: baseline.intent.agent_document.clone(),
        session_id: "session".into(),
        session_doc_id: Some(baseline.intent.session.clone()),
        issued_at: "2026-11-04T00:00:00Z".into(),
        expires_at: "2026-11-04T00:01:00Z".into(),
        expected: Some(GoalRevision {
            document: "goal".into(),
            composite: "revision".into(),
        }),
        operation: GoalOperation::Pause {},
        initial_request: None,
        native_evidence: None,
        signature: Vec::new(),
    };
    command.signature = user
        .sign(&command.signing_payload().unwrap())
        .await
        .unwrap();
    for name in ["wrong_session", "wrong_agent_document"] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        let mut changed = command.clone();
        changed.session_doc_id = Some(case.observation.session.clone());
        changed.agent_doc_id = case.observation.agent_document.clone();
        let verified = user
            .verify(
                user.did(),
                &changed.signing_payload().unwrap(),
                &command.signature,
            )
            .await
            .unwrap_or(false);
        assert_eq!(
            verified,
            case.decision == gents_lean_vocab::LeanCrossActorDecision::Claimed,
            "{name}"
        );
    }
    assert!(user
        .verify(
            user.did(),
            &command.signing_payload().unwrap(),
            &command.signature
        )
        .await
        .unwrap());
}
