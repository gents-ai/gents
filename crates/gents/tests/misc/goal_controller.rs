use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::Node as EmbeddedNode;
use gents_automation::goal::{seed_goal_from_access, session_token_usage};
use gents_automation::{
    triggers::engine::goal_source::GoalSource, triggers::engine::TriggerSource,
    triggers::runtime_snapshot::ActiveRuntimeSnapshot,
};
use gents_protocol::goal_status::GoalStatus;
use gents_store::graphql::escape_graphql_string;
use gents_store::subscription_source::UpdateSubscriptionSource;
use gents_store::{config_client::ConfigAccess, identity::NodeIdentity};
use serde::Deserialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::support::mock_subscription::MockUpdateSubscriptionSource;
use crate::support::{test_db, TestDb};

const SESSION: &str = "goal-session";
const RESCAN: Duration = Duration::from_millis(20);

async fn goal_creation_actors(
    db: &TestDb,
) -> (gents_store::identity::KeyIdentity, Arc<dyn NodeIdentity>) {
    let fixture = crate::support::managed_agent_fixture(db).await;
    let admin = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse().unwrap());
    for actor in [fixture.user.did(), fixture.agent.did()] {
        for permission in [
            gents_store::config_client::NodePermission::DocumentDelete,
            gents_store::config_client::NodePermission::CollectionGet,
            gents_store::config_client::NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    reserve_goal_session(db, &fixture.user, &fixture.agent, SESSION).await;
    (fixture.user, fixture.agent)
}

async fn load_goal_as(
    access: &ConfigAccess,
    node_did: &str,
) -> Option<gents_automation::goal::GoalDocument> {
    access
        .transact_readonly("test.load_client_goal", |txn| {
            Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                txn, node_did, SESSION,
            ))
        })
        .await
        .unwrap()
}

fn snapshot(local_did: &str, agent_did: &str) -> Arc<ActiveRuntimeSnapshot> {
    Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: local_did.to_string(),
        default_agent_did: agent_did.to_string(),
        tool_configs: Default::default(),
        agents: RapidHashMap::new(),
        tool_surfaces: RapidHashMap::new(),
        backend_admission_configs: RapidHashMap::new(),
        unavailable_agents: RapidHashMap::new(),
        active_schedules: RapidHashMap::new(),
        unavailable_schedules: RapidHashSet::new(),
        active_event_triggers: RapidHashMap::new(),
        unavailable_event_triggers: RapidHashSet::new(),
        active_tasks: RapidHashMap::new(),
        dispatchers: RapidHashMap::new(),
        agent_executor_capacities: RapidHashMap::new(),
        agent_executor_queue_capacities: RapidHashMap::new(),
    })
}

async fn publish_node_readiness_for_agent(
    db: &TestDb,
    process_state: gents_protocol::row::NodeReadinessProcessState,
    behavior: Option<gents_protocol::row::AgentReadinessUnavailableReason>,
    agent_did: &str,
) {
    use gents_protocol::row::{
        AgentReadinessEntry, AgentReadinessState, NodeReadinessSnapshot,
        NODE_READINESS_FORMAT_VERSION,
    };
    let snapshot = NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state,
        active_generation: 1,
        router_generation: 1,
        default_agent_did: agent_did.to_string(),
        agents: vec![AgentReadinessEntry {
            agent_did: agent_did.to_string(),
            state: if behavior.is_some() {
                AgentReadinessState::Unavailable
            } else {
                AgentReadinessState::Ready
            },
            reason: behavior,
        }],
    };
    let did = gents_store::graphql::escape_graphql_string(db.node_identity.did());
    let snapshot_json =
        gents_store::graphql::escape_graphql_string(&serde_json::to_string(&snapshot).unwrap());
    let updated_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let access = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse().unwrap());
    let response = access
        .execute(&format!(
            r#"{{ Node(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID}}
            NodeReadiness(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID}} }}"#
        ))
        .await
        .unwrap();
    let parent = response["data"]["Node"][0]["_docID"]
        .as_str()
        .expect("visible readiness Node");
    let readiness = response["data"]["NodeReadiness"].as_array().unwrap();
    if let Some(existing) = readiness.first() {
        assert_eq!(readiness.len(), 1);
        let doc = escape_graphql_string(existing["_docID"].as_str().unwrap());
        let response = access.write("test.goal_readiness.update", &format!(r#"mutation {{update_NodeReadiness(docID: "{doc}", input: {{snapshot_json: "{snapshot_json}", updated_at: "{updated_at}"}}) {{_docID}}}}"#)).await.unwrap();
        assert_eq!(
            response["data"]["update_NodeReadiness"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    } else {
        let escaped_parent = escape_graphql_string(parent);
        let response = access.write("test.goal_readiness.create", &format!(r#"mutation {{create_NodeReadiness(input: {{node_did: "{did}", node_doc_id: "{escaped_parent}", parent_linked: false, snapshot_json: "{snapshot_json}", updated_at: "{updated_at}"}}) {{_docID}}}}"#)).await.unwrap();
        let doc = gents_store::graphql::created_doc_id(&response, "NodeReadiness").unwrap();
        access
            .add_resource_relationship("NodeReadiness", &doc, "parent", "nodes", parent)
            .await
            .unwrap();
        let doc = escape_graphql_string(&doc);
        let response = access.write("test.goal_readiness.linked", &format!(r#"mutation {{update_NodeReadiness(docID: "{doc}", input: {{parent_linked: true}}) {{_docID}}}}"#)).await.unwrap();
        assert_eq!(
            response["data"]["update_NodeReadiness"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

async fn source_for_agent(
    db: &TestDb,
    agent_identity: &Arc<dyn NodeIdentity>,
) -> (GoalSource, watch::Sender<Arc<ActiveRuntimeSnapshot>>) {
    publish_node_readiness_for_agent(
        db,
        gents_protocol::row::NodeReadinessProcessState::Ready,
        None,
        agent_identity.did(),
    )
    .await;
    unready_source_for_agent(db, agent_identity)
}

fn agent_snapshot(
    db: &TestDb,
    agent_identity: &Arc<dyn NodeIdentity>,
) -> Arc<ActiveRuntimeSnapshot> {
    let node =
        crate::support::fixtures::test_node_for(db.node_identity.clone(), agent_identity.did());
    let agent =
        crate::support::fixtures::test_agent_with_identity(node.clone(), agent_identity.clone());
    let mut current = (*snapshot(db.node_identity.did(), agent_identity.did())).clone();
    current.node = Some(node);
    current.default_agent_did = agent_identity.did().to_owned();
    current
        .agents
        .insert(agent_identity.did().to_owned(), Arc::new(agent));
    Arc::new(current)
}

fn unready_source_for_agent(
    db: &TestDb,
    agent_identity: &Arc<dyn NodeIdentity>,
) -> (GoalSource, watch::Sender<Arc<ActiveRuntimeSnapshot>>) {
    let (tx, rx) = watch::channel(agent_snapshot(db, agent_identity));
    let subscriptions: Arc<dyn UpdateSubscriptionSource> =
        Arc::new(MockUpdateSubscriptionSource::new());
    (
        GoalSource::with_subscription_source(
            subscriptions,
            rx,
            db.node.clone(),
            CancellationToken::new(),
        )
        .with_rescan_interval(RESCAN),
        tx,
    )
}

struct GoalBackgroundHandoff {
    runtime: crate::support::accepted_turn::AcceptedTurnRuntime,
    agent_identity: Arc<dyn NodeIdentity>,
    parent_doc_id: String,
    background_executions: gents_toolcalls::call::hook::BackgroundExecutionRegistry,
    process_tool_call_id: String,
}

async fn boot_goal_background_handoff_with_plans(
    db: &TestDb,
    fixture: &crate::support::ManagedAgentFixture,
    request_id: &str,
    entered_path: &std::path::Path,
    release_path: &std::path::Path,
    child_plans: Vec<crate::support::streaming_backend::StreamPlan>,
) -> GoalBackgroundHandoff {
    let did = db.node_identity.did();
    let agent_identity = fixture.agent.clone();
    let behavior = agent_identity.did();
    let prompt = "start late background handoff";
    let prepared = crate::support::accepted_turn::prepare_managed_accepted_turn(
        db,
        fixture,
        crate::support::accepted_turn::AcceptedTurnSpec {
            backend_id: "goal-background-backend",
            model: "test-model",
            parent_agent_did: behavior,
            configured_agent_dids: &[behavior],
            request_id,
            session_id: SESSION,
            prompt,
            accepted_chunks: vec![crate::support::streaming_backend::StreamChunk::tool_call(
                "goal-background-spawn",
                "spawn_process",
                serde_json::json!({
                    "tool_name": "bash",
                    "args": {
                        "command": "sh",
                        "args": [
                            "-c",
                            ": > \"$1\"; n=0; while [ ! -f \"$2\" ] && [ \"$n\" -lt 3000 ]; do sleep 0.02; n=$((n + 1)); done; [ -f \"$2\" ] || exit 124; printf 'late result required for final wrapup'",
                            "goal-background-handoff",
                            entered_path.to_string_lossy(),
                            release_path.to_string_lossy(),
                        ]
                    }
                })
                .to_string(),
            )],
            child_plans,
            valid_until: None,
            request_hop: None,
            request_setup: None,
        },
    )
    .await;
    prepared.backend.enable_dynamic_followups(prompt);
    crate::support::fixtures::configure_agent_tools_as(
        &fixture.access,
        did,
        behavior,
        None,
        gents_documents::Tools {
            tools_id: format!("{behavior}:tools"),
            node_did: did.to_string(),
            host: Some(gents_documents::HostTools {
                bash: Some(gents_documents::BashTools {
                    mode: gents_documents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sh".into()]),
                    background_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents_tools::surface::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let background_executions = agent.background_execution_registry();
    let runtime =
        crate::support::accepted_turn::boot_prepared_accepted_turn(db, prepared, agent).await;
    let request = escape_graphql_string(request_id);
    let response = fixture
        .access
        .execute(&format!(
            r#"{{AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}, limit: 2) {{_docID}}}}"#
        ))
        .await
        .unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let parent_doc = rows[0]["_docID"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        let response = fixture.access.execute(&format!(r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "bash" }}, lifecycle_state: {{ _eq: "running" }} }}, limit: 1) {{ _docID tool_call_id }} }}"#, gents_store::graphql::escape_graphql_string(&parent_doc))).await.unwrap();
        if let Some(row) = response
            .get("data")
            .and_then(|data| data["AgentToolCall"].as_array())
            .and_then(|rows| rows.first())
        {
            if entered_path.exists() {
                return GoalBackgroundHandoff {
                    runtime,
                    agent_identity,
                    parent_doc_id: parent_doc,
                    background_executions,
                    process_tool_call_id: row["tool_call_id"]
                        .as_str()
                        .expect("running process has its registered tool call ID")
                        .to_owned(),
                };
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("accepted goal background handoff did not start")
}

#[derive(Debug, Deserialize)]
struct ChildRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    node_did: String,
    session_id: String,
    agent_did: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    input: Option<serde_json::Value>,
    lifecycle_state: Option<String>,
    retry_key: Option<String>,
    request_hop: Option<i64>,
    workspace_id: Option<String>,
    workspace_authority: Option<String>,
    workspace_owner_node_did: Option<String>,
    workspace_seal_hash: Option<String>,
}

#[tokio::test]
async fn exhausted_budget_after_failed_or_dead_request_materializes_wrapup_not_retry() {
    use crate::support::streaming_backend::{
        StreamChunk, StreamPlan, StreamResponse, StreamScript,
    };

    let objective = "Respect the budget even when execution fails";
    for terminal in ["failed", "dead"] {
        let db = test_db(&format!("goal-budget-after-{terminal}")).await;
        let fixture = crate::support::managed_agent_fixture(&db).await;
        let agent_identity = fixture.agent.clone();
        let access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            agent_identity.did().parse().unwrap(),
            agent_identity.as_ref(),
        )
        .unwrap();
        reserve_goal_session(&db, &fixture.user, &agent_identity, SESSION).await;
        seed_goal_from_access(
            &access,
            db.node_identity.did(),
            SESSION,
            Some(objective),
            Some(GoalStatus::Active),
            Some(Some(100)),
            None,
        )
        .await
        .unwrap();
        let parent = "parent-over-budget";
        let barrier = tempfile::tempdir().unwrap();
        let entered_path = barrier.path().join("entered");
        let release_path = barrier.path().join("release");
        let hold_wrapup = StreamPlan::current_authored_user(
            objective,
            vec![StreamResponse::Stream(StreamScript::paused_before(
                objective,
                vec![StreamChunk::text("wrapup reached provider")],
            ))],
        );
        let GoalBackgroundHandoff {
            runtime,
            agent_identity,
            parent_doc_id: parent_doc,
            background_executions,
            process_tool_call_id,
        } = boot_goal_background_handoff_with_plans(
            &db,
            &fixture,
            parent,
            &entered_path,
            &release_path,
            vec![hold_wrapup],
        )
        .await;
        let prior_usage = session_token_usage(
            &db.node,
            &agent_identity.did().parse().unwrap(),
            db.node_identity.did(),
            SESSION,
        )
        .await
        .unwrap();
        assert!(
            prior_usage < 100,
            "the held parent has not exhausted the Goal budget"
        );
        let expected_usage = prior_usage + 105;
        fail_request_with_usage(
            &access,
            &agent_identity,
            db.node_identity.did(),
            parent,
            &parent_doc,
            terminal,
        )
        .await;
        assert_eq!(
            session_token_usage(
                &db.node,
                &agent_identity.did().parse().unwrap(),
                db.node_identity.did(),
                SESSION
            )
            .await
            .unwrap(),
            expected_usage,
        );

        for _ in 0..200 {
            if goal_children_from_access(&access).await.len() == 1
                && runtime.backend.observed_requests(objective) > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            goal_children_from_access(&access).await.len(),
            1,
            "the Goal wrapup is published"
        );
        assert_eq!(
            runtime.backend.observed_requests(objective),
            1,
            "the wrapup reached its held provider response"
        );
        std::fs::write(&release_path, b"release").unwrap();
        let completion = tokio::time::timeout(
            Duration::from_secs(5),
            background_executions.wait_for_completion(&process_tool_call_id),
        )
        .await;
        let observed = fixture.access.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state status completion_notification_delivered_at }} }}"#,
            gents_store::graphql::escape_graphql_string(&process_tool_call_id),
        )).await.unwrap();
        let delivery = observed["data"].clone();
        assert!(
            completion.is_ok(),
            "background process {process_tool_call_id} did not finish its completion side effects after {terminal}; observed={delivery}"
        );
        let rows = delivery["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "exact background process: {delivery}");
        assert!(
            rows[0]["completion_notification_delivered_at"].is_string(),
            "the worker must publish its completion before ownership ends: {delivery}"
        );
        gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
            &db.node,
            db.node_identity.did(),
            std::slice::from_ref(&agent_identity),
        )
        .await
        .unwrap();
        let observed = fixture.access.execute(
            "{ AgentRequest { _docID request_id created_at lifecycle_state execution_origin caused_by_trigger_kind caused_by_parent_request_id } AgentMessage { _docID node_did requester_did request_doc_id } AgentToolCall { _docID tool_name lifecycle_state completion_notification_delivered_at } }",
        ).await.unwrap();
        let data = observed["data"].clone();
        assert_eq!(
            data["AgentRequest"].as_array().unwrap().len(),
            3,
            "expected the parent, the Goal wrapup and one completion wake; observed={data}"
        );
        let requests = data["AgentRequest"].as_array().unwrap();
        let goal_requests = requests
            .iter()
            .filter(|row| row["caused_by_trigger_kind"].as_str() == Some("goal"))
            .collect::<Vec<_>>();
        assert_eq!(
            goal_requests.len(),
            1,
            "expected exactly one Goal wrapup: {requests:?}"
        );
        let wakes = requests
            .iter()
            .filter(|row| {
                row["request_id"] != parent && row["_docID"] != goal_requests[0]["_docID"]
            })
            .collect::<Vec<_>>();
        assert_eq!(
            wakes.len(),
            1,
            "besides the parent and the Goal wrapup, only the completion wake exists: {requests:?}"
        );
        let wrapup = goal_requests[0];
        assert_eq!(
            wrapup["caused_by_parent_request_id"], parent,
            "the wrapup must continue the session's latest request: {requests:?}"
        );
        assert_eq!(goal_requests[0]["execution_origin"], "scheduled");
        let messages = data["AgentMessage"].as_array().unwrap();
        let mut completions = Vec::new();
        for header in messages {
            let (_, message) = gents_session::session::load_canonical_message(
                &fixture.access,
                header["_docID"].as_str().unwrap(),
                header["node_did"].as_str().unwrap(),
                header["requester_did"].as_str(),
            )
            .await
            .unwrap();
            let rendered = format!("{message:?}");
            if rendered.contains("tool-completion") {
                completions.push((header, rendered));
            }
        }
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].0["request_doc_id"], wakes[0]["_docID"]);
        let background_rows = data["AgentToolCall"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["tool_name"] == "bash")
            .collect::<Vec<_>>();
        assert_eq!(
            background_rows.len(),
            1,
            "the real background execution must have one physical lifecycle row: {}",
            data["AgentToolCall"]
        );
        assert!(
            background_rows[0]["completion_notification_delivered_at"].is_string(),
            "the physical background row must durably account its notification delivery: {}",
            data["AgentToolCall"]
        );
        let goal = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(
            goal.parsed_status(),
            Some(GoalStatus::BudgetLimited),
            "the runtime's sole GoalSource owns the budget decision after {terminal}"
        );
        runtime.shutdown().await;
        assert_eq!(goal.tokens_used, Some(expected_usage), "{terminal}");
        assert_eq!(goal.wrapup_requested, Some(true), "{terminal}");
        assert!(!goal.wrapup_completed.unwrap_or(false), "{terminal}");
        assert_eq!(
            goal.infrastructure_retry_count.unwrap_or_default(),
            0,
            "{terminal}"
        );
        let children = goal_children_from_access(&access).await;
        assert_eq!(children.len(), 1, "{terminal}");
        let visible = goal_children_from_access(&fixture.access).await;
        assert_eq!(visible.len(), 1, "User must see the Goal wrapup");
        assert_eq!(visible[0].doc_id, children[0].doc_id);
        assert_eq!(
            children[0].caused_by_parent_request_id.as_deref(),
            Some(parent)
        );
        let input = children[0].input.as_ref().unwrap();
        assert_eq!(input["goal_continuation"]["wrapup"], true, "{terminal}");
        let history = load_history_as(
            &fixture.access,
            &children[0].node_did,
            fixture.user.did(),
            &children[0].session_id,
        )
        .await;
        assert!(
            serde_json::to_string(&history)
                .unwrap()
                .contains("late result required for final wrapup"),
            "the Goal child must be able to load the parent's durable background input"
        );

        let (mut restarted, _restart_tx) = source_for_agent(&db, &agent_identity).await;
        let restarted_fire =
            tokio::time::timeout(Duration::from_millis(200), restarted.next_fire()).await;
        assert!(
            restarted_fire.is_err(),
            "{terminal}: unexpected restart result; existing children={children:?}, emitted={:?}",
            restarted_fire.as_ref().ok().map(|result| result
                .as_ref()
                .map(|intent| &intent.pre_materialized_request_id))
        );
        let current = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(current.parsed_status(), Some(GoalStatus::BudgetLimited));
        assert_eq!(current.tokens_used, Some(expected_usage));
        assert_eq!(current.continuation_sequence, goal.continuation_sequence);
        assert_eq!(current.infrastructure_retry_count.unwrap_or_default(), 0);
        let persisted = goal_children_from_access(&access).await;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].doc_id, children[0].doc_id);
        drop(restarted);
    }
}

#[path = "goal_controller/control_failure.rs"]
mod control_failure;

async fn goal_fixture_request_with_state(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    request_id: &str,
    state: &str,
) -> String {
    goal_fixture_request_with_state_at(db, user, agent, request_id, state, "2026-07-15T00:00:00Z")
        .await
}

async fn goal_fixture_request_with_state_at(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    request_id: &str,
    state: &str,
    created_at: &str,
) -> String {
    goal_fixture_request_in_session_at(db, user, agent, SESSION, request_id, state, created_at)
        .await
}

async fn goal_fixture_request_in_session_at(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    session: &str,
    request_id: &str,
    state: &str,
    created_at: &str,
) -> String {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_session::request::lifecycle::{
        build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
        RequestSigner, RequestSpec,
    };
    let client = db.client_access(user);
    let spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            node_did: db.node_identity.did().into(),
            requester_did: Some(user.did().into()),
            agent_did: agent.did().into(),
            session_id: session.into(),
            request_id: request_id.into(),
            content: "completed baseline request".into(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: created_at.into(),
        },
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let create = build_signed_client_request(&client, spec, RequestSigner::Identity(user))
        .await
        .unwrap();
    submit_client_request(&client, &create).await.unwrap();
    let request = escape_graphql_string(request_id);
    let rows = client
        .execute(&format!(
            r#"{{AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}) {{_docID}}}}"#
        ))
        .await
        .unwrap();
    let doc = rows["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let escaped = escape_graphql_string(&doc);
    let state = escape_graphql_string(state);
    let changed = agent_access.write("test.goal_baseline_complete", &format!(r#"mutation {{update_AgentRequest(docID: "{escaped}", input: {{lifecycle_state: "{state}"}}) {{_docID}}}}"#)).await.unwrap();
    assert_eq!(
        changed["data"]["update_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    doc
}

async fn reserve_goal_session(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    session: &str,
) -> String {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_session::request::lifecycle::{
        build_signed_client_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
    };

    let access = db.client_access(user);
    let spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            node_did: db.node_identity.did().into(),
            requester_did: Some(user.did().into()),
            agent_did: agent.did().into(),
            session_id: session.into(),
            request_id: format!("goal-session-reservation-{session}"),
            content: "reserve session".into(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: "2026-07-14T00:00:00Z".into(),
        },
        AgentRequestAdmissionRecord::client(user.did()),
    );
    build_signed_client_request(&access, spec, RequestSigner::Identity(user))
        .await
        .expect("reserve user-owned goal session")
        .session_doc_id
        .expect("reserved physical session")
}

async fn publish_owned_terminal_activity(
    access: &ConfigAccess,
    agent: &dyn NodeIdentity,
    request_doc_id: &str,
    content: &str,
) {
    use gents_protocol::output::rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
    let escaped = escape_graphql_string(request_doc_id);
    let response = access.execute(&format!(r#"{{AgentRequest(docID: "{escaped}") {{node_did requester_did session_id session_doc_id agent_did}}}}"#)).await.unwrap();
    let row = &response["data"]["AgentRequest"][0];
    let node_did = row["node_did"].as_str().unwrap();
    let requester_did = row["requester_did"].as_str();
    let session_id = row["session_id"].as_str().unwrap();
    let session_doc = row["session_doc_id"].as_str().unwrap();
    assert_eq!(row["agent_did"], agent.did());
    let existing = access.execute(&format!(r#"{{AgentMessage(filter: {{session_doc_id: {{_eq: "{}"}}}}, order: {{sequence: DESC}}, limit: 1) {{sequence}}}}"#, escape_graphql_string(session_doc))).await.unwrap();
    let sequence = existing["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .first()
        .and_then(|row| row["sequence"].as_u64())
        .unwrap_or(0) as u32
        + 1;
    let generation = format!("goal-observation-{request_doc_id}");
    let source = OutputSource::ProviderTurn {
        scope: CaptureScope {
            kind: CaptureScopeKind::Inference,
            seq: u64::from(sequence),
        },
        turn_index: 0,
        attempt: 0,
    };
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: requester_did.map(Into::into),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source,
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: content.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: content.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![content.len() as u64],
        }),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    access.transact_write("test.goal_owned_terminal_output", |txn| {
        let generation = &generation;
        let segment = &segment;
        let escaped = &escaped;
        Box::pin(async move {
            let mut variables = output_segment_create_variables(segment)?;
            variables["input"]["session_doc_id"] = serde_json::json!(session_doc);
            variables["input"]["agent_did"] = serde_json::json!(agent.did());
            variables["input"]["creator_did"] = serde_json::json!(agent.did());
            variables["input"]["parent_linked"] = serde_json::json!(true);
            let created = txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables).await?;
            let close_doc_id = gents_store::graphql::created_doc_id(&created, "AgentOutputSegment")?;
            txn.add_resource_relationship("AgentOutputSegment", &close_doc_id, "parent", "sessions", session_doc).await?;
            txn.add_actor_relationship("AgentOutputSegment", &close_doc_id, "working_agent", &agent.did().parse()?).await?;
            let message = TranscriptMessage {
                native_request_id: None, message_key: gents_session::session::sequence_message_key(node_did, session_id, requester_did, sequence),
                session_id: session_id.into(), node_did: node_did.into(), requester_did: requester_did.map(Into::into),
                request_doc_id: Some(request_doc_id.into()), publication: MessagePublication::RequestExecution {execution_generation: generation.clone()},
                outcome: OutputOutcome::Complete, sequence, role: MessageRole::Assistant, native_id: Some(format!("goal-terminal-{sequence}")),
                blocks: vec![MessageBlock::Text { text: PresentedPayload {output: PayloadRef {close_doc_id, stream: 0}, presentation: PayloadPresentation::Full} }],
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let mut variables = transcript_message_create_variables(&message)?;
            variables["input"]["session_doc_id"] = serde_json::json!(session_doc);
            variables["input"]["agent_did"] = serde_json::json!(agent.did());
            variables["input"]["creator_did"] = serde_json::json!(agent.did());
            variables["input"]["parent_linked"] = serde_json::json!(true);
            let created = txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &variables).await?;
            let message_doc_id = gents_store::graphql::created_doc_id(&created, "AgentMessage")?;
            txn.add_resource_relationship("AgentMessage", &message_doc_id, "parent", "sessions", session_doc).await?;
            txn.add_actor_relationship("AgentMessage", &message_doc_id, "working_agent", &agent.did().parse()?).await?;
            let terminal = gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(TerminalOutput::Message {message_doc_id})?)?;
            txn.execute(&format!(r#"mutation {{update_AgentRequest(docID: "{escaped}", input: {{execution_generation: "{}", terminal_output: {terminal}}}) {{_docID}}}}"#, escape_graphql_string(generation))).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
}

async fn goal_recovery_actors(
    db: &TestDb,
    request_id: &str,
    state: &str,
) -> (
    gents_store::identity::KeyIdentity,
    Arc<dyn NodeIdentity>,
    ConfigAccess,
) {
    let (user, agent) = goal_creation_actors(db).await;
    let doc = goal_fixture_request_with_state(db, &user, &agent, request_id, state).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    if state == "completed" {
        publish_owned_terminal_activity(&access, agent.as_ref(), &doc, "durable progress").await;
    }
    (user, agent, access)
}

async fn goal_children_from_access(access: &ConfigAccess) -> Vec<ChildRow> {
    let response = access.execute(r#"{
        AgentRequest(filter: {caused_by_trigger_kind: {_eq: "goal"}}) {
            _docID request_id node_did requester_did session_id agent_did caused_by_parent_request_id caused_by_parent_request_doc_id
            caused_by_trigger_id caused_by_trigger_kind input lifecycle_state retry_key request_hop workspace_id workspace_authority
            workspace_owner_node_did workspace_seal_hash
        }
    }"#).await.expect("query selected Agent's goal children");
    serde_json::from_value(response["data"]["AgentRequest"].clone()).expect("decode goal children")
}

async fn wait_for_goal_transition_as(
    access: &ConfigAccess,
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    source: &mut GoalSource,
    description: &str,
    reached: impl Fn(&gents_automation::goal::GoalDocument) -> bool,
) -> gents_automation::goal::GoalDocument {
    let mut updates = node.subscribe_updates();
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = source.next_fire() => panic!("unexpected goal fire while waiting for {description}"),
            goal = async {
                loop {
                    let goal = load_goal_as(access, node_did).await.expect("Goal exists");
                    if reached(&goal) { break goal; }
                    updates.recv().await.expect("native update subscription closed");
                }
            } => goal,
        }
    }).await.unwrap_or_else(|_| panic!("goal transition did not complete: {description}"))
}

async fn set_request_state_as(access: &ConfigAccess, doc: &str, state: &str) {
    let response = access.write("test.goal_request_state", &format!(
        r#"mutation {{update_AgentRequest(docID: "{}", input: {{lifecycle_state: "{}"}}) {{_docID}}}}"#,
        escape_graphql_string(doc), escape_graphql_string(state),
    )).await.unwrap();
    assert_eq!(
        response["data"]["update_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn load_history_as(
    access: &ConfigAccess,
    node_did: &str,
    requester_did: &str,
    session_id: &str,
) -> Vec<gents_inference::llm::message::Message> {
    let node = escape_graphql_string(node_did);
    let requester = escape_graphql_string(requester_did);
    let session = escape_graphql_string(session_id);
    let response = access.execute(&format!(r#"{{AgentMessage(filter: {{node_did: {{_eq: "{node}"}}, requester_did: {{_eq: "{requester}"}}, session_id: {{_eq: "{session}"}}}}, order: {{sequence: ASC}}) {{_docID}}}}"#)).await.unwrap();
    let mut messages = Vec::new();
    for row in response["data"]["AgentMessage"].as_array().unwrap() {
        let doc = row["_docID"].as_str().unwrap();
        let (_, message) = gents_session::session::load_canonical_message(
            access,
            doc,
            node_did,
            Some(requester_did),
        )
        .await
        .unwrap();
        messages.push(message);
    }
    messages
}

async fn fail_request_with_usage(
    access: &ConfigAccess,
    agent: &Arc<dyn NodeIdentity>,
    node_did: &str,
    request_id: &str,
    request_doc: &str,
    terminal: &str,
) {
    access.transact_write("test.goal_failure_and_budget_usage", |txn| {
        Box::pin(async move {
            let request = txn.execute(&format!(r#"{{AgentRequest(docID: "{}") {{session_doc_id}}}}"#, escape_graphql_string(request_doc))).await?;
            let session_doc = request["data"]["AgentRequest"][0]["session_doc_id"].as_str().expect("physical session");
            let changed = txn.execute(&format!(r#"mutation {{update_AgentRequest(docID: "{}", input: {{lifecycle_state: "{}"}}) {{_docID}}}}"#, escape_graphql_string(request_doc), escape_graphql_string(terminal))).await?;
            anyhow::ensure!(changed["data"]["update_AgentRequest"].as_array().is_some_and(|rows| rows.len()==1), "parent was not terminalized");
            let input = serde_json::json!({"call_id":"over-budget-failed-call", "node_did":node_did,
                "agent_did":agent.did(), "session_id":SESSION, "request_id":request_id, "request_doc_id":request_doc,
                "backend_id":"goal-background-backend", "call_seq":1, "attempt":1, "call_kind":"inference",
                "call_state":"failed", "failure_reason":"provider stream interrupted after reporting usage",
                "prompt_tokens":100, "completion_tokens":5});
            let created = txn.execute_with_variables("mutation($input: InferenceCallMutationInputArg!) {add_InferenceCall(input:$input) {_docID}}", &serde_json::json!({"input":input})).await?;
            let doc = gents_store::graphql::created_doc_id(&created, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
}
