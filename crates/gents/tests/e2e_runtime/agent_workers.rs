use std::sync::Arc;

use gents::{DocumentRuntimeOptions, Gents};
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use gents_lean_vocab::families::agent_workers_held_turns as family;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_session::request::lifecycle::{
    build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
    RequestSigner, RequestSpec,
};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::read_when_changed;
use gents_store::{collection::Collection, identity::NodeIdentity};
use gents_tools::surface::ToolCeiling;
use serde_json::json;

use crate::support::fixtures::{agent_target, test_identity};
use crate::support::interrupt::BootedAgent;
use crate::support::trial_store;
use gents_store::test_support::TestStore;

const BACKEND_ID: &str = "agent-workers-backend";
const MODEL: &str = "agent-workers-model";
const TARGET: &str = "agent-workers-self";
const PARENT_PROMPT: &str = "agent-workers-parent-prompt";
const CHILD_PROMPT: &str = "agent-workers-child-prompt";
const PARENT_HELD: &str = "agent-workers-parent-held";
const CHILD_HELD: &str = "agent-workers-child-held";
const SESSION: &str = "agent-workers-session";

/// One Agent whose only target is itself, on a backend admitting
/// `backend_limit` calls.
async fn configure_agent(
    db: &TestStore,
    owner: &dyn NodeIdentity,
    endpoint: &str,
    backend_limit: u64,
) -> String {
    let node_did = db.node_identity.did();
    let access = db.client_access(owner);
    let admin = ConfigAccess::local_as(db.node.clone(), node_did.parse().unwrap());
    let user_did = owner.did().parse().unwrap();
    gents_store::identity::GrantSet::OperatorUser
        .grant(&admin, &user_did)
        .await
        .expect("grant user operator access");
    let node = json!({"node_did": node_did, "enabled": true});
    let binding = {
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::Node,
            add: node.clone(),
            update: node.clone(),
        }])
        .unwrap();
        access
            .transact_write("test.agent_workers.node", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .expect("create user-owned node");
        gents_store::identity::provision_agent_identity_authorized(
            &db.node,
            db.node_identity.as_ref(),
            &user_did,
        )
        .await
        .expect("provision agent")
    };
    let documents = [
        (Collection::Node, node),
        (
            Collection::DefaultAgentSelection,
            json!({"node_did": node_did, "default_agent_did": binding.agent_did}),
        ),
        (
            Collection::InferenceBackend,
            json!({
                "node_did": node_did, "backend_id": BACKEND_ID, "name": BACKEND_ID,
                "provider_kind": "OpenAiCompatible", "openai_wire_api": "chat_completions",
                "endpoint": endpoint, "auth": {"kind": "unauthenticated"},
                "max_concurrent": backend_limit, "max_queue_depth": 100, "enabled": true
            }),
        ),
        (
            Collection::AgentTarget,
            serde_json::to_value(agent_target(node_did, TARGET, node_did, &binding.agent_did))
                .unwrap(),
        ),
        (
            Collection::Tools,
            json!({"node_did": node_did, "tools_id": "agent-workers-tools", "agents": {"enabled": true, "target_ids": [TARGET]}}),
        ),
        (
            Collection::AgentContext,
            json!({"node_did": node_did, "context_id": "agent-workers-context", "tools_id": "agent-workers-tools"}),
        ),
        (
            Collection::InferenceProfile,
            json!({"node_did": node_did, "profile_id": "agent-workers-profile", "backend_id": BACKEND_ID, "model_name": MODEL}),
        ),
        (
            Collection::Agent,
            json!({"node_did": node_did, "agent_did": binding.agent_did, "creation_nonce": binding.creation_nonce, "inference_profile_id": "agent-workers-profile", "context_id": "agent-workers-context", "enabled": true}),
        ),
    ];
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
        .transact_write("test.agent_workers.configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .expect("create user-owned agent configuration");
    gents_inference::backends::registry::set_backend_probe_status_as(
        &admin, node_did, BACKEND_ID, "healthy",
    )
    .await
    .expect("record runtime backend health");
    binding.agent_did
}

fn scripted_backend(children: u64) -> ScriptedBackend {
    let delegations = (0..children)
        .map(|index| {
            StreamChunk::tool_call(
                format!("agent-workers-delegate-{index}"),
                gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
                json!({"agent": TARGET, "prompt": CHILD_PROMPT}).to_string(),
            )
        })
        .collect();
    ScriptedBackend::start_with_plans(
        MODEL,
        vec![
            StreamPlan::current_authored_user(
                PARENT_PROMPT,
                vec![
                    StreamResponse::streams(PARENT_PROMPT, delegations),
                    StreamResponse::Stream(StreamScript::paused(PARENT_HELD, ["parent held"])),
                ],
            ),
            StreamPlan::current_authored_user(
                CHILD_PROMPT,
                vec![StreamResponse::Stream(StreamScript::paused(
                    CHILD_HELD,
                    ["child held"],
                ))],
            ),
        ],
    )
    .expect("start backend")
}

struct Turns {
    requests: u64,
    running: u64,
    at_server: u64,
    queued: u64,
    other_running: u64,
    session_title_started: bool,
}

impl Turns {
    /// Every turn holds a worker with its call held at the server or queued,
    /// the turn's session has its title audit started, no other call holds a
    /// backend permit, and a queued call waits only because the backend admits
    /// no more.
    fn settled(&self, children: u64, backend_limit: u64) -> bool {
        self.session_title_started
            && self.requests == children + 1
            && self.running == self.requests
            && self.at_server + self.queued == self.running
            && self.other_running == 0
            && (self.queued == 0 || self.at_server == backend_limit)
    }
}

/// The turn and its delegates' requests holding a worker, their model calls
/// running or queued at the backend's admission, the running calls of
/// anything else, and whether the title audit of the turn's session, the
/// second request on that session, has left the executor queue.
async fn turns(agent: &ConfigAccess, parent_request_id: &str) -> (Turns, Vec<String>) {
    use gents_store::test_support::snapshot_reader::SnapshotReader;
    let escaped = escape_graphql_string(parent_request_id);
    let requests = agent
        .read_data(&format!(
            r#"{{ AgentRequest(filter: {{ purpose: {{ _eq: "normal" }}, _or: [{{ request_id: {{ _eq: "{escaped}" }} }}, {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }}] }}) {{ request_id lifecycle_state }} }}"#
        ))
        .await;
    let rows = requests["AgentRequest"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let titles = agent
        .read_data(&format!(
            r#"{{ AgentRequest(filter: {{ purpose: {{ _eq: "title-audit" }}, session_id: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            escape_graphql_string(SESSION)
        ))
        .await;
    let session_title_started = titles["AgentRequest"].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row["lifecycle_state"]
                .as_str()
                .is_some_and(|state| state != "pending")
        })
    });
    let ids = rows
        .iter()
        .filter_map(|row| row["request_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let running = rows
        .iter()
        .filter(|row| {
            matches!(
                row["lifecycle_state"].as_str(),
                Some("claimed" | "processing")
            )
        })
        .count() as u64;
    let calls = agent
        .read_data(
            r#"{ InferenceCall(filter: { call_state: { _in: ["queued", "running"] } }) { request_id call_kind call_state } }"#,
        )
        .await;
    let calls = calls["InferenceCall"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let turn_call = |row: &serde_json::Value| {
        row["call_kind"].as_str() == Some("inference")
            && row["request_id"]
                .as_str()
                .is_some_and(|id| ids.iter().any(|turn| turn == id))
    };
    let count = |turn: bool, state: &str| {
        calls
            .iter()
            .filter(|row| turn_call(row) == turn && row["call_state"].as_str() == Some(state))
            .count() as u64
    };
    (
        Turns {
            requests: ids.len() as u64,
            running,
            at_server: count(true, "running"),
            queued: count(true, "queued"),
            other_running: count(false, "running"),
            session_title_started,
        },
        ids,
    )
}

async fn agent_workers(node: &ConfigAccess, agent_did: &str) -> u64 {
    use gents_store::test_support::snapshot_reader::SnapshotReader;
    read_when_changed(node, "NodeRuntime", "agent worker capacity", async || {
        let rows = node
            .read_data("{ NodeRuntime { agent_executor_status_json } }")
            .await;
        rows["NodeRuntime"]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["agent_executor_status_json"].as_str())
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .and_then(|statuses| statuses[agent_did]["worker_capacity"].as_u64())
            .ok_or_else(|| format!("{rows}"))
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delegates_start_while_their_turn_holds_the_backend() {
    for case in family::cases() {
        let backend = scripted_backend(case.input.children);
        let db = trial_store("agent-workers-held-turns").await;
        let user = test_identity("agent-workers-user");
        let agent_did =
            configure_agent(&db, &user, backend.endpoint(), case.input.backend_limit).await;
        let access = db.client_access(&user);
        let parent_request_id = format!("agent-workers-parent-{}", case.input.backend_limit);
        let request = build_signed_client_request(
            &access,
            RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    request_id: parent_request_id.clone(),
                    node_did: db.node_identity.did().into(),
                    requester_did: Some(user.did().into()),
                    agent_did: agent_did.clone(),
                    session_id: SESSION.into(),
                    content: PARENT_PROMPT.into(),
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
                backend_admission: Some(db.backend_admission()),
                tool_ceiling: ToolCeiling::meta_only(),
                ..Default::default()
            },
        )
        .await
        .expect("build runtime");
        let node_did = agent.node_did().to_string();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let runtime = BootedAgent::new(shutdown_tx, tokio::spawn(agent.run(shutdown_rx)), node_did);

        let agent_access = ConfigAccess::local_as(db.node.clone(), agent_did.parse().unwrap());
        let node_access =
            ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse().unwrap());
        let held = read_when_changed(
            &agent_access,
            "InferenceCall",
            "every turn holding a worker with its call held at the server or queued",
            async || {
                let (turns, _) = turns(&agent_access, &parent_request_id).await;
                if turns.settled(case.input.children, case.input.backend_limit) {
                    Ok(turns)
                } else {
                    Err(format!(
                        "backend limit {} with {} delegates: {} requests, {} running, {} calls at the server, {} queued, {} other calls running, session title started {}",
                        case.input.backend_limit,
                        case.input.children,
                        turns.requests,
                        turns.running,
                        turns.at_server,
                        turns.queued,
                        turns.other_running,
                        turns.session_title_started
                    ))
                }
            },
        )
        .await;
        backend.wait_for_in_flight(held.at_server as usize).await;

        backend.release(PARENT_HELD);
        backend.release(CHILD_HELD);
        let (_, ids) = turns(&agent_access, &parent_request_id).await;
        for id in &ids {
            crate::support::live_inference::wait_for_request_terminal(&agent_access, id).await;
        }
        let workers = agent_workers(&node_access, &agent_did).await;
        case.check(family::HeldTurnsObservation {
            workers,
            running: held.running,
            at_server: held.at_server,
            queued: held.queued,
            peak: backend.peak_in_flight() as u64,
            session_title_started: held.session_title_started,
        });
        runtime.shutdown().await;
    }
}
