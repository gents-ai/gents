use super::*;
use axum::{body::Bytes, extract::State, Json, Router};
use gents::NodeIdentity;
use gents_protocol::goal_control::{
    GoalRevision, ObservedGoal, SessionGoalObservation, SESSION_GOAL_CONTROL_VERSION,
};
use gents_protocol::request_input::GoalAssignmentInput;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct Source {
    did: String,
    session_readable: bool,
    observation: Arc<Mutex<Value>>,
    pub(crate) queries: Arc<Mutex<Vec<String>>>,
    pub(crate) requests: Arc<Mutex<Value>>,
}

async fn endpoint(State(source): State<Source>, body: Bytes) -> Json<Value> {
    let body: Value = serde_json::from_slice(&body).unwrap_or_default();
    let Some(query) = body["query"].as_str() else {
        return Json(json!({"id":17}));
    };
    source.queries.lock().unwrap().push(query.to_owned());
    let data = if query.contains("AgentSession") {
        json!({"AgentSession":if source.session_readable {json!([{
            "_docID":"session-document", "session_id":"chat", "node_did":source.did,
            "requester_did":source.did, "agent_id":"engineer", "created_at":"2026-10-21T00:00:00Z"
        }])} else {json!([])}})
    } else if query.contains("GoalObservation") {
        json!({"GoalObservation":source.observation.lock().unwrap().clone()})
    } else if query.contains("AgentRequest(") {
        json!({"AgentRequest":source.requests.lock().unwrap().clone()})
    } else if query.contains("Node(") {
        json!({"Node":[{"node_did":source.did, "default_agent_id":"engineer", "enabled":true}],
            "Agent":[{"agent_id":"engineer", "node_did":source.did, "enabled":true}]})
    } else {
        panic!("unexpected source query: {query}")
    };
    Json(json!({"data":data}))
}

pub(crate) fn reference(value: &Value) -> Option<GoalAssignmentInput> {
    (!value.is_null()).then(|| GoalAssignmentInput {
        goal_id: format!("goal:{}", value["goal"].as_u64().unwrap()),
        root_request_doc_id: value["root"]
            .as_u64()
            .map(|root| format!("request-document:{root}")),
    })
}

pub(crate) async fn fixture(
    readable: bool,
    verified: bool,
    observed: Option<GoalAssignmentInput>,
    fault: Option<&str>,
) -> Result<(
    gents::KeyIdentity,
    GraphqlEndpoint,
    Source,
    tokio::task::JoinHandle<()>,
)> {
    let dir = tempfile::tempdir()?;
    let identity = gents::KeyIdentity::load_or_create(dir.path().join("source.key"), None)?;
    let mut observation = SessionGoalObservation {
        version: SESSION_GOAL_CONTROL_VERSION,
        node_did: identity.did().into(),
        session_id: "chat".into(),
        session_doc_id: "session-document".into(),
        requester_did: Some(identity.did().into()),
        observed_at: "2026-10-21T00:00:00Z".into(),
        goal: observed.map(|reference| {
            let goal: gents::goal::GoalDocument = serde_json::from_value(json!({
                "_docID":"goal-document", "node_did":identity.did(), "session_id":"chat",
                "goal_id":reference.goal_id, "objective":"Keep this assignment", "status":"active"
            }))
            .unwrap();
            ObservedGoal {
                revision: GoalRevision {
                    document: goal.doc_id.clone(),
                    composite: "source-composite".into(),
                },
                assignment_root_request_doc_id: reference.root_request_doc_id,
                snapshot: gents::goal::snapshot(&goal, Utc::now()),
            }
        }),
        signature: vec![],
    };
    if fault == Some("foreign-session") {
        observation.session_doc_id = "foreign-session-document".into();
    }
    observation.signature = identity.sign(&observation.signing_payload()?).await?;
    if fault == Some("tampered-root") {
        observation
            .goal
            .as_mut()
            .unwrap()
            .assignment_root_request_doc_id = Some("forged-root".into());
    }
    let rows = if verified {
        json!([{"_docID":"observation-document", "node_did":identity.did(), "session_id":"chat",
            "session_doc_id":"session-document", "requester_did":identity.did(),
            "observation_json":serde_json::to_string(&observation)?}])
    } else {
        json!([])
    };
    let source = Source {
        did: identity.did().into(),
        session_readable: readable,
        observation: Arc::new(Mutex::new(rows)),
        queries: Arc::new(Mutex::new(vec![])),
        requests: Arc::new(Mutex::new(json!([]))),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = Router::new().fallback(endpoint).with_state(source.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Ok((
        identity,
        GraphqlEndpoint::anonymous(format!("http://{address}/api/v0/graphql")),
        source,
        server,
    ))
}

pub(crate) fn authored_input() -> RequestInput {
    serde_json::from_value(json!({
        "cwd":"/workspace", "selected_skill_ids":["skill:one"],
        "captured_context":[{"kind":"selection", "source":"editor", "capturedAt":"2026-10-21T00:00:00Z",
            "uri":"file:///workspace/main.rs", "terminalName":null, "workspaceUri":"file:///workspace",
            "version":4, "unsaved":true, "range":{"start":{"line":1,"character":0},"end":{"line":2,"character":0}},
            "content":"private unsaved capture"}]
    })).unwrap()
}

#[tokio::test]
async fn generated_cli_preparation_captures_goal_before_signing() -> Result<()> {
    let contract: Value = gents_lean_contract::load_contract_snapshot()?;
    let cases = contract["goal_input_binding_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let (identity, graphql, source, server) = fixture(
            case["session_readable"].as_bool().unwrap(),
            case["observation_verified"].as_bool().unwrap(),
            reference(&case["observed"]),
            None,
        )
        .await?;
        let mut input = authored_input();
        input.goal_assignment = reference(&case["captured"]);
        let original = input.clone();
        let actual = prepare_agent_request(
            &graphql,
            identity.did(),
            "hello",
            Some("chat"),
            Some("engineer"),
            Some("fixed-request".into()),
            RequestSubmitOptions {
                input: Some(input),
                ..Default::default()
            },
        )
        .await;
        server.abort();
        if case["expected"]["kind"] == "pending" {
            assert!(
                actual.unwrap_err().to_string().contains("still syncing"),
                "{case}"
            );
        } else {
            let create = actual?.create;
            let mut expected = original.clone();
            expected.goal_assignment = reference(&case["expected"]["reference"]);
            assert_eq!(create.input, expected, "{case}");
            assert!(
                identity
                    .verify(
                        identity.did(),
                        &create.signing_payload(),
                        &create.admission.signature
                    )
                    .await?
            );
            if original.goal_assignment.is_some() {
                assert!(
                    !source
                        .queries
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|q| q.contains("AgentSession") || q.contains("GoalObservation")),
                    "{case}"
                );
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn cli_preparation_rejects_tampered_and_foreign_goal_observations() -> Result<()> {
    let contract: Value = gents_lean_contract::load_contract_snapshot()?;
    let modeled = contract["goal_input_binding_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "unverified_goal_is_unknown")
        .unwrap();
    assert_eq!(modeled["expected"]["kind"], "pending");
    for fault in ["tampered-root", "foreign-session"] {
        let (identity, graphql, _, server) =
            fixture(true, true, reference(&modeled["observed"]), Some(fault)).await?;
        let actual = prepare_agent_request(
            &graphql,
            identity.did(),
            "hello",
            Some("chat"),
            Some("engineer"),
            None,
            RequestSubmitOptions {
                input: Some(authored_input()),
                ..Default::default()
            },
        )
        .await;
        server.abort();
        assert!(
            actual.unwrap_err().to_string().contains("still syncing"),
            "{fault}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn cli_fresh_session_does_not_capture_an_unrelated_goal() -> Result<()> {
    let (identity, graphql, source, server) = fixture(
        true,
        true,
        Some(GoalAssignmentInput {
            goal_id: "unrelated-goal".into(),
            root_request_doc_id: Some("unrelated-root".into()),
        }),
        None,
    )
    .await?;
    let input = authored_input();
    let create = prepare_agent_request(
        &graphql,
        identity.did(),
        "hello",
        None,
        Some("engineer"),
        None,
        RequestSubmitOptions {
            input: Some(input.clone()),
            ..Default::default()
        },
    )
    .await?
    .create;
    server.abort();
    assert_ne!(create.session_id, "chat");
    assert_eq!(create.input, input);
    assert!(!source
        .queries
        .lock()
        .unwrap()
        .iter()
        .any(|q| q.contains("AgentSession") || q.contains("GoalObservation")));
    assert!(
        identity
            .verify(
                identity.did(),
                &create.signing_payload(),
                &create.admission.signature
            )
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn cli_retry_keeps_captured_absence_after_a_new_goal() -> Result<()> {
    let (identity, graphql, source, server) = fixture(
        true,
        true,
        Some(GoalAssignmentInput {
            goal_id: "new-goal".into(),
            root_request_doc_id: Some("new-root".into()),
        }),
        None,
    )
    .await?;
    let input = authored_input();
    let create = prepare_agent_request(
        &graphql,
        identity.did(),
        "retry",
        Some("chat"),
        Some("engineer"),
        None,
        RequestSubmitOptions {
            input: Some(input.clone()),
            retry_parent_request: Some("old-request".into()),
            retry_parent_request_doc_id: Some("old-physical-request".into()),
            ..Default::default()
        },
    )
    .await?
    .create;
    server.abort();
    assert_eq!(create.input, input);
    assert!(!source
        .queries
        .lock()
        .unwrap()
        .iter()
        .any(|q| q.contains("AgentSession") || q.contains("GoalObservation")));
    assert!(
        identity
            .verify(
                identity.did(),
                &create.signing_payload(),
                &create.admission.signature
            )
            .await?
    );
    Ok(())
}
