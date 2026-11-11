use super::*;
use crate::goal::operator_resume::support::{Fixture, SESSION};
use futures::FutureExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct Contracts {
    goal_observation_cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    scope: Scope,
    goal: Option<Reference>,
    verified: bool,
    accepted: bool,
    recovery: Option<RecoveryTrace>,
}

#[derive(Deserialize)]
struct RecoveryTrace {
    before: Option<Reference>,
    after: Option<Reference>,
}

#[derive(Deserialize)]
struct Scope {
    node: String,
    agent: String,
    session: String,
    session_document: String,
    requester: Option<String>,
}

#[derive(Deserialize)]
struct Reference {
    node: String,
    session: String,
    document: String,
    composite: String,
}

async fn fixture() -> Fixture {
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    Fixture::new(&before).await
}

async fn session(f: &Fixture) -> SessionOwnerRow {
    f.agent_access()
        .transact_readonly("test.observed_session", |txn| {
            Box::pin(async {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    f.identity.did(),
                    SESSION,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn generated_goal_observations_drive_signature_and_scope_validation() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_observation_cases.len(), 504);
    let f = fixture().await;
    let current = session(&f).await;
    let native = f
        .agent_access()
        .transact_readonly("test.native_goal", |txn| {
            Box::pin(async { load_canonical_goal_in_txn(txn, f.identity.did(), SESSION).await })
        })
        .await
        .unwrap()
        .unwrap();
    let map = |value: &str| match value {
        "node" => f.identity.did().to_owned(),
        "requester" => f.user.did().to_owned(),
        "agent" => f.parent.agent_did.clone(),
        "session" => SESSION.to_owned(),
        "session-document" => current.doc_id.clone(),
        other => other.to_owned(),
    };
    for case in contracts.goal_observation_cases {
        let mut observation = SessionGoalObservation {
            version: SESSION_GOAL_CONTROL_VERSION,
            node_did: map(&case.scope.node),
            agent_did: map(&case.scope.agent),
            session_id: map(&case.scope.session),
            session_doc_id: map(&case.scope.session_document),
            requester_did: case.scope.requester.as_deref().map(map),
            observed_at: Utc::now().to_rfc3339(),
            goal: case.goal.map(|reference| {
                let mut snapshot = snapshot(&native, Utc::now());
                snapshot.node_did = map(&reference.node);
                snapshot.session_id = map(&reference.session);
                ObservedGoal {
                    assignment_root_request_doc_id: native.assignment_root_request_doc_id.clone(),
                    revision: GoalRevision {
                        document: reference.document,
                        composite: reference.composite,
                    },
                    snapshot,
                }
            }),
            signature: Vec::new(),
        };
        observation.signature = f
            .agent
            .sign(&observation.signing_payload().unwrap())
            .await
            .unwrap();
        if !case.verified {
            observation.signature[0] ^= 1;
        }
        let row = ObservationRow {
            doc_id: "observation-document".into(),
            node_did: observation.node_did.clone(),
            session_id: observation.session_id.clone(),
            session_doc_id: observation.session_doc_id.clone(),
            requester_did: observation.requester_did.clone(),
            observation_json: Some(serde_json::to_string(&observation).unwrap()),
        };
        if case.accepted {
            let mut node_signed = observation.clone();
            node_signed.signature = f
                .identity
                .sign(&node_signed.signing_payload().unwrap())
                .await
                .unwrap();
            let node_row = ObservationRow {
                doc_id: row.doc_id.clone(),
                node_did: row.node_did.clone(),
                session_id: row.session_id.clone(),
                session_doc_id: row.session_doc_id.clone(),
                requester_did: row.requester_did.clone(),
                observation_json: Some(serde_json::to_string(&node_signed).unwrap()),
            };
            assert!(
                verified(f.identity.as_ref(), &current, &node_row)
                    .await
                    .unwrap()
                    .is_none(),
                "node signature must not replace selected agent signature"
            );
        }
        assert_eq!(
            verified(f.identity.as_ref(), &current, &row)
                .await
                .unwrap()
                .is_some(),
            case.accepted,
            "observation: {observation:?}"
        );
    }
    f.node.shutdown().await.expect("node shutdown");
}

async fn durable_rows(f: &Fixture) -> Value {
    f.agent_access()
        .execute("{ GoalObservation { _docID _version { cid } observation_json } }")
        .await
        .unwrap()["data"]["GoalObservation"]
        .clone()
}

#[tokio::test]
async fn invalid_session_does_not_starve_other_goal_observations() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = f.agent_access();
    access
        .write(
            "test.incomplete_goal_session",
            &format!(
                r#"mutation {{ create_AgentSession(input: {{
        node_did: "{}", session_id: "bad-session"
    }}) {{ _docID }} }}"#,
                escape_graphql_string(identity.did())
            ),
        )
        .await
        .unwrap();
    assert!(reconcile_goal_observations_with_identities(
        &f.node,
        &identity,
        std::slice::from_ref(&f.agent),
        true,
        &[]
    )
    .await
    .is_ok());
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(f.user.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .is_some());
    let settled = durable_rows(&f).await;
    assert!(reconcile_goal_observations_with_identities(
        &f.node,
        &identity,
        std::slice::from_ref(&f.agent),
        true,
        &[]
    )
    .await
    .is_ok());
    assert_eq!(durable_rows(&f).await, settled);
    f.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn goal_projection_is_signed_scoped_and_settles_without_writes() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = f.agent_access();
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(f.user.did())
    )
    .await
    .unwrap()
    .is_none());
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    let first = load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(f.user.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        first.goal.as_ref().unwrap().snapshot.goal_id,
        f.goal.goal_id
    );
    let settled = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    assert_eq!(durable_rows(&f).await, settled);
    let id = settled[0]["_docID"].as_str().unwrap();
    access.write("test.forged_goal_observation", &format!(r#"mutation {{ update_GoalObservation(docID: "{}", input: {{ observation_json: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(id), escape_graphql_string("{}"))).await.unwrap();
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(f.user.did())
    )
    .await
    .unwrap()
    .is_none());
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(f.user.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .is_some());
    access
        .write(
            "test.delete_goal",
            &format!(
                r#"mutation {{ delete_Goal(docID: "{}") {{ _docID }} }}"#,
                escape_graphql_string(&f.goal.doc_id)
            ),
        )
        .await
        .unwrap();
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    let empty = load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(f.user.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(empty.goal.is_none());
    let settled = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    assert_eq!(durable_rows(&f).await, settled);
    f.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn retained_signed_observation_refreshes_its_root_without_changing_goal_revision() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = f.agent_access();
    access.write("test.goal_physical_root", &format!(
        "mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(&f.goal.doc_id), escape_graphql_string(&f.parent.doc_id))).await.unwrap();
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    let mut retained = load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.user.did()),
    )
    .await
    .unwrap()
    .unwrap();
    let revision = retained.goal.as_ref().unwrap().revision.clone();
    assert_eq!(
        retained
            .goal
            .as_ref()
            .unwrap()
            .assignment_root_request_doc_id
            .as_deref(),
        Some(f.parent.doc_id.as_str())
    );
    retained
        .goal
        .as_mut()
        .unwrap()
        .assignment_root_request_doc_id = None;
    retained.signature = f
        .agent
        .sign(&retained.signing_payload().unwrap())
        .await
        .unwrap();
    assert!(serde_json::to_value(retained.goal.as_ref().unwrap())
        .unwrap()
        .get("assignment_root_request_doc_id")
        .is_none());
    let rows = durable_rows(&f).await;
    let doc = rows[0]["_docID"].as_str().unwrap();
    access.write("test.retained_goal_observation", &format!(
        "mutation {{update_GoalObservation(docID: \"{}\", input: {{observation_json: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(doc),escape_graphql_string(&serde_json::to_string(&retained).unwrap()))).await.unwrap();
    assert!(load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.user.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .unwrap()
    .assignment_root_request_doc_id
    .is_none());
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    let refreshed = load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.user.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(refreshed.goal.as_ref().unwrap().revision, revision);
    assert_eq!(
        refreshed
            .goal
            .as_ref()
            .unwrap()
            .assignment_root_request_doc_id
            .as_deref(),
        Some(f.parent.doc_id.as_str())
    );
    let stable = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    assert_eq!(durable_rows(&f).await, stable);
    let mut substituted = refreshed;
    substituted
        .goal
        .as_mut()
        .unwrap()
        .assignment_root_request_doc_id = Some("another-physical-request".into());
    access.write("test.substituted_goal_root", &format!(
        "mutation {{update_GoalObservation(docID: \"{}\", input: {{observation_json: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(doc),escape_graphql_string(&serde_json::to_string(&substituted).unwrap()))).await.unwrap();
    assert!(load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.user.did())
    )
    .await
    .unwrap()
    .is_none());
    f.node.shutdown().await.expect("node shutdown");
}

fn modeled_reference(value: &Value) -> Option<gents_protocol::request_input::GoalAssignmentInput> {
    (!value.is_null()).then(|| gents_protocol::request_input::GoalAssignmentInput {
        goal_id: format!("goal:{}", value["goal"].as_u64().unwrap()),
        root_request_doc_id: value["root"]
            .as_u64()
            .map(|root| format!("request-document:{root}")),
    })
}

fn modeled_observation(goal: &Value, agent_did: &str) -> SessionGoalObservation {
    SessionGoalObservation {
        version: SESSION_GOAL_CONTROL_VERSION,
        node_did: "did:key:node".into(),
        agent_did: agent_did.into(),
        session_id: "chat".into(),
        session_doc_id: "session-document".into(),
        requester_did: Some("did:key:user".into()),
        observed_at: "2026-11-08T00:00:00Z".into(),
        goal: modeled_reference(goal).map(|reference| {
            let document: GoalDocument = serde_json::from_value(json!({
                "_docID": "goal-document", "node_did": "did:key:node", "session_id": "chat",
                "goal_id": reference.goal_id, "objective": "Modeled", "status": "active"
            }))
            .unwrap();
            ObservedGoal {
                assignment_root_request_doc_id: reference.root_request_doc_id,
                revision: GoalRevision {
                    document: document.doc_id.clone(),
                    composite: "composite".into(),
                },
                snapshot: snapshot(&document, Utc::now()),
            }
        }),
        signature: Vec::new(),
    }
}

fn modeled_state(value: &SessionGoalObservationState) -> Value {
    match value {
        SessionGoalObservationState::NoReadableSession => json!({"kind": "no_readable_session"}),
        SessionGoalObservationState::Unknown(cause) => json!({"kind": "unknown", "cause": cause}),
        SessionGoalObservationState::Observed(observation) => json!({
            "kind": "observed",
            "goal": observation.goal.as_ref().map(|goal| json!({
                "goal": goal.snapshot.goal_id.trim_start_matches("goal:").parse::<u64>().unwrap(),
                "root": goal.assignment_root_request_doc_id.as_deref().map(|root|
                    root.trim_start_matches("request-document:").parse::<u64>().unwrap()),
            })),
        }),
    }
}

/// The GraphQL responses one modeled read transaction returns: the session
/// row list and, for a present row, the projection row list, with a projection
/// signed by the selected Agent (or a corrupted signature when unverified).
async fn modeled_responses(case: &Value, agent: &dyn NodeIdentity) -> (Value, Value) {
    let session_rows = match case["session"].as_str().unwrap() {
        "absent" => json!([]),
        "present" => json!([{
            "_docID": "session-document", "session_id": "chat", "node_did": "did:key:node",
            "requester_did": "did:key:user", "agent_did": agent.did(),
            "created_at": "2026-11-08T00:00:00Z",
        }]),
        other => panic!("unmodeled session read {other}"),
    };
    let projection_rows = match case["projection"]["kind"].as_str().unwrap() {
        "missing" => json!([]),
        kind @ ("unverified" | "verified") => {
            let mut observation = modeled_observation(&case["projection"]["goal"], agent.did());
            observation.signature = agent
                .sign(&observation.signing_payload().unwrap())
                .await
                .unwrap();
            if kind == "unverified" {
                observation.signature[0] ^= 1;
            }
            json!([{
                "_docID": "observation-document", "node_did": observation.node_did,
                "session_id": observation.session_id,
                "session_doc_id": observation.session_doc_id,
                "requester_did": observation.requester_did,
                "observation_json": serde_json::to_string(&observation).unwrap(),
            }])
        }
        other => panic!("unmodeled projection read {other}"),
    };
    (
        json!({"data": {"AgentSession": session_rows}}),
        json!({"data": {"GoalObservation": projection_rows}}),
    )
}

/// Replays Lean `SessionObservation.observe`, `next`, `bind` and `refusal`
/// from the read transaction's responses: through the session and projection
/// decoders, signature and scope classification, the classifier, and the
/// binder every send uses. Only the transport call itself is not replayed.
#[tokio::test]
async fn generated_session_goal_reads_classify_and_bind() {
    #[derive(Deserialize)]
    struct Contracts {
        goal_session_observation_cases: Vec<Value>,
    }
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_session_observation_cases.len(), 20);
    let home = tempfile::tempdir().unwrap();
    let agent =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("agent.key"), None)
            .unwrap();
    for case in &contracts.goal_session_observation_cases {
        let (session_response, projection_response) = modeled_responses(case, &agent).await;
        let read = || async {
            let session = gents_store::config_client::agent_session::decode_agent_session_read(
                &session_response,
                "chat",
            )
            .unwrap();
            let projection = match &session {
                AgentSessionRowRead::Present(row) => classify_projection(
                    &agent,
                    row,
                    &decode_observation_rows(&projection_response, row).unwrap(),
                )
                .await
                .unwrap(),
                AgentSessionRowRead::Absent => SessionGoalProjectionRead::Missing,
            };
            observe_session_goal(&session, projection)
        };
        let state = read().await;
        assert_eq!(modeled_state(&state), case["expected_state"], "{case}");
        assert_eq!(json!(state.next()), case["expected_next"], "{case}");
        let input = gents_protocol::request_input::RequestInput {
            goal_assignment: modeled_reference(&case["captured"]),
            ..Default::default()
        };
        let bound = bind_goal_input(input.clone(), state);
        assert_eq!(
            json!(bound.as_ref().err().map(|refused| refused.cause())),
            case["expected_refusal"],
            "{case}"
        );
        match case["expected_bind"]["kind"].as_str().unwrap() {
            "pending" => assert!(bound.is_err(), "{case}"),
            "prepared" => {
                let mut expected = input;
                expected.goal_assignment = modeled_reference(&case["expected_bind"]["reference"]);
                assert_eq!(bound.unwrap(), expected, "{case}");
            }
            other => panic!("unmodeled bind outcome {other}"),
        }
    }
}

/// Omission is not a modeled state (Lean `SessionObservation.SessionRead`):
/// both reads of one transaction fail a response without its row list alike.
#[test]
fn both_reads_fail_a_response_that_omits_its_rows() {
    let omitted = json!({"data": {}});
    assert!(
        gents_store::config_client::agent_session::decode_agent_session_read(&omitted, "chat")
            .is_err()
    );
    let row = gents_protocol::session::decode_session_row(&json!({
        "_docID": "session-document", "session_id": "chat", "node_did": "did:key:node",
        "requester_did": "did:key:user", "agent_did": "did:key:agent",
        "created_at": "2026-11-08T00:00:00Z",
    }))
    .unwrap();
    assert!(decode_observation_rows(&omitted, &row).is_err());
}

/// The client read is one transaction: a missing projection on a readable
/// session is unknown, a published one is observed, and another scope reads no
/// session.
#[tokio::test]
async fn client_goal_read_is_unknown_until_the_agent_publishes() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = f.agent_access();
    let read = |session: &'static str| {
        let access = access.clone();
        let identity = identity.clone();
        async move {
            load_client_session_goal_observation_state(
                &access,
                identity.as_ref(),
                identity.did(),
                session,
            )
            .await
            .unwrap()
        }
    };
    assert!(matches!(
        read(SESSION).await,
        SessionGoalObservationState::Unknown(SessionGoalUnknownCause::ProjectionMissing)
    ));
    assert!(matches!(
        read("another-session").await,
        SessionGoalObservationState::NoReadableSession
    ));
    project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
        .await
        .unwrap();
    let SessionGoalObservationState::Observed(observed) = read(SESSION).await else {
        panic!("published projection must be observed");
    };
    assert_eq!(observed.goal.unwrap().snapshot.goal_id, f.goal.goal_id);
    for (requester, collapsed) in [
        (Some(f.user.did()), true),
        (Some("did:key:other"), false),
        (None, false),
    ] {
        assert_eq!(
            load_session_goal_observation(
                &access,
                identity.as_ref(),
                identity.did(),
                SESSION,
                requester
            )
            .await
            .unwrap()
            .is_some(),
            collapsed,
            "the collapse yields only the envelope verified for {requester:?}"
        );
    }
    f.node.shutdown().await.expect("node shutdown");
}
