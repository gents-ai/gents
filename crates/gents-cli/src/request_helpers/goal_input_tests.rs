use super::*;
use axum::body::Bytes;
use gents::NodeIdentity;
use gents_protocol::goal_control::{
    GoalRevision, ObservedGoal, SessionGoalObservation, SESSION_GOAL_CONTROL_VERSION,
};
use gents_protocol::request_input::GoalAssignmentInput;
use serde_json::json;
use std::sync::{Arc, Mutex};

/// A served home whose enrolled User reads and submits through a recording
/// proxy: the Node and its managed Agents are the User's, the session is the
/// Agent's own, and a verified Goal observation is the Agent's signed row.
pub(crate) struct Source {
    pub(crate) home: crate::shared::test_support::ClientHome,
    /// The User's signed endpoint, through the recording proxy.
    pub(crate) graphql: GraphqlEndpoint,
    pub(crate) node_did: String,
    pub(crate) agent_did: String,
    pub(crate) default_agent_did: String,
    queries: Arc<Mutex<Vec<String>>>,
    _proxy: crate::shared::test_support::ForwardingProxy,
}

impl Source {
    pub(crate) fn user(&self) -> &gents::KeyIdentity {
        &self.home.user
    }

    /// Whether any exchange through the proxy named `needle`.
    pub(crate) fn queried(&self, needle: &str) -> bool {
        self.queries
            .lock()
            .unwrap()
            .iter()
            .any(|body| body.contains(needle))
    }

    pub(crate) fn exchanges(&self) -> usize {
        self.queries.lock().unwrap().len()
    }

    /// Admits a Task fire's request as the User, keyed `manual-fire`, as the
    /// Task invocation submits it.
    pub(crate) async fn admit(&self, input: RequestInput) -> Result<()> {
        let access = self.home.access.clone();
        let user = self.user();
        let requester =
            gents::session::client_requester_scope(&access, &self.node_did, "chat", user.did())
                .await?;
        let create = gents::request::lifecycle::build_signed_client_request(
            &access,
            gents::RequestSpec {
                input,
                retry_key: Some("manual-fire".into()),
                ..gents::RequestSpec::new(
                    gents_protocol::request_admission::RequestPurpose::Normal,
                    gents::RequestIdentity {
                        request_id: "manual-task".into(),
                        node_did: self.node_did.clone(),
                        requester_did: Some(requester),
                        agent_did: self.agent_did.clone(),
                        session_id: "chat".into(),
                        content: "Continue the Task".into(),
                        execution_origin: gents::request::lifecycle::ExecutionOrigin::Interactive,
                        created_at: "2026-10-21T00:00:00Z".into(),
                    },
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                        user.did(),
                    ),
                )
            },
            gents::RequestSigner::Identity(user),
        )
        .await?;
        gents::request::lifecycle::submit_client_request(&access, &create).await?;
        Ok(())
    }
}

pub(crate) fn reference(value: &Value) -> Option<GoalAssignmentInput> {
    (!value.is_null()).then(|| GoalAssignmentInput {
        goal_id: format!("goal:{}", value["goal"].as_u64().unwrap()),
        root_request_doc_id: value["root"]
            .as_u64()
            .map(|root| format!("request-document:{root}")),
    })
}

async fn publish_default_agent(
    home: &crate::shared::test_support::ClientHome,
    agent_did: &str,
) -> Result<()> {
    use gents::documents::apply_desired_state_plan;
    use gents::store::config_client::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let node_did = home.node_did().to_owned();
    home.access
        .transact_write("test.goal_input.default_agent", |txn| {
            let node_did = &node_did;
            Box::pin(async move {
                let (_, mut node) =
                    read_desired_state_record_in_txn(txn, gents::Collection::Node, node_did, node_did)
                        .await?
                        .expect("published Node");
                node["default_agent_did"] = agent_did.into();
                node["enabled"] = true.into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::Node,
                    add: node.clone(),
                    update: node,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
}

/// Publishes the Agent's signed observation of `session_doc`, as its runtime
/// projects one, with `fault` applied before or after signing.
async fn publish_observation(
    home: &crate::shared::test_support::ClientHome,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_doc: &str,
    observed: Option<GoalAssignmentInput>,
    fault: Option<&str>,
) -> Result<()> {
    let node_did = home.node_did().to_owned();
    let agent = gents::store::identity::resolve_agent_identity(home.home.identity.as_ref(), binding)?;
    let mut observation = SessionGoalObservation {
        version: SESSION_GOAL_CONTROL_VERSION,
        node_did: node_did.clone(),
        agent_did: binding.agent_did.clone(),
        session_id: "chat".into(),
        session_doc_id: session_doc.into(),
        requester_did: Some(binding.agent_did.clone()),
        observed_at: "2026-10-21T00:00:00Z".into(),
        goal: observed.map(|reference| {
            let goal: gents::goal::GoalDocument = serde_json::from_value(json!({
                "_docID":"goal-document", "node_did":node_did, "session_id":"chat",
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
    observation.signature = agent.sign(&observation.signing_payload()?).await?;
    if fault == Some("tampered-root") {
        observation
            .goal
            .as_mut()
            .unwrap()
            .assignment_root_request_doc_id = Some("forged-root".into());
    }
    let input = json!({
        "node_did": node_did, "session_id": "chat", "session_doc_id": session_doc,
        "requester_did": binding.agent_did, "agent_did": binding.agent_did,
        "observation_json": serde_json::to_string(&observation)?,
    });
    home.agent_access(binding)
        .await
        .transact_write("test.goal_input.observation", |txn| {
            let input = &input;
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        "mutation($input: GoalObservationMutationInputArg!) { create_GoalObservation(input: $input) { _docID } }",
                        &json!({"input": input}),
                    )
                    .await?;
                let doc = gents::store::graphql::created_doc_id(&response, "GoalObservation")?;
                txn.add_resource_relationship("GoalObservation", &doc, "parent", "sessions", session_doc)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
}

pub(crate) async fn fixture(
    readable: bool,
    verified: bool,
    observed: Option<GoalAssignmentInput>,
    fault: Option<&str>,
) -> Result<Source> {
    use crate::shared::test_support::{ForwardingProxy, ProxyDecision};
    let home = crate::shared::test_support::ClientHome::new().await;
    let binding = home.agent("Goal agent").await;
    let default_agent_did = if fault == Some("different-default-agent") {
        home.agent("Default agent").await.agent_did
    } else {
        binding.agent_did.clone()
    };
    publish_default_agent(&home, &default_agent_did).await?;
    if readable {
        let session_doc = home.agent_session(&binding, "chat").await;
        if verified {
            publish_observation(&home, &binding, &session_doc, observed, fault).await?;
        }
    }
    let queries = Arc::new(Mutex::new(Vec::new()));
    let hook = {
        let queries = queries.clone();
        Arc::new(move |_path: String, body: Bytes| {
            queries
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            Box::pin(async { ProxyDecision::Forward })
                as futures_util::future::BoxFuture<'static, ProxyDecision>
        })
    };
    let proxy = ForwardingProxy::spawn(home.graphql(), hook).await;
    let graphql = crate::shared::test_support::user_endpoint(&proxy.graphql, &home.user);
    Ok(Source {
        node_did: home.node_did().to_owned(),
        agent_did: binding.agent_did,
        default_agent_did,
        graphql,
        queries,
        _proxy: proxy,
        home,
    })
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

async fn signed_by_user(source: &Source, create: &AgentRequestCreate) -> Result<bool> {
    let user = source.user();
    Ok(user
        .verify(user.did(), &create.signing_payload(), &create.admission.signature)
        .await?)
}

#[tokio::test]
async fn generated_cli_preparation_captures_goal_before_signing() -> Result<()> {
    let contract: Value = gents_lean_contract::load_contract_snapshot()?;
    let cases = contract["goal_input_binding_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let source = fixture(
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
            &source.graphql,
            &source.node_did,
            "hello",
            Some("chat"),
            Some(&source.agent_did),
            Some("fixed-request".into()),
            RequestSubmitOptions {
                input: Some(input),
                ..Default::default()
            },
        )
        .await;
        if case["expected"]["kind"] == "pending" {
            let error = actual.unwrap_err();
            assert!(
                gents_protocol::error_chain::find_source::<gents::goal::GoalInputRefused>(
                    error.as_ref()
                )
                .is_some(),
                "{case}: {error:#}"
            );
        } else {
            let create = actual?.create;
            let mut expected = original.clone();
            expected.goal_assignment = reference(&case["expected"]["reference"]);
            assert_eq!(create.input, expected, "{case}");
            assert!(signed_by_user(&source, &create).await?, "{case}");
            if original.goal_assignment.is_some() {
                assert!(!source.queried("GoalObservation"), "{case}");
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
        let source = fixture(true, true, reference(&modeled["observed"]), Some(fault)).await?;
        let actual = prepare_agent_request(
            &source.graphql,
            &source.node_did,
            "hello",
            Some("chat"),
            Some(&source.agent_did),
            None,
            RequestSubmitOptions {
                input: Some(authored_input()),
                ..Default::default()
            },
        )
        .await;
        let error = actual.unwrap_err();
        assert_eq!(
            gents_protocol::error_chain::find_source::<gents::goal::GoalInputRefused>(
                error.as_ref()
            )
            .copied(),
            Some(gents::goal::GoalInputRefused::ProjectionUnverified),
            "{fault}: {error:#}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn cli_fresh_session_does_not_capture_an_unrelated_goal() -> Result<()> {
    let source = fixture(
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
        &source.graphql,
        &source.node_did,
        "hello",
        None,
        Some(&source.agent_did),
        None,
        RequestSubmitOptions {
            input: Some(input.clone()),
            ..Default::default()
        },
    )
    .await?
    .create;
    assert_ne!(create.session_id, "chat");
    assert_eq!(create.input, input);
    assert!(!source.queried("GoalObservation"));
    assert!(signed_by_user(&source, &create).await?);
    Ok(())
}

#[tokio::test]
async fn cli_retry_keeps_captured_absence_after_a_new_goal() -> Result<()> {
    let source = fixture(
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
        &source.graphql,
        &source.node_did,
        "retry",
        Some("chat"),
        Some(&source.agent_did),
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
    assert_eq!(create.input, input);
    assert!(!source.queried("GoalObservation"));
    assert!(signed_by_user(&source, &create).await?);
    Ok(())
}

#[tokio::test]
async fn client_request_signs_as_user_and_retains_agent_owned_session_scope() -> Result<()> {
    let snapshot: Value = gents_lean_contract::load_contract_snapshot()?;
    let modeled = snapshot["agent_request_admission_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "client-in-agent-owned-session")
        .unwrap();
    assert_eq!(modeled["expected_admitted"], true);
    let source = fixture(true, true, None, None).await?;
    let user = source.user();
    let create = prepare_agent_request_with_input(
        &source.graphql,
        &source.node_did,
        "continue",
        Some("chat"),
        Some(&source.agent_did),
        None,
        RequestSubmitOptions::default(),
    )
    .await?
    .create;
    assert_eq!(create.requester_did, source.agent_did);
    assert_eq!(create.admission.signer_did, user.did());
    assert_eq!(
        create.admission.kind,
        gents_protocol::request_admission::AgentRequestAdmissionKind::Client
    );
    assert!(signed_by_user(&source, &create).await?);
    // The User's admission is not the Node's: the Node's key refuses it.
    assert!(!matches!(
        source
            .home
            .home
            .identity
            .verify(
                &source.node_did,
                &create.signing_payload(),
                &create.admission.signature
            )
            .await,
        Ok(true)
    ));
    Ok(())
}

#[tokio::test]
async fn request_selection_preserves_existing_session_agent_after_default_changes() -> Result<()> {
    let source = fixture(true, false, None, Some("different-default-agent")).await?;
    assert_ne!(source.default_agent_did, source.agent_did);
    let selected =
        resolve_request_agent_did(&source.graphql, &source.node_did, Some("chat"), None).await?;
    assert_eq!(selected, source.agent_did);
    assert!(resolve_request_agent_did(
        &source.graphql,
        &source.node_did,
        Some("chat"),
        Some(&source.default_agent_did)
    )
    .await
    .is_err());
    Ok(())
}
