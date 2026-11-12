use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::goal::{
    session_token_usage, set_goal_from_access, CreateGoalForSessionError, CreateGoalForSessionOutcome,
};
use crate::{
    triggers::engine::goal_source::GoalSource, triggers::engine::TriggerSource,
    triggers::runtime_snapshot::ActiveRuntimeSnapshot,
};
use defra_node::Node as EmbeddedNode;
use events::{
    Bus, ChannelBus, DocumentChangeSubscription, EventName, Message, Subscription, Update,
};
use gents_inference::backends::provider::BackendProviderKind;
use gents_inference::resolved::ResolvedAgent;
use gents_inference::test_support::TestDb;
use gents_protocol::goal_status::GoalStatus;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::RuntimeNode;
use gents_store::subscription_source::UpdateSubscriptionSource;
use gents_store::{config_client::ConfigAccess, identity::NodeIdentity};
use serde::Deserialize;
use tokio::sync::{watch, Notify};
use tokio_util::sync::CancellationToken;

async fn test_db(name: &str) -> TestDb {
    gents_inference::test_support::test_db(name, crate::lifecycle_hooks::composition()).await
}

async fn managed_agent(db: &TestDb) -> Arc<dyn NodeIdentity> {
    managed_agent_with_binding(db).await.0
}

async fn managed_agent_with_binding(
    db: &TestDb,
) -> (
    Arc<dyn NodeIdentity>,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let fixture = managed_agent_fixture(db).await;
    (fixture.agent, fixture.binding)
}

struct ManagedAgentFixture {
    pub user: gents_store::identity::KeyIdentity,
    pub access: gents_store::config_client::ConfigAccess,
    pub agent: Arc<dyn NodeIdentity>,
    pub binding: gents_protocol::agent_identity::AgentIdentityBinding,
}

async fn managed_fixture_user(db: &TestDb) -> gents_store::identity::KeyIdentity {
    let path = db.data_path().join("fixture-user.key");
    tokio::task::spawn_blocking(move || {
        gents_store::identity::KeyIdentity::load_or_create(path, None)
    })
    .await
    .expect("join held test user setup")
    .expect("create held test user")
}

async fn managed_agent_fixture(db: &TestDb) -> ManagedAgentFixture {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};

    let user = managed_fixture_user(db).await;
    let user_did = identity::Did::new(user.did()).expect("valid user DID");
    let runtime = db.node_identity.as_ref();
    assert_ne!(user.did(), runtime.did());
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(runtime.did()).expect("valid runtime DID"),
    );
    // An enrolled User's grants bound what its Agent creation may project.
    gents_store::identity::GrantSet::OperatorUser
        .grant(&admin, &user_did)
        .await
        .expect("grant test user operator access");
    let access = db.client_access(&user);
    let node = serde_json::json!({"node_did": runtime.did()});
    let node_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Node,
        add: node.clone(),
        update: node,
    }])
    .expect("render test Node");
    access
        .transact_write("test.managed_agent.node", |txn| {
            let plan = &node_plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .expect("install test Node as user");
    let (agent, binding) = install_managed_agent(db, &access, &user, "Test agent").await;
    ManagedAgentFixture {
        user,
        access,
        agent,
        binding,
    }
}

async fn install_managed_agent(
    db: &TestDb,
    access: &gents_store::config_client::ConfigAccess,
    user: &gents_store::identity::KeyIdentity,
    display_name: &str,
) -> (
    Arc<dyn NodeIdentity>,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let runtime = db.node_identity.as_ref();
    let user_did = identity::Did::new(user.did()).expect("valid user DID");
    let binding =
        gents_store::identity::provision_agent_identity_authorized(&db.node, runtime, &user_did)
            .await
            .expect("provision managed test Agent");
    let agent = gents_store::identity::resolve_agent_identity(runtime, &binding)
        .expect("resolve managed test Agent");
    let document = serde_json::json!({
        "node_did": runtime.did(),
        "agent_did": binding.agent_did,
        "creation_nonce": binding.creation_nonce,
        "display_name": display_name,
        "inference_profile_id": format!("{}:inference", binding.agent_did),
        "enabled": true,
    });
    let backend_id = format!("{}:backend", binding.agent_did);
    let profile_id = format!("{}:inference", binding.agent_did);
    let backend = serde_json::json!({
        "node_did": runtime.did(),
        "backend_id": backend_id,
        "name": "Test inference",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind": "unauthenticated"},
    });
    let profile = serde_json::json!({
        "node_did": runtime.did(),
        "profile_id": profile_id,
        "backend_id": backend_id,
        "model_name": "test-model",
    });
    let agent_plan = DesiredStateApplyPlan::new(
        [
            (
                gents_store::collection::Collection::InferenceBackend,
                backend,
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                profile,
            ),
            (gents_store::collection::Collection::Agent, document),
        ]
        .into_iter()
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect(),
    )
    .expect("render test Agent");
    access
        .transact_write("test.managed_agent", |txn| {
            let plan = &agent_plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .expect("install managed test Agent as user");
    (agent, binding)
}

fn test_node_for(
    identity: Arc<dyn gents_store::identity::NodeIdentity>,
    default_agent_did: impl Into<String>,
) -> Arc<RuntimeNode> {
    Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_did: default_agent_did.into(),
        display_name: None,
        enabled: true,
    })
}

fn test_agent_with_identity(
    principal: Arc<RuntimeNode>,
    agent_identity: Arc<dyn gents_store::identity::NodeIdentity>,
) -> ResolvedAgent {
    let agent_did = agent_identity.did().to_owned();
    ResolvedAgent {
        configuration: gents_inference::resolved::ResolvedConfiguration::Static,
        profile_id: None,
        skills: Vec::new(),
        oauth_pool: Vec::new(),
        agent_identity,
        agent_did,
        node: principal,
        backend_id: None,
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        claude_platform: None,
        backend_auth: gents_documents::BackendAuth::Unauthenticated,
        model_name: gents_inference::resolved::DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: gents_inference::resolved::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents_inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents_inference::resolved::DEFAULT_MAX_TURNS,
        max_turns_provenance: gents_inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: gents_inference::resolved::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        provider_idle_timeout: Duration::from_secs(
            gents_inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry: gents_loop::completion_retry::CompletionRetryProfileFields::default(),
        sampling: gents_inference::resolved::SamplingConfig::default(),
    }
}

/// The verified signer of `cid`, read under document ACP as `reader`: a
/// private row's block is visible only to an actor its policy admits, so an
/// anonymous node read of it fails closed.
async fn verified_block_signer_as(node: &EmbeddedNode, reader: &str, cid: &str) -> String {
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(reader))
        .await
        .unwrap_or_else(|error| panic!("read {cid} as {reader}: {error:#}"));
    let block = defra_core::block::Block::from_dag_cbor(&block).expect("decode signed block");
    let signature =
        defra_core::block::Signature::from_dag_cbor(&signature).expect("decode block signature");
    db::block::verify::verified_signature_signer_did(&block, &signature)
        .unwrap_or_else(|error| panic!("verify {cid}: {error}"))
}

#[derive(Clone)]
struct MockUpdateSubscriptionSource {
    bus: Arc<ChannelBus>,
    subscriber_count: Arc<AtomicUsize>,
    subscriber_notify: Arc<Notify>,
}

impl MockUpdateSubscriptionSource {
    pub fn new() -> Self {
        Self {
            bus: Arc::new(ChannelBus::new()),
            subscriber_count: Arc::new(AtomicUsize::new(0)),
            subscriber_notify: Arc::new(Notify::new()),
        }
    }

    pub fn close(&self) {
        self.bus.close();
    }

    pub fn publish_update(&self, collection_id: impl Into<String>, doc_id: impl Into<String>) {
        let collection_id = collection_id.into();
        let doc_id = doc_id.into();
        let block = format!("{collection_id}:{doc_id}").into_bytes();
        let cid = defra_core::block::generate_cid_from_bytes(&block)
            .expect("synthetic update block bytes must produce a CID");
        let update = Update::new(doc_id, cid, collection_id, block, false, true);
        self.bus.publish(Message::update(update));
    }
}

impl Default for MockUpdateSubscriptionSource {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateSubscriptionSource for MockUpdateSubscriptionSource {
    fn subscribe_updates(&self) -> Subscription {
        let subscription = self.bus.subscribe(&[EventName::Update]);
        self.subscriber_count.fetch_add(1, Ordering::SeqCst);
        self.subscriber_notify.notify_waiters();
        subscription
    }

    fn subscribe_collection_changes(
        &self,
        collection_ids: Vec<String>,
    ) -> DocumentChangeSubscription {
        let subscription = self.bus.subscribe_collection_changes(collection_ids);
        self.subscriber_count.fetch_add(1, Ordering::SeqCst);
        self.subscriber_notify.notify_waiters();
        subscription
    }
}

const SESSION: &str = "goal-session";

const RESCAN: Duration = Duration::from_millis(20);

async fn atomic_goal_link_case(actor: &'static str, collection: &'static str) -> serde_json::Value {
    tokio::task::spawn_blocking(move || {
        let contracts: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        contracts["goal_link_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| {
                case["actor"] == actor
                    && case["collection"] == collection
                    && case["mode"] == "fresh"
                    && case["commit"] == true
            })
            .unwrap()
            .clone()
    })
    .await
    .unwrap()
}

async fn goal_creation_actors(
    db: &TestDb,
) -> (gents_store::identity::KeyIdentity, Arc<dyn NodeIdentity>) {
    let fixture = managed_agent_fixture(db).await;
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

async fn goal_fixture_request(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    request_id: &str,
) -> String {
    goal_fixture_request_with_state(db, user, agent, request_id, "completed").await
}

async fn goal_fixture_usage(
    db: &TestDb,
    agent: &Arc<dyn NodeIdentity>,
    request_id: &str,
    request_doc: &str,
    call_id: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
) {
    goal_fixture_usage_for_node(
        &db.node,
        db.node_identity.did(),
        SESSION,
        agent,
        request_id,
        request_doc,
        call_id,
        prompt_tokens,
        completion_tokens,
        0,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn goal_fixture_usage_for_node(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    agent: &Arc<dyn NodeIdentity>,
    request_id: &str,
    request_doc: &str,
    call_id: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_input_tokens: i64,
) {
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    access.transact_write("test.goal_baseline_usage", |txn| {
        Box::pin(async move {
            let session = txn.execute(&format!(r#"{{AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                escape_graphql_string(node_did), escape_graphql_string(session_id))).await?;
            let session_doc = session["data"]["AgentSession"][0]["_docID"].as_str().expect("baseline session document");
            let input = serde_json::json!({
                "call_id": call_id, "runtime_instance_id": "goal-baseline-test", "request_id": request_id,
                "request_doc_id": request_doc, "session_id": session_id, "call_seq": 1,
                "backend_id": "backend-test", "agent_did": agent.did(), "node_did": node_did,
                "call_kind": "inference", "attempt": 1, "call_state": "completed",
                "queued_at": "2026-07-15T00:00:00Z", "started_at": "2026-07-15T00:00:00Z",
                "ended_at": "2026-07-15T00:00:01Z", "priority": 0, "queue_depth_at_enqueue": 0,
                "controller_generation": 1, "backend_config_fingerprint": "goal-baseline-test",
                "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "cached_input_tokens": cached_input_tokens,
            });
            let created = txn.execute_with_variables(
                "mutation($input: InferenceCallMutationInputArg!) { add_InferenceCall(input: $input) { _docID } }",
                &serde_json::json!({"input": input}),
            ).await?;
            let doc = gents_store::graphql::created_doc_id(&created, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.expect("seed agent-owned usage linked to the session");
}

/// The working Agent's own signed access, the only actor its controller writes run as.
fn agent_access(db: &TestDb, agent: &Arc<dyn NodeIdentity>) -> ConfigAccess {
    ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap()
}

async fn load_goal_as(access: &ConfigAccess, node_did: &str) -> Option<crate::goal::GoalDocument> {
    access
        .transact_readonly("test.load_client_goal", |txn| {
            Box::pin(crate::goal::load_canonical_goal_in_txn(
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
    let node = test_node_for(db.node_identity.clone(), agent_identity.did());
    let agent = test_agent_with_identity(node.clone(), agent_identity.clone());
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

async fn assert_stale_controller_cannot_mutate_new_continuation(status: GoalStatus) {
    let db = test_db("goal-controller-continuation-cas").await;
    let (user, agent) = goal_creation_actors(&db).await;
    goal_fixture_request(&db, &user, &agent, "newly-claimed-parent").await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let stale = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Preserve the newer continuation from stale controller decisions"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set active goal");
    assert_eq!(stale.continuation_sequence(), 0);
    assert!(crate::goal::claim_continuation_from_access(
        &agent_access(&db, &agent),
        &stale,
        "newly-claimed-parent"
    )
    .await
    .expect("claim newer continuation"));
    let claimed = load_goal_as(&access, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(claimed.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(claimed.continuation_sequence(), 1);
    assert_eq!(
        claimed.last_continued_from_request_id.as_deref(),
        Some("newly-claimed-parent")
    );
    let fields = format!(
        r#"status: "{}", last_failure: "stale controller observation""#,
        status.as_str()
    );
    assert!(
        !crate::goal::update_goal_fields_if_status_from_access(
            &agent_access(&db, &agent),
            &stale,
            GoalStatus::Active,
            &fields,
        )
        .await
        .expect("stale controller mutation returns cleanly"),
        "same status must not authorize an older continuation snapshot"
    );
    let current = load_goal_as(&access, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(current.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(
        current.continuation_sequence(),
        claimed.continuation_sequence()
    );
    assert_eq!(
        current.last_continued_from_request_id,
        claimed.last_continued_from_request_id
    );
    assert_eq!(current.last_failure, claimed.last_failure);
    assert!(crate::goal::update_goal_fields_if_status_from_access(
        &agent_access(&db, &agent),
        &current,
        GoalStatus::Active,
        &fields,
    )
    .await
    .expect("current controller mutation"));
    let updated = load_goal_as(&access, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(updated.parsed_status(), Some(status));
    assert_eq!(updated.continuation_sequence(), 1);
}

async fn signed_goal_backed_request(
    db: &TestDb,
    agent: &Arc<dyn NodeIdentity>,
    request_id: &str,
) -> gents_protocol::request_admission::AgentRequestCreate {
    let did = db.node_identity.did();
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(agent.did());
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        did,
        agent.did(),
        agent.did(),
        SESSION,
        "Begin durable work",
        "interactive",
        "2026-07-15T00:00:00Z",
        admission,
    );
    create.retry_key = Some(format!("goal-submit:{SESSION}"));
    // A goal-backed submission binds the Agent's existing session and its
    // Agent document; the Agent reserves its own local-self session first.
    let access = agent_access(db, agent);
    create.session_doc_id = Some(
        gents_session::request::lifecycle::reserve_goal_session(&access, &create)
            .await
            .expect("reserve Agent-owned session"),
    );
    create.agent_doc_id = Some(
        access
            .transact_readonly("test.goal_backed.agent", |txn| {
                let agent_did = agent.did();
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents_documents::read_desired_state_record_in_txn(
                            txn,
                            gents_store::collection::Collection::Agent,
                            did,
                            agent_did,
                        )
                        .await?
                        .expect("goal-backed request Agent")
                        .0,
                    )
                })
            })
            .await
            .expect("read goal-backed request Agent"),
    );
    gents_session::request::admission::sign_agent_request_create(agent.as_ref(), &mut create)
        .await
        .expect("sign goal-backed request");
    create
}

const RENDERED_LIMIT: &str =
    "provider usage limit reached (resets at 2026-07-15T05:00:00Z): capped";

async fn usage_limited_goal_after_failed_request(
    db: &TestDb,
    agent: &Arc<dyn NodeIdentity>,
    access: &ConfigAccess,
) -> Option<String> {
    set_goal_from_access(
        access,
        db.node_identity.did(),
        SESSION,
        Some("Stop on the call that ended the request"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    let (mut source, _snapshot_tx) = source_for_agent(db, agent).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_goal_as(access, db.node_identity.did())
        .await
        .expect("goal exists");
    (goal.parsed_status() == Some(GoalStatus::UsageLimited))
        .then_some(goal.last_failure)
        .flatten()
}

async fn seed_a_passed_reset(
    db: &TestDb,
    opted_in: bool,
) -> (
    gents_store::identity::KeyIdentity,
    Arc<dyn NodeIdentity>,
    ConfigAccess,
) {
    let (user, agent, access) = goal_recovery_actors(db, "usage-limited-request", "failed").await;
    let result = access.execute("{AgentRequest(filter: {request_id: {_eq: \"usage-limited-request\"}}) {_docID session_doc_id}}").await.unwrap();
    let parent = &result["data"]["AgentRequest"][0];
    seed_failed_call_from_access(
        &access,
        &agent,
        db.node_identity.did(),
        parent["_docID"].as_str().unwrap(),
        parent["session_doc_id"].as_str().unwrap(),
        "limited-call",
        "inference",
        1,
        "2026-07-15T00:00:10Z",
        RENDERED_LIMIT,
    )
    .await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Resume at the reported reset"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    if opted_in {
        set_goal_from_access(
            &db.client_access(&user),
            db.node_identity.did(),
            SESSION,
            None,
            None,
            None,
            Some(true),
        )
        .await
        .expect("opt in");
    }
    (user, agent, access)
}

async fn goal_and_children(db: &TestDb, access: &ConfigAccess) -> (Option<GoalStatus>, usize) {
    let goal = load_goal_as(access, db.node_identity.did())
        .await
        .expect("goal exists");
    (
        goal.parsed_status(),
        goal_children_from_access(access).await.len(),
    )
}

async fn goal_after_a_passed_reset(name: &str, opted_in: bool) -> (Option<GoalStatus>, usize) {
    let db = test_db(name).await;
    let (_user, agent, access) = seed_a_passed_reset(&db, opted_in).await;
    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent).await;
    wait_for_goal_transition_as(
        &access,
        &db.node,
        db.node_identity.did(),
        &mut source,
        "provider reset decision",
        |goal| {
            if opted_in {
                goal.parsed_status() == Some(GoalStatus::Active)
                    && goal.continuation_sequence == Some(1)
            } else {
                goal.parsed_status() == Some(GoalStatus::UsageLimited)
            }
        },
    )
    .await;
    if opted_in {
        assert!(
            tokio::time::timeout(RESCAN * 5, source.next_fire())
                .await
                .is_err(),
            "reset must not publish a second continuation"
        );
    }
    goal_and_children(&db, &access).await
}

async fn seed_same_second_canonical_goal_child(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    access: &ConfigAccess,
    parent_id: &str,
    paused: bool,
) -> (String, String) {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_session::request::lifecycle::{
        build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
        RequestSigner, RequestSpec,
    };
    use sha2::{Digest, Sha256};
    let did = db.node_identity.did();
    let parent_doc = goal_fixture_request_with_state_at(
        db,
        user,
        agent,
        parent_id,
        "completed",
        "2026-07-15T00:00:00Z",
    )
    .await;
    let goal = set_goal_from_access(
        access,
        did,
        SESSION,
        Some("Continue from the actual causal head"),
        Some(if paused {
            GoalStatus::Paused
        } else {
            GoalStatus::Active
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let digest = Sha256::digest(format!("{}\0{parent_id}", goal.goal_id).as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let child_id = format!("goal-cont-{:020}-{suffix}", 1);
    assert!(
        child_id.as_str() < parent_id,
        "fixture must defeat lexical head selection"
    );
    let identity = RequestIdentity {
        requester_did: Some(user.did().into()),
        request_id: child_id.clone(),
        node_did: did.to_owned(),
        agent_did: agent.did().into(),
        session_id: SESSION.to_owned(),
        content: "The canonical continuation made durable progress".to_owned(),
        execution_origin: ExecutionOrigin::Scheduled,
        created_at: "2026-07-15T00:00:00Z".into(),
    };
    let mut spec = RequestSpec::new(
        RequestPurpose::Normal,
        identity,
        AgentRequestAdmissionRecord::client(user.did()),
    );
    spec.trigger_lineage.trigger_id = Some(goal.goal_id.clone());
    spec.trigger_lineage.trigger_kind = Some("goal".to_owned());
    spec.parent = Some(gents_session::request::lifecycle::ParentLink {
        parent_request_id: parent_id.to_owned(),
        parent_request_doc_id: parent_doc.clone(),
        ..Default::default()
    });
    spec.retry_key = Some(format!("goal-continuation:{suffix}"));
    spec.input = serde_json::from_value(serde_json::json!({
        "queue": {"source":"goal", "policy":"coalesce", "key":format!("goal:{suffix}"), "queued_after_request_id":parent_id},
        "goal_continuation": {"sequence":1, "wrapup":false},
        "goal_assignment": {"goal_id":goal.goal_id, "root_request_doc_id":goal.assignment_root_request_doc_id}
    })).unwrap();
    let client = db.client_access(user);
    let child = build_signed_client_request(&client, spec, RequestSigner::Identity(user))
        .await
        .unwrap();
    submit_client_request(&client, &child).await.unwrap();
    let children = goal_children_from_access(access).await;
    assert_eq!(children.len(), 1);
    let child_doc = children[0].doc_id.clone();
    set_request_state_as(
        access,
        &child_doc,
        if paused { "interrupted" } else { "completed" },
    )
    .await;
    if paused {
        let terminal = gents_protocol::graphql::graphql_input_literal(
            &serde_json::to_value(gents_protocol::output::TerminalOutput::NoMessage).unwrap(),
        )
        .unwrap();
        let changed = access.write("test.goal_no_terminal_message", &format!(r#"mutation {{update_AgentRequest(docID: "{}", input: {{terminal_output: {terminal}}}) {{_docID}}}}"#, escape_graphql_string(&child_doc))).await.unwrap();
        assert_eq!(
            changed["data"]["update_AgentRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    } else {
        publish_owned_terminal_activity(
            access,
            agent.as_ref(),
            &child_doc,
            "canonical child produced durable activity",
        )
        .await;
    }
    // The goal source binds an unbound Goal's assignment root to the first
    // continuation it publishes; the client-submitted fixture child does not.
    let root = goal
        .assignment_root_request_doc_id
        .clone()
        .unwrap_or_else(|| child_doc.clone());
    seed_goal_fields_from_access(
        access,
        &goal,
        &format!(
            "continuation_sequence: 1, last_continued_from_request_id: \"{}\", assignment_root_request_doc_id: \"{}\"",
            escape_graphql_string(parent_id),
            escape_graphql_string(&root)
        ),
    )
    .await;
    (child_id, child_doc)
}

async fn reject_before_claim(access: &ConfigAccess, doc_id: &str, reason: &str) {
    let doc_id = escape_graphql_string(doc_id);
    let reason = escape_graphql_string(reason);
    let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let response = access.write("test.goal_readiness_rejection", &format!(r#"mutation {{ update_AgentRequest(
        filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "pending" }} }},
        input: {{ lifecycle_state: "failed", failure_reason: "{reason}", terminalized_at: "{terminalized_at}" }}
    ) {{ _docID execution_generation }} }}"#)).await.unwrap();
    let rows = response["data"]["update_AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["execution_generation"].is_null());
}

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

async fn seed_goal_fields_from_access(
    access: &ConfigAccess,
    goal: &crate::goal::GoalDocument,
    fields: &str,
) {
    let doc = escape_graphql_string(&goal.doc_id);
    let response = access
        .write(
            "test.goal_fixture_fields",
            &format!(r#"mutation {{update_Goal(docID: "{doc}", input: {{{fields}}}) {{_docID}}}}"#),
        )
        .await
        .unwrap();
    assert_eq!(response["data"]["update_Goal"].as_array().unwrap().len(), 1);
}

async fn wait_for_goal_transition_as(
    access: &ConfigAccess,
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    source: &mut GoalSource,
    description: &str,
    reached: impl Fn(&crate::goal::GoalDocument) -> bool,
) -> crate::goal::GoalDocument {
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

#[expect(
    clippy::too_many_arguments,
    reason = "fixture binds the failed call to its Agent and physical request"
)]
async fn seed_failed_call_from_access(
    access: &ConfigAccess,
    agent: &Arc<dyn NodeIdentity>,
    node_did: &str,
    request_doc: &str,
    session_doc: &str,
    call_id: &str,
    call_kind: &str,
    attempt: i64,
    ended_at: &str,
    failure_reason: &str,
) {
    access.transact_write("test.goal_failed_call", |txn| {
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: InferenceCallMutationInputArg!) {add_InferenceCall(input: $input) {_docID}}",
                &serde_json::json!({"input": {"call_id": call_id, "node_did": node_did,
                    "request_id": "usage-limited-request", "request_doc_id": request_doc,
                    "session_id": SESSION, "agent_did": agent.did(), "backend_id": format!("{}:backend", agent.did()),
                    "call_seq": 1, "call_kind": call_kind, "attempt": attempt, "call_state": "failed",
                    "queued_at": "2026-07-15T00:00:00Z", "started_at": "2026-07-15T00:00:00Z",
                    "ended_at": ended_at, "failure_reason": failure_reason}}),
            ).await?;
            let doc = gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
}

async fn operator_resume_child_row_from_access(
    access: &ConfigAccess,
    doc_id: &str,
) -> serde_json::Value {
    let doc = escape_graphql_string(doc_id);
    let response = access.execute(&format!(r#"{{AgentRequest(docID: "{doc}") {{
        _docID request_id node_did requester_did session_id agent_did lifecycle_state
        content created_at execution_origin retry_key input admission_kind admission_signer_did
        admission_signature runtime_issuer_did runtime_source_request_id runtime_source_kind
        caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind caused_by_correlation
        caused_by_source_doc_id caused_by_trigger_context caused_by_parent_request_id caused_by_parent_request_doc_id
        caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id request_hop workspace_id workspace_authority
        workspace_owner_node_did workspace_seal_hash
    }}}}"#)).await.unwrap();
    response["data"]["AgentRequest"][0].clone()
}

async fn seed_operator_resume_parent_with_actors(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    request_id: &str,
) -> String {
    goal_fixture_request_with_state_at(
        db,
        user,
        agent,
        request_id,
        "interrupted",
        "2026-07-15T00:00:00Z",
    )
    .await
}

/// A foreign Node's User submits a colliding request here; it stays pending,
/// since only that Node's own Agent signs its lifecycle and P2P carries only
/// discovery and enrollment, so no foreign terminal row can reach this runtime.
async fn create_visible_foreign_collision(
    db: &TestDb,
    observer: &dyn NodeIdentity,
    request_id: &str,
    retry_key: Option<&str>,
) -> String {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_session::request::lifecycle::{
        build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
        RequestSigner, RequestSpec,
    };
    let directory = tempfile::tempdir().unwrap();
    let foreign_node = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("foreign-node.key"),
        None,
    )
    .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&foreign_node).unwrap();
    let foreign_agent =
        gents_store::identity::resolve_agent_identity(&foreign_node, &binding).unwrap();
    let foreign_user = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("foreign-user.key"),
        None,
    )
    .unwrap();
    let admin = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse().unwrap());
    for actor in [foreign_user.did(), foreign_agent.did()] {
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::CollectionGet,
            gents_store::config_client::NodePermission::DacRelationAdd,
            gents_store::config_client::NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let client = db.client_access(&foreign_user);
    let node = serde_json::json!({"node_did": foreign_node.did()});
    let backend_id = format!("{}:backend", foreign_agent.did());
    let profile_id = format!("{}:inference", foreign_agent.did());
    let backend = serde_json::json!({"node_did": foreign_node.did(), "backend_id": backend_id,
        "name": "Foreign inference", "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:1/v1", "auth": {"kind": "unauthenticated"}});
    let profile = serde_json::json!({"node_did": foreign_node.did(), "profile_id": profile_id,
        "backend_id": backend_id, "model_name": "test-model"});
    let agent = serde_json::json!({"node_did": foreign_node.did(), "agent_did": foreign_agent.did(),
        "creation_nonce": binding.creation_nonce, "display_name": "Foreign collision Agent",
        "inference_profile_id": profile_id, "enabled": true});
    let plan = gents_documents::DesiredStateApplyPlan::new(
        [
            (gents_store::collection::Collection::Node, node),
            (
                gents_store::collection::Collection::InferenceBackend,
                backend,
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                profile,
            ),
            (gents_store::collection::Collection::Agent, agent),
        ]
        .into_iter()
        .map(
            |(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    client
        .transact_write("test.foreign_collision.configuration", |txn| {
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
        .unwrap();
    let mut spec = RequestSpec::new(
        RequestPurpose::Normal,
        RequestIdentity {
            node_did: foreign_node.did().into(),
            requester_did: Some(foreign_user.did().into()),
            agent_did: foreign_agent.did().into(),
            session_id: "foreign-session".into(),
            request_id: request_id.into(),
            content: "foreign collision".into(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: "2026-07-15T00:00:01Z".into(),
        },
        AgentRequestAdmissionRecord::client(foreign_user.did()),
    );
    spec.retry_key = retry_key.map(str::to_owned);
    let create = build_signed_client_request(&client, spec, RequestSigner::Identity(&foreign_user))
        .await
        .unwrap();
    submit_client_request(&client, &create).await.unwrap();
    let request = escape_graphql_string(request_id);
    let result = client.execute(&format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}, limit: 2) {{_docID node_did session_doc_id agent_doc_id}}}}"#)).await.unwrap();
    let [row] = result["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("one foreign collision request")
    };
    let doc = row["_docID"].as_str().unwrap().to_owned();
    assert_eq!(
        row["session_doc_id"],
        create.session_doc_id.as_deref().unwrap()
    );
    assert_eq!(row["agent_doc_id"], create.agent_doc_id.as_deref().unwrap());
    client
        .add_actor_relationship(
            "AgentRequest",
            &doc,
            "reader",
            &observer.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let observer_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        observer.did().parse().unwrap(),
        observer,
    )
    .unwrap();
    let query = format!(
        r#"{{AgentRequest(docID: "{}") {{_docID node_did}}}}"#,
        escape_graphql_string(&doc)
    );
    let visible = observer_access.execute(&query).await.unwrap();
    assert_eq!(
        visible["data"]["AgentRequest"][0]["node_did"],
        foreign_node.did()
    );
    doc
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

async fn seed_foreign_goal_usage(
    access: &ConfigAccess,
    agent: &Arc<dyn NodeIdentity>,
    foreign_node: &str,
    request_doc: &str,
    session_doc: &str,
) {
    let doc = access.transact_write("test.goal_foreign_usage", |txn| {
        Box::pin(async move {
            let input = serde_json::json!({"call_id":"goal-budget-call-foreign", "node_did":foreign_node,
                "agent_did":agent.did(), "session_id":SESSION, "request_id":"parent-budget",
                "request_doc_id":request_doc, "call_seq":1, "call_kind":"inference", "attempt":1,
                "call_state":"completed", "prompt_tokens":10000, "completion_tokens":10000, "cached_input_tokens":0});
            let result = txn.execute_with_variables("mutation($input: InferenceCallMutationInputArg!) {add_InferenceCall(input:$input) {_docID}}", &serde_json::json!({"input":input})).await?;
            let doc = gents_store::graphql::created_doc_id(&result, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap();
    let observed = access
        .execute(&format!(
            r#"{{InferenceCall(docID: "{}") {{_docID node_did request_doc_id}}}}"#,
            escape_graphql_string(&doc)
        ))
        .await
        .unwrap();
    assert_eq!(
        observed["data"]["InferenceCall"][0]["node_did"],
        foreign_node
    );
    assert_eq!(
        observed["data"]["InferenceCall"][0]["request_doc_id"],
        request_doc
    );
}

async fn seed_lineage_tool(
    access: &ConfigAccess,
    agent: &Arc<dyn NodeIdentity>,
    user: &gents_store::identity::KeyIdentity,
    node_did: &str,
    request_id: &str,
    request_doc: &str,
    tool_id: &str,
) -> String {
    let response = access
        .execute(&format!(
            r#"{{AgentRequest(docID: "{}") {{session_doc_id}}}}"#,
            escape_graphql_string(request_doc)
        ))
        .await
        .unwrap();
    let session_doc = response["data"]["AgentRequest"][0]["session_doc_id"]
        .as_str()
        .unwrap();
    access.transact_write("test.goal_lineage_tool", |txn| {
        Box::pin(async move {
            let input = serde_json::json!({"tool_call_key":tool_id, "tool_call_id":tool_id,
                "node_did":node_did,"requester_did":user.did(),"session_id":SESSION,"session_doc_id":session_doc,
                "agent_did":agent.did(),"creator_did":agent.did(),"parent_linked":true,
                "request_id":request_id,"request_doc_id":request_doc,"tool_name":"agent_message",
                "status":"completed","lifecycle_state":"completed"});
            let result = txn.execute_with_variables("mutation($input: AgentToolCallMutationInputArg!) {create_AgentToolCall(input:$input) {_docID}}", &serde_json::json!({"input":input})).await?;
            let doc = gents_store::graphql::created_doc_id(&result, "AgentToolCall")?;
            txn.add_resource_relationship("AgentToolCall", &doc, "parent", "sessions", session_doc).await?;
            txn.add_actor_relationship("AgentToolCall", &doc, "working_agent", &agent.did().parse()?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap()
}

#[expect(
    clippy::too_many_arguments,
    reason = "fixture binds workspace lineage to physical requests and tool calls"
)]
async fn seed_lineage_parent(
    db: &TestDb,
    user: &gents_store::identity::KeyIdentity,
    agent: &Arc<dyn NodeIdentity>,
    access: &ConfigAccess,
    request_id: &str,
    terminal: &str,
    workspace_id: &str,
    seal_hash: &str,
) -> (String, String) {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_session::request::lifecycle::{
        build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
        RequestSigner, RequestSpec,
    };
    let client = db.client_access(user);
    let root_id = "upstream-root";
    let root = goal_fixture_request_with_state_at(
        db,
        user,
        agent,
        root_id,
        "completed",
        "2026-07-14T23:59:58Z",
    )
    .await;
    let mut predecessor_id = root_id.to_owned();
    let mut predecessor_doc = root.clone();
    for (id, created_at, hop, tool_id) in [
        (
            "upstream-middle",
            "2026-07-14T23:59:59Z",
            1,
            "upstream-root-call",
        ),
        (
            request_id,
            "2026-07-15T00:00:00Z",
            2,
            "upstream-middle-call",
        ),
    ] {
        let tool_doc = seed_lineage_tool(
            access,
            agent,
            user,
            db.node_identity.did(),
            &predecessor_id,
            &predecessor_doc,
            tool_id,
        )
        .await;
        let spec = RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                requester_did: Some(user.did().into()),
                request_id: id.into(),
                node_did: db.node_identity.did().into(),
                agent_did: agent.did().into(),
                session_id: SESSION.into(),
                content: "Continue the original graph work".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: created_at.into(),
            },
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let mut request = build_signed_client_request(&client, spec, RequestSigner::Identity(user))
            .await
            .unwrap();
        request.request_hop = hop;
        request.caused_by_parent_request_id = Some(predecessor_id.clone());
        request.caused_by_parent_request_doc_id = Some(predecessor_doc.clone());
        request.caused_by_parent_tool_call_id = Some(tool_id.to_owned());
        request.caused_by_parent_tool_call_doc_id = Some(tool_doc);
        if hop == 2 {
            request.caused_by_trigger_id = Some("original-graph-trigger".into());
            request.caused_by_trigger_kind = Some("event".into());
            request.caused_by_correlation = Some("original-graph-correlation".into());
            request.caused_by_source_doc_id = Some(root.clone());
            request.caused_by_trigger_context =
                Some(r#"{"source_fields":{"artifact":"original"}}"#.into());
            request.workspace_id = Some(workspace_id.into());
            request.workspace_authority = Some("readOnly".into());
            request.workspace_owner_node_did = Some(db.node_identity.did().into());
            request.workspace_seal_hash = Some(seal_hash.into());
        }
        gents_session::request::admission::sign_agent_request_create(user, &mut request)
            .await
            .unwrap();
        submit_client_request(&client, &request).await.unwrap();
        let id_escaped = escape_graphql_string(id);
        let response = client.execute(&format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{id_escaped}"}}}}, limit: 2) {{_docID}}}}"#)).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        predecessor_doc = rows[0]["_docID"].as_str().unwrap().to_owned();
        set_request_state_as(
            access,
            &predecessor_doc,
            if hop == 2 { terminal } else { "completed" },
        )
        .await;
        predecessor_id = id.to_owned();
    }
    (predecessor_doc, root)
}

#[tokio::test]
async fn goal_continuation_preserves_nested_workspace_lineage() {
    let db = test_db("goal-nested-workspace-lineage").await;
    let (user, agent_identity) = goal_creation_actors(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent_identity.did().parse().unwrap(),
        agent_identity.as_ref(),
    )
    .unwrap();
    let did = db.node_identity.did();
    let (parent_doc_id, _source_doc) = seed_lineage_parent(
        &db,
        &user,
        &agent_identity,
        &access,
        "parent-nested-workspace",
        "completed",
        "workspace-goal",
        "seal-hash",
    )
    .await;
    publish_owned_terminal_activity(
        &access,
        agent_identity.as_ref(),
        &parent_doc_id,
        "durable progress",
    )
    .await;
    set_goal_from_access(
        &access,
        did,
        SESSION,
        Some("Preserve nested workspace scope"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _tx) = source_for_agent(&db, &agent_identity).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("lineage continuation timed out")
        .expect("lineage continuation intent");
    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    let visible = goal_children_from_access(&db.client_access(&user)).await;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].doc_id, child.doc_id);
    assert_eq!(child.request_hop, Some(2));
    assert_eq!(child.workspace_id.as_deref(), Some("workspace-goal"));
    assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
    assert_eq!(child.workspace_owner_node_did.as_deref(), Some(did));
    assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-hash"));
    let child_identity = (
        child.doc_id.clone(),
        child.request_id.clone(),
        child.retry_key.clone(),
    );
    let continuation_sequence = load_goal_as(&access, did)
        .await
        .unwrap()
        .continuation_sequence;
    drop(source);
    for _ in 0..2 {
        let recovered =
            gents_session::request::lifecycle::RequestLifecycle::recover_all_with_identities(
                &db.node,
                did,
                std::slice::from_ref(&agent_identity),
            )
            .await
            .unwrap();
        assert_eq!(recovered.requests_recovered, 0);
        let (mut restarted, _restart_tx) = source_for_agent(&db, &agent_identity).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
                .await
                .is_err(),
            "repeated maintenance and source reconciliation must not emit another continuation"
        );
        let children = goal_children_from_access(&access).await;
        assert_eq!(children.len(), 1);
        let child = &children[0];
        assert_eq!(
            (
                child.doc_id.clone(),
                child.request_id.clone(),
                child.retry_key.clone()
            ),
            child_identity
        );
        assert_eq!(child.request_hop, Some(2));
        assert_eq!(child.workspace_id.as_deref(), Some("workspace-goal"));
        assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
        assert_eq!(child.workspace_owner_node_did.as_deref(), Some(did));
        assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-hash"));
        assert_eq!(
            load_goal_as(&access, did)
                .await
                .unwrap()
                .continuation_sequence,
            continuation_sequence
        );
    }
}

#[tokio::test]
async fn delimiter_ambiguous_goal_parent_pairs_get_distinct_retry_keys() {
    let db = test_db("goal-delimiter-safe-key").await;
    let did = db.node_identity.did();
    let (user, agent) = goal_creation_actors(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    for (session, parent) in [("b:c", "d"), ("b", "c:d")] {
        reserve_goal_session(&db, &user, &agent, session).await;
        let doc_id = goal_fixture_request_in_session_at(
            &db,
            &user,
            &agent,
            session,
            parent,
            "completed",
            "2026-11-04T00:00:00Z",
        )
        .await;
        publish_owned_terminal_activity(&access, agent.as_ref(), &doc_id, "progress").await;
        assert!(!doc_id.is_empty());
        set_goal_from_access(
            &access,
            did,
            session,
            Some(&format!("goal for {session}")),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .expect("set delimiter goal");
    }

    let (mut source, _tx) = source_for_agent(&db, &agent).await;
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .expect("delimiter continuation timed out")
            .expect("delimiter continuation intent");
    }
    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 2);
    let keys = children
        .iter()
        .map(|child| child.retry_key.as_deref().expect("retry key"))
        .collect::<RapidHashSet<_>>();
    assert_eq!(keys.len(), 2);
}

#[tokio::test]
async fn completed_request_materializes_exactly_one_same_session_goal_child() {
    let db = test_db("goal-exactly-once").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let parent_doc = goal_fixture_request(&db, &user, &agent, "parent-complete").await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    publish_owned_terminal_activity(&access, agent.as_ref(), &parent_doc, "durable progress").await;
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Finish the durable objective"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent).await;
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out")
        .expect("goal continuation intent");
    assert!(intent.pre_materialized_request_id.is_some());

    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    let visible = goal_children_from_access(&db.client_access(&user)).await;
    assert_eq!(
        visible.len(),
        1,
        "the session owner must see the continuation"
    );
    assert_eq!(visible[0].doc_id, child.doc_id);
    assert_eq!(child.session_id, SESSION);
    assert_eq!(child.agent_did.as_deref(), Some(agent.did()));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some("parent-complete")
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(goal.goal_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("goal"));

    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err(),
        "a pending child must suppress duplicate continuation"
    );
    assert_eq!(goal_children_from_access(&access).await.len(), 1);
}

#[tokio::test]
async fn foreign_request_id_collision_cannot_preempt_owned_goal_continuation() {
    use sha2::{Digest, Sha256};

    let db = test_db("goal-foreign-request-id-collision").await;
    let (_user, agent, access) =
        goal_recovery_actors(&db, "parent-owned-collision", "completed").await;
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Ignore foreign request IDs"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    let digest =
        Sha256::digest(format!("{}\0{}", goal.goal_id, "parent-owned-collision").as_bytes());
    let collision_id = format!(
        "goal-cont-{:020}-{}",
        1,
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    create_visible_foreign_collision(&db, agent.as_ref(), &collision_id, None).await;

    let (mut source, _tx) = source_for_agent(&db, &agent).await;
    let fired = tokio::time::timeout(Duration::from_secs(2), source.next_fire()).await;
    if !matches!(fired, Ok(Some(_))) {
        let state = access
            .execute("{ Goal { goal_id status last_continued_from_request_id continuation_sequence } AgentRequest { request_id node_did session_id retry_key caused_by_trigger_kind } }")
            .await;
        let outcome = match fired {
            Err(_) => "timeout",
            Ok(None) => "source ended",
            Ok(Some(_)) => unreachable!(),
        };
        panic!("owned continuation did not fire ({outcome}); state={state:?}");
    }
    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].request_id, collision_id);
    assert_eq!(children[0].session_id, SESSION);
}

#[tokio::test]
async fn foreign_retry_key_collision_is_rejected_instead_of_reused() {
    use sha2::{Digest, Sha256};

    let db = test_db("goal-foreign-retry-key-collision").await;
    let (_user, agent, access) =
        goal_recovery_actors(&db, "parent-owned-retry-collision", "completed").await;
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Reject foreign retry keys"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    let digest =
        Sha256::digest(format!("{}\0{}", goal.goal_id, "parent-owned-retry-collision").as_bytes());
    let retry_key = format!(
        "goal-continuation:{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    create_visible_foreign_collision(
        &db,
        agent.as_ref(),
        "foreign-retry-collision",
        Some(&retry_key),
    )
    .await;

    let (mut source, _tx) = source_for_agent(&db, &agent).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err(),
        "a conflicting retry key must not be reported as an owned continuation"
    );
    assert!(goal_children_from_access(&access).await.is_empty());
}

#[tokio::test]
async fn restart_reconcile_does_not_duplicate_a_goal_continuation() {
    let db = test_db("goal-restart-no-duplicate").await;
    let (_user, agent, access) = goal_recovery_actors(&db, "parent-restart", "completed").await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Continue exactly once across restart"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");

    let (mut first, _first_tx) = source_for_agent(&db, &agent).await;
    tokio::time::timeout(Duration::from_secs(2), first.next_fire())
        .await
        .expect("first source timed out")
        .expect("first continuation");
    drop(first);

    let (mut restarted, _restart_tx) = source_for_agent(&db, &agent).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
            .await
            .is_err(),
        "restart must not emit a second child"
    );
    assert_eq!(goal_children_from_access(&access).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_a_claimed_continuation_after_crash() {
    let db = test_db("goal-restart-claimed").await;
    let (_user, agent, access) = goal_recovery_actors(&db, "parent-claimed", "completed").await;
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Recover the claimed continuation"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    assert!(crate::goal::claim_continuation_from_access(
        &agent_access(&db, &agent),
        &goal,
        "parent-claimed"
    )
    .await
    .expect("claim continuation"));
    assert!(goal_children_from_access(&access).await.is_empty());

    let (mut restarted, _restart_tx) = source_for_agent(&db, &agent).await;
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("recovery source timed out")
        .expect("recovered continuation");
    assert_eq!(goal_children_from_access(&access).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_claimed_wrapup_retry_without_charging_it_twice() {
    let db = test_db("goal-restart-claimed-wrapup-retry").await;
    let (_user, agent, access) =
        goal_recovery_actors(&db, "parent-claimed-wrapup-retry", "failed").await;
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Recover one charged wrap-up retry"),
        Some(GoalStatus::BudgetLimited),
        None,
        None,
    )
    .await
    .expect("set budget-limited goal");
    assert!(crate::goal::claim_retry_continuation_from_access(
        &agent_access(&db, &agent),
        &goal,
        "parent-claimed-wrapup-retry",
        1,
        "failed",
    )
    .await
    .expect("atomically charge and claim wrap-up retry"));

    let (mut restarted, _tx) = source_for_agent(&db, &agent).await;
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("wrap-up retry recovery timed out")
        .expect("wrap-up retry recovery intent");
    let recovered = load_goal_as(&access, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(recovered.infrastructure_retry_count, Some(1));
    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 1);
    let input = children[0].input.as_ref().expect("goal continuation input");
    assert_eq!(input["goal_continuation"]["wrapup"], true);
}

#[tokio::test]
async fn continuation_claim_rejects_a_stale_active_status_snapshot() {
    let db = test_db("goal-claim-status-cas").await;
    let (user, agent) = goal_creation_actors(&db).await;
    goal_fixture_request(&db, &user, &agent, "parent-stale").await;
    let client = db.client_access(&user);
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let stale = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Do not race an operator pause"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    set_goal_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        None,
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .expect("user pauses goal");
    assert!(!crate::goal::claim_continuation_from_access(
        &agent_access(&db, &agent),
        &stale,
        "parent-stale"
    )
    .await
    .expect("stale claim returns cleanly"));
}

#[tokio::test]
async fn controller_status_cas_never_overwrites_a_newer_completion() {
    let db = test_db("goal-controller-status-cas").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let stale = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Completion wins controller races"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    set_goal_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        None,
        Some(GoalStatus::Complete),
        None,
        None,
    )
    .await
    .expect("user completes goal");
    assert!(!crate::goal::update_goal_fields_if_status_from_access(
        &agent_access(&db, &agent),
        &stale,
        GoalStatus::Active,
        r#"status: "paused", last_failure: "stale controller""#,
    )
    .await
    .expect("conditional controller update"));
    let current = load_goal_as(&access, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(current.parsed_status(), Some(GoalStatus::Complete));
}

#[tokio::test]
async fn controller_pause_rejects_same_status_from_an_older_continuation() {
    assert_stale_controller_cannot_mutate_new_continuation(GoalStatus::Paused).await;
}

#[tokio::test]
async fn controller_block_rejects_same_status_from_an_older_continuation() {
    assert_stale_controller_cannot_mutate_new_continuation(GoalStatus::Blocked).await;
}

#[tokio::test]
async fn model_goal_create_is_idempotent_and_never_mutates_a_conflict() {
    let db = test_db("goal-model-create").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let created = crate::goal::create_goal_for_session_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        "Ship the feature",
        Some(1_000),
    )
    .await
    .expect("create model goal");
    assert!(matches!(created, CreateGoalForSessionOutcome::Created(_)));
    assert_ne!(user.did(), db.node_identity.did());
    assert_ne!(user.did(), agent.did());
    let rows = client.execute("{ Goal { _docID session_doc_id agent_did creator_did parent_linked } GoalCreationClaim { _docID session_doc_id agent_did creator_did parent_linked } }").await.unwrap();
    for collection in ["Goal", "GoalCreationClaim"] {
        let row = &rows["data"][collection][0];
        assert_eq!(row["agent_did"], agent.did());
        assert_eq!(row["creator_did"], user.did());
        let modeled = atomic_goal_link_case("user", collection).await;
        assert_eq!(row["parent_linked"], modeled["initial"]["linked"]);
        let doc = row["_docID"].as_str().unwrap().to_owned();
        let owners = client
            .transact_readonly("test.client_goal_owner", |txn| {
                let doc = &doc;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc))
                        .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0].owner.as_ref().map(|owner| owner.as_str()),
            Some(user.did())
        );
        let escaped = escape_graphql_string(&doc);
        let history = client.execute(&format!(r#"{{ _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#)).await.unwrap();
        let cid = history["data"]["_commits"][0]["cid"].as_str().unwrap();
        assert_eq!(
            verified_block_signer_as(&db.node, user.did(), cid).await,
            user.did()
        );
    }

    let duplicate = crate::goal::create_goal_for_session_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        " Ship the feature ",
        Some(1_000),
    )
    .await
    .expect("idempotent model goal retry");
    assert!(matches!(
        duplicate,
        CreateGoalForSessionOutcome::Idempotent(_)
    ));
    assert_eq!(
        client.execute("{ Goal { _docID } }").await.unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let error = crate::goal::create_goal_for_session_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        "Different objective",
        Some(1_000),
    )
    .await
    .expect_err("conflicting create must fail");
    assert!(
        matches!(error, CreateGoalForSessionError::Conflict),
        "{error:?}"
    );
    let unchanged = load_goal_as(&client, db.node_identity.did())
        .await
        .expect("goal remains");
    assert_eq!(unchanged.objective, "Ship the feature");
    assert_eq!(unchanged.token_budget, Some(1_000));
}

#[tokio::test]
async fn clearing_a_goal_also_clears_its_creation_claim() {
    let db = test_db("goal-model-create-clear").await;
    let (_user, agent) = goal_creation_actors(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    crate::goal::create_goal_for_session_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        "First objective",
        None,
    )
    .await
    .expect("create first goal");
    let cleared = access.transact_write("test.clear_agent_goal", |txn| {
        Box::pin(async move {
            let goal = txn.execute(r#"mutation { delete_Goal(filter: {session_id: {_eq: "goal-session"}}) {_docID} }"#).await?;
            let claim = txn.execute(r#"mutation { delete_GoalCreationClaim(filter: {session_id: {_eq: "goal-session"}}) {_docID} }"#).await?;
            let goals = goal["data"]["delete_Goal"].as_array().map_or(0, Vec::len);
            let claims = claim["data"]["delete_GoalCreationClaim"].as_array().map_or(0, Vec::len);
            Ok::<_, anyhow::Error>((goals, claims))
        })
    }).await.expect("clear Goal and creation claim as their Agent owner");
    assert_eq!(cleared, (1, 1));
    let replacement = crate::goal::create_goal_for_session_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        "Replacement objective",
        None,
    )
    .await
    .expect("create replacement goal");
    assert!(matches!(
        replacement,
        CreateGoalForSessionOutcome::Created(_)
    ));
}

#[tokio::test]
async fn fresh_goal_baseline_snapshots_prior_session_usage_and_charges_only_new_spend() {
    let db = test_db("goal-baseline-prior-usage").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let request_doc = goal_fixture_request(&db, &user, &agent, "parent-baseline").await;
    goal_fixture_usage(
        &db,
        &agent,
        "parent-baseline",
        &request_doc,
        "baseline-call",
        1000,
        50,
    )
    .await;

    // The fresh create must snapshot the session's usage at creation time so
    // pre-goal spend is never charged against the new goal.
    let created = crate::goal::create_goal_for_session_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        "Baseline prior usage",
        Some(10_000),
    )
    .await
    .expect("create goal");
    let goal = match created {
        CreateGoalForSessionOutcome::Created(goal) => goal,
        outcome => panic!("expected fresh create, got {outcome:?}"),
    };
    assert_eq!(goal.token_usage_baseline, Some(1050));
    assert_eq!(goal.tokens_used, Some(0));

    // Only spend arriving after creation is charged.
    goal_fixture_usage(
        &db,
        &agent,
        "parent-baseline",
        &request_doc,
        "post-goal-call",
        400,
        50,
    )
    .await;
    let refreshed = load_goal_as(&client, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(refreshed.token_usage_baseline, Some(1050));
    assert_eq!(refreshed.tokens_used, Some(0));
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let tokens = crate::goal::refresh_goal_usage_from_access(&agent_access, &refreshed)
        .await
        .expect("refresh usage");
    assert_eq!(tokens, 450);
    let charged = load_goal_as(&client, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(charged.token_usage_baseline, Some(1050));
    assert_eq!(charged.tokens_used, Some(450));
}

#[tokio::test]
async fn idempotent_goal_recreate_preserves_the_existing_baseline() {
    let db = test_db("goal-baseline-idempotent").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let request_doc = goal_fixture_request(&db, &user, &agent, "parent-idempotent").await;
    goal_fixture_usage(
        &db,
        &agent,
        "parent-idempotent",
        &request_doc,
        "first-call",
        1000,
        50,
    )
    .await;
    let created = crate::goal::create_goal_for_session_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        "Baseline prior usage",
        Some(10_000),
    )
    .await
    .expect("create goal");
    let goal = match created {
        CreateGoalForSessionOutcome::Created(goal) => goal,
        outcome => panic!("expected fresh create, got {outcome:?}"),
    };
    assert_eq!(goal.token_usage_baseline, Some(1050));

    // More spend arrives, then the same create replays. The idempotent path
    // must return the existing row untouched: baseline stays at the creation
    // snapshot even though session usage has moved on.
    goal_fixture_usage(
        &db,
        &agent,
        "parent-idempotent",
        &request_doc,
        "second-call",
        400,
        50,
    )
    .await;
    let duplicate = crate::goal::create_goal_for_session_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        " Baseline prior usage ",
        Some(10_000),
    )
    .await
    .expect("idempotent replay");
    let goal = match duplicate {
        CreateGoalForSessionOutcome::Idempotent(goal) => goal,
        outcome => panic!("expected idempotent replay, got {outcome:?}"),
    };
    assert_eq!(goal.token_usage_baseline, Some(1050));
    assert_eq!(goal.tokens_used, Some(0));

    // An owner-level set_goal replay is also idempotent and must not reset the
    // baseline after post-goal spend.
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let replayed = set_goal_from_access(
        &agent_access,
        db.node_identity.did(),
        SESSION,
        Some("Baseline prior usage"),
        Some(GoalStatus::Active),
        Some(Some(10_000)),
        None,
    )
    .await
    .expect("set_goal replay");
    assert_eq!(replayed.token_usage_baseline, Some(1050));
    assert_eq!(replayed.tokens_used, Some(0));
    assert_eq!(
        client.execute("{ Goal { _docID } }").await.unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn fresh_goal_with_zero_prior_usage_starts_charging_from_zero() {
    let db = test_db("goal-baseline-zero").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let request_doc = goal_fixture_request(&db, &user, &agent, "parent-zero").await;
    let created = crate::goal::create_goal_for_session_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        "Zero prior usage",
        Some(10_000),
    )
    .await
    .expect("create goal");
    let goal = match created {
        CreateGoalForSessionOutcome::Created(goal) => goal,
        outcome => panic!("expected fresh create, got {outcome:?}"),
    };
    assert_eq!(goal.token_usage_baseline, Some(0));
    assert_eq!(goal.tokens_used, Some(0));
    goal_fixture_usage(
        &db,
        &agent,
        "parent-zero",
        &request_doc,
        "only-call",
        400,
        50,
    )
    .await;
    let refreshed = load_goal_as(&client, db.node_identity.did())
        .await
        .expect("goal exists");
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let tokens = crate::goal::refresh_goal_usage_from_access(&agent_access, &refreshed)
        .await
        .expect("refresh usage");
    assert_eq!(tokens, 450);
    let charged = load_goal_as(&client, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(charged.token_usage_baseline, Some(0));
    assert_eq!(charged.tokens_used, Some(450));
}

#[tokio::test]
async fn set_goal_over_http_transactions_computes_the_same_baseline() {
    let db = test_db("http-goal-baseline").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let access = db.client_access(&user);
    let did = db.node_identity.did();
    let request_doc = goal_fixture_request(&db, &user, &agent, "http-baseline-request").await;
    goal_fixture_usage_for_node(
        &db.node,
        did,
        SESSION,
        &agent,
        "http-baseline-request",
        &request_doc,
        "http-baseline-call",
        1000,
        50,
        0,
    )
    .await;
    let request = access.execute("{AgentRequest(filter: {request_id: {_eq: \"http-baseline-request\"}}) {session_doc_id}}").await.unwrap();
    let session_doc = request["data"]["AgentRequest"][0]["session_doc_id"]
        .as_str()
        .unwrap();
    let goal = set_goal_from_access(
        &access,
        did,
        SESSION,
        Some("HTTP baseline"),
        None,
        None,
        None,
    )
    .await
    .expect("set goal over HTTP");
    assert_eq!(goal.token_usage_baseline, Some(1050));
    assert_eq!(goal.tokens_used, Some(0));
    let escaped = escape_graphql_string(&goal.doc_id);
    let rows = access.execute(&format!(r#"{{ Goal(filter: {{_docID: {{_eq: "{escaped}"}}}}) {{session_doc_id agent_did creator_did parent_linked}} _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#)).await.unwrap();
    let goal_row = &rows["data"]["Goal"][0];
    assert_eq!(goal_row["session_doc_id"], session_doc);
    assert_eq!(goal_row["agent_did"], agent.did());
    assert_eq!(goal_row["creator_did"], user.did());
    let modeled = atomic_goal_link_case("user", "Goal").await;
    assert_eq!(goal_row["parent_linked"], modeled["initial"]["linked"]);
    let cid = rows["data"]["_commits"][0]["cid"].as_str().unwrap();
    assert_eq!(
        verified_block_signer_as(&db.node, user.did(), cid).await,
        user.did()
    );

    let client_session = "http-client-goal";
    let client_session_doc = reserve_goal_session(&db, &user, &agent, client_session).await;
    let created = crate::goal::create_goal_for_session_from_access(
        &access,
        did,
        client_session,
        "Client-authored Goal",
        Some(500),
    )
    .await
    .expect("create client Goal over HTTP");
    assert!(matches!(created, CreateGoalForSessionOutcome::Created(_)));
    let retried = crate::goal::create_goal_for_session_from_access(
        &access,
        did,
        client_session,
        "Client-authored Goal",
        Some(500),
    )
    .await
    .expect("retry client Goal over HTTP");
    assert!(matches!(
        retried,
        CreateGoalForSessionOutcome::Idempotent(_)
    ));
    assert_eq!(created.goal().doc_id, retried.goal().doc_id);
    for collection in ["Goal", "GoalCreationClaim"] {
        let session = escape_graphql_string(&client_session_doc);
        let rows = access.execute(&format!(r#"{{ {collection}(filter: {{session_doc_id: {{_eq: "{session}"}}}}) {{_docID session_doc_id agent_did creator_did parent_linked}} }}"#)).await.unwrap();
        let rows = rows["data"][collection].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["session_doc_id"], client_session_doc);
        assert_eq!(row["agent_did"], agent.did());
        assert_eq!(row["creator_did"], user.did());
        let modeled = atomic_goal_link_case("user", collection).await;
        assert_eq!(row["parent_linked"], modeled["initial"]["linked"]);
        let doc = escape_graphql_string(row["_docID"].as_str().unwrap());
        let history = access.execute(&format!(r#"{{_commits(docID: "{doc}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}}}}"#)).await.unwrap();
        let cid = history["data"]["_commits"][0]["cid"].as_str().unwrap();
        assert_eq!(
            verified_block_signer_as(&db.node, user.did(), cid).await,
            user.did()
        );
    }
}

#[tokio::test]
async fn goal_backed_submission_commits_goal_before_request_visibility_and_retries_idempotently() {
    let db = test_db("goal-backed-submit").await;
    let agent = managed_agent(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let create = signed_goal_backed_request(&db, &agent, "goal-backed-first").await;
    let first = crate::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("atomic goal submission");
    assert_eq!(first, crate::goal::GoalBackedRequestDisposition::Created);
    let binding = access.execute(
        "{ AgentSession { _docID agent_did requester_did } Goal { _docID session_doc_id agent_did creator_did parent_linked assignment_root_request_doc_id } GoalCreationClaim { _docID session_doc_id agent_did creator_did parent_linked } AgentRequest { _docID } }",
    ).await.unwrap();
    let session = &binding["data"]["AgentSession"][0];
    let request = &binding["data"]["AgentRequest"][0];
    assert_eq!(session["agent_did"], agent.did());
    assert_eq!(session["requester_did"], agent.did());
    for collection in ["Goal", "GoalCreationClaim"] {
        let row = &binding["data"][collection][0];
        assert_eq!(row["session_doc_id"], session["_docID"]);
        assert_eq!(row["agent_did"], agent.did());
        assert_eq!(row["creator_did"], agent.did());
        let modeled = atomic_goal_link_case("agent", collection).await;
        assert_eq!(row["parent_linked"], modeled["initial"]["linked"]);
        let doc = row["_docID"].as_str().unwrap().to_owned();
        let owners = access
            .transact_readonly("test.goal_backed_native_owner", |txn| {
                let doc = &doc;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc))
                        .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0].owner.as_ref().map(|owner| owner.as_str()),
            Some(agent.did())
        );
    }
    assert_eq!(
        binding["data"]["Goal"][0]["assignment_root_request_doc_id"],
        request["_docID"]
    );

    let response = access.execute(r#"{ Goal { goal_id } AgentRequest(filter: { retry_key: { _eq: "goal-submit:goal-session" } }) { request_id lifecycle_state } }"#)
        .await.expect("load atomic pair as selected Agent");
    assert_eq!(response["data"]["Goal"].as_array().unwrap().len(), 1);
    let requests = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["lifecycle_state"], "pending");

    let retry = crate::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("idempotent atomic retry");
    assert_eq!(retry, crate::goal::GoalBackedRequestDisposition::Idempotent);

    let committed_goal = access
        .transact_readonly("test.load_committed_goal", |txn| {
            Box::pin(crate::goal::load_canonical_goal_in_txn(
                txn,
                db.node_identity.did(),
                SESSION,
            ))
        })
        .await
        .expect("load committed goal")
        .expect("goal exists");
    access.write("test.goal_backed_progress", &format!(
        r#"mutation {{ update_Goal(docID: "{}", input: {{ status: "complete" }}) {{ _docID }} }}"#,
        escape_graphql_string(&committed_goal.doc_id),
    )).await.expect("simulate progress after an ambiguous commit acknowledgement");
    let late_retry = crate::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("exact retry remains idempotent after goal progress");
    assert_eq!(
        late_retry,
        crate::goal::GoalBackedRequestDisposition::Idempotent
    );
}

#[tokio::test]
async fn goal_backed_retry_rejects_changed_logical_request_fields() {
    let db = test_db("goal-backed-submit-fingerprint-conflict").await;
    let agent = managed_agent(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let create = signed_goal_backed_request(&db, &agent, "goal-backed-fingerprint").await;
    crate::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship one exact request",
        None,
        &create,
    )
    .await
    .expect("initial goal submission");

    let mut changed = create.clone();
    changed.input = serde_json::from_value(serde_json::json!({
        "queue": {"source": "goal", "policy": "coalesce", "key": "changed"}
    }))
    .unwrap();
    gents_session::request::admission::sign_agent_request_create(agent.as_ref(), &mut changed)
        .await
        .expect("re-sign changed request");
    let error = crate::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship one exact request",
        None,
        &changed,
    )
    .await
    .expect_err("changed retry semantics must conflict");
    assert!(error
        .to_string()
        .contains("different immutable request fields"));
}

#[tokio::test]
async fn goal_backed_submission_rejects_a_matching_terminal_goal_without_publishing() {
    let db = test_db("goal-backed-submit-terminal-goal").await;
    let agent = managed_agent(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let create = signed_goal_backed_request(&db, &agent, "goal-backed-terminal").await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Already terminal"),
        Some(GoalStatus::Complete),
        None,
        None,
    )
    .await
    .expect("set terminal goal");
    let error = crate::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Already terminal",
        None,
        &create,
    )
    .await
    .expect_err("terminal goal must reject a new first request");
    assert!(error.to_string().contains("non-active goal"));
    let response = access
        .execute("{ AgentRequest { request_id } GoalCreationClaim { creation_key } }")
        .await
        .expect("query terminal rollback as selected Agent");
    assert!(response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(response["data"]["GoalCreationClaim"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn goal_backed_submission_discards_goal_when_request_staging_fails() {
    let db = test_db("goal-backed-submit-rollback").await;
    let agent = managed_agent(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let mut invalid = signed_goal_backed_request(&db, &agent, "goal-backed-invalid").await;
    invalid.initial_lifecycle_state =
        gents_protocol::client_protocol::RequestLifecycleState::Processing;
    gents_session::request::admission::sign_agent_request_create(agent.as_ref(), &mut invalid)
        .await
        .expect("re-sign invalid request");
    let reserved = access
        .execute("{ AgentSession { _docID } }")
        .await
        .expect("load reserved session")["data"]["AgentSession"]
        .clone();
    let error = crate::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Never publish partially",
        None,
        &invalid,
    )
    .await
    .expect_err("invalid request must abort transaction");
    assert!(error.to_string().contains("pending state"));
    let response = access.execute("{ AgentSession { _docID } Goal { _docID } GoalCreationClaim { creation_key } AgentRequest { request_id } }")
        .await.expect("load rollback state as selected Agent");
    assert_eq!(
        response["data"]["AgentSession"], reserved,
        "the aborted transaction left the reserved session unchanged"
    );
    assert_eq!(reserved.as_array().map(Vec::len), Some(1));
    for collection in ["Goal", "GoalCreationClaim", "AgentRequest"] {
        assert!(
            response["data"][collection].as_array().unwrap().is_empty(),
            "{collection} escaped the aborted transaction"
        );
    }
}

#[tokio::test]
async fn concurrent_model_goal_create_converges_to_one_physical_goal() {
    let db = test_db("goal-model-create-concurrent").await;
    let (user, _agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let left_client = client.clone();
    let observer = client.clone();
    let did = db.node_identity.did().to_owned();
    let left_did = did.clone();
    let left = tokio::spawn(async move {
        crate::goal::create_goal_for_session_from_access(
            &left_client,
            &left_did,
            SESSION,
            "Concurrent objective",
            None,
        )
        .await
    });
    let right = tokio::spawn(async move {
        crate::goal::create_goal_for_session_from_access(
            &client,
            &did,
            SESSION,
            "Concurrent objective",
            None,
        )
        .await
    });
    left.await.expect("left task").expect("left create");
    right.await.expect("right task").expect("right create");
    assert_eq!(
        observer.execute("{ Goal { _docID } }").await.unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn any_newer_active_request_blocks_goal_continuation_for_the_whole_session() {
    let db = test_db("goal-session-idle").await;
    let (user, agent, access) = goal_recovery_actors(&db, "older-complete", "completed").await;
    goal_fixture_request_with_state_at(
        &db,
        &user,
        &agent,
        "newer-manual",
        "pending",
        "2026-11-04T00:01:00Z",
    )
    .await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Wait for session idleness"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err()
    );
    assert!(goal_children_from_access(&access).await.is_empty());
}

#[tokio::test]
async fn retired_queued_inputs_do_not_pause_completed_goal_execution() {
    for (name, state, reason) in [
        (
            "folded",
            "superseded",
            gents_session::request::lifecycle::FOLDED_REASON,
        ),
        (
            "replaced",
            "superseded",
            gents_session::request::lifecycle::REPLACED_REASON,
        ),
        (
            "removed",
            "interrupted",
            gents_session::request::lifecycle::REMOVED_REASON,
        ),
    ] {
        let db = test_db(&format!("goal-retired-input-{name}")).await;
        let (user, agent, access) =
            goal_recovery_actors(&db, "execution-parent", "completed").await;
        let parent = {
            let query =
                "{AgentRequest(filter: {request_id: {_eq: \"execution-parent\"}}) {_docID}}";
            access.execute(query).await.unwrap()["data"]["AgentRequest"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let input =
            goal_fixture_request_with_state(&db, &user, &agent, "zz-retired-input", state).await;
        access
            .write(
                "test.goal_retired_input",
                &format!(
                    r#"mutation {{ update_AgentRequest(docID: "{}", input: {{
                failure_reason: "{}", superseded_by_request: "execution-parent",
                superseded_by_request_doc_id: "{}"
            }}) {{ _docID }} }}"#,
                    escape_graphql_string(&input),
                    escape_graphql_string(reason),
                    escape_graphql_string(&parent),
                ),
            )
            .await
            .unwrap();
        set_goal_from_access(
            &access,
            db.node_identity.did(),
            SESSION,
            Some("Continue the existing objective"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let (mut source, _tx) = source_for_agent(&db, &agent).await;
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .unwrap_or_else(|_| panic!("{name}: the retired input prevented continuation"))
            .expect("continued Goal request");
        let goal = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(goal.parsed_status(), Some(GoalStatus::Active), "{name}");
        let children = goal_children_from_access(&access).await;
        assert_eq!(children.len(), 1, "{name}");
    }
}

#[tokio::test]
async fn interrupted_terminal_pauses_instead_of_self_continuing() {
    let db = test_db("goal-interrupted").await;
    let (_user, agent, access) =
        goal_recovery_actors(&db, "parent-interrupted", "interrupted").await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Respect human interruption"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent).await;
    wait_for_goal_transition_as(
        &access,
        &db.node,
        db.node_identity.did(),
        &mut source,
        "interrupted goal to pause",
        |goal| goal.parsed_status() == Some(GoalStatus::Paused),
    )
    .await;
    assert!(goal_children_from_access(&access).await.is_empty());
}

#[tokio::test]
async fn token_budget_materializes_one_wrapup_and_never_repeats_it() {
    let db = test_db("goal-budget-wrapup").await;
    let (user, agent_identity, access) =
        goal_recovery_actors(&db, "parent-budget", "completed").await;
    let parent_doc = access.execute("{AgentRequest(filter: {request_id: {_eq: \"parent-budget\"}}) {_docID session_doc_id}}").await.unwrap()["data"]["AgentRequest"][0].clone();
    let parent_doc_id = parent_doc["_docID"].as_str().unwrap();
    let session_doc_id = parent_doc["session_doc_id"].as_str().unwrap();
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Stop at the durable budget"),
        Some(GoalStatus::Active),
        Some(Some(10)),
        None,
    )
    .await
    .expect("set goal");
    goal_fixture_usage_for_node(
        &db.node,
        db.node_identity.did(),
        SESSION,
        &agent_identity,
        "parent-budget",
        parent_doc_id,
        "goal-budget-call",
        100,
        5,
        90,
    )
    .await;
    let foreign_dir = tempfile::tempdir().unwrap();
    let foreign_node = gents_store::identity::KeyIdentity::load_or_create(
        foreign_dir.path().join("foreign-node.key"),
        None,
    )
    .unwrap();
    seed_foreign_goal_usage(
        &access,
        &agent_identity,
        foreign_node.did(),
        parent_doc_id,
        session_doc_id,
    )
    .await;

    // The goal already existed when the usage was seeded: an idempotent
    // set_goal replay must leave the creation-time baseline and the charged
    // tokens untouched instead of resetting or re-deriving them.
    let replayed = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Stop at the durable budget"),
        Some(GoalStatus::Active),
        Some(Some(10)),
        None,
    )
    .await
    .expect("idempotent set goal replay");
    assert_eq!(replayed.token_usage_baseline, Some(0));
    assert_eq!(replayed.tokens_used, Some(0));
    assert_eq!(
        access.execute("{Goal {_docID}}").await.unwrap()["data"]["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent_identity).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out")
        .expect("wrapup intent");
    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 1);
    let user_children = goal_children_from_access(&db.client_access(&user)).await;
    assert_eq!(user_children.len(), 1);
    assert_eq!(user_children[0].doc_id, children[0].doc_id);
    let input = children[0].input.as_ref().expect("goal continuation input");
    assert_eq!(
        input.pointer("/goal_continuation/wrapup"),
        Some(&serde_json::json!(true))
    );

    set_request_state_as(&access, &children[0].doc_id, "completed").await;
    publish_owned_terminal_activity(
        &access,
        agent_identity.as_ref(),
        &children[0].doc_id,
        "final durable wrapup",
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_goal_as(&access, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::BudgetLimited));
    assert_eq!(goal.tokens_used, Some(105));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(goal.wrapup_completed, Some(true));
    assert_eq!(goal_children_from_access(&access).await.len(), 1);
    let child_identity = (
        children[0].doc_id.clone(),
        children[0].request_id.clone(),
        children[0].retry_key.clone(),
    );
    let continuation_sequence = goal.continuation_sequence;
    assert_eq!(
        session_token_usage(
            &db.node,
            &agent_identity.did().parse().unwrap(),
            db.node_identity.did(),
            SESSION
        )
        .await
        .expect("session usage before repeated recovery"),
        105,
    );
    drop(source);
    for _ in 0..2 {
        let recovered =
            gents_session::request::lifecycle::RequestLifecycle::recover_all_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(&agent_identity),
            )
            .await
            .expect("repeat lease recovery");
        assert_eq!(recovered.requests_recovered, 0);
        assert_eq!(recovered.responses_recovered, 0);
        let (mut restarted, _restart_tx) = source_for_agent(&db, &agent_identity).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
                .await
                .is_err(),
            "recovery and goal reconciliation must not emit a second wrapup",
        );
        let current = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(current.parsed_status(), Some(GoalStatus::BudgetLimited));
        assert_eq!(
            current.tokens_used,
            Some(105),
            "the same persisted inference usage must not be charged again"
        );
        assert_eq!(current.continuation_sequence, continuation_sequence);
        assert_eq!(current.wrapup_completed, Some(true));
        assert_eq!(
            session_token_usage(
                &db.node,
                &agent_identity.did().parse().unwrap(),
                db.node_identity.did(),
                SESSION
            )
            .await
            .unwrap(),
            105,
            "recovery must preserve the session charge, including cached input exactly once",
        );
        let after = goal_children_from_access(&access).await;
        assert_eq!(after.len(), 1);
        assert_eq!(
            (
                after[0].doc_id.clone(),
                after[0].request_id.clone(),
                after[0].retry_key.clone()
            ),
            child_identity
        );
    }
}

#[tokio::test]
async fn session_token_usage_charges_cached_input_as_part_of_the_total() {
    let db = test_db("goal-cached-input").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let doc = goal_fixture_request(&db, &user, &agent, "parent-cached").await;
    goal_fixture_usage_for_node(
        &db.node,
        db.node_identity.did(),
        SESSION,
        &agent,
        "parent-cached",
        &doc,
        "goal-cached-call",
        1000,
        50,
        800,
    )
    .await;
    let tokens = session_token_usage(
        &db.node,
        &agent.did().parse().unwrap(),
        db.node_identity.did(),
        SESSION,
    )
    .await
    .expect("session token usage");
    assert_eq!(tokens, 1050);
}

#[tokio::test]
async fn provider_usage_limit_moves_active_goal_to_usage_limited() {
    let db = test_db("goal-provider-usage-limit").await;
    let (_user, agent, access) = goal_recovery_actors(&db, "usage-limited-request", "failed").await;
    let parent = access.execute("{AgentRequest(filter: {request_id: {_eq: \"usage-limited-request\"}}) {_docID session_doc_id}}").await.unwrap();
    let parent = &parent["data"]["AgentRequest"][0];
    seed_failed_call_from_access(
        &access,
        &agent,
        db.node_identity.did(),
        parent["_docID"].as_str().unwrap(),
        parent["session_doc_id"].as_str().unwrap(),
        "usage-limited-call",
        "inference",
        1,
        "2026-07-15T00:00:01Z",
        "provider insufficient_quota: credit balance exhausted",
    )
    .await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Stop on provider quota exhaustion"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent).await;
    let goal = wait_for_goal_transition_as(
        &access,
        &db.node,
        db.node_identity.did(),
        &mut source,
        "provider usage limit",
        |goal| goal.parsed_status() == Some(GoalStatus::UsageLimited),
    )
    .await;
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("insufficient_quota")));
}

#[tokio::test]
async fn compaction_usage_limit_moves_goal_to_usage_limited() {
    let db = test_db("goal-compaction-usage-limit").await;
    let (_user, agent, access) = goal_recovery_actors(&db, "usage-limited-request", "failed").await;
    let request = access.execute("{AgentRequest(filter: {request_id: {_eq: \"usage-limited-request\"}}) {_docID session_doc_id}}").await.unwrap();
    let parent = &request["data"]["AgentRequest"][0];
    let request_doc = parent["_docID"].as_str().unwrap();
    let session_doc = parent["session_doc_id"].as_str().unwrap();
    seed_failed_call_from_access(
        &access,
        &agent,
        db.node_identity.did(),
        request_doc,
        session_doc,
        "compaction-call",
        "compaction",
        1,
        "2026-07-15T00:00:10Z",
        RENDERED_LIMIT,
    )
    .await;
    let last_failure = usage_limited_goal_after_failed_request(&db, &agent, &access).await;
    assert!(
        last_failure
            .as_deref()
            .is_some_and(|reason| reason.contains("resets at 2026-07-15T05:00:00Z")),
        "{last_failure:?}"
    );
}

#[tokio::test]
async fn a_later_limited_call_wins_over_an_earlier_retry() {
    let db = test_db("goal-later-limited-call").await;
    let (_user, agent, access) = goal_recovery_actors(&db, "usage-limited-request", "failed").await;
    let request = access.execute("{AgentRequest(filter: {request_id: {_eq: \"usage-limited-request\"}}) {_docID session_doc_id}}").await.unwrap();
    let parent = &request["data"]["AgentRequest"][0];
    let request_doc = parent["_docID"].as_str().unwrap();
    let session_doc = parent["session_doc_id"].as_str().unwrap();
    seed_failed_call_from_access(
        &access,
        &agent,
        db.node_identity.did(),
        request_doc,
        session_doc,
        "earlier-retry",
        "inference",
        2,
        "2026-07-15T00:00:05Z",
        "HttpError: Invalid status code 500",
    )
    .await;
    seed_failed_call_from_access(
        &access,
        &agent,
        db.node_identity.did(),
        request_doc,
        session_doc,
        "later-compaction",
        "compaction",
        1,
        "2026-07-15T00:00:10Z",
        RENDERED_LIMIT,
    )
    .await;
    let last_failure = usage_limited_goal_after_failed_request(&db, &agent, &access).await;
    assert!(
        last_failure
            .as_deref()
            .is_some_and(|reason| reason.contains("resets at 2026-07-15T05:00:00Z")),
        "{last_failure:?}"
    );
}

#[tokio::test]
async fn a_database_update_leaves_a_passed_reset_to_the_rescan_tick() {
    let db = test_db("goal-reset-update").await;
    let (_user, agent, access) = seed_a_passed_reset(&db, true).await;
    publish_node_readiness_for_agent(
        &db,
        gents_protocol::row::NodeReadinessProcessState::Ready,
        None,
        agent.did(),
    )
    .await;
    let updates = MockUpdateSubscriptionSource::new();
    let (_snapshot_tx, rx) = watch::channel(agent_snapshot(&db, &agent));
    let mut source = GoalSource::with_subscription_source(
        Arc::new(updates.clone()),
        rx,
        db.node.clone(),
        CancellationToken::new(),
    )
    .with_rescan_interval(Duration::from_secs(3600));
    let pass = Duration::from_millis(300);
    assert!(tokio::time::timeout(pass, source.next_fire())
        .await
        .is_err());
    assert_eq!(
        goal_and_children(&db, &access).await,
        (Some(GoalStatus::UsageLimited), 0)
    );
    let goal_collection_id = gents_store::subscription_source::collection_ids(&db.node, ["Goal"])
        .unwrap()
        .remove(0);
    updates.publish_update(goal_collection_id, "goal-doc");
    assert!(tokio::time::timeout(pass, source.next_fire())
        .await
        .is_err());
    assert_eq!(
        goal_and_children(&db, &access).await,
        (Some(GoalStatus::UsageLimited), 0)
    );
}

#[tokio::test]
async fn opted_in_goal_resumes_at_its_reported_reset() {
    assert_eq!(
        goal_after_a_passed_reset("goal-reset-opted-in", true).await,
        (Some(GoalStatus::Active), 1)
    );
    assert_eq!(
        goal_after_a_passed_reset("goal-reset-not-opted-in", false).await,
        (Some(GoalStatus::UsageLimited), 0)
    );
}

#[tokio::test]
async fn openrouter_credits_402_moves_active_goal_to_usage_limited() {
    let db = test_db("goal-openrouter-credits-402").await;
    let (_user, agent, access) = goal_recovery_actors(&db, "usage-limited-request", "failed").await;
    let request = access.execute("{AgentRequest(filter: {request_id: {_eq: \"usage-limited-request\"}}) {_docID session_doc_id}}").await.unwrap();
    let parent = &request["data"]["AgentRequest"][0];
    let reason = gents_loop::provider_limit::persisted_failure_reason(
        r#"HttpError: Invalid status code 402 Payment Required with message: {"error":{"code":402,"message":"Insufficient credits. Add more using https://openrouter.ai/credits"}}"#,
        chrono::Utc::now(),
    );
    seed_failed_call_from_access(
        &access,
        &agent,
        db.node_identity.did(),
        parent["_docID"].as_str().unwrap(),
        parent["session_doc_id"].as_str().unwrap(),
        "openrouter-credits-call",
        "inference",
        1,
        "2026-07-15T00:00:01Z",
        &reason,
    )
    .await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Stop on OpenRouter credits exhaustion"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_goal_as(&access, db.node_identity.did())
        .await
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::UsageLimited));
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("Insufficient credits")));
}

#[tokio::test]
async fn failed_wrapup_retries_twice_then_is_durably_abandoned() {
    let db = test_db("goal-wrapup-retry-bound").await;
    let (_user, agent, access) =
        goal_recovery_actors(&db, "parent-wrapup-retry", "completed").await;
    let parent_doc = access
        .execute("{AgentRequest(filter: {request_id: {_eq: \"parent-wrapup-retry\"}}) {_docID}}")
        .await
        .unwrap()["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Bound failed wrap-up retries"),
        Some(GoalStatus::Active),
        Some(Some(1)),
        None,
    )
    .await
    .expect("set goal");
    goal_fixture_usage(
        &db,
        &agent,
        "parent-wrapup-retry",
        &parent_doc,
        "wrapup-budget-call",
        2,
        0,
    )
    .await;

    let (mut source, _snapshot_tx) = source_for_agent(&db, &agent).await;
    for expected_children in 1..=3 {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .unwrap_or_else(|_| panic!("goal source timed out before child {expected_children}"))
            .expect("wrapup or retry intent");
        let children = goal_children_from_access(&access).await;
        assert_eq!(children.len(), expected_children);
        let child = children
            .iter()
            .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
            .expect("new pending wrap-up child");
        set_request_state_as(&access, &child.doc_id, "failed").await;
    }

    let goal = wait_for_goal_transition_as(
        &access,
        &db.node,
        db.node_identity.did(),
        &mut source,
        "failed wrap-up abandonment",
        |goal| {
            goal.parsed_status() == Some(GoalStatus::BudgetLimited)
                && goal.wrapup_completed == Some(true)
        },
    )
    .await;
    assert_eq!(goal.infrastructure_retry_count, Some(2));
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("after 2 retries")));
    assert!(
        tokio::time::timeout(RESCAN * 5, source.next_fire())
            .await
            .is_err(),
        "bounded failed wrap-up must not spawn a fourth child"
    );
    assert_eq!(goal_children_from_access(&access).await.len(), 3);
}

#[tokio::test]
async fn continuation_sequence_exhaustion_rejects_claims_without_retry_charge() {
    for retry in [false, true] {
        let name = if retry {
            "goal-retry-sequence-exhaustion"
        } else {
            "goal-sequence-exhaustion"
        };
        let db = test_db(name).await;
        let (user, agent) = goal_creation_actors(&db).await;
        goal_fixture_request(&db, &user, &agent, "last-parent").await;
        goal_fixture_request(&db, &user, &agent, "must-not-be-claimed").await;
        let access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let initial = set_goal_from_access(
            &access,
            db.node_identity.did(),
            SESSION,
            Some("Never reuse an exhausted continuation sequence"),
            Some(GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        seed_goal_fields_from_access(
            &access,
            &initial,
            &format!(
                "continuation_sequence: {}, infrastructure_retry_count: 0, last_failure: null, last_continued_from_request_id: null",
                i64::MAX - 1
            ),
        )
        .await;
        let before = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        let claimed = if retry {
            crate::goal::claim_retry_continuation_from_access(
                &agent_access(&db, &agent),
                &before,
                "last-parent",
                1,
                "failed",
            )
            .await
        } else {
            crate::goal::claim_continuation_from_access(
                &agent_access(&db, &agent),
                &before,
                "last-parent",
            )
            .await
        };
        assert!(claimed.expect("MAX-1 must permit one final sequence advance"));
        let exhausted = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(exhausted.continuation_sequence(), i64::MAX);
        assert_eq!(
            exhausted.last_continued_from_request_id.as_deref(),
            Some("last-parent")
        );
        assert_eq!(exhausted.infrastructure_retry_count, Some(i64::from(retry)));
        let rejected = if retry {
            crate::goal::claim_retry_continuation_from_access(
                &agent_access(&db, &agent),
                &exhausted,
                "must-not-be-claimed",
                2,
                "must-not-be-charged",
            )
            .await
        } else {
            crate::goal::claim_continuation_from_access(
                &agent_access(&db, &agent),
                &exhausted,
                "must-not-be-claimed",
            )
            .await
        };
        assert!(
            rejected.is_err(),
            "exhaustion must fail explicitly: {rejected:?}"
        );
        let after = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&exhausted).unwrap(),
            "rejected claim must not change watermark, retry charge, failure, or timestamp"
        );
    }
}

#[tokio::test]
async fn stale_usage_refresh_preserves_completed_goal_time_accounting() {
    let db = test_db("goal-stale-usage-after-completion").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let initial = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("A stale observer cannot restart completed goal accounting"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_fields_from_access(
        &access,
        &initial,
        "active_time_seconds: 7, active_started_at: null",
    )
    .await;
    let stale = load_goal_as(&access, db.node_identity.did()).await.unwrap();
    set_goal_from_access(
        &client,
        db.node_identity.did(),
        SESSION,
        None,
        Some(GoalStatus::Complete),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_fields_from_access(
        &access,
        &stale,
        "active_time_seconds: 73, active_started_at: null",
    )
    .await;
    let completed = load_goal_as(&access, db.node_identity.did()).await.unwrap();
    assert_eq!(completed.status, "complete");
    assert_eq!(completed.active_time_seconds, Some(73));
    assert!(completed.active_started_at.is_none());

    crate::goal::refresh_goal_usage_from_access(&access, &stale)
        .await
        .expect("stale usage observation must safely skip obsolete accounting writes");
    let after = load_goal_as(&access, db.node_identity.did()).await.unwrap();
    assert_eq!(after.status, "complete");
    assert_eq!(after.active_time_seconds, completed.active_time_seconds);
    assert_eq!(after.active_started_at, completed.active_started_at);
    assert_eq!(
        after.continuation_sequence(),
        completed.continuation_sequence()
    );
}

#[tokio::test]
async fn operator_resume_publishes_one_lineage_child_and_fences_old_pause() {
    let db = test_db("operator-resume-lineage").await;
    let did = db.node_identity.did();
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let (parent_doc, source_doc) = seed_lineage_parent(
        &db,
        &user,
        &agent,
        &access,
        "resume-parent",
        "interrupted",
        "resume-workspace",
        "resume-seal",
    )
    .await;
    let stale_active = set_goal_from_access(
        &client,
        did,
        SESSION,
        Some("Complete graph work"),
        Some(GoalStatus::Active),
        Some(Some(1000)),
        None,
    )
    .await
    .unwrap();
    set_goal_from_access(
        &client,
        did,
        SESSION,
        None,
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    let receipt = crate::goal::resume_goal_request(&client, &user, did, SESSION, "resume-parent")
        .await
        .unwrap();
    assert!(receipt.created);
    let goal = load_goal_as(&client, did).await.unwrap();
    assert_eq!(receipt.goal_id, goal.goal_id);
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.continuation_sequence(), 1);
    assert_eq!(
        goal.last_continued_from_request_id.as_deref(),
        Some("resume-parent")
    );
    assert_eq!(goal.token_budget, Some(1000));
    assert_eq!(goal_children_from_access(&client).await.len(), 1);
    let child = operator_resume_child_row_from_access(&client, &receipt.doc_id).await;
    assert_eq!(child["request_id"], receipt.request_id);
    assert_eq!(child["node_did"], did);
    assert_eq!(child["requester_did"], user.did());
    assert_eq!(child["admission_signer_did"], user.did());
    assert_eq!(child["admission_kind"], "client");
    assert!(child["runtime_issuer_did"].is_null());
    assert_eq!(child["session_id"], SESSION);
    assert_eq!(child["agent_did"], agent.did());
    assert_eq!(child["lifecycle_state"], "pending");
    assert_eq!(child["execution_origin"], "scheduled");
    assert!(child["runtime_source_request_id"].is_null());
    assert!(child["admission_signature"]
        .as_str()
        .is_some_and(|v| !v.is_empty()));
    assert_eq!(child["caused_by_trigger_id"], goal.goal_id);
    assert_eq!(child["caused_by_trigger_kind"], "goal");
    assert!(child["caused_by_trigger_doc_id"].is_null());
    assert_eq!(child["caused_by_correlation"], "original-graph-correlation");
    assert_eq!(child["caused_by_source_doc_id"], source_doc);
    assert_eq!(
        child["caused_by_trigger_context"],
        r#"{"source_fields":{"artifact":"original"}}"#
    );
    assert_eq!(child["caused_by_parent_request_id"], "resume-parent");
    assert_eq!(child["caused_by_parent_request_doc_id"], parent_doc);
    assert!(child["caused_by_parent_tool_call_id"].is_null());
    assert!(child["caused_by_parent_tool_call_doc_id"].is_null());
    assert_eq!(child["request_hop"], 2);
    assert_eq!(child["workspace_id"], "resume-workspace");
    assert_eq!(child["workspace_authority"], "readOnly");
    assert_eq!(child["workspace_owner_node_did"], did);
    assert_eq!(child["workspace_seal_hash"], "resume-seal");
    assert!(
        !crate::goal::update_goal_fields_if_status_from_access(
            &agent_access(&db, &agent),
            &stale_active,
            GoalStatus::Active,
            r#"status: "paused""#
        )
        .await
        .unwrap(),
        "pre-resume Active snapshot must lose ABA"
    );
    assert_eq!(
        load_goal_as(&client, did).await.unwrap().parsed_status(),
        Some(GoalStatus::Active)
    );
}

#[tokio::test]
async fn resume_resets_blocked_audit_identity_and_count() {
    let db = test_db("goal-resume-audit-reset").await;
    let did = db.node_identity.did();
    let (user, agent) = goal_creation_actors(&db).await;
    let access = db.client_access(&user);
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    seed_operator_resume_parent_with_actors(&db, &user, &agent, "request-3").await;
    let goal = set_goal_from_access(
        &agent_access,
        did,
        SESSION,
        Some("Resume with a fresh blocked audit"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_fields_from_access(&agent_access, &goal,
        r#"status: "blocked", consecutive_blocked_audits: 3, last_blocked_request_id: "request-3", last_blocked_reason: "needs approval", active_started_at: null"#).await;
    let receipt = crate::goal::resume_goal_request(&access, &user, did, SESSION, "request-3")
        .await
        .unwrap();
    assert!(receipt.created);
    let resumed = load_goal_as(&access, did).await.unwrap();
    assert_eq!(resumed.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(resumed.consecutive_blocked_audits, Some(0));
    assert_eq!(resumed.last_blocked_request_id, None);
    assert_eq!(resumed.last_blocked_reason, None);
    assert_eq!(goal_children_from_access(&agent_access).await.len(), 1);
}

#[tokio::test]
async fn concurrent_operator_resumes_publish_one_child() {
    let db = test_db("operator-resume-concurrent").await;
    let did = db.node_identity.did();
    let (user, agent) = goal_creation_actors(&db).await;
    let access = db.client_access(&user);
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    seed_operator_resume_parent_with_actors(&db, &user, &agent, "concurrent-parent").await;
    set_goal_from_access(
        &access,
        did,
        SESSION,
        Some("One operator continuation"),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    let (left, right) = tokio::join!(
        crate::goal::resume_goal_request(&access, &user, did, SESSION, "concurrent-parent"),
        crate::goal::resume_goal_request(&access, &user, did, SESSION, "concurrent-parent"),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_ne!(left.created, right.created);
    assert_eq!(
        (left.request_id, left.doc_id),
        (right.request_id, right.doc_id)
    );
    assert_eq!(goal_children_from_access(&agent_access).await.len(), 1);
    let goal = load_goal_as(&access, did).await.unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.continuation_sequence(), 1);
}

#[tokio::test]
async fn operator_resume_rejects_corrupted_child_receipt_without_reactivation() {
    let db = test_db("operator-resume-corrupt-receipt").await;
    let did = db.node_identity.did();
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    seed_operator_resume_parent_with_actors(&db, &user, &agent, "corrupt-parent").await;
    set_goal_from_access(
        &client,
        did,
        SESSION,
        Some("Original signed work"),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    let first = crate::goal::resume_goal_request(&client, &user, did, SESSION, "corrupt-parent")
        .await
        .unwrap();
    set_request_state_as(&access, &first.doc_id, "interrupted").await;
    let goal_before = set_goal_from_access(
        &client,
        did,
        SESSION,
        None,
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    let original = client.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
            request_id purpose node_did requester_did session_doc_id agent_doc_id admission_kind admission_signer_did admission_signature enrollment_request_id enrollment_request_digest enrollment_admin_did enrollment_authorization_sequence enrollment_authorization_expires_at runtime_issuer_did runtime_source_request_id runtime_source_kind agent_did session_id retry_parent_request retry_parent_request_doc_id retry_root_request retry_key content max_total_tokens input execution_origin caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind caused_by_correlation caused_by_trigger_context caused_by_source_doc_id created_at retry_count max_retries valid_until request_hop caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id workspace_id workspace_authority workspace_owner_node_did workspace_seal_hash lifecycle_state
        }} }}"#, gents_store::graphql::escape_graphql_string(&first.doc_id),
    )).await.unwrap();
    let mut forged = original["data"]["AgentRequest"][0].clone();
    forged["content"] = serde_json::json!("unsigned replacement");
    let agent_rows = client
        .execute(&format!(
            r#"{{Agent(filter: {{agent_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}}}}"#,
            escape_graphql_string(agent.did())
        ))
        .await
        .unwrap();
    let agent_doc = agent_rows["data"]["Agent"][0]["_docID"].as_str().unwrap();
    client
        .add_actor_relationship(
            "Agent",
            agent_doc,
            "administrator",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let removed = access
        .write(
            "test.corrupt_goal_receipt.delete",
            &format!(
                r#"mutation {{delete_AgentRequest(docID: "{}") {{_docID}}}}"#,
                escape_graphql_string(&first.doc_id),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        removed["data"]["delete_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let session_doc = forged["session_doc_id"].as_str().unwrap().to_owned();
    let agent_doc = forged["agent_doc_id"].as_str().unwrap().to_owned();
    let agent_did = agent.did();
    let forged_doc_id = access
        .transact_write("test.corrupt_goal_receipt.insert", |txn| {
            let forged = &forged;
            let session_doc = &session_doc;
            let agent_doc = &agent_doc;
            Box::pin(async move {
                let input = gents_protocol::graphql::graphql_input_literal(forged)?;
                let result = txn
                    .execute(&format!(
                        "mutation {{add_AgentRequest(input: {input}) {{_docID}}}}"
                    ))
                    .await?;
                let doc = gents_store::graphql::created_doc_id(&result, "AgentRequest")?;
                txn.add_resource_relationship("AgentRequest", &doc, "parent", "agents", agent_doc)
                    .await?;
                txn.add_resource_relationship(
                    "AgentRequest",
                    &doc,
                    "session",
                    "sessions",
                    session_doc,
                )
                .await?;
                txn.add_actor_relationship(
                    "AgentRequest",
                    &doc,
                    "working_agent",
                    &agent_did.parse()?,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .unwrap();
    let child_before = operator_resume_child_row_from_access(&client, &forged_doc_id).await;
    assert_eq!(child_before["content"], "unsigned replacement");
    assert!(
        crate::goal::resume_goal_request(&client, &user, did, SESSION, "corrupt-parent",)
            .await
            .is_err(),
        "a matching retry key must not authenticate changed content"
    );
    let goal_after = load_goal_as(&client, did).await.unwrap();
    assert_eq!(
        serde_json::to_value(goal_after).unwrap(),
        serde_json::to_value(goal_before).unwrap()
    );
    assert_eq!(
        operator_resume_child_row_from_access(&client, &forged_doc_id).await,
        child_before
    );
    assert_eq!(goal_children_from_access(&client).await.len(), 1);
}

#[tokio::test]
async fn operator_resume_rejects_older_unfinished_siblings_without_mutation() {
    for (label, state) in [
        ("missing", None),
        ("unknown", Some("unknownState")),
        ("workspace", Some("workspaceBindingPending")),
    ] {
        let db = test_db(&format!("operator-resume-unfinished-{label}")).await;
        let did = db.node_identity.did();
        let (user, agent) = goal_creation_actors(&db).await;
        let client = db.client_access(&user);
        let agent_access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let sibling = seed_operator_resume_parent_with_actors(&db, &user, &agent, "a-older").await;
        seed_operator_resume_parent_with_actors(&db, &user, &agent, "z-parent").await;
        let state = state
            .map(|value| format!("\"{}\"", gents_store::graphql::escape_graphql_string(value)))
            .unwrap_or_else(|| "null".to_owned());
        let response = agent_access.write("test.resume_sibling_state", &format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: {state} }}) {{ _docID }} }}"#,
            gents_store::graphql::escape_graphql_string(&sibling)
        )).await.unwrap();
        assert_eq!(
            response["data"]["update_AgentRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let before = set_goal_from_access(
            &client,
            did,
            SESSION,
            Some("Do not bypass unfinished sibling work"),
            Some(GoalStatus::Paused),
            None,
            None,
        )
        .await
        .unwrap();
        let error = crate::goal::resume_goal_request(&client, &user, did, SESSION, "z-parent")
            .await
            .expect_err("every unfinished sibling must block resume");
        let detail = format!("{error:#}");
        assert!(
            !detail.contains("no longer the latest"),
            "wrong guard masked unfinished row: {detail}"
        );
        if label != "unknown" {
            assert!(detail.contains("unfinished"), "unexpected guard: {detail}");
        }
        assert!(goal_children_from_access(&agent_access).await.is_empty());
        let after = load_goal_as(&client, did).await.unwrap();
        assert_eq!(
            serde_json::to_value(after).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}

#[tokio::test]
async fn operator_resume_preserves_pending_wrapup_in_child_policy() {
    let db = test_db("operator-resume-retained-wrapup").await;
    let did = db.node_identity.did();
    let (user, agent) = goal_creation_actors(&db).await;
    let client = db.client_access(&user);
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    seed_operator_resume_parent_with_actors(&db, &user, &agent, "wrapup-parent").await;
    let paused = set_goal_from_access(
        &client,
        did,
        SESSION,
        Some("Finish the already requested wrap-up"),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    seed_goal_fields_from_access(
        &agent_access,
        &paused,
        "wrapup_requested: true, wrapup_completed: false",
    )
    .await;
    let receipt = crate::goal::resume_goal_request(&client, &user, did, SESSION, "wrapup-parent")
        .await
        .unwrap();
    let goal = load_goal_as(&client, did).await.unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::BudgetLimited));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(goal.wrapup_completed, Some(false));
    let child = operator_resume_child_row_from_access(&client, &receipt.doc_id).await;
    let input = &child["input"];
    assert_eq!(
        input
            .pointer("/goal_continuation/wrapup")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let content = child["content"].as_str().unwrap();
    assert!(content.contains("one final wrap-up turn"), "{content}");
    assert!(
        content.contains("Do not expect another automatic continuation"),
        "{content}"
    );
}

#[tokio::test]
async fn goal_source_continues_same_second_child_that_sorts_before_root() {
    for parent_id in ["graph-parent", "task-goal-request:parent"] {
        let db = test_db(&format!("goal-head-order-{}", parent_id.replace(':', "-"))).await;
        let (user, agent) = goal_creation_actors(&db).await;
        let access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let (child_id, _) =
            seed_same_second_canonical_goal_child(&db, &user, &agent, &access, parent_id, false)
                .await;
        let (mut controller, _snapshot_tx) = source_for_agent(&db, &agent).await;
        let intent = tokio::time::timeout(Duration::from_secs(2), controller.next_fire())
            .await
            .expect("causal child must not be hidden by lexical root ordering")
            .expect("continuation intent");
        let next_id = intent
            .pre_materialized_request_id
            .expect("materialized child");
        let children = goal_children_from_access(&access).await;
        let next = children
            .iter()
            .find(|row| row.request_id == next_id)
            .unwrap();
        assert_eq!(
            next.caused_by_parent_request_id.as_deref(),
            Some(child_id.as_str())
        );
        let goal = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(goal.continuation_sequence(), 2);
        assert_eq!(
            goal.last_continued_from_request_id.as_deref(),
            Some(child_id.as_str())
        );
    }
}

#[tokio::test]
async fn operator_resume_accepts_same_second_child_that_sorts_before_root() {
    for parent_id in ["graph-parent", "task-goal-request:parent"] {
        let db = test_db(&format!(
            "resume-head-order-{}",
            parent_id.replace(':', "-")
        ))
        .await;
        let (user, agent) = goal_creation_actors(&db).await;
        let access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let client = db.client_access(&user);
        let (child_id, child_doc) =
            seed_same_second_canonical_goal_child(&db, &user, &agent, &access, parent_id, true)
                .await;
        let receipt = crate::goal::resume_goal_request(
            &client,
            &user,
            db.node_identity.did(),
            SESSION,
            &child_id,
        )
        .await
        .expect("canonical child is latest despite its lexical position");
        let next = operator_resume_child_row_from_access(&client, &receipt.doc_id).await;
        assert_eq!(next["caused_by_parent_request_id"], child_id);
        assert_eq!(next["caused_by_parent_request_doc_id"], child_doc);
        let goal = load_goal_as(&client, db.node_identity.did()).await.unwrap();
        assert_eq!(goal.continuation_sequence(), 2);
        assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    }
}

#[tokio::test]
async fn unready_behavior_defers_goal_continuation_without_writes_or_retry_charge() {
    use gents_protocol::row::{
        AgentReadinessUnavailableReason as Reason, NodeReadinessProcessState as Process,
    };
    let db = test_db("goal-readiness-wait").await;
    let (_user, agent, access) =
        goal_recovery_actors(&db, "parent-waits-for-readiness", "completed").await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Wait for the behavior instead of spending retries"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    publish_node_readiness_for_agent(&db, Process::Recovering, None, agent.did()).await;
    let before =
        serde_json::to_value(load_goal_as(&access, db.node_identity.did()).await.unwrap()).unwrap();
    let (mut source, _tx) = unready_source_for_agent(&db, &agent);

    for (process, reason) in [
        (Process::Recovering, None),
        (Process::Ready, Some(Reason::RuntimeConfigurationInvalid)),
        (Process::Ready, Some(Reason::BackendTemporarilyUnavailable)),
    ] {
        publish_node_readiness_for_agent(&db, process, reason, agent.did()).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), source.next_fire())
                .await
                .is_err(),
            "{process:?}/{reason:?}: an unready behavior must not receive a continuation"
        );
        let waiting =
            serde_json::to_value(load_goal_as(&access, db.node_identity.did()).await.unwrap())
                .unwrap();
        assert!(
            waiting["last_failure"]
                .as_str()
                .is_some_and(|reason| reason.starts_with(crate::goal::GOAL_READINESS_WAIT_PREFIX)),
            "{process:?}/{reason:?}: the wait must be recorded: {waiting}"
        );
        for field in [
            "status",
            "continuation_sequence",
            "infrastructure_retry_count",
            "tokens_used",
        ] {
            assert_eq!(
                waiting[field], before[field],
                "{process:?}/{reason:?}: {field}"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), source.next_fire())
                .await
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(load_goal_as(&access, db.node_identity.did()).await.unwrap())
                .unwrap(),
            waiting,
            "{process:?}/{reason:?}: an unchanged wait must not rewrite the Goal"
        );
        assert!(goal_children_from_access(&access).await.is_empty());
    }

    publish_node_readiness_for_agent(&db, Process::Ready, None, agent.did()).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after readiness recovered")
        .expect("goal continuation intent");
    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].session_id, SESSION);
    assert_eq!(children[0].agent_did.as_deref(), Some(agent.did()));
    assert_eq!(
        children[0].caused_by_parent_request_id.as_deref(),
        Some("parent-waits-for-readiness")
    );
    let goal = load_goal_as(&access, db.node_identity.did()).await.unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.continuation_sequence(), 1);
    assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
    assert_eq!(goal.last_failure, None);

    drop(source);
    let (mut restarted, _restart_tx) = source_for_agent(&db, &agent).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
            .await
            .is_err(),
        "restart must not duplicate the recovered continuation"
    );
    assert_eq!(goal_children_from_access(&access).await.len(), 1);
}

#[tokio::test]
async fn readiness_rejected_goal_children_never_spend_the_retry_budget() {
    use gents_protocol::row::{
        AgentReadinessUnavailableReason as Reason, NodeReadinessProcessState as Process,
    };
    let db = test_db("goal-readiness-race").await;
    let (_user, agent, access) = goal_recovery_actors(&db, "parent-before-race", "completed").await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Survive readiness races"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    let (mut source, _tx) = source_for_agent(&db, &agent).await;

    for round in 1..=(crate::goal::MAX_INFRASTRUCTURE_RETRIES as usize + 1) {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .unwrap_or_else(|_| panic!("round {round}: goal source timed out"))
            .expect("goal continuation intent");
        let children = goal_children_from_access(&access).await;
        assert_eq!(children.len(), round);
        let pending = children
            .iter()
            .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
            .expect("pending goal child");
        assert_eq!(
            pending.input.as_ref().unwrap()["goal_continuation"]["wrapup"],
            false
        );

        publish_node_readiness_for_agent(
            &db,
            Process::Ready,
            Some(Reason::RuntimeConfigurationInvalid),
            agent.did(),
        )
        .await;
        reject_before_claim(
            &access,
            &pending.doc_id,
            Reason::RuntimeConfigurationInvalid.public_message(),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), source.next_fire())
                .await
                .is_err(),
            "round {round}: a rejected child must wait for readiness"
        );
        let goal = load_goal_as(&access, db.node_identity.did()).await.unwrap();
        assert_eq!(
            goal.parsed_status(),
            Some(GoalStatus::Active),
            "round {round}"
        );
        assert_eq!(
            goal.infrastructure_retry_count.unwrap_or_default(),
            0,
            "round {round}"
        );
        publish_node_readiness_for_agent(&db, Process::Ready, None, agent.did()).await;
    }

    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after the final race")
        .expect("goal continuation intent");
    let children = goal_children_from_access(&access).await;
    let pending = children
        .iter()
        .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
        .expect("pending goal child");
    reject_before_claim(
        &access,
        &pending.doc_id,
        Reason::RuntimeConfigurationInvalid.public_message(),
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), source.next_fire())
            .await
            .is_err(),
        "stale readiness must not re-issue a rejected child"
    );
    assert_eq!(
        goal_children_from_access(&access).await.len(),
        children.len()
    );
    let waiting = load_goal_as(&access, db.node_identity.did()).await.unwrap();
    assert_eq!(waiting.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(waiting.infrastructure_retry_count.unwrap_or_default(), 0);
    assert!(waiting
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("not been republished since its rejection")));
    publish_node_readiness_for_agent(&db, Process::Ready, None, agent.did()).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after readiness was republished")
        .expect("goal continuation intent");
    assert_eq!(
        goal_children_from_access(&access).await.len(),
        children.len() + 1
    );
    let goal = load_goal_as(&access, db.node_identity.did()).await.unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
    assert_eq!(goal.last_failure, None);
}

#[tokio::test]
async fn claimed_continuation_waits_for_readiness_before_materializing() {
    use gents_protocol::row::NodeReadinessProcessState as Process;
    let db = test_db("goal-claimed-readiness").await;
    let (_user, agent, access) =
        goal_recovery_actors(&db, "parent-claimed-waits", "completed").await;
    let goal = set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Hold a durable claim until the behavior is ready"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("set goal");
    assert!(crate::goal::claim_continuation_from_access(
        &agent_access(&db, &agent),
        &goal,
        "parent-claimed-waits"
    )
    .await
    .expect("claim continuation"));
    publish_node_readiness_for_agent(&db, Process::Recovering, None, agent.did()).await;
    let claimed =
        serde_json::to_value(load_goal_as(&access, db.node_identity.did()).await.unwrap()).unwrap();
    let (mut source, _tx) = unready_source_for_agent(&db, &agent);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), source.next_fire())
            .await
            .is_err(),
        "a claimed child must not be published to an unready behavior"
    );
    assert!(goal_children_from_access(&access).await.is_empty());
    assert_eq!(
        serde_json::to_value(load_goal_as(&access, db.node_identity.did()).await.unwrap()).unwrap(),
        claimed,
        "waiting must leave the durable claim untouched"
    );

    publish_node_readiness_for_agent(&db, Process::Ready, None, agent.did()).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after readiness recovered")
        .expect("claimed continuation");
    let children = goal_children_from_access(&access).await;
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].caused_by_parent_request_id.as_deref(),
        Some("parent-claimed-waits")
    );
}

#[tokio::test]
async fn closed_goal_notifications_keep_durable_continuation_and_cancellation_live() {
    let db = test_db("goal-closed-notifications").await;
    let (user, agent) = goal_creation_actors(&db).await;
    let access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    publish_node_readiness_for_agent(
        &db,
        gents_protocol::row::NodeReadinessProcessState::Ready,
        None,
        agent.did(),
    )
    .await;
    let updates = MockUpdateSubscriptionSource::new();
    updates.close();
    let (_snapshot_tx, rx) = watch::channel(agent_snapshot(&db, &agent));
    let cancel = CancellationToken::new();
    let mut source = GoalSource::with_subscription_source(
        Arc::new(updates),
        rx,
        db.node.clone(),
        cancel.clone(),
    )
    .with_rescan_interval(RESCAN);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), source.next_fire())
            .await
            .is_err(),
        "closing notifications must not retire the durable Goal source"
    );
    let doc = goal_fixture_request(&db, &user, &agent, "completed-before-continuation").await;
    publish_owned_terminal_activity(&access, agent.as_ref(), &doc, "durable progress").await;
    set_goal_from_access(
        &access,
        db.node_identity.did(),
        SESSION,
        Some("Continue the existing objective"),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let _fire = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("durable rescan must find the active Goal")
        .expect("closed notifications must not terminate the source");
    assert_eq!(goal_children_from_access(&access).await.len(), 1);
    let goal = load_goal_as(&access, db.node_identity.did()).await.unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), source.next_fire())
            .await
            .expect("cancellation must stop the source")
            .is_none()
    );
}
