use std::sync::Arc;
use std::time::Duration;

use gents::inference::resolved::{MaxTurnsProvenance, ResolvedAgent, DEFAULT_MAX_TURNS};
use gents::request::lifecycle::{
    build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
    RequestSigner, RequestSpec,
};
use gents::store::config_client::{
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan, NodePermission,
};
use gents::store::graphql::escape_graphql_string;
use gents::{Collection, DocumentRuntimeOptions, Gents, NodeIdentity, ToolCeiling};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use serde::Deserialize;
use serde_json::json;

use crate::support::accepted_turn::AcceptedTurnRuntime;
use crate::support::fixtures::{agent_target, test_identity};
use crate::support::interrupt::BootedAgent;
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse,
};

struct TestDb {
    _directory: tempfile::TempDir,
    node: Arc<gents::defra_node::Node>,
    node_identity: Arc<dyn NodeIdentity>,
}

async fn test_db(name: &str) -> TestDb {
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    }
    let directory = tempfile::Builder::new().prefix(name).tempdir().unwrap();
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap(),
    );
    let node = Arc::new(
        gents::defra_node::Node::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    TestDb {
        _directory: directory,
        node,
        node_identity: identity,
    }
}

const CHILD_NAME: &str = "e2e-session-limit-child";
const BACKEND_ID: &str = "e2e-session-limit-backend";
const MODEL: &str = "e2e-session-limit-model";
const PARENT_TOOL_CALL_ID: &str = "e2e-session-limit-tool-call";
const PARENT_PROMPT: &str = "e2e-session-limit-parent-prompt";
const CHILD_PROMPT: &str = "e2e-session-limit-child-prompt";
const EXPLICIT_CHILD_MAX_TURNS: i64 = 3;
/// rig's max-turns display, matched as a fixed byte string by
/// `scripts/harbor/run_gents.sh`.
const PINNED_PREFIX: &str = "agent stream failed: PromptError: MaxTurnError: ";

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    session_id: String,
    session_doc_id: Option<String>,
    agent_doc_id: Option<String>,
    relationships_pending: Option<bool>,
    agent_did: Option<String>,
    requester_did: Option<String>,
    admission_signer_did: Option<String>,
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
}

async fn configure_session_chain(
    db: &TestDb,
    owner: &dyn NodeIdentity,
    endpoint: &str,
    max_turns: Option<i64>,
) -> (String, String) {
    let node_did = db.node_identity.did();
    let access = ConfigAccess::local_as(db.node.clone(), owner.did().parse().unwrap());
    let admin = ConfigAccess::local_as(db.node.clone(), node_did.parse().unwrap());
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        admin
            .grant_node_permission(&owner.did().parse().unwrap(), permission)
            .await
            .expect("grant user node permission");
    }
    let node = json!({"node_did": node_did, "enabled": true});
    let node_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Node,
        add: node.clone(),
        update: node,
    }])
    .unwrap();
    access
        .transact_write("test.session_limit.node", |txn| {
            let node_plan = &node_plan;
            Box::pin(async move {
                gents::store::config_client::apply_desired_state_plan(txn, node_plan).await
            })
        })
        .await
        .expect("create user-owned node");

    let user_did = owner.did().parse().unwrap();
    let parent = gents::store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &user_did,
    )
    .await
    .expect("provision parent agent");
    let child = gents::store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &user_did,
    )
    .await
    .expect("provision child agent");
    let mut documents = vec![
        (
            Collection::Node,
            json!({"node_did": node_did, "enabled": true, "default_agent_did": parent.agent_did}),
        ),
        (
            Collection::InferenceBackend,
            json!({
                "node_did": node_did, "backend_id": BACKEND_ID, "name": BACKEND_ID,
                "provider_kind": "OpenAiCompatible", "openai_wire_api": "chat_completions",
                "endpoint": endpoint, "auth": {"kind": "unauthenticated"},
                "max_concurrent": 1, "max_queue_depth": 100, "enabled": true
            }),
        ),
        (
            Collection::AgentTarget,
            serde_json::to_value(agent_target(
                node_did,
                CHILD_NAME,
                node_did,
                &child.agent_did,
            ))
            .unwrap(),
        ),
        (
            Collection::Tools,
            json!({"node_did": node_did, "tools_id": "parent-tools", "agents": {"enabled": true, "target_ids": [CHILD_NAME]}}),
        ),
        (
            Collection::AgentContext,
            json!({"node_did": node_did, "context_id": "parent-context", "tools_id": "parent-tools"}),
        ),
    ];
    for (binding, profile, context) in [
        (&parent, "parent-profile", Some("parent-context")),
        (&child, "child-profile", None),
    ] {
        let mut profile_doc = json!({"node_did": node_did, "profile_id": profile, "backend_id": BACKEND_ID, "model_name": MODEL});
        if profile == "child-profile" && max_turns.is_some() {
            profile_doc["execution_id"] = "child-execution".into();
        }
        documents.push((Collection::InferenceProfile, profile_doc));
        let mut agent = json!({"node_did": node_did, "agent_did": binding.agent_did, "creation_nonce": binding.creation_nonce, "inference_profile_id": profile, "enabled": true});
        if let Some(context) = context {
            agent["context_id"] = context.into();
        }
        documents.push((Collection::Agent, agent));
    }
    if let Some(max_turns) = max_turns {
        documents.push((Collection::InferenceExecution, json!({"node_did": node_did, "execution_id": "child-execution", "max_turns": max_turns})));
    }
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact_write("test.session_limit.configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .expect("create user-owned agent configuration");
    gents::inference::backends::registry::set_backend_probe_status_as(
        &admin, node_did, BACKEND_ID, "healthy",
    )
    .await
    .expect("record runtime backend health");
    let escaped = escape_graphql_string(&child.agent_did);
    let response = access
        .execute(&format!(
            r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    let child_doc = response["data"]["Agent"][0]["_docID"]
        .as_str()
        .expect("child agent document");
    access
        .add_actor_relationship(
            "Agent",
            child_doc,
            "session_writer",
            &parent.agent_did.parse().unwrap(),
        )
        .await
        .expect("authorize parent to start child sessions");
    (parent.agent_did, child.agent_did)
}

async fn child_of(
    access: &ConfigAccess,
    parent_request_id: &str,
    child_did: &str,
) -> Option<ChildRequestRow> {
    let escaped = escape_graphql_string(parent_request_id);
    let response = access.execute(&format!(r#"{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }}) {{ _docID request_id session_id session_doc_id agent_doc_id relationships_pending agent_did requester_did admission_signer_did lifecycle_state failure_reason }} }}"#)).await.expect("read child request");
    response["data"]["AgentRequest"]
        .as_array()
        .into_iter()
        .flatten()
        .cloned()
        .map(|row| serde_json::from_value::<ChildRequestRow>(row).expect("decode child request"))
        .find(|row| row.agent_did.as_deref() == Some(child_did))
}

async fn wait_for_terminal_child(
    db: &TestDb,
    access: &ConfigAccess,
    parent_request_id: &str,
    parent_did: &str,
    child_did: &str,
    runtime: &AcceptedTurnRuntime,
) -> ChildRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let parent = ConfigAccess::local_as(db.node.clone(), parent_did.parse().unwrap());
    loop {
        if let Some(child) = child_of(&parent, parent_request_id, child_did).await {
            if matches!(
                child.lifecycle_state.as_deref(),
                Some("completed") | Some("failed")
            ) {
                let user_view = child_of(access, parent_request_id, child_did)
                    .await
                    .expect("the originating user can read the child request");
                assert_eq!(user_view.doc_id, child.doc_id);
                assert_eq!(child.relationships_pending, Some(false));
                let selected = ConfigAccess::local_as(db.node.clone(), child_did.parse().unwrap());
                let selected_view = child_of(&selected, parent_request_id, child_did)
                    .await
                    .expect("the selected agent can read its child request");
                assert_eq!(selected_view.doc_id, child.doc_id);
                let session = escape_graphql_string(&child.session_id);
                let visible = access.execute(&format!(r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 2) {{ _docID }} }}"#)).await.expect("read child session as originating user");
                let rows = visible["data"]["AgentSession"].as_array().unwrap();
                assert_eq!(rows.len(), 1, "originating user can read the child session");
                let doc_id = rows[0]["_docID"].as_str().unwrap().to_owned();
                assert_eq!(child.session_doc_id.as_deref(), Some(doc_id.as_str()));
                let agent = escape_graphql_string(child_did);
                let agents = access.execute(&format!(r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{agent}" }} }}, limit: 2) {{ _docID }} }}"#)).await.expect("read selected Agent as originating user");
                let agents = agents["data"]["Agent"].as_array().unwrap();
                assert_eq!(
                    agents.len(),
                    1,
                    "originating user can read the selected Agent"
                );
                assert_eq!(child.agent_doc_id.as_deref(), agents[0]["_docID"].as_str());
                let user: identity::Did = access.actor_did().unwrap().parse().unwrap();
                let permissions = access
                    .transact_readonly("test.child_session_control", |txn| {
                        let doc_id = doc_id.clone();
                        let user = user.clone();
                        Box::pin(async move {
                            txn.observe_document_access(
                                &user,
                                "AgentSession",
                                &[doc_id],
                                gents::defra_node::DocumentAccessMode::Named {
                                    permission: "update".to_owned(),
                                    node_permission: NodePermission::DocumentUpdate,
                                },
                            )
                            .await
                        })
                    })
                    .await
                    .expect("observe originating user's child-session control permission");
                assert_eq!(
                    permissions,
                    [gents::defra_node::DocumentAccessDecision::Allowed]
                );
                return child;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let query = r#"{ AgentRequest { _docID request_id agent_did requester_did session_id lifecycle_state failure_reason terminal_output relationships_pending caused_by_parent_request_id } AgentToolCall { _docID request_id agent_did creator_did parent_linked tool_name tool_call_id lifecycle_state tool_failure_class terminal_output } AgentSession { _docID session_id requester_did agent_did relationships_pending } AgentMessage { _docID request_doc_id session_id agent_did role blocks parent_linked } }"#;
            let child = ConfigAccess::local_as(db.node.clone(), child_did.parse().unwrap());
            let snapshots = [
                ("user", access.execute(query).await),
                ("parent agent", parent.execute(query).await),
                ("child agent", child.execute(query).await),
            ];
            panic!(
                "timed out waiting for a terminal child of {parent_request_id}; actor-scoped rows: {snapshots:?}; provider bodies: {:?}",
                runtime.backend.observed_completion_bodies()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn child_tool_turn(index: usize) -> StreamResponse {
    StreamResponse::streams(
        CHILD_PROMPT,
        vec![StreamChunk::tool_call(
            format!("e2e-session-limit-turn-{index}"),
            "discover_tools",
            "{}".to_string(),
        )],
    )
}

async fn start_child_session(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
    child_responses: Vec<StreamResponse>,
    max_turns: Option<i64>,
) -> (
    String,
    String,
    ConfigAccess,
    Arc<ResolvedAgent>,
    AcceptedTurnRuntime,
) {
    let backend = MockStreamingBackend::start_with_plans(
        MODEL,
        vec![
            StreamPlan::current_authored_user(
                PARENT_PROMPT,
                vec![
                    StreamResponse::streams(
                        PARENT_PROMPT,
                        vec![StreamChunk::tool_call(
                            PARENT_TOOL_CALL_ID,
                            gents::tools::native::AGENT_NEW_TOOL_NAME,
                            json!({"agent": CHILD_NAME, "prompt": CHILD_PROMPT}).to_string(),
                        )],
                    ),
                    StreamResponse::completes(PARENT_PROMPT, ["parent complete"]),
                ],
            ),
            StreamPlan::current_authored_user(CHILD_PROMPT, child_responses),
        ],
    )
    .expect("start backend");
    let user = test_identity("session-limit-user");
    let (parent_did, child_did) =
        configure_session_chain(db, &user, backend.endpoint(), max_turns).await;
    let access = ConfigAccess::local_as(db.node.clone(), user.did().parse().unwrap());
    let request = build_signed_client_request(
        &access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.into(),
                node_did: db.node_identity.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: parent_did.clone(),
                session_id: session_id.into(),
                content: PARENT_PROMPT.into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(user.did()),
        ),
        RequestSigner::Identity(&user),
    )
    .await
    .expect("build signed client request");
    submit_client_request(&access, &request)
        .await
        .expect("publish client request");
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build child-limit runtime");
    let child_behavior = agent
        .agents()
        .iter()
        .find(|behavior| behavior.agent_did == child_did)
        .expect("child agent resolved from documents")
        .clone();
    let node_did = agent.node_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    let runtime = AcceptedTurnRuntime {
        runtime: BootedAgent::new(shutdown_tx, handle, node_did),
        backend,
    };
    (parent_did, child_did, access, child_behavior, runtime)
}

#[tokio::test]
async fn started_session_without_an_execution_document_runs_past_a_low_turn_limit() {
    let db = test_db("e2e-session-limit-default").await;
    let mut responses = (0..6).map(child_tool_turn).collect::<Vec<_>>();
    responses.push(StreamResponse::completes(CHILD_PROMPT, ["child done"]));
    let (parent_did, child_did, access, child_behavior, runtime) = start_child_session(
        &db,
        "e2e-session-limit-default-request",
        "e2e-session-limit-default-session",
        responses,
        None,
    )
    .await;
    let child = wait_for_terminal_child(
        &db,
        &access,
        "e2e-session-limit-default-request",
        &parent_did,
        &child_did,
        &runtime,
    )
    .await;
    assert_eq!(child.agent_did.as_deref(), Some(child_did.as_str()));
    let model = tokio::task::spawn_blocking(|| {
        let json = gents_lean_contract::load_contract_json().expect("Lean contract snapshot");
        let snapshot: serde_json::Value = serde_json::from_str(&json).unwrap();
        snapshot["tool_admission_handoff_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == "skip-with-hook-effect")
            .unwrap()
            .clone()
    })
    .await
    .unwrap();
    let expected_author = match model["expected_author"].as_str() {
        Some("selected-agent") => Some(parent_did.as_str()),
        None => None,
        other => panic!("unexpected modeled effect author {other:?}"),
    };
    assert_eq!(child.requester_did.as_deref(), expected_author);
    assert_eq!(child.admission_signer_did.as_deref(), expected_author);

    assert_eq!(
        child.lifecycle_state.as_deref(),
        Some("completed"),
        "child {} must finish every scripted turn, not exhaust a limit: {:?}",
        child.request_id,
        child.failure_reason
    );
    assert_eq!(child_behavior.max_turns, DEFAULT_MAX_TURNS);
    assert_eq!(child_behavior.max_turns, 1_000);
    assert_eq!(
        child_behavior.max_turns_provenance,
        MaxTurnsProvenance::Default
    );
    runtime.runtime.shutdown().await;
}

#[tokio::test]
async fn started_session_loop_enforces_its_execution_documents_turn_limit() {
    let db = test_db("e2e-session-limit-explicit").await;
    let (parent_did, child_did, access, child_behavior, runtime) = start_child_session(
        &db,
        "e2e-session-limit-explicit-request",
        "e2e-session-limit-explicit-session",
        (0..8).map(child_tool_turn).collect(),
        Some(EXPLICIT_CHILD_MAX_TURNS),
    )
    .await;
    let child = wait_for_terminal_child(
        &db,
        &access,
        "e2e-session-limit-explicit-request",
        &parent_did,
        &child_did,
        &runtime,
    )
    .await;
    assert_eq!(child.agent_did.as_deref(), Some(child_did.as_str()));
    assert_eq!(child.lifecycle_state.as_deref(), Some("failed"));
    let reason = child.failure_reason.unwrap_or_default();
    assert!(
        reason.starts_with(PINNED_PREFIX),
        "child failure must keep the pinned prefix: {reason}"
    );
    assert!(
        reason.contains(&format!(
            "reached max turn limit: {EXPLICIT_CHILD_MAX_TURNS})"
        )),
        "child loop must enforce the configured limit, not another one: {reason}"
    );
    assert!(
        reason.contains(MaxTurnsProvenance::ExecutionProfile.describe()),
        "the failure must name the knob that set the applied limit: {reason}"
    );
    assert!(
        !reason.contains(MaxTurnsProvenance::Default.describe()),
        "a document-bound limit must not report the built-in default: {reason}"
    );
    assert_eq!(
        child_behavior.max_turns,
        usize::try_from(EXPLICIT_CHILD_MAX_TURNS).unwrap()
    );
    assert_eq!(
        child_behavior.max_turns_provenance,
        MaxTurnsProvenance::ExecutionProfile
    );
    runtime.runtime.shutdown().await;
}
