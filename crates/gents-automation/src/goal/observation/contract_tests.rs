use super::*;
use crate::goal::operator_resume::support::{Fixture, SESSION};
use gents_lean_vocab::families::goal_session_observation_contracts_classification as wire;
use serde_json::{json, Value};

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
    for modeled in gents_lean_vocab::families::goal_observation_contracts_acceptance::cases() {
        let case = &modeled.input;
        let mut observation = SessionGoalObservation {
            version: SESSION_GOAL_CONTROL_VERSION,
            node_did: map(&case.scope.node),
            agent_did: map(&case.scope.agent),
            session_id: map(&case.scope.session),
            session_doc_id: map(&case.scope.session_document),
            requester_did: case.scope.requester.as_deref().map(map),
            observed_at: Utc::now().to_rfc3339(),
            goal: case.goal.as_ref().map(|reference| {
                let mut snapshot = snapshot(&native, Utc::now());
                snapshot.node_did = map(&reference.node);
                snapshot.session_id = map(&reference.session);
                ObservedGoal {
                    assignment_root_request_doc_id: native.assignment_root_request_doc_id.clone(),
                    revision: GoalRevision {
                        document: reference.document.clone(),
                        composite: reference.composite.clone(),
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
        let accepted = verified(f.identity.as_ref(), &current, &row)
            .await
            .unwrap()
            .is_some();
        if accepted {
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
        modeled.check(accepted);
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
async fn retained_goal_observation_roots_match_the_lean_projection_model() {
    for case in gents_lean_vocab::families::goal_observation_contracts_retained_root::cases() {
        let f = fixture().await;
        let identity: Arc<dyn NodeIdentity> = f.identity.clone();
        let access = f.agent_access();
        access.write("test.goal_physical_root", &format!(
            "mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
            escape_graphql_string(&f.goal.doc_id), escape_graphql_string(&f.parent.doc_id))).await.unwrap();
        let load = || {
            load_session_goal_observation(
                &access,
                f.identity.as_ref(),
                f.identity.did(),
                SESSION,
                Some(f.user.did()),
            )
        };
        let root_view = |observation: Option<SessionGoalObservation>| {
            observation.map(|observation| {
                match observation.goal.unwrap().assignment_root_request_doc_id {
                    None => "absent".to_string(),
                    Some(root) if root == f.parent.doc_id => "goal_root".to_string(),
                    Some(_) => "other".to_string(),
                }
            })
        };
        project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
            .await
            .unwrap();
        let mut retained = load().await.unwrap().unwrap();
        let revision = retained.goal.as_ref().unwrap().revision.clone();
        if !case.input.retained_keeps_root {
            retained
                .goal
                .as_mut()
                .unwrap()
                .assignment_root_request_doc_id = None;
            assert!(serde_json::to_value(retained.goal.as_ref().unwrap())
                .unwrap()
                .get("assignment_root_request_doc_id")
                .is_none());
        }
        retained.signature = f
            .agent
            .sign(&retained.signing_payload().unwrap())
            .await
            .unwrap();
        let rows = durable_rows(&f).await;
        let doc = rows[0]["_docID"].as_str().unwrap().to_owned();
        access.write("test.retained_goal_observation", &format!(
            "mutation {{update_GoalObservation(docID: \"{}\", input: {{observation_json: \"{}\"}}) {{_docID}}}}",
            escape_graphql_string(&doc),escape_graphql_string(&serde_json::to_string(&retained).unwrap()))).await.unwrap();
        let retained_load = root_view(load().await.unwrap());
        let before_refresh = durable_rows(&f).await;
        project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
            .await
            .unwrap();
        let after_refresh = durable_rows(&f).await;
        let refreshed = load().await.unwrap();
        let revision_kept = refreshed
            .as_ref()
            .is_some_and(|observation| observation.goal.as_ref().unwrap().revision == revision);
        let refreshed_root = root_view(refreshed.clone());
        project_session_goal(&f.node, &identity, &f.agent, SESSION, Some(f.user.did()))
            .await
            .unwrap();
        let rewrote_on_repeat = durable_rows(&f).await != after_refresh;
        if case.input.substitute_root {
            let mut substituted = refreshed.unwrap();
            substituted
                .goal
                .as_mut()
                .unwrap()
                .assignment_root_request_doc_id = Some("another-physical-request".into());
            access.write("test.substituted_goal_root", &format!(
                "mutation {{update_GoalObservation(docID: \"{}\", input: {{observation_json: \"{}\"}}) {{_docID}}}}",
                escape_graphql_string(&doc),escape_graphql_string(&serde_json::to_string(&substituted).unwrap()))).await.unwrap();
        }
        case.check(gents_lean_vocab::families::goal_observation_contracts_retained_root::RetainedRootOutput {
            retained_load,
            refresh_writes: after_refresh != before_refresh,
            refreshed_root,
            revision_kept,
            rewrote_on_repeat,
            final_loaded: load().await.unwrap().is_some(),
        });
        f.node.shutdown().await.expect("node shutdown");
    }
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

fn modeled_number(value: &str, prefix: &str) -> u64 {
    value.strip_prefix(prefix).unwrap().parse().unwrap()
}

fn modeled_goal(
    reference: &gents_protocol::request_input::GoalAssignmentInput,
) -> wire::Reference {
    wire::Reference {
        goal: modeled_number(&reference.goal_id, "goal:"),
        root: reference
            .root_request_doc_id
            .as_deref()
            .map(|root| modeled_number(root, "request-document:")),
    }
}

fn modeled_state(value: &SessionGoalObservationState) -> wire::StateWire<String> {
    let cause = |cause: &SessionGoalUnknownCause| {
        serde_json::from_value(serde_json::to_value(cause).unwrap()).unwrap()
    };
    match value {
        SessionGoalObservationState::NoReadableSession => wire::StateWire {
            kind: "no_readable_session".into(),
            cause: None,
            goal: None,
        },
        SessionGoalObservationState::Unknown(unknown) => wire::StateWire {
            kind: "unknown".into(),
            cause: Some(cause(unknown)),
            goal: None,
        },
        SessionGoalObservationState::Observed(observation) => wire::StateWire {
            kind: "observed".into(),
            cause: None,
            goal: observation.goal.as_ref().map(|goal| wire::Reference {
                goal: modeled_number(&goal.snapshot.goal_id, "goal:"),
                root: goal
                    .assignment_root_request_doc_id
                    .as_deref()
                    .map(|root| modeled_number(root, "request-document:")),
            }),
        },
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
    let home = tempfile::tempdir().unwrap();
    let agent =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("agent.key"), None)
            .unwrap();
    for modeled in gents_lean_vocab::families::goal_session_observation_contracts_classification::cases::<String, String, String>() {
        let case = serde_json::to_value(&modeled.input).unwrap();
        let (session_response, projection_response) = modeled_responses(&case, &agent).await;
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
        let observed_state = modeled_state(&state);
        let next = serde_json::from_value(json!(state.next())).unwrap();
        let input = gents_protocol::request_input::RequestInput {
            goal_assignment: modeled_reference(&case["captured"]),
            ..Default::default()
        };
        let bound = bind_goal_input(input.clone(), state);
        let refusal = bound
            .as_ref()
            .err()
            .map(|refused| serde_json::from_value(json!(refused.cause())).unwrap());
        let bind = match bound {
            Err(_) => wire::PreparedWire {
                kind: "pending".into(),
                reference: None,
            },
            Ok(prepared) => {
                let mut unchanged = prepared.clone();
                unchanged.goal_assignment = input.goal_assignment.clone();
                assert_eq!(unchanged, input, "{case}: binding changes only the assignment");
                wire::PreparedWire {
                    kind: "prepared".into(),
                    reference: prepared.goal_assignment.as_ref().map(modeled_goal),
                }
            }
        };
        modeled.check(wire::ClassificationOutput {
            state: observed_state,
            next,
            bind,
            refusal,
        });
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
