use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch, Mutex, Notify, Semaphore};

use super::*;
use crate::runtime::agent::retry::RetryPolicy;
use crate::runtime::agent::worker_capacity::WorkerCapacity;
use crate::runtime::agent::PendingAgent;
use crate::runtime::status::{RuntimeStatusHandle, RuntimeStatusOwner};
use gents_inference::admission::BackendAdmissionConfig;
use gents_inference::resolved::ResolvedAgent;
use gents_protocol::row::AgentRequest;
use gents_store::identity::RuntimeNode;
use gents_store::schema::ensure_runtime_schemas;
use gents_tools::surface::{AgentToolSurfaceConfig, AgentWithTools, ToolCeiling, ToolSurface};

async fn test_node() -> gents_store::test_support::TestStore {
    crate::support::trial_store("reconcile").await
}

fn stub_runtime_node() -> Arc<RuntimeNode> {
    let identity: Arc<dyn gents_store::identity::NodeIdentity> =
        Arc::new(gents_store::test_support::test_identity(&format!(
            "stub-runtime-node-{}",
            uuid::Uuid::new_v4()
        )));
    crate::support::fixtures::test_node_for(identity, "")
}

fn fast_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 1,
        base_delay_ms: 1,
        max_delay_ms: 2,
    }
}

/// Runtime status rows are keyed by the node's own identity, so fixtures pass it here.
fn runtime_status_pair(
    node: &Arc<defra_node::Node>,
    node_did: &str,
) -> (RuntimeStatusOwner, RuntimeStatusHandle) {
    RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        node_did,
        Arc::new(crate::test_support::readiness::AcceptingReadinessWriter),
        Duration::from_millis(1),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    )
}

fn runtime_status_handle(node: &Arc<defra_node::Node>, node_did: &str) -> RuntimeStatusHandle {
    runtime_status_pair(node, node_did).1
}

async fn resolve_surface(
    node: &Arc<defra_node::Node>,
    agent_config: &AgentWithTools,
) -> Arc<ToolSurface> {
    Arc::new(
        agent_config
            .tools
            .resolve(
                node,
                node.node_identity_did()
                    .expect("fixture node has an identity"),
                &gents_tools::plugin_host::NoPluginHost,
            )
            .await
            .unwrap(),
    )
}

async fn drain_until_shutdown(
    request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            message = async {
                let mut receiver = request_rx.lock().await;
                receiver.recv().await
            } => {
                if message.is_none() {
                    return Ok(());
                }
            }
        }
    }
}

/// Revisions of one Agent share its provisioned binding and root identity, so they
/// resolve to the same agent DID. Each fresh provisioning is a different Agent.
struct AgentLineage {
    root: Arc<dyn gents_store::identity::NodeIdentity>,
    binding: gents_protocol::agent_identity::AgentIdentityBinding,
}

impl AgentLineage {
    fn provision(name: &str) -> Self {
        let root: Arc<dyn gents_store::identity::NodeIdentity> =
            Arc::new(gents_store::test_support::test_identity(name));
        let binding = gents_store::identity::provision_agent_identity(root.as_ref()).unwrap();
        Self { root, binding }
    }

    fn revision(&self, name: &str) -> AgentWithTools {
        PendingAgent::new(name)
            .with_identity_binding_for_test(self.binding.clone())
            .build_with_identity_arc_for_test(self.root.clone())
    }
}

async fn snapshot_for_agents_with_admission_and_node(
    node: &Arc<defra_node::Node>,
    default_agent_did: &str,
    agents: Vec<AgentWithTools>,
    backend_admission_configs: RapidHashMap<String, BackendAdmissionConfig>,
    runtime_node: Arc<RuntimeNode>,
) -> ResolvedRuntimeSnapshot {
    let mut tool_surfaces = RapidHashMap::new();
    for agent_config in &agents {
        let tool_surface = resolve_surface(node, agent_config).await;
        tool_surfaces.insert(agent_config.agent.agent_did.clone(), tool_surface);
    }
    ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_agent_did.to_string(),
        agents,
        tool_surfaces,
        backend_admission_configs,
        RapidHashMap::new(),
    )
    .with_node(runtime_node)
}

async fn snapshot_for_agents(
    node: &Arc<defra_node::Node>,
    default_agent_did: &str,
    agents: Vec<AgentWithTools>,
) -> ResolvedRuntimeSnapshot {
    snapshot_for_agents_with_admission_and_node(
        node,
        default_agent_did,
        agents,
        RapidHashMap::new(),
        stub_runtime_node(),
    )
    .await
}

async fn snapshot_for_agents_with_node(
    node: &Arc<defra_node::Node>,
    default_agent_did: &str,
    agents: Vec<AgentWithTools>,
    runtime_node: Arc<RuntimeNode>,
) -> ResolvedRuntimeSnapshot {
    snapshot_for_agents_with_admission_and_node(
        node,
        default_agent_did,
        agents,
        RapidHashMap::new(),
        runtime_node,
    )
    .await
}

fn backend_admission_config(
    backend_id: &str,
    max_concurrent: usize,
    max_queue_depth: usize,
) -> BackendAdmissionConfig {
    BackendAdmissionConfig {
        backend_id: backend_id.to_string(),
        max_concurrent,
        max_queue_depth,
        enabled: true,
        probe_status: gents_inference::backends::registry::HEALTHY_PROBE_STATUS.to_string(),
        measured_unhealthy: false,
        config_fingerprint: format!("{backend_id}:{max_concurrent}:{max_queue_depth}"),
    }
}

fn background_child_request(index: usize, agent_did: &str) -> AgentRequest {
    let mut request = crate::support::build_request(
        format!("child-doc-{index}"),
        format!("child-request-{index}"),
        format!("child-session-{index}"),
        chrono::Utc::now().to_rfc3339(),
    );
    request.node_did = "did:test:background-fanout-test".to_string();
    request.agent_did = agent_did.to_string();
    request.content = format!("background child {index}");
    request.execution_origin = Some("interactive".to_string());
    request.request_hop = 1;
    request.caused_by_parent_request_id = Some("parent-request".to_string());
    request.caused_by_parent_request_doc_id = Some("parent-request-doc".to_string());
    request.caused_by_parent_tool_call_id = Some("parent-tool-call".to_string());
    request.caused_by_parent_tool_call_doc_id = Some("parent-tool-call-doc".to_string());
    request
}

/// Runs a held in-flight request while proposing a switched agent snapshot,
/// then observes the queued request on the new slot and the drained held
/// request on the retired slot.
async fn run_restage_and_drain<T, F>(
    initial_agent: AgentWithTools,
    switched_agent: AgentWithTools,
    observe: F,
) -> (u64, (String, u64, T), (String, u64, T))
where
    T: Send + 'static,
    F: Fn(&AgentWithTools, u64) -> T + Clone + Send + Sync + 'static,
{
    let db = test_node().await;
    let node = db.node.clone();
    let node_did = node
        .node_identity_did()
        .expect("fixture node has an identity");
    let runtime_status = runtime_status_handle(&node, node_did);
    let agent_did = initial_agent.agent.agent_did.clone();
    let runtime_node = stub_runtime_node();
    let initial_snapshot =
        snapshot_for_agents_with_node(&node, &agent_did, vec![initial_agent], runtime_node.clone())
            .await;
    let switched_snapshot =
        snapshot_for_agents_with_node(&node, &agent_did, vec![switched_agent], runtime_node).await;

    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(String, u64, T)>();
    let (held_tx, mut held_rx) = mpsc::unbounded_channel::<String>();
    let release = Arc::new(Notify::new());
    let runner = {
        let release = release.clone();
        move |agent_config: AgentWithTools,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              _capacity: Arc<WorkerCapacity>,
              generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let (done_tx, held_tx, release, observe) = (
                done_tx.clone(),
                held_tx.clone(),
                release.clone(),
                observe.clone(),
            );
            async move {
                loop {
                    let request = tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        message = async {
                            let mut receiver = request_rx.lock().await;
                            receiver.recv().await
                        } => match message {
                            Some(request) => request,
                            None => return Ok(()),
                        },
                    };
                    if request.request_id == "held" {
                        let _ = held_tx.send(request.request_id.clone());
                        release.notified().await;
                    }
                    let _ = done_tx.send((
                        request.request_id,
                        generation,
                        observe(&agent_config, generation),
                    ));
                }
            }
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        gents_inference::admission::AdmissionRegistry::new(node.clone()),
        RetryPolicy {
            max_retries: 3,
            base_delay_ms: 5,
            max_delay_ms: 25,
        },
        runner,
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let initial = supervisor.current_snapshot();
    let mut held = background_child_request(0, &agent_did);
    held.request_id = "held".to_string();
    initial.dispatchers[&agent_did]
        .for_request(&held)
        .send(held)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), held_rx.recv())
        .await
        .expect("the old slot claims the first request")
        .unwrap();

    let (active_tx, mut active_rx) = watch::channel(initial.clone());
    let (proposal_tx, proposal_rx) = mpsc::channel(4);
    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));
    proposal_tx.send(switched_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), active_rx.changed())
        .await
        .expect("a config change publishes a generation while a call is in flight")
        .unwrap();
    let updated = active_rx.borrow().clone();
    assert!(
        !initial.dispatchers[&agent_did].same_channel(&updated.dispatchers[&agent_did]),
        "a restaging change must rotate the slot dispatcher"
    );
    drop(initial);

    let mut queued = background_child_request(1, &agent_did);
    queued.request_id = "queued".to_string();
    updated.dispatchers[&agent_did]
        .for_request(&queued)
        .send(queued)
        .await
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(5), done_rx.recv())
        .await
        .expect("the queued request runs on the new slot")
        .unwrap();

    release.notify_one();
    let drained = tokio::time::timeout(Duration::from_secs(5), done_rx.recv())
        .await
        .expect("the call in flight finishes on the retired slot")
        .unwrap();

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
    node.shutdown().await.unwrap();
    (updated.generation, next, drained)
}

type SharedRequestRx = Arc<Mutex<mpsc::Receiver<AgentRequest>>>;
type SlotWorkerFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;

#[derive(Clone)]
struct GatedExitGates {
    gen1_exit: Arc<Semaphore>,
    gen2_exit: Arc<Semaphore>,
    gen2_waiting: Arc<Notify>,
    exited: Arc<AtomicUsize>,
}

impl GatedExitGates {
    fn new() -> Self {
        Self {
            gen1_exit: Arc::new(Semaphore::new(0)),
            gen2_exit: Arc::new(Semaphore::new(0)),
            gen2_waiting: Arc::new(Notify::new()),
            exited: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn runner(
        &self,
    ) -> impl Fn(
        AgentWithTools,
        Arc<ToolSurface>,
        SharedRequestRx,
        Arc<WorkerCapacity>,
        u64,
        watch::Receiver<bool>,
    ) -> SlotWorkerFuture
           + Clone
           + Send
           + Sync
           + 'static {
        let gates = self.clone();
        move |_agent, _tool_surface, request_rx, _capacity, generation, shutdown| {
            let gates = gates.clone();
            Box::pin(async move {
                let _ = drain_until_shutdown(request_rx, shutdown).await;
                if generation == 2 {
                    gates.gen2_waiting.notify_one();
                }
                let permit = if generation == 1 {
                    gates.gen1_exit.acquire().await.unwrap()
                } else {
                    gates.gen2_exit.acquire().await.unwrap()
                };
                permit.forget();
                gates.exited.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }
}

#[tokio::test]
async fn reconcile_install_applies_added_agent() {
    let db = test_node().await;
    let node = db.node.clone();
    let agent_config = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("pairing-contract-install"),
    );
    let current_resolved = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        Vec::new(),
        RapidHashMap::new(),
        RapidHashMap::new(),
    )
    .with_node(stub_runtime_node());
    let proposed = snapshot_for_agents(&node, "general", vec![agent_config]).await;
    let current = current_resolved.activate(1, RapidHashMap::new());
    let diff = diff_counts(&current, &proposed);
    let applied = proposed.clone().activate(2, RapidHashMap::new());
    let rediff = diff_counts(&applied, &proposed);

    assert_eq!(diff.added, 1, "install registers one added agent");
    assert_eq!(diff.updated, 0);
    assert_eq!(diff.removed, 0);
    assert_eq!(rediff.added, 0, "applying the added agent converges");
    assert_eq!(rediff.updated, 0);
    assert_eq!(rediff.removed, 0);
}

#[tokio::test]
async fn reconcile_teardown_applies_removed_agent() {
    let db = test_node().await;
    let node = db.node.clone();
    let agent_config = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("pairing-contract-teardown"),
    );
    let current_resolved = snapshot_for_agents(&node, "general", vec![agent_config]).await;
    let proposed = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        Vec::new(),
        RapidHashMap::new(),
        RapidHashMap::new(),
    )
    .with_node(stub_runtime_node());
    let current = current_resolved.activate(1, RapidHashMap::new());
    let diff = diff_counts(&current, &proposed);
    let applied = proposed.clone().activate(2, RapidHashMap::new());
    let rediff = diff_counts(&applied, &proposed);

    assert_eq!(diff.removed, 1, "teardown registers one removed agent");
    assert_eq!(diff.added, 0);
    assert_eq!(diff.updated, 0);
    assert_eq!(rediff.added, 0, "applying the removal converges");
    assert_eq!(rediff.updated, 0);
    assert_eq!(rediff.removed, 0);
}

#[tokio::test]
async fn slot_panic_restarts_agent() {
    let db = test_node().await;
    let node = db.node.clone();
    let agent_config = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("pairing-contract-slot-crash"),
    );
    let tool_surface = resolve_surface(&node, &agent_config).await;
    let starts = Arc::new(AtomicUsize::new(0));
    let (starts_tx, mut starts_rx) = watch::channel(0usize);
    let runner = {
        let (starts, starts_tx) = (starts.clone(), starts_tx.clone());
        move |_agent: AgentWithTools,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              _capacity: Arc<WorkerCapacity>,
              _generation: u64,
              shutdown: watch::Receiver<bool>| {
            let (starts, starts_tx) = (starts.clone(), starts_tx.clone());
            async move {
                let attempt = starts.fetch_add(1, Ordering::SeqCst) + 1;
                starts_tx.send_replace(attempt);
                if attempt == 1 {
                    panic!("contract probe panic");
                }
                drain_until_shutdown(request_rx, shutdown).await
            }
        }
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let slot = spawn_slot(
        agent_config,
        tool_surface,
        RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 1,
        },
        runner,
        shutdown_rx,
    );

    let restarted_start = slot.worker_task_count + 1;
    let restarted = tokio::time::timeout(
        Duration::from_secs(30),
        starts_rx.wait_for(|starts| *starts >= restarted_start),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    let _ = shutdown_tx.send(true);
    retire_slot(slot);
    assert!(
        restarted,
        "a panicked slot must restart the agent on its retry policy"
    );
}

#[tokio::test]
async fn dropping_agent_slot_aborts_a_held_executor() {
    struct DropProbe(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let db = test_node().await;
    let node = db.node.clone();
    let agent_config = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("slot-abort-on-owner-drop"),
    );
    let tool_surface = resolve_surface(&node, &agent_config).await;
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let runner = {
        let (entered, dropped) = (entered.clone(), dropped.clone());
        move |_, _, _, _, _, _| {
            let (entered, dropped) = (entered.clone(), dropped.clone());
            async move {
                let _probe = DropProbe(dropped);
                entered.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            }
        }
    };
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let slot = spawn_slot(
        agent_config,
        tool_surface,
        RetryPolicy::default(),
        runner,
        shutdown_rx,
    );
    entered.notified().await;

    drop(slot);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping the slot owner must abort and drop its held executor");
    node.shutdown().await.expect("node shutdown");
}

/// Admission persists InferenceCall rows through the calling Agent's signed access, so the
/// fixture Agent gets the same node grants the admission fixtures give a managed Agent.
async fn grant_fixture_agent_call_access(
    node: &Arc<defra_node::Node>,
    signer: &gents_store::identity::ManagedAgentSigner,
) {
    use gents_store::config_client::NodePermission;
    let admin = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(signer.node_did()).unwrap(),
    );
    let actor = identity::Did::new(signer.did()).unwrap();
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DocumentDelete,
        NodePermission::DacRelationAdd,
        NodePermission::DacRelationDelete,
    ] {
        admin
            .grant_node_permission(&actor, permission)
            .await
            .unwrap();
    }
}

/// Admission captures the physical session of the call's scope, so the fixture publishes the
/// session through the Agent's own signed access, as the Agent's requests are published.
async fn seed_fixture_agent_session(
    node: &Arc<defra_node::Node>,
    signer: &gents_store::identity::ManagedAgentSigner,
    session_id: &str,
) {
    let actor = identity::Did::new(signer.did()).unwrap();
    let access = gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        actor.clone(),
        signer.clone(),
    )
    .unwrap();
    let node_did = signer.node_did().to_string();
    let agent_did = signer.did().to_string();
    let session_id = session_id.to_string();
    access
        .transact_write("test.reconcile.seed_session", |txn| {
            let (actor, node_did, agent_did, session_id) =
                (&actor, &node_did, &agent_did, &session_id);
            Box::pin(async move {
                let (agent_doc, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .expect("selected Agent must be visible to its signer");
                let response = txn
                    .execute_with_variables(
                        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                        &serde_json::json!({"input": {"node_did": node_did, "agent_did": agent_did,
                            "requester_did": null, "session_id": session_id,
                            "created_at": "2026-04-15T00:00:00Z"}}),
                    )
                    .await?;
                let session_doc = gents_store::graphql::created_doc_id(&response, "AgentSession")?;
                gents_session::session::stage_session_relationships_in_txn(
                    txn,
                    &session_doc,
                    &agent_doc,
                    actor,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("publish signed Agent session");
}

#[tokio::test]
async fn generation_supervisor_restages_slot_on_api_key_rotation() {
    let signed_home = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("reconcile-api-key-rotation").await,
        "reconcile-api-key-rotation",
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, _user, _access) = signed_home.into_parts();
    let node = db.node.clone();
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = db.node_identity.did().to_string();
    let runtime_status = runtime_status_handle(&node, &node_did);

    let mut agent_config = PendingAgent::new("general")
        .with_identity_binding_for_test(binding)
        .build_with_identity_arc_for_test(db.node_identity.clone());
    let agent_did = agent_config.agent.agent_did.clone();
    let signer = agent_config
        .agent
        .agent_identity
        .managed_agent_signer()
        .unwrap();
    grant_fixture_agent_call_access(&node, &signer).await;
    seed_fixture_agent_session(&node, &signer, "session-test").await;
    Arc::make_mut(&mut agent_config.agent).backend_id = Some("backend-general".to_string());
    Arc::make_mut(&mut agent_config.agent).backend_auth = gents_documents::BackendAuth::ApiKey {
        key: "fixture-old-key".into(),
    };
    let mut rotated = agent_config.clone();
    Arc::make_mut(&mut rotated.agent).backend_auth = gents_documents::BackendAuth::ApiKey {
        key: "fixture-new-key".into(),
    };
    assert_eq!(format!("{agent_config:?}"), format!("{rotated:?}"));
    assert!(!format!("{rotated:?}").contains("fixture-new-key"));
    let new_connection =
        gents_inference::connection_fingerprint::agent_connection_fingerprint(&rotated.agent);
    assert_ne!(
        gents_inference::connection_fingerprint::agent_connection_fingerprint(&agent_config.agent),
        new_connection
    );
    let admission = |connection: String| {
        let mut config = backend_admission_config("backend-general", 1, 100);
        config.config_fingerprint = connection;
        RapidHashMap::from_iter([("backend-general".to_string(), config)])
    };
    let initial_snapshot = snapshot_for_agents_with_admission_and_node(
        &node,
        &agent_did,
        vec![agent_config.clone()],
        admission(
            gents_inference::connection_fingerprint::agent_connection_fingerprint(
                &agent_config.agent,
            ),
        ),
        agent_config.agent.node.clone(),
    )
    .await;
    let rotated_snapshot = snapshot_for_agents_with_admission_and_node(
        &node,
        &agent_did,
        vec![rotated],
        admission(new_connection.clone()),
        agent_config.agent.node.clone(),
    )
    .await;

    let (built_tx, mut built_rx) = mpsc::unbounded_channel::<(u64, String)>();
    let runner = move |agent_config: AgentWithTools,
                       _tool_surface: Arc<ToolSurface>,
                       request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
                       _capacity: Arc<WorkerCapacity>,
                       generation: u64,
                       shutdown: watch::Receiver<bool>| {
        let built_tx = built_tx.clone();
        async move {
            let key = match &agent_config.agent.backend_auth {
                gents_documents::BackendAuth::ApiKey { key } => key.clone(),
                _ => String::new(),
            };
            let _ = built_tx.send((generation, key));
            drain_until_shutdown(request_rx, shutdown).await
        }
    };

    let registry = gents_inference::admission::AdmissionRegistry::new(node.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        registry.clone(),
        RetryPolicy {
            max_retries: 3,
            base_delay_ms: 5,
            max_delay_ms: 25,
        },
        runner,
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let initial_dispatcher = supervisor.current_snapshot().dispatchers[&agent_did].clone();
    let first = tokio::time::timeout(Duration::from_secs(5), built_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.1, "fixture-old-key");
    let (active_tx, mut active_rx) = watch::channel(supervisor.current_snapshot());
    let (proposal_tx, proposal_rx) = mpsc::channel(4);
    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));

    proposal_tx.send(rotated_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), active_rx.changed())
        .await
        .expect("key rotation should publish a generation")
        .unwrap();
    let updated_dispatcher = active_rx.borrow().dispatchers[&agent_did].clone();
    assert!(
        !initial_dispatcher.same_channel(&updated_dispatcher),
        "a key rotation must restage the slot"
    );
    let rebuilt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let built = built_rx.recv().await.unwrap();
            if built.0 != first.0 {
                break built;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        rebuilt.1, "fixture-new-key",
        "the new slot builds the new client"
    );

    let mut admitted = registry
        .acquire_with_connection_and_signer_for_test(
            "req-rotated-slot",
            "backend-general",
            &agent_config.agent,
            gents_loop::provider_call::admission::CallKind::Inference,
            &new_connection,
            Some(signer),
        )
        .await
        .expect("calls from the rotated slot are admitted");
    admitted.finish_success(None).await.unwrap();

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn backend_change_restages_the_slot_and_drains_the_call_in_flight() {
    use gents_documents::BackendAuth;

    let mut agent_config = PendingAgent::new("general")
        .build_with_identity_for_test(gents_store::test_support::test_identity("switch-general"));
    Arc::make_mut(&mut agent_config.agent).backend_id = Some("claude".to_string());
    Arc::make_mut(&mut agent_config.agent).backend_auth =
        BackendAuth::NodeOAuth { account_ref: None };
    let mut switched = agent_config.clone();
    Arc::make_mut(&mut switched.agent).backend_id = Some("claude-subscription-acct-b".to_string());
    Arc::make_mut(&mut switched.agent).backend_auth = BackendAuth::NodeOAuth {
        account_ref: Some("acct-b".to_string()),
    };

    let (updated_generation, next, drained) =
        run_restage_and_drain(agent_config, switched, |agent_config, _generation| {
            (
                agent_config.agent.backend_id.clone(),
                agent_config
                    .agent
                    .backend_auth
                    .oauth_account_ref()
                    .map(str::to_owned),
            )
        })
        .await;
    assert_eq!(
        next,
        (
            "queued".to_string(),
            updated_generation,
            (
                Some("claude-subscription-acct-b".to_string()),
                Some("acct-b".to_string()),
            ),
        )
    );
    assert_eq!(drained.0, "held");
    assert_ne!(drained.1, updated_generation);
    assert_eq!(
        drained.2,
        (Some("claude".to_string()), None),
        "the call in flight keeps the old account"
    );
}

#[tokio::test]
async fn model_effort_or_tier_change_restages_the_slot_and_drains_the_call_in_flight() {
    use gents_documents::inference_defaults::ReasoningEffort;
    use gents_inference::resolved::ServiceTier;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Settings {
        model_name: &'static str,
        reasoning_effort: Option<ReasoningEffort>,
        service_tier: Option<ServiceTier>,
    }
    const ORIGINAL: Settings = Settings {
        model_name: "model-a",
        reasoning_effort: Some(ReasoningEffort::Low),
        service_tier: None,
    };
    let changes = [
        (
            "model",
            Settings {
                model_name: "model-b",
                ..ORIGINAL
            },
        ),
        (
            "effort",
            Settings {
                reasoning_effort: Some(ReasoningEffort::High),
                ..ORIGINAL
            },
        ),
        (
            "service_tier",
            Settings {
                service_tier: Some(ServiceTier::Fast),
                ..ORIGINAL
            },
        ),
    ];

    let apply = |agent_config: &mut ResolvedAgent, settings: Settings| {
        agent_config.model_name = settings.model_name.to_string();
        agent_config.sampling.reasoning_effort = settings.reasoning_effort;
        agent_config.service_tier = settings.service_tier;
    };

    for (changed, switched_settings) in changes {
        let mut agent_config = PendingAgent::new("general").build_with_identity_for_test(
            gents_store::test_support::test_identity(&format!("settings-{changed}")),
        );
        Arc::make_mut(&mut agent_config.agent).backend_id = Some("backend-a".to_string());
        apply(Arc::make_mut(&mut agent_config.agent), ORIGINAL);
        let mut switched = agent_config.clone();
        apply(Arc::make_mut(&mut switched.agent), switched_settings);

        let (updated_generation, next, drained) =
            run_restage_and_drain(agent_config, switched, |agent_config, generation| {
                agent_config.agent.executed_inference(generation)
            })
            .await;

        assert_eq!(next.0, "queued");
        assert_eq!(next.1, updated_generation);
        assert_eq!(next.2.model_name, switched_settings.model_name);
        assert_eq!(
            next.2.reasoning_effort.as_deref(),
            switched_settings
                .reasoning_effort
                .map(ReasoningEffort::as_str)
        );
        assert_eq!(
            next.2.service_tier.as_deref(),
            switched_settings.service_tier.map(ServiceTier::as_str)
        );
        assert_eq!(next.2.runtime_generation, Some(updated_generation));

        assert_eq!(drained.0, "held");
        assert_ne!(drained.1, updated_generation);
        assert_eq!(drained.2.model_name, ORIGINAL.model_name);
        assert_eq!(
            drained.2.reasoning_effort.as_deref(),
            ORIGINAL.reasoning_effort.map(ReasoningEffort::as_str),
            "the call in flight keeps the effort it was dispatched with"
        );
        assert_eq!(
            drained.2.service_tier.as_deref(),
            ORIGINAL.service_tier.map(ServiceTier::as_str),
            "the call in flight keeps the tier it was dispatched with"
        );
        assert_ne!(drained.2.runtime_generation, Some(updated_generation));
    }
}

#[tokio::test]
async fn published_readiness_carries_the_generations_effective_inference() {
    use gents_documents::inference_defaults::ReasoningEffort;
    use gents_inference::resolved::ServiceTier;
    use gents_protocol::node_readiness::{
        NodeReadinessRow, ReasoningEffortBinding, ServiceTierBinding,
    };

    let db = test_node().await;
    let node = db.node.clone();
    let node_did = node
        .node_identity_did()
        .expect("fixture node has an identity")
        .to_owned();
    let writer = Arc::new(crate::test_support::readiness::CapturingReadinessWriter::default());
    let (owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        &node_did,
        writer.clone(),
        Duration::from_millis(1),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );

    let mut agent_config = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("readiness-effective-inference"),
    );
    let agent_did = agent_config.agent.agent_did.clone();
    let resolved = Arc::make_mut(&mut agent_config.agent);
    resolved.backend_id = Some("backend-a".to_string());
    resolved.model_name = "model-a".to_string();
    resolved.sampling.reasoning_effort = Some(ReasoningEffort::Low);
    resolved.service_tier = Some(ServiceTier::Fast);
    resolved.profile_id = Some("profile-a".to_string());
    let snapshot =
        snapshot_for_agents_with_node(&node, &agent_did, vec![agent_config], stub_runtime_node())
            .await;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        snapshot,
        gents_inference::admission::AdmissionRegistry::new(node.clone()),
        fast_retry_policy(),
        |_, _, _, _, _, _| async { Ok(()) },
        runtime_status.clone(),
        shutdown_rx,
        None,
    )
    .await
    .unwrap();
    let active = supervisor.current_snapshot();
    runtime_status
        .readiness()
        .initialize(&agent_did)
        .await
        .unwrap();
    runtime_status
        .readiness()
        .publish_snapshot(active.as_ref())
        .await
        .unwrap();

    let published = writer
        .snapshots()
        .last()
        .cloned()
        .expect("the publisher wrote a snapshot");
    let row = NodeReadinessRow {
        node_did: node_did.clone(),
        snapshot_json: serde_json::to_string(&published).unwrap(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let decoded = gents_protocol::node_readiness::decode_node_readiness_snapshot(&row, &node_did)
        .expect("the runtime writes a snapshot its own decoder accepts");
    let entry = decoded
        .agents
        .iter()
        .find(|entry| entry.agent_did == agent_did)
        .expect("the staged agent is published");
    let effective = entry
        .effective_inference
        .as_ref()
        .expect("a ready agent publishes the settings its slot will bind");
    assert_eq!(effective.model_name, "model-a");
    assert_eq!(effective.backend_id.as_deref(), Some("backend-a"));
    assert_eq!(effective.profile_id.as_deref(), Some("profile-a"));
    assert_eq!(effective.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(
        effective.service_tier,
        ServiceTierBinding::Selected {
            tier: "fast".to_string()
        }
    );
    assert_eq!(
        effective.reasoning_effort_dispatch,
        ReasoningEffortBinding::Sent {
            effort: "low".to_string()
        }
    );
    assert_eq!(decoded.active_generation, active.generation);

    let _ = shutdown_tx.send(true);
    drop(supervisor);
    owner.close().await.unwrap();
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn registration_failure_rolls_back_standing_before_any_staged_slot_spawns() {
    let db = test_node().await;
    let node = db.node.clone();
    let lineage = AgentLineage::provision("register-failure");
    let agent_did = lineage.binding.agent_did.clone();
    let initial = lineage.revision("general");
    let mut changed = lineage.revision("general");
    Arc::make_mut(&mut changed.agent).system_prompt = "changed".to_string();
    let added = PendingAgent::new("second").build_with_identity_for_test(
        gents_store::test_support::test_identity("register-failure-second"),
    );
    let initial_snapshot = snapshot_for_agents(&node, &agent_did, vec![initial]).await;
    let replacement_snapshot = snapshot_for_agents(&node, &agent_did, vec![changed, added]).await;
    let started_generations = Arc::new(StdMutex::new(Vec::new()));
    let runner = {
        let started_generations = started_generations.clone();
        move |_, _, _, _, generation: u64, mut shutdown: watch::Receiver<bool>| {
            started_generations.lock().unwrap().push(generation);
            async move {
                let _ = shutdown.changed().await;
                Ok(())
            }
        }
    };
    let (runtime_status_owner, runtime_status) = runtime_status_pair(
        &node,
        node.node_identity_did()
            .expect("fixture node has an identity"),
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        gents_inference::admission::AdmissionRegistry::new(node),
        fast_retry_policy(),
        runner,
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let policy = Arc::new(crate::test_support::runtime::FailSecondRegistrationPolicy {
        registrations: AtomicUsize::new(0),
        retired: StdMutex::new(Vec::new()),
    });
    supervisor.slot_failure_policy = Some(policy.clone());
    let (active_tx, _active_rx) = watch::channel(supervisor.current_snapshot());

    let error = supervisor
        .apply_snapshot(replacement_snapshot, 2, &active_tx, shutdown_rx)
        .await
        .expect_err("second slot registration must reject the generation");
    assert!(error.to_string().contains("register staged agent slot"));
    assert_eq!(supervisor.current_snapshot.generation, 1);
    assert_eq!(supervisor.active_slots[&agent_did].generation, 1);
    assert_eq!(policy.registrations.load(Ordering::SeqCst), 2);
    assert_eq!(policy.retired.lock().unwrap().len(), 1);
    assert!(
        !started_generations.lock().unwrap().contains(&2),
        "registration is validated before any candidate executor starts"
    );

    shutdown_tx.send_replace(true);
    supervisor.shutdown_slots().await.unwrap();
    assert!(
        policy
            .retired
            .lock()
            .unwrap()
            .contains(&(agent_did.clone(), 1)),
        "supervisor shutdown must retire the active generation standing"
    );
    runtime_status_owner.close().await.unwrap();
}

#[tokio::test]
async fn retired_slot_drain_failure_reaches_supervisor_shutdown() {
    let db = test_node().await;
    let node = db.node.clone();
    let runtime_node = stub_runtime_node();
    let initial = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("retired-drain-failure"),
    );
    let agent_did = initial.agent.agent_did.clone();
    let mut replacement = initial.clone();
    Arc::make_mut(&mut replacement.agent).system_prompt = "replacement generation".to_string();
    let initial_snapshot =
        snapshot_for_agents_with_node(&node, &agent_did, vec![initial], runtime_node.clone()).await;
    let replacement_snapshot =
        snapshot_for_agents_with_node(&node, &agent_did, vec![replacement], runtime_node).await;
    let (runtime_status_owner, runtime_status) = runtime_status_pair(
        &node,
        node.node_identity_did()
            .expect("fixture node has an identity"),
    );
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let runner = move |_, _, _, _, generation: u64, mut shutdown: watch::Receiver<bool>| {
        let started_tx = started_tx.clone();
        async move {
            let _ = started_tx.send(generation);
            shutdown.changed().await?;
            if generation == 1 {
                anyhow::bail!("injected retired slot drain failure");
            }
            Ok(())
        }
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        gents_inference::admission::AdmissionRegistry::new(node.clone()),
        fast_retry_policy(),
        runner,
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let worker_count = supervisor.active_slots[&agent_did].worker_task_count;
    for _ in 0..worker_count {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(30), started_rx.recv())
                .await
                .expect("initial slot must start"),
            Some(1)
        );
    }
    let (active_tx, _active_rx) = watch::channel(supervisor.current_snapshot());
    supervisor
        .apply_snapshot(replacement_snapshot, 2, &active_tx, shutdown_rx)
        .await
        .unwrap();
    assert_eq!(supervisor.current_snapshot.generation, 2);
    for _ in 0..worker_count {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(30), started_rx.recv())
                .await
                .expect("replacement slot must start"),
            Some(2)
        );
    }
    shutdown_tx.send_replace(true);
    let error = tokio::time::timeout(Duration::from_secs(30), supervisor.shutdown_slots())
        .await
        .expect("supervisor shutdown must join retired slot")
        .expect_err("retired slot drain failure cannot become clean shutdown");
    assert!(
        format!("{error:#}").contains("injected retired slot drain failure"),
        "supervisor lost retired slot drain failure: {error:#}"
    );
    runtime_status_owner.close().await.unwrap();
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn source_publish_failure_rolls_back_and_joins_staged_slots() {
    let db = test_node().await;
    let node = db.node.clone();
    let lineage = AgentLineage::provision("source-failure");
    let agent_did = lineage.binding.agent_did.clone();
    let initial = lineage.revision("general");
    let mut replacement = lineage.revision("general");
    Arc::make_mut(&mut replacement.agent).system_prompt = "replacement".to_string();
    let initial_snapshot = snapshot_for_agents(&node, &agent_did, vec![initial]).await;
    let replacement_snapshot = snapshot_for_agents(&node, &agent_did, vec![replacement]).await;

    let fatal_attempted = Arc::new(Notify::new());
    let fatal_release = Arc::new(Semaphore::new(0));
    let gates = GatedExitGates::new();
    let writer = Arc::new(
        crate::test_support::readiness::GatedGenerationSourceWriter::new(
            2,
            fatal_attempted.clone(),
            fatal_release.clone(),
            true,
        ),
    );
    let (runtime_status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        "did:test:source-failure-rollback",
        writer,
        Duration::from_millis(1),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );
    runtime_status.initialize_startup(&agent_did).await.unwrap();
    let slot_failure_policy = Arc::new(
        crate::test_support::runtime::PublisherBackedSlotFailurePolicy {
            runtime_status: runtime_status.clone(),
        },
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        gents_inference::admission::AdmissionRegistry::new(node),
        fast_retry_policy(),
        gates.runner(),
        runtime_status.clone(),
        shutdown_rx.clone(),
        Some(slot_failure_policy),
    )
    .await
    .unwrap();
    let workers_per_slot = supervisor.active_slots[&agent_did].worker_task_count;
    runtime_status
        .readiness()
        .publish_snapshot(supervisor.current_snapshot().as_ref())
        .await
        .unwrap();
    assert!(runtime_status
        .readiness()
        .demote_slot(&agent_did, 1, "generation one demoted".to_string())
        .await
        .unwrap());
    assert_eq!(
        runtime_status
            .readiness()
            .observation()
            .demotion_reason(&agent_did),
        Some("generation one demoted")
    );
    let (active_tx, _active_rx) = watch::channel(supervisor.current_snapshot());
    let apply = tokio::spawn(async move {
        let mut supervisor = supervisor;
        let result = supervisor
            .apply_snapshot(replacement_snapshot, 2, &active_tx, shutdown_rx)
            .await;
        (supervisor, result)
    });

    fatal_attempted.notified().await;
    fatal_release.add_permits(1);
    gates.gen2_waiting.notified().await;
    assert!(
        !apply.is_finished(),
        "failed source publication detached the staged generation"
    );
    gates.gen2_exit.add_permits(workers_per_slot);
    let (supervisor, result) = apply.await.unwrap();
    let error = result.expect_err("injected source publication must fail apply");
    assert!(error.to_string().contains("node readiness"));
    assert_eq!(supervisor.current_snapshot.generation, 1);
    assert_eq!(supervisor.active_slots[&agent_did].generation, 1);
    let rolled_back = runtime_status.readiness().observation();
    assert_eq!(rolled_back.source_generation(), 1);
    assert_eq!(
        rolled_back.demotion_reason(&agent_did),
        Some("generation one demoted"),
        "failed source publication must preserve the old slot's demotion"
    );
    assert!(
        runtime_status
            .readiness()
            .mark_slot_ready(&agent_did, 1)
            .await
            .unwrap(),
        "retiring the staged generation must preserve old generation CAS standing"
    );

    shutdown_tx.send_replace(true);
    let mut shutdown = tokio::spawn(async move { supervisor.shutdown_slots().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
            .await
            .is_err(),
        "old active generation was detached during staged rollback"
    );
    gates.gen1_exit.add_permits(workers_per_slot);
    shutdown.await.unwrap().unwrap();
    runtime_status_owner.close().await.unwrap();
}

#[tokio::test]
async fn closed_snapshot_receiver_does_not_detach_retired_or_active_slots() {
    let db = test_node().await;
    let node = db.node.clone();
    let source_attempted = Arc::new(Notify::new());
    let source_release = Arc::new(Semaphore::new(0));
    let (runtime_status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        "did:test:closed-active-watch",
        Arc::new(
            crate::test_support::readiness::GatedGenerationSourceWriter::new(
                2,
                source_attempted.clone(),
                source_release.clone(),
                false,
            ),
        ),
        Duration::from_millis(1),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );
    let lineage = AgentLineage::provision("closed-watch");
    let agent_did = lineage.binding.agent_did.clone();
    let initial = lineage.revision("general");
    let mut replacement = lineage.revision("general");
    Arc::make_mut(&mut replacement.agent).system_prompt = "replacement".to_string();
    let initial_snapshot = snapshot_for_agents(&node, &agent_did, vec![initial]).await;
    let replacement_snapshot = snapshot_for_agents(&node, &agent_did, vec![replacement]).await;
    let gates = GatedExitGates::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        gents_inference::admission::AdmissionRegistry::new(node),
        fast_retry_policy(),
        gates.runner(),
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let workers_per_slot = supervisor.active_slots[&agent_did].worker_task_count;
    let (active_tx, active_rx) = watch::channel(supervisor.current_snapshot());

    let apply = tokio::spawn(async move {
        let mut supervisor = supervisor;
        let result = supervisor
            .apply_snapshot(replacement_snapshot, 2, &active_tx, shutdown_rx)
            .await;
        (supervisor, result)
    });
    source_attempted.notified().await;
    drop(active_rx);
    source_release.add_permits(1);
    gates.gen2_waiting.notified().await;
    assert!(
        !apply.is_finished(),
        "closed watch rollback must retain the staged generation until it joins"
    );
    gates.gen2_exit.add_permits(workers_per_slot);
    let (supervisor, result) = apply.await.unwrap();
    let error = result.expect_err("closed active snapshot receiver must reject publication");
    assert!(error.to_string().contains("receiver closed"));
    assert_eq!(supervisor.current_snapshot.generation, 1);
    assert_eq!(supervisor.active_slots[&agent_did].generation, 1);
    shutdown_tx.send_replace(true);
    let mut shutdown_task = tokio::spawn(async move { supervisor.shutdown_slots().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown_task)
            .await
            .is_err(),
        "shutdown detached the blocked slot owners"
    );
    assert_eq!(gates.exited.load(Ordering::SeqCst), workers_per_slot);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown_task)
            .await
            .is_err(),
        "shutdown completed before joining the retired generation-one slot"
    );
    gates.gen1_exit.add_permits(workers_per_slot);
    shutdown_task.await.unwrap().unwrap();
    runtime_status_owner.close().await.unwrap();
    assert_eq!(
        gates.exited.load(Ordering::SeqCst),
        2 * workers_per_slot,
        "both the retired generation and replacement generation must be joined"
    );
}

#[tokio::test]
async fn generation_supervisor_rotates_dispatcher_on_tool_surface_change() {
    let db = test_node().await;
    let node = db.node.clone();
    let node_did = node
        .node_identity_did()
        .expect("fixture node has an identity");
    let runtime_status = runtime_status_handle(&node, node_did);
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = Arc::new(
        gents_store::test_support::test_identity("tool-surface-general"),
    );
    let runtime_node = crate::support::fixtures::test_node_for(identity, "");
    let mut resolved =
        crate::support::fixtures::test_agent_for_node("general", runtime_node.clone());
    resolved.backend_id = Some("backend-general".to_string());
    resolved.backend_endpoint = "http://127.0.0.1:8999/v1".to_string();
    resolved.model_name = "default".to_string();
    resolved.system_prompt = "initial".to_string();
    let agent_did = resolved.agent_did.clone();
    let initial_agent = AgentWithTools::new(resolved, AgentToolSurfaceConfig::meta_only());
    let updated_tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "general-tools",
        "node_did": runtime_node.node_did,
        "host": {"files": {"mode": "ReadOnly"}},
        "built_ins": {"enable_context_budget": true}
    }))
    .unwrap();
    let updated_agent = AgentWithTools {
        tools: Arc::new(
            AgentToolSurfaceConfig::from_tools_document(
                "general",
                &updated_tools,
                &ToolCeiling::readonly(),
                Vec::new(),
            )
            .unwrap(),
        ),
        ..initial_agent.clone()
    };

    let initial_snapshot = snapshot_for_agents(&node, &agent_did, vec![initial_agent]).await;
    let updated_snapshot = snapshot_for_agents(&node, &agent_did, vec![updated_agent]).await;

    let observed_tool_names = Arc::new(StdMutex::new(Vec::<Vec<String>>::new()));
    let observed_notify = Arc::new(Notify::new());
    let runner = {
        let (observed_tool_names, observed_notify) =
            (observed_tool_names.clone(), observed_notify.clone());
        move |_agent: AgentWithTools,
              tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              _capacity: Arc<WorkerCapacity>,
              _generation: u64,
              shutdown: watch::Receiver<bool>| {
            let (observed_tool_names, observed_notify) =
                (observed_tool_names.clone(), observed_notify.clone());
            async move {
                observed_tool_names
                    .lock()
                    .unwrap()
                    .push(tool_surface.tool_names());
                observed_notify.notify_waiters();
                drain_until_shutdown(request_rx, shutdown).await
            }
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        gents_inference::admission::AdmissionRegistry::new(node.clone()),
        RetryPolicy {
            max_retries: 3,
            base_delay_ms: 5,
            max_delay_ms: 25,
        },
        runner,
        runtime_status.clone(),
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let (active_tx, mut active_rx) = watch::channel(supervisor.current_snapshot());
    let (proposal_tx, proposal_rx) = mpsc::channel(4);
    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));

    proposal_tx.send(updated_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), active_rx.changed())
        .await
        .expect("tool-surface update should publish")
        .unwrap();
    assert_eq!(active_rx.borrow().generation, 2);

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let notified = observed_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if observed_tool_names
                .lock()
                .unwrap()
                .iter()
                .any(|tool_names| tool_names.contains(&"read_file".to_string()))
            {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("replacement slot should observe file tools");

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn retiring_a_slot_notifies_the_failure_policy() {
    let db = test_node().await;
    let node = db.node.clone();
    let runtime_status = runtime_status_handle(
        &node,
        node.node_identity_did()
            .expect("fixture node has an identity"),
    );
    let agent_config = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("policy-retirement-559"),
    );
    let agent_did = agent_config.agent.agent_did.clone();
    let initial_snapshot = snapshot_for_agents(&node, &agent_did, vec![agent_config.clone()]).await;
    let runner = |_, _, _, _, _, mut shutdown: watch::Receiver<bool>| async move {
        let _ = shutdown.changed().await;
        Ok(())
    };

    let policy = Arc::new(crate::test_support::runtime::RecordingSlotFailurePolicy {
        retired: std::sync::Mutex::new(Vec::new()),
        retired_notify: Notify::new(),
        demote_calls: AtomicUsize::new(0),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        gents_inference::admission::AdmissionRegistry::new(node.clone()),
        fast_retry_policy(),
        runner,
        runtime_status,
        shutdown_rx.clone(),
        Some(policy.clone() as Arc<dyn slot::SlotFailurePolicy>),
    )
    .await
    .unwrap();
    let (active_tx, mut active_rx) = watch::channel(supervisor.current_snapshot());
    let (proposal_tx, proposal_rx) = mpsc::channel(4);
    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));

    let replacement = PendingAgent::new("replacement").build_with_identity_for_test(
        gents_store::test_support::test_identity("policy-replacement-559"),
    );
    let replacement_did = replacement.agent.agent_did.clone();
    let replacement_snapshot =
        snapshot_for_agents(&node, &replacement_did, vec![replacement]).await;
    proposal_tx.send(replacement_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), active_rx.changed())
        .await
        .expect("removal generation should publish")
        .unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let notified = policy.retired_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if policy
                .retired
                .lock()
                .expect("retired mutex")
                .contains(&(agent_did.clone(), false))
            {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("retirement must notify the policy so the barrier is released");

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

async fn plugin_resolution_surfaces(
    node: &Arc<defra_node::Node>,
) -> (AgentWithTools, Arc<ToolSurface>, Arc<ToolSurface>) {
    let mut agent_config = PendingAgent::new("general").build_with_identity_for_test(
        gents_store::test_support::test_identity("plugin-resolution-fingerprint"),
    );
    let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{}:tools", agent_config.agent.agent_did),
        "node_did": agent_config.agent.node_did(),
        "integrations": {"plugins": [{"plugin": "fixture/list_files"}]},
    }))
    .unwrap();
    agent_config.tools = Arc::new(
        AgentToolSurfaceConfig::from_tools_document(
            &agent_config.agent.agent_did.clone(),
            &tools,
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
        .unwrap(),
    );

    let absent_home = tempfile::tempdir().unwrap();
    let absent_plugins = Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
        Some(absent_home.path().to_path_buf()),
    ));
    let installed_home = tempfile::tempdir().unwrap();
    let record: gents_pack::plugin::store::InstalledPlugin =
        serde_json::from_value(serde_json::json!({
            "namespace": "fixture",
            "name": "list_files",
            "version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "language": "rust",
            "declaration": {
                "name": "list_files",
                "description": "lists files",
                "artifact": "plugins/list_files.afb",
                "language": "rust",
                "input_schema": {"type": "object"}
            }
        }))
        .unwrap();
    gents_pack::plugin::store::write_record(installed_home.path(), &record).unwrap();
    let installed_plugins = Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
        Some(installed_home.path().to_path_buf()),
    ));

    let node_did = node
        .node_identity_did()
        .expect("fixture node has an identity");
    let absent = Arc::new(
        agent_config
            .tools
            .resolve(node, node_did, &*absent_plugins)
            .await
            .unwrap(),
    );
    let installed = Arc::new(
        agent_config
            .tools
            .resolve(node, node_did, &*installed_plugins)
            .await
            .unwrap(),
    );
    let plugin_ref = gents_documents::PluginToolRef {
        tool_calls: false,
        plugin: "fixture/list_files".to_string(),
        digest: None,
        input_fields: Vec::new(),
        job: None,
    };
    assert_eq!(absent.plugin_resolutions(), &[(plugin_ref.clone(), None)]);
    assert_eq!(
        installed.plugin_resolutions(),
        &[(plugin_ref, Some(record.clone()))]
    );
    (agent_config, absent, installed)
}

#[tokio::test]
async fn a_changed_plugin_resolution_changes_the_fingerprint_and_recreates_the_slot() {
    let db = test_node().await;
    let node = db.node.clone();
    let runtime_node = stub_runtime_node();
    let (agent_config, absent_surface, installed_surface) = plugin_resolution_surfaces(&node).await;

    let snapshot_for = |surface: Arc<ToolSurface>| {
        ResolvedRuntimeSnapshot::from_parts(
            "general".to_string(),
            vec![agent_config.clone()],
            RapidHashMap::from_iter([("general".to_string(), surface)]),
            RapidHashMap::new(),
        )
        .with_node(Arc::clone(&runtime_node))
    };
    let absent_snapshot = snapshot_for(Arc::clone(&absent_surface));
    let installed_snapshot = snapshot_for(Arc::clone(&installed_surface));

    assert_ne!(
        absent_snapshot.configuration_fingerprint(),
        installed_snapshot.configuration_fingerprint(),
        "installing the named plugin must change the configuration fingerprint"
    );
    let active_absent = absent_snapshot.activate(1, RapidHashMap::new());
    let diff = diff_counts(&active_absent, &installed_snapshot);
    assert_eq!(diff.updated, 1);
    assert_eq!(diff.added, 0);
    assert_eq!(diff.removed, 0);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let slot = spawn_slot(
        agent_config.clone(),
        Arc::clone(&absent_surface),
        RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 1,
        },
        |_, _, _, _, _, _| async { Ok(()) },
        shutdown_rx,
    );
    assert!(
        slot.matches(&agent_config, &absent_surface),
        "the slot keeps its own surface"
    );
    assert!(
        !slot.matches(&agent_config, &installed_surface),
        "a changed plugin resolution must recreate the slot so the Agent \
         re-admits with a fresh build budget"
    );
    let _ = shutdown_tx.send(true);
    retire_slot(slot);

    let mut reinstalled = installed_surface
        .plugin_resolutions()
        .first()
        .and_then(|(_, record)| record.clone())
        .expect("installed record");
    reinstalled.declaration.description = "lists files, reworded".to_owned();
    let home = tempfile::tempdir().unwrap();
    gents_pack::plugin::store::write_record(home.path(), &reinstalled).unwrap();
    let plugins =
        gents_toolcalls::plugin::executor::PluginExecutor::new(Some(home.path().to_path_buf()));
    let reinstalled_surface = Arc::new(
        agent_config
            .tools
            .resolve(
                &node,
                node.node_identity_did()
                    .expect("fixture node has an identity"),
                &plugins,
            )
            .await
            .unwrap(),
    );
    assert_ne!(
        snapshot_for(reinstalled_surface).configuration_fingerprint(),
        installed_snapshot.configuration_fingerprint(),
        "a same-artifact reinstall that changes the declaration must refresh the slot"
    );
}

mod placement;
