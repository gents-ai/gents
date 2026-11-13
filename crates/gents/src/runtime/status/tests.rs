use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::mpsc;

use gents_protocol::node_readiness::AgentReadinessUnavailableReason;

use super::*;
use gents_lean_vocab::{
    lean_process_transition_cases, lean_runtime_reconcile_case, LeanLifecycleTransitionCase,
};

#[derive(Debug, Deserialize)]
struct NodeRuntimeRow {
    reconcile_phase: String,
    agent_executor_capacity: i64,
    agent_executor_queue_depth: i64,
    agent_executor_status_json: String,
    last_reconcile_result: String,
    last_reconcile_completed_at: String,
}

#[derive(Debug, Deserialize)]
struct NodeReadinessRow {
    snapshot_json: String,
    updated_at: String,
}

async fn test_node() -> gents_inference::test_support::TestDb {
    let db = gents_inference::test_support::test_db(
        "status",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let did = db.node_identity.did();
    let access = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(did).unwrap(),
    );
    access
        .write(
            "test.status.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                escape_graphql_string(did),
            ),
        )
        .await
        .unwrap();
    db
}

#[tokio::test]
async fn runtime_status_publication_preserves_native_ownership_and_revocation() -> anyhow::Result<()>
{
    use gents_lean_vocab::{lean_identity_household_cases, LeanIdentityAction};
    use gents_store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
    use gents_store::identity::NodeIdentity;

    let model = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "operations-parent-publication")
        .context("missing operations model")?;
    let expected: Vec<_> = model
        .actions
        .iter()
        .filter_map(|action| match action {
            LeanIdentityAction::Check { expected, .. } => Some(*expected),
            _ => None,
        })
        .collect();
    let dir = tempfile::tempdir()?;
    let user = gents_store::identity::KeyIdentity::load_or_create(dir.path().join("user"), None)?;
    let runtime =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("runtime"), None)?;
    let agent = gents_store::identity::KeyIdentity::load_or_create(dir.path().join("agent"), None)?;
    let sibling =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
    let outsider =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("outsider"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(dir.path().join("node"))
            .build()
            .await?,
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection(gents_protocol::schemas::NODE).await?;
    node.add_collection(gents_protocol::schemas::NODE_RUNTIME)
        .await?;
    let access = |actor: &gents_store::identity::KeyIdentity| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        ))
    };
    let user_access = access(&user)?;
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    for actor in [&user, &agent, &sibling, &outsider] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            runtime_access
                .grant_node_permission(&identity::Did::new(actor.did())?, permission)
                .await?;
        }
    }
    // Public relationship routes require `get-collection`, which production
    // Users hold (`GrantSets.operatorUser`).
    runtime_access
        .grant_node_permission(
            &identity::Did::new(user.did())?,
            NodePermission::CollectionGet,
        )
        .await?;
    let parent = user_access
        .write(
            "test.status.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                escape_graphql_string(runtime.did()),
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &parent,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let mut row = RuntimeStatusRow::new(runtime.did().to_owned());
    upsert_runtime_status(&node, &row).await?;
    let visible = |response: &serde_json::Value| {
        !gents_protocol::graphql::graphql_rows_from_response(response, "NodeRuntime").is_empty()
    };
    let query = "{ NodeRuntime { _docID reconcile_phase } }";
    let user_view = user_access.execute(query).await?;
    let doc = user_view["data"]["NodeRuntime"][0]["_docID"]
        .as_str()
        .context("status not visible to Node owner")?;
    let update = format!(
        r#"mutation {{ update_NodeRuntime(docID: "{}", input: {{reconcile_phase: "applying"}}) {{_docID}} }}"#,
        escape_graphql_string(doc)
    );
    let user_updated = user_access.write("test.status.denied", &update).await;
    let runtime_updated = runtime_access.write("test.status.update", &update).await;
    let affected = |result: anyhow::Result<serde_json::Value>| {
        result.is_ok_and(|response| {
            gents_protocol::graphql::extract_mutation_doc_id(&response, "NodeRuntime").is_ok()
        })
    };
    let actual = [
        visible(&user_view),
        affected(user_updated.map_err(Into::into)),
        affected(runtime_updated.map_err(Into::into)),
        visible(&access(&agent)?.execute(query).await?),
        visible(&access(&sibling)?.execute(query).await?),
        affected(
            access(&outsider)?
                .write("test.status.outsider", &update)
                .await
                .map_err(Into::into),
        ),
    ];
    assert_eq!(
        actual,
        [
            expected[2],
            expected[4],
            expected[5],
            expected[6],
            expected[7],
            expected[8]
        ]
    );
    runtime_access
        .delete_resource_relationship("NodeRuntime", doc, "parent", "nodes", &parent)
        .await?;
    row.reconcile_phase = "idle".to_owned();
    let republished = upsert_runtime_status(&node, &row).await;
    assert_eq!(republished.is_ok(), expected[10]);
    republished?;
    assert_eq!(visible(&user_access.execute(query).await?), expected[9]);
    drop(user_access);
    drop(runtime_access);
    node.shutdown().await?;
    drop(node);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let restarted = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(dir.path().join("node"))
            .build()
            .await?,
    );
    let user_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?,
    );
    assert!(!visible(&user_access.execute(query).await?));
    row.reconcile_phase = "applying".into();
    upsert_runtime_status(&restarted, &row).await?;
    assert!(!visible(&user_access.execute(query).await?));
    restarted.shutdown().await?;
    Ok(())
}

fn status_test_request(request_id: &str) -> gents_protocol::row::AgentRequest {
    gents_protocol::row::AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: format!("{request_id}-doc"),
        request_id: request_id.to_string(),
        node_did: "did:test:status-test".to_string(),
        requester_did: None,
        agent_did: "general".to_string(),
        session_id: format!("{request_id}-session"),
        content: "status test".to_string(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: Some("interactive".to_string()),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_authority: None,
        workspace_owner_node_did: None,
        workspace_seal_hash: None,
        executed_model_name: None,
        executed_reasoning_effort: None,
        executed_service_tier: None,
        executed_inference_profile_id: None,
        executed_runtime_generation: None,
    }
}

fn unavailable_general(
) -> RapidHashMap<String, gents_automation::triggers::runtime_snapshot::UnavailableAgent> {
    RapidHashMap::from_iter([(
        "general".to_string(),
        gents_automation::triggers::runtime_snapshot::UnavailableAgent::new(
            AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
            "test runtime is not configured",
        ),
    )])
}

async fn fetch_runtime_row(node: &Arc<defra_node::Node>, node_did: &str) -> NodeRuntimeRow {
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeRuntime(filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }}, limit: 1) {{
                reconcile_phase
                agent_executor_capacity
                agent_executor_queue_depth
                agent_executor_status_json
                last_reconcile_result
                last_reconcile_completed_at
            }}
        }}"#
    );
    let actor = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
    let response = gents_store::config_client::ConfigAccess::local_as(node.clone(), actor)
        .execute(&query)
        .await
        .unwrap();
    let value = response
        .get("data")
        .and_then(|data| data.get("NodeRuntime"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("NodeRuntime row");
    serde_json::from_value(value).expect("decode NodeRuntime row")
}

async fn fetch_node_readiness_row(
    node: &Arc<defra_node::Node>,
    node_did: &str,
) -> NodeReadinessRow {
    let escaped_node_did = escape_graphql_string(node_did);
    let actor = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
    let response = gents_store::config_client::ConfigAccess::local_as(node.clone(), actor)
        .execute(&format!(
            r#"{{
                NodeReadiness(
                    filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                    limit: 1
                ) {{ snapshot_json updated_at }}
            }}"#
        ))
        .await
        .unwrap();
    let value = response
        .get("data")
        .and_then(|data| data.get("NodeReadiness"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("NodeReadiness row");
    serde_json::from_value(value).expect("decode NodeReadiness row")
}

async fn read_node_readiness_row(
    node: &Arc<defra_node::Node>,
    node_did: &str,
) -> gents_protocol::node_readiness::NodeReadinessRow {
    let row = fetch_node_readiness_row(node, node_did).await;
    gents_protocol::node_readiness::NodeReadinessRow {
        node_did: node_did.to_owned(),
        snapshot_json: row.snapshot_json,
        updated_at: row.updated_at,
    }
}

#[tokio::test]
async fn restart_publishes_fail_closed_readiness_before_other_runtime_work() {
    let db = test_node().await;
    let node = db.node.clone();
    let node_did = node.node_identity_did().unwrap();

    let (first_owner, first) = RuntimeStatusHandle::start(
        node.clone(),
        node_did,
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );
    first.initialize_startup("general").await.unwrap();
    first
        .set_process_state_durable(ProcessLifecycleState::Ready)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<gents_protocol::node_readiness::NodeReadinessSnapshot>(
            &fetch_node_readiness_row(&node, node_did)
                .await
                .snapshot_json
        )
        .unwrap()
        .process_state,
        gents_protocol::node_readiness::NodeReadinessProcessState::Ready
    );
    first_owner.close().await.unwrap();

    let (second_owner, second) = RuntimeStatusHandle::start(
        node.clone(),
        node_did,
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );
    second.initialize_startup("general").await.unwrap();
    let restarted = serde_json::from_str::<gents_protocol::node_readiness::NodeReadinessSnapshot>(
        &fetch_node_readiness_row(&node, node_did)
            .await
            .snapshot_json,
    )
    .unwrap();
    assert_eq!(
        restarted.process_state,
        gents_protocol::node_readiness::NodeReadinessProcessState::Recovering,
        "a prior durable Ready must be overwritten before restart work can fail"
    );
    second_owner.close().await.unwrap();
}

fn status_test_snapshot(
    generation: u64,
    dispatcher: mpsc::Sender<gents_protocol::row::AgentRequest>,
) -> ActiveRuntimeSnapshot {
    ActiveRuntimeSnapshot {
        generation,
        node: None,
        local_did: String::new(),
        default_agent_did: "general".to_string(),
        agents: RapidHashMap::new(),
        tool_configs: RapidHashMap::new(),
        tool_surfaces: RapidHashMap::new(),
        backend_admission_configs: RapidHashMap::new(),
        unavailable_agents: RapidHashMap::new(),
        active_schedules: RapidHashMap::new(),
        unavailable_schedules: RapidHashSet::new(),
        active_event_triggers: RapidHashMap::new(),
        unavailable_event_triggers: RapidHashSet::new(),
        active_tasks: RapidHashMap::new(),
        dispatchers: RapidHashMap::from_iter([("general".to_string(), dispatcher)]),
        agent_executor_capacities: RapidHashMap::from_iter([("general".to_string(), 1)]),
        agent_executor_queue_capacities: RapidHashMap::from_iter([("general".to_string(), 1)]),
    }
}

#[tokio::test]
async fn startup_claim_demotes_a_killed_process_ready_row_before_any_runtime_work() {
    let db = test_node().await;
    let node = db.node.clone();
    let node_did = node.node_identity_did().unwrap().to_owned();
    let claim = gents_lean_vocab::families::client_agent_readiness_restart_claim::cases::<
        gents_protocol::node_readiness::AgentReadinessUnavailableReason,
        gents_protocol::node_readiness::NodeReadinessProcessState,
    >()
    .into_iter()
    .find(|claim| claim.input.name == "restart_claim_demotes_stale_ready")
    .expect("modeled restart claim case");
    let case = &claim.input;
    let (dispatcher, _requests) = mpsc::channel(1);

    let (killed_owner, killed) = RuntimeStatusHandle::start(
        node.clone(),
        node_did.clone(),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );
    killed.initialize_startup("general").await.unwrap();
    killed
        .publish_startup_snapshot(&status_test_snapshot(case.active_generation, dispatcher))
        .await
        .unwrap();
    killed
        .publish_router_generation(case.router_generation)
        .await
        .unwrap();
    killed
        .set_process_state_durable(ProcessLifecycleState::Ready)
        .await
        .unwrap();
    drop(killed_owner);
    drop(killed);

    let row = read_node_readiness_row(&node, &node_did).await;
    assert!(
        !matches!(
            gents_protocol::node_readiness::project_node_readiness_summary(Some(&row), &node_did),
            gents_protocol::node_readiness::ProjectedNodeReadinessSummary::Unknown(_)
        ),
        "the killed process must leave a row every reader still projects as current"
    );

    crate::runtime::startup_readiness::claim_startup_readiness(&node, &node_did)
        .await
        .unwrap();

    let claimed = read_node_readiness_row(&node, &node_did).await;
    let (state, reason) = match gents_protocol::node_readiness::project_node_readiness_summary(
        Some(&claimed),
        &node_did,
    ) {
        gents_protocol::node_readiness::ProjectedNodeReadinessSummary::Observed(_) => {
            ("observed", None)
        }
        gents_protocol::node_readiness::ProjectedNodeReadinessSummary::Unknown(reason) => (
            "unknown",
            serde_json::to_value(reason)
                .unwrap()
                .as_str()
                .map(str::to_owned),
        ),
    };
    claim.check(
        gents_lean_vocab::families::client_agent_readiness_restart_claim::ClaimedRowOutput {
            state: state.to_owned(),
            reason,
        },
    );
    let snapshot =
        gents_protocol::node_readiness::decode_node_readiness_snapshot(&claimed, &node_did)
            .unwrap();
    assert_eq!(snapshot.process_state, case.process_state);
    assert_eq!(snapshot.active_generation, case.active_generation);
    assert_eq!(snapshot.router_generation, case.router_generation);
    assert_eq!(snapshot.default_agent_did, "general");
    assert!(
        snapshot
            .agents
            .iter()
            .any(|entry| entry.agent_did == "general"),
        "the claim keeps the predecessor's agent picture readable for diagnosis: {snapshot:?}"
    );
}

fn rust_process_transition_action(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("uninitialized", "recovering") => Some("startupRecover"),
        ("uninitialized", "ready") => Some("startupClean"),
        ("recovering", "ready") => Some("recoveryComplete"),
        ("ready", "shuttingDown") => Some("beginShutdown"),
        ("shuttingDown", "shutdown") => Some("finishShutdown"),
        (from, "recovering") if from != "shutdown" => Some("restartClaim"),
        _ => None,
    }
}

fn process_state_from_contract(state: &str) -> ProcessLifecycleState {
    match state {
        "uninitialized" => ProcessLifecycleState::Uninitialized,
        "recovering" => ProcessLifecycleState::Recovering,
        "ready" => ProcessLifecycleState::Ready,
        "shuttingDown" => ProcessLifecycleState::ShuttingDown,
        "shutdown" => ProcessLifecycleState::Shutdown,
        other => panic!("unknown generated Process state {other:?}"),
    }
}

async fn drive_generated_process_legal_case(case: &LeanLifecycleTransitionCase) {
    let db = test_node().await;
    let node = db.node.clone();
    let node_did = node.node_identity_did().unwrap().to_owned();
    let (owner, status) = RuntimeStatusHandle::start(
        node.clone(),
        node_did.clone(),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );
    status.readiness().initialize("general").await.unwrap();
    let action = case
        .action
        .as_deref()
        .expect("legal Process transition case must carry an action");

    match action {
        "startupRecover" | "startupClean" => {
            status
                .set_process_state(process_state_from_contract(&case.to))
                .await;
        }
        "recoveryComplete" => {
            status
                .set_process_state(ProcessLifecycleState::Recovering)
                .await;
            status.set_process_state(ProcessLifecycleState::Ready).await;
        }
        "beginShutdown" => {
            status.set_process_state(ProcessLifecycleState::Ready).await;
            status
                .set_process_state(ProcessLifecycleState::ShuttingDown)
                .await;
        }
        "finishShutdown" => {
            status
                .set_process_state(ProcessLifecycleState::ShuttingDown)
                .await;
            status
                .set_process_state(ProcessLifecycleState::Shutdown)
                .await;
        }
        "restartClaim" => {
            match case.from.as_str() {
                "recovering" => {
                    status
                        .set_process_state(ProcessLifecycleState::Recovering)
                        .await;
                }
                "ready" => {
                    status.set_process_state(ProcessLifecycleState::Ready).await;
                }
                "shuttingDown" => {
                    status.set_process_state(ProcessLifecycleState::Ready).await;
                    status
                        .set_process_state(ProcessLifecycleState::ShuttingDown)
                        .await;
                }
                other => panic!(
                    "generated Process transition {} claims from unsupported state {other:?}",
                    case.name
                ),
            }
            crate::runtime::startup_readiness::claim_startup_readiness(&node, &node_did)
                .await
                .unwrap();
        }
        other => panic!(
            "generated Process transition {} has unsupported action {other:?}",
            case.name
        ),
    }

    let readiness = serde_json::from_str::<gents_protocol::node_readiness::NodeReadinessSnapshot>(
        &fetch_node_readiness_row(&node, &node_did)
            .await
            .snapshot_json,
    )
    .expect("decode process readiness");
    assert_eq!(
        readiness.process_state.as_str(),
        case.to,
        "generated Process transition {} expected {} -> {} classified as {} via {:?}, got authoritative process_state={}",
        case.name,
        case.from,
        case.to,
        case.classification,
        case.action,
        readiness.process_state.as_str()
    );
    owner.close().await.unwrap();
}

#[tokio::test]
async fn generated_process_transition_cases_match_runtime_status_policy() {
    let mut legal_count = 0;
    let mut illegal_count = 0;

    for case in lean_process_transition_cases() {
        let rust_classification = if rust_process_transition_action(&case.from, &case.to).is_some()
        {
            "legal"
        } else {
            "illegal"
        };
        assert_eq!(
            case.classification, rust_classification,
            "Process transition {} expected classification drift for {} -> {}; Lean action={:?} boundary={:?}",
            case.name, case.from, case.to, case.action, case.boundary
        );

        match case.classification.as_str() {
            "legal" => {
                legal_count += 1;
                assert_eq!(
                    case.action.as_deref(),
                    rust_process_transition_action(&case.from, &case.to),
                    "Process transition {} legal writer action drifted for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                drive_generated_process_legal_case(case).await;
            }
            "illegal" => {
                illegal_count += 1;
                assert!(
                    rust_process_transition_action(&case.from, &case.to).is_none(),
                    "Process transition {} is ordinary illegal but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            other => panic!(
                "generated Process transition {} has unknown classification {other:?}",
                case.name
            ),
        }
    }

    assert!(legal_count > 0);
    assert!(illegal_count > 0);
    assert_eq!(
        legal_count + illegal_count,
        lean_process_transition_cases().len()
    );
}

#[tokio::test]
async fn runtime_status_persists_agent_executor_capacity_and_queue_depth() {
    let db = test_node().await;
    let node = db.node.clone();

    let (tx, _rx) = mpsc::channel(4);
    tx.try_send(status_test_request("queued-1")).unwrap();
    tx.try_send(status_test_request("queued-2")).unwrap();

    let status = RuntimeStatusHandle::new(node.clone(), node.node_identity_did().unwrap());
    status
        .publish_startup_snapshot(&ActiveRuntimeSnapshot {
            generation: 1,
            node: None,
            local_did: String::new(),
            default_agent_did: "general".to_string(),
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
            dispatchers: RapidHashMap::from_iter([("general".to_string(), tx)]),
            agent_executor_capacities: RapidHashMap::from_iter([("general".to_string(), 3)]),
            agent_executor_queue_capacities: RapidHashMap::from_iter([("general".to_string(), 4)]),
        })
        .await
        .unwrap();

    let row = fetch_runtime_row(&node, node.node_identity_did().unwrap()).await;
    assert_eq!(row.agent_executor_capacity, 3);
    assert_eq!(row.agent_executor_queue_depth, 2);

    let executor_status: serde_json::Value =
        serde_json::from_str(&row.agent_executor_status_json).unwrap();
    assert_eq!(
        executor_status,
        serde_json::json!({
            "general": {
                "worker_capacity": 3,
                "queue_depth": 2,
                "queue_capacity": 4
            }
        })
    );
}

#[tokio::test]
async fn executor_metrics_do_not_republish_unchanged_node_readiness() {
    let db = test_node().await;
    let node = db.node.clone();

    let (tx, _rx) = mpsc::channel(2);
    let snapshot = ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: String::new(),
        default_agent_did: "general".to_string(),
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
        dispatchers: RapidHashMap::from_iter([("general".to_string(), tx.clone())]),
        agent_executor_capacities: RapidHashMap::from_iter([("general".to_string(), 1)]),
        agent_executor_queue_capacities: RapidHashMap::from_iter([("general".to_string(), 2)]),
    };
    let node_did = node.node_identity_did().unwrap();
    let status = RuntimeStatusHandle::new(node.clone(), node_did);
    status.publish_startup_snapshot(&snapshot).await.unwrap();
    let before = fetch_node_readiness_row(&node, node_did).await;

    tx.try_send(status_test_request("metric-only-change"))
        .unwrap();
    status.publish_executor_snapshot(&snapshot).await;
    let after = fetch_node_readiness_row(&node, node_did).await;

    assert_eq!(after.snapshot_json, before.snapshot_json);
    assert_eq!(
        after.updated_at, before.updated_at,
        "executor polling must not become an incidental readiness writer"
    );
}

#[tokio::test]
async fn runtime_status_serializes_persisted_generation_updates() {
    let db = test_node().await;
    let node = db.node.clone();

    let status = RuntimeStatusHandle::new(node.clone(), node.node_identity_did().unwrap());
    let startup = ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: String::new(),
        default_agent_did: "general".to_string(),
        tool_configs: Default::default(),
        agents: RapidHashMap::new(),
        tool_surfaces: RapidHashMap::new(),
        backend_admission_configs: RapidHashMap::new(),
        unavailable_agents: unavailable_general(),
        active_schedules: RapidHashMap::new(),
        unavailable_schedules: RapidHashSet::new(),
        active_event_triggers: RapidHashMap::new(),
        unavailable_event_triggers: RapidHashSet::new(),
        active_tasks: RapidHashMap::new(),
        dispatchers: RapidHashMap::new(),
        agent_executor_capacities: RapidHashMap::new(),
        agent_executor_queue_capacities: RapidHashMap::new(),
    };
    let applied = ActiveRuntimeSnapshot {
        generation: 2,
        node: None,
        local_did: String::new(),
        default_agent_did: "general".to_string(),
        tool_configs: Default::default(),
        agents: RapidHashMap::new(),
        tool_surfaces: RapidHashMap::new(),
        backend_admission_configs: RapidHashMap::new(),
        unavailable_agents: unavailable_general(),
        active_schedules: RapidHashMap::new(),
        unavailable_schedules: RapidHashSet::new(),
        active_event_triggers: RapidHashMap::new(),
        unavailable_event_triggers: RapidHashSet::new(),
        active_tasks: RapidHashMap::new(),
        dispatchers: RapidHashMap::new(),
        agent_executor_capacities: RapidHashMap::new(),
        agent_executor_queue_capacities: RapidHashMap::new(),
    };

    status.publish_startup_snapshot(&startup).await.unwrap();
    let status_for_snapshot = status.clone();
    let status_for_router = status.clone();
    let publish_snapshot = tokio::spawn(async move {
        status_for_snapshot.publish_applied(&applied).await;
    });
    let publish_router = tokio::spawn(async move {
        status_for_router
            .publish_router_generation(2)
            .await
            .unwrap();
    });
    publish_snapshot.await.unwrap();
    publish_router.await.unwrap();

    let row = fetch_runtime_row(&node, node.node_identity_did().unwrap()).await;
    assert_eq!(row.last_reconcile_result, "applied");
    assert!(
        !row.last_reconcile_completed_at.is_empty(),
        "applied publish must stamp last_reconcile_completed_at"
    );
    let readiness = fetch_node_readiness_row(&node, node.node_identity_did().unwrap()).await;
    let readiness: gents_protocol::node_readiness::NodeReadinessSnapshot =
        serde_json::from_str(&readiness.snapshot_json).expect("decode serialized readiness");
    assert_eq!(readiness.active_generation, 2);
    assert_eq!(readiness.router_generation, 2);
}

#[tokio::test]
async fn runtime_status_generation_updates_match_lean_runtime_reconcile_cases() {
    let publish = lean_runtime_reconcile_case("publish_changed_snapshot");
    let router = lean_runtime_reconcile_case("router_observe_published_generation");
    assert!(publish.legal);
    assert!(router.legal);

    let db = test_node().await;
    let node = db.node.clone();

    let status = RuntimeStatusHandle::new(node.clone(), node.node_identity_did().unwrap());
    let startup = ActiveRuntimeSnapshot {
        generation: publish.pre_active_generation as u64,
        node: None,
        local_did: String::new(),
        default_agent_did: "general".to_string(),
        tool_configs: Default::default(),
        agents: RapidHashMap::new(),
        tool_surfaces: RapidHashMap::new(),
        backend_admission_configs: RapidHashMap::new(),
        unavailable_agents: unavailable_general(),
        active_schedules: RapidHashMap::new(),
        unavailable_schedules: RapidHashSet::new(),
        active_event_triggers: RapidHashMap::new(),
        unavailable_event_triggers: RapidHashSet::new(),
        active_tasks: RapidHashMap::new(),
        dispatchers: RapidHashMap::new(),
        agent_executor_capacities: RapidHashMap::new(),
        agent_executor_queue_capacities: RapidHashMap::new(),
    };
    let applied = ActiveRuntimeSnapshot {
        generation: publish.post_active_generation as u64,
        node: None,
        local_did: String::new(),
        default_agent_did: "general".to_string(),
        tool_configs: Default::default(),
        agents: RapidHashMap::new(),
        tool_surfaces: RapidHashMap::new(),
        backend_admission_configs: RapidHashMap::new(),
        unavailable_agents: unavailable_general(),
        active_schedules: RapidHashMap::new(),
        unavailable_schedules: RapidHashSet::new(),
        active_event_triggers: RapidHashMap::new(),
        unavailable_event_triggers: RapidHashSet::new(),
        active_tasks: RapidHashMap::new(),
        dispatchers: RapidHashMap::new(),
        agent_executor_capacities: RapidHashMap::new(),
        agent_executor_queue_capacities: RapidHashMap::new(),
    };

    status.publish_startup_snapshot(&startup).await.unwrap();
    status
        .publish_router_generation(publish.pre_router_generation as u64)
        .await
        .unwrap();
    status.set_reconcile_phase(ReconcilePhase::Applying).await;
    status.publish_applied(&applied).await;
    let row = fetch_runtime_row(&node, node.node_identity_did().unwrap()).await;
    assert_eq!(row.reconcile_phase, publish.post_phase.as_str());
    assert_eq!(row.last_reconcile_result, "applied");
    let readiness = serde_json::from_str::<gents_protocol::node_readiness::NodeReadinessSnapshot>(
        &fetch_node_readiness_row(&node, node.node_identity_did().unwrap())
            .await
            .snapshot_json,
    )
    .expect("decode published runtime contract readiness");
    assert_eq!(
        readiness.active_generation,
        publish.post_active_generation as u64
    );
    assert_eq!(
        readiness.router_generation,
        publish.post_router_generation as u64
    );

    status
        .publish_router_generation(router.post_router_generation as u64)
        .await
        .unwrap();
    let row = fetch_runtime_row(&node, node.node_identity_did().unwrap()).await;
    assert_eq!(row.reconcile_phase, router.post_phase.as_str());
    let readiness = serde_json::from_str::<gents_protocol::node_readiness::NodeReadinessSnapshot>(
        &fetch_node_readiness_row(&node, node.node_identity_did().unwrap())
            .await
            .snapshot_json,
    )
    .expect("decode router-observed readiness");
    assert_eq!(
        readiness.active_generation,
        router.post_active_generation as u64
    );
    assert_eq!(
        readiness.router_generation,
        router.post_router_generation as u64
    );
}

#[tokio::test]
async fn goal_settled_uses_live_runtime_status_cases() {
    let db = test_node().await;
    let node = db.node.clone();
    let node_did = db.node_identity.did().to_owned();
    let status = RuntimeStatusHandle::new(node, node_did);
    for case in gents_lean_vocab::families::identity_operations_readiness_settled::cases() {
        {
            let mut row = status.state.lock().await;
            row.reconcile_phase = if case.input.idle { "idle" } else { "applying" }.to_owned();
            row.last_reconcile_result = if !case.input.result_known {
                ""
            } else if case.input.result_error {
                "error"
            } else {
                "applied"
            }
            .to_owned();
        }
        let live = case.input.live_present.then_some(&status);
        case.check(gents_automation::goal::live_goal_reconcile_settled(live).await);
    }
}

#[tokio::test]
async fn storage_full_readiness_write_is_retained_as_a_process_local_fault() {
    use gents_protocol::node_readiness::{
        project_node_readiness_summary, project_node_readiness_summary_with_storage,
        AgentReadinessEntry, AgentReadinessState, NodeReadinessProcessState as ProcessState,
        NodeReadinessRow as ReadinessRow, NodeReadinessSnapshot, ProjectedNodeReadinessSummary,
        RuntimeStorageObservation, NODE_READINESS_FORMAT_VERSION,
    };

    #[derive(Debug, thiserror::Error)]
    #[error("native transaction")]
    struct NativeTransaction(#[source] storage::corekv::Error);

    struct StorageFullWriter;

    #[async_trait::async_trait]
    impl crate::runtime::node_readiness_publisher::NodeReadinessWriter for StorageFullWriter {
        async fn upsert(
            &self,
            _node_did: &str,
            _snapshot: &gents_protocol::row::NodeReadinessSnapshot,
            _updated_at: &str,
        ) -> anyhow::Result<()> {
            Err(anyhow::Error::new(NativeTransaction(
                storage::corekv::Error::from(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "write-ahead log left in an unknown state: No space left on device (os error 28)",
                )),
            )))
        }
    }

    let db = test_node().await;
    let node = db.node.clone();
    let node_did = node.node_identity_did().unwrap().to_owned();
    let (owner, status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        node_did.clone(),
        Arc::new(StorageFullWriter),
        std::time::Duration::from_millis(1),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );

    assert!(!status.storage_fault().is_unavailable());
    let error = status
        .initialize_startup("did:test:agent")
        .await
        .expect_err("a latched store cannot persist readiness");
    assert!(
        gents_store::graphql::is_storage_full(error.as_ref()),
        "{}",
        gents_protocol::error_chain::ErrorChain(error.as_ref()),
    );
    assert!(
        status.storage_fault().is_unavailable(),
        "the write that cannot be persisted must still be observable"
    );

    let stale_ready = ReadinessRow {
        node_did: node_did.clone(),
        snapshot_json: serde_json::to_string(&NodeReadinessSnapshot {
            format_version: NODE_READINESS_FORMAT_VERSION,
            process_state: ProcessState::Ready,
            active_generation: 1,
            router_generation: 1,
            default_agent_did: "did:test:agent".to_string(),
            agents: vec![AgentReadinessEntry {
                effective_inference: None,

                agent_did: "did:test:agent".to_string(),
                state: AgentReadinessState::Ready,
                reason: None,
            }],
        })
        .unwrap(),
        updated_at: "2026-11-12T00:00:00Z".to_string(),
    };
    assert!(
        matches!(
            project_node_readiness_summary(Some(&stale_ready), &node_did),
            ProjectedNodeReadinessSummary::Observed(_)
        ),
        "the durable row alone cannot carry the fault"
    );
    assert_eq!(
        project_node_readiness_summary_with_storage(
            Some(&stale_ready),
            &node_did,
            status.storage_fault().observation(),
        ),
        ProjectedNodeReadinessSummary::Unknown(
            gents_protocol::row::AgentReadinessUnknownReason::StorageUnavailable
        ),
        "the reader must join the process-local fault with the frozen row"
    );
    assert_eq!(
        status.storage_fault().observation(),
        RuntimeStorageObservation::Unavailable
    );

    owner.close().await.ok();
    node.shutdown().await.unwrap();
}
