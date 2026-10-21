use super::*;
use crate::goal::operator_resume::support::{Fixture, SESSION};
use serde_json::{json, Value};

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
}

#[derive(Deserialize)]
struct Scope {
    node: String,
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
    let f = Fixture::new(&before).await;
    crate::session::ensure_session_with_agent_id_and_requester_did(
        &f.node,
        SESSION,
        f.identity.did(),
        "contract-behavior",
        Some(f.identity.did()),
    )
    .await
    .unwrap();
    f
}

async fn session(f: &Fixture) -> SessionOwnerRow {
    ConfigAccess::local(f.node.clone())
        .transact_readonly("test.observed_session", |txn| {
            Box::pin(async {
                crate::session::load_agent_session_owner_in_txn(txn, f.identity.did(), SESSION)
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
    assert_eq!(contracts.goal_observation_cases.len(), 288);
    let f = fixture().await;
    let current = session(&f).await;
    let native = load_canonical_goal(&f.node, f.identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let map = |value: &str| match value {
        "node" | "requester" => f.identity.did().to_owned(),
        "session" => SESSION.to_owned(),
        "session-document" => current.doc_id.clone(),
        other => other.to_owned(),
    };
    for case in contracts.goal_observation_cases {
        let mut observation = SessionGoalObservation {
            version: SESSION_GOAL_CONTROL_VERSION,
            node_did: map(&case.scope.node),
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
            .identity
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
        assert_eq!(
            verified(f.identity.as_ref(), &current, &row)
                .await
                .unwrap()
                .is_some(),
            case.accepted,
            "observation: {observation:?}"
        );
    }
    f.node.shutdown().await;
}

async fn durable_rows(f: &Fixture) -> Value {
    ConfigAccess::local(f.node.clone())
        .execute("{ GoalObservation { _docID _version { cid } observation_json } }")
        .await
        .unwrap()["data"]["GoalObservation"]
        .clone()
}

#[tokio::test]
async fn invalid_session_does_not_starve_other_goal_observations() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = ConfigAccess::local(f.node.clone());
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
    assert!(reconcile_goal_observations(&f.node, &identity, true, &[])
        .await
        .is_err());
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .is_some());
    let settled = durable_rows(&f).await;
    assert!(reconcile_goal_observations(&f.node, &identity, true, &[])
        .await
        .is_err());
    assert_eq!(durable_rows(&f).await, settled);
    f.node.shutdown().await;
}

#[tokio::test]
async fn goal_projection_is_signed_scoped_and_settles_without_writes() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = ConfigAccess::local(f.node.clone());
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did())
    )
    .await
    .unwrap()
    .is_none());
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    let first = load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        first.goal.as_ref().unwrap().snapshot.goal_id,
        f.goal.goal_id
    );
    let settled = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
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
        Some(identity.did())
    )
    .await
    .unwrap()
    .is_none());
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .is_some());
    let goal = load_canonical_goal(&f.node, identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    delete_goal(&f.node, &goal).await.unwrap();
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    let empty = load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(empty.goal.is_none());
    let settled = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    assert_eq!(durable_rows(&f).await, settled);
    f.node.shutdown().await;
}

#[tokio::test]
async fn retained_signed_observation_refreshes_its_root_without_changing_goal_revision() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = ConfigAccess::local(f.node.clone());
    access.write("test.goal_physical_root", &format!(
        "mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(&f.goal.doc_id), escape_graphql_string(&f.parent.doc_id))).await.unwrap();
    project_session_goal(&f.node, &identity, SESSION, Some(f.identity.did()))
        .await
        .unwrap();
    let mut retained = load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.identity.did()),
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
        .identity
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
        Some(f.identity.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .unwrap()
    .assignment_root_request_doc_id
    .is_none());
    project_session_goal(&f.node, &identity, SESSION, Some(f.identity.did()))
        .await
        .unwrap();
    let refreshed = load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.identity.did()),
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
    project_session_goal(&f.node, &identity, SESSION, Some(f.identity.did()))
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
        Some(f.identity.did())
    )
    .await
    .unwrap()
    .is_none());
    f.node.shutdown().await;
}
