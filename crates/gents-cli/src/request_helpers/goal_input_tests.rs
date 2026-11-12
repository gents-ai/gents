use anyhow::Result;
use axum::body::Bytes;
use chrono::Utc;
use gents_protocol::goal_control::{
    GoalRevision, ObservedGoal, SessionGoalObservation, SESSION_GOAL_CONTROL_VERSION,
};
use gents_protocol::request_input::{GoalAssignmentInput, RequestInput};
use gents_store::config_client::GraphqlEndpoint;
use gents_store::identity::NodeIdentity;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// A served home whose enrolled User reads and submits through a recording
/// proxy: the Node and its managed Agents are the User's, the session is the
/// Agent's own, and a verified Goal observation is the Agent's signed row.
pub(crate) struct Source {
    pub(crate) home: gents_host::test_support::ClientHome,
    /// The User's signed endpoint, through the recording proxy.
    pub(crate) graphql: GraphqlEndpoint,
    pub(crate) node_did: String,
    pub(crate) agent_did: String,
    queries: Arc<Mutex<Vec<String>>>,
    _proxy: gents_host::test_support::ForwardingProxy,
}

impl Source {
    pub(crate) fn user(&self) -> &gents_store::identity::KeyIdentity {
        &self.home.user
    }

    pub(crate) fn exchanges(&self) -> usize {
        self.queries.lock().unwrap().len()
    }

    /// Admits a Task fire's request as the User, keyed `manual-fire`, as the
    /// Task invocation submits it.
    pub(crate) async fn admit(&self, input: RequestInput) -> Result<()> {
        let access = self.home.access.clone();
        let user = self.user();
        let requester = gents_session::session::client_requester_scope(
            &access,
            &self.node_did,
            "chat",
            user.did(),
        )
        .await?;
        let create = gents_session::request::lifecycle::build_signed_client_request(
            &access,
            gents_session::request::lifecycle::RequestSpec {
                input,
                retry_key: Some("manual-fire".into()),
                ..gents_session::request::lifecycle::RequestSpec::new(
                    gents_protocol::request_admission::RequestPurpose::Normal,
                    gents_session::request::lifecycle::RequestIdentity {
                        request_id: "manual-task".into(),
                        node_did: self.node_did.clone(),
                        requester_did: Some(requester),
                        agent_did: self.agent_did.clone(),
                        session_id: "chat".into(),
                        content: "Continue the Task".into(),
                        execution_origin:
                            gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                        created_at: "2026-10-21T00:00:00Z".into(),
                    },
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                        user.did(),
                    ),
                )
            },
            gents_session::request::lifecycle::RequestSigner::Identity(user),
        )
        .await?;
        gents_session::request::lifecycle::submit_client_request(&access, &create).await?;
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
    home: &gents_host::test_support::ClientHome,
    agent_did: &str,
) -> Result<()> {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let node_did = home.node_did().to_owned();
    home.access
        .transact_write("test.goal_input.default_agent", |txn| {
            let node_did = &node_did;
            Box::pin(async move {
                let (_, mut node) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Node,
                    node_did,
                    node_did,
                )
                .await?
                .expect("published Node");
                node["enabled"] = true.into();
                let selection = serde_json::to_value(gents_documents::DefaultAgentSelection::new(
                    node_did, agent_did,
                ))?;
                let plan = DesiredStateApplyPlan::new(vec![
                    DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Node,
                        add: node.clone(),
                        update: node,
                    },
                    DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::DefaultAgentSelection,
                        add: selection.clone(),
                        update: selection,
                    },
                ])?;
                apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map(|_| ())
            })
        })
        .await
}

/// Publishes the Agent's signed observation of `session_doc`, as its runtime
/// projects one, with `fault` applied before or after signing.
async fn publish_observation(
    home: &gents_host::test_support::ClientHome,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_doc: &str,
    observed: Option<GoalAssignmentInput>,
    fault: Option<&str>,
) -> Result<()> {
    let node_did = home.node_did().to_owned();
    let agent =
        gents_store::identity::resolve_agent_identity(home.home.identity.as_ref(), binding)?;
    let mut observation = SessionGoalObservation {
        version: SESSION_GOAL_CONTROL_VERSION,
        node_did: node_did.clone(),
        agent_did: binding.agent_did.clone(),
        session_id: "chat".into(),
        session_doc_id: session_doc.into(),
        requester_did: Some(binding.agent_did.clone()),
        observed_at: "2026-10-21T00:00:00Z".into(),
        goal: observed.map(|reference| {
            let goal: gents_automation::goal::GoalDocument = serde_json::from_value(json!({
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
                snapshot: gents_automation::goal::snapshot(&goal, Utc::now()),
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
                let doc = gents_store::graphql::created_doc_id(&response, "GoalObservation")?;
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
    use gents_host::test_support::ForwardingProxy;
    use gents_host::test_support::ProxyDecision;
    let home = gents_host::test_support::ClientHome::new().await;
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
    let graphql = gents_host::test_support::user_endpoint(&proxy.graphql, &home.user);
    Ok(Source {
        node_did: home.node_did().to_owned(),
        agent_did: binding.agent_did,
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
