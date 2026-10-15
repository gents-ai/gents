use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch, Mutex, Notify, Semaphore};

use super::*;
use crate::admission::BackendAdmissionConfig;
use crate::agent::PendingAgent;
use crate::backend_provider::BackendProviderKind;
use crate::config::{MaxTurnsProvenance, ResolvedAgent, DEFAULT_MAX_TURNS};
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::identity::{KeyIdentity, NodeIdentity as _, RuntimeNode};
use crate::lean_vocab_test::lean_runtime_reconcile_case;
use crate::runtime_status::RuntimeStatusHandle;
use crate::tool_surface::{AgentToolSurfaceConfig, ToolCeiling, ToolSurface};
use crate::watcher::AgentRequest;

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

fn stub_runtime_node() -> Arc<RuntimeNode> {
    let identity: Arc<dyn crate::identity::NodeIdentity> = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("stub-runtime-node-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_id: String::new(),
        display_name: None,
        enabled: true,
    })
}

async fn snapshot_for_agents(
    node: &defra_node::EmbeddedNode,
    default_agent_id: &str,
    agents: Vec<Arc<ResolvedAgent>>,
) -> ResolvedRuntimeSnapshot {
    snapshot_for_agents_with_node(node, default_agent_id, agents, stub_runtime_node()).await
}

/// `runtime_snapshot::configuration_fingerprint` hashes the node's DID
/// alongside each agent_config, so isolating one agent_config field requires both
/// snapshots to carry the same node.
async fn snapshot_for_agents_with_node(
    node: &defra_node::EmbeddedNode,
    default_agent_id: &str,
    agents: Vec<Arc<ResolvedAgent>>,
    runtime_node: Arc<RuntimeNode>,
) -> ResolvedRuntimeSnapshot {
    let mut tool_surfaces = HashMap::new();
    for agent_config in &agents {
        let tool_surface = agent_config
            .tools
            .resolve(node, agent_config.node_did(), &Default::default())
            .await
            .unwrap();
        tool_surfaces.insert(agent_config.agent_id.clone(), Arc::new(tool_surface));
    }
    ResolvedRuntimeSnapshot::from_parts(
        default_agent_id.to_string(),
        agents,
        tool_surfaces,
        HashMap::new(),
    )
    .with_node(runtime_node)
}

async fn snapshot_for_agents_with_admission(
    node: &defra_node::EmbeddedNode,
    default_agent_id: &str,
    agents: Vec<Arc<ResolvedAgent>>,
    backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
) -> ResolvedRuntimeSnapshot {
    let mut tool_surfaces = HashMap::new();
    for agent_config in &agents {
        let tool_surface = agent_config
            .tools
            .resolve(node, agent_config.node_did(), &Default::default())
            .await
            .unwrap();
        tool_surfaces.insert(agent_config.agent_id.clone(), Arc::new(tool_surface));
    }
    ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_agent_id.to_string(),
        agents,
        tool_surfaces,
        backend_admission_configs,
        HashMap::new(),
    )
    .with_node(stub_runtime_node())
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
        probe_status: crate::backend_registry::HEALTHY_PROBE_STATUS.to_string(),
        measured_unhealthy: false,
        config_fingerprint: format!("{backend_id}:{max_concurrent}:{max_queue_depth}"),
    }
}

fn background_child_request(index: usize, agent_id: &str) -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: format!("child-doc-{index}"),
        request_id: format!("child-request-{index}"),
        node_did: "did:test:background-fanout-test".to_string(),
        requester_did: None,
        agent_id: agent_id.to_string(),
        session_id: format!("child-session-{index}"),
        content: format!("background child {index}"),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: Some("interactive".to_string()),
        created_at: chrono::Utc::now().to_rfc3339(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 1,
        caused_by_parent_request_id: Some("parent-request".to_string()),
        caused_by_parent_request_doc_id: Some("parent-request-doc".to_string()),
        caused_by_parent_tool_call_id: Some("parent-tool-call".to_string()),
        caused_by_parent_tool_call_doc_id: Some("parent-tool-call-doc".to_string()),
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_authority: None,
        workspace_owner_node_did: None,
        workspace_seal_hash: None,
    }
}

#[tokio::test]
async fn operator_write_changes_snapshot_fingerprint() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let mut initial_agent = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-initial"));
    initial_agent.system_prompt = "before operator write".to_string();
    let mut updated_agent = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-updated"));
    updated_agent.system_prompt = "after operator write".to_string();
    let current_resolved =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(initial_agent)]).await;
    let proposed =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(updated_agent)]).await;
    let current = current_resolved.activate(1, HashMap::new());
    let diff = diff_counts(&current, &proposed);

    assert_ne!(
        current.configuration_fingerprint(),
        proposed.configuration_fingerprint(),
        "an operator write must change the configuration fingerprint"
    );
    assert_eq!(diff.updated, 1);
    assert_eq!(diff.added, 0);
    assert_eq!(diff.removed, 0);
}

#[tokio::test]
async fn max_turns_provenance_only_edit_changes_snapshot_and_slot_fingerprints() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let runtime_node = stub_runtime_node();

    let mut unset = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("max-turns-provenance"));
    unset.max_turns = DEFAULT_MAX_TURNS;
    unset.max_turns_provenance = MaxTurnsProvenance::Default;
    let mut explicit = unset.clone();
    explicit.max_turns_provenance = MaxTurnsProvenance::ExecutionProfile;
    assert_eq!(unset.max_turns, explicit.max_turns);

    assert_ne!(
        crate::completion_factory::agent_slot_fingerprint(&unset),
        crate::completion_factory::agent_slot_fingerprint(&explicit),
        "slot selection must rebuild when only max_turns provenance changed"
    );

    let unset = Arc::new(unset);
    let explicit = Arc::new(explicit);
    let unset_snapshot = snapshot_for_agents_with_node(
        node.as_ref(),
        "general",
        vec![Arc::clone(&unset)],
        Arc::clone(&runtime_node),
    )
    .await;
    let explicit_snapshot = snapshot_for_agents_with_node(
        node.as_ref(),
        "general",
        vec![Arc::clone(&explicit)],
        Arc::clone(&runtime_node),
    )
    .await;

    assert_ne!(
        unset_snapshot.configuration_fingerprint(),
        explicit_snapshot.configuration_fingerprint(),
        "reconcile must not treat a provenance-only edit as a no-op"
    );

    let active_unset = unset_snapshot.activate(1, HashMap::new());
    let setting = diff_counts(&active_unset, &explicit_snapshot);
    assert_eq!(setting.updated, 1);
    assert_eq!(setting.added, 0);
    assert_eq!(setting.removed, 0);

    let explicit_snapshot = snapshot_for_agents_with_node(
        node.as_ref(),
        "general",
        vec![explicit],
        Arc::clone(&runtime_node),
    )
    .await;
    let unset_snapshot =
        snapshot_for_agents_with_node(node.as_ref(), "general", vec![unset], runtime_node).await;
    let active_explicit = explicit_snapshot.activate(1, HashMap::new());
    assert_ne!(
        active_explicit.configuration_fingerprint(),
        unset_snapshot.configuration_fingerprint(),
        "clearing an explicit value equal to the default must also reconcile"
    );
    let clearing = diff_counts(&active_explicit, &unset_snapshot);
    assert_eq!(clearing.updated, 1);
    assert_eq!(clearing.added, 0);
    assert_eq!(clearing.removed, 0);
}

#[tokio::test]
async fn pairing_desired_read_failure_applies_no_operations() {
    assert!(read_failure_is_noop_self_loop(test_node().await).await);
}

/// Probe for the `readFailure` transition: a failed `load_desired` read makes a
/// tick a no-op self-loop — `desired_read_failed` is set and NO ops are applied,
/// so the state is unchanged. Exercises the real `reconcile_peer_tick` with a
/// store whose desired read errors (the admin is never reached on this path).
async fn read_failure_is_noop_self_loop(node: Arc<defra_node::EmbeddedNode>) -> bool {
    use crate::agent::p2p_reconcile::{
        reconcile_peer_tick, EmbeddedRemoteP2pAdmin, LoadedPairingApplied, PairingDesired,
        PairingStateStore,
    };

    struct FailingDesiredStore;
    #[async_trait::async_trait]
    impl PairingStateStore for FailingDesiredStore {
        async fn load_desired(&self, _peer_id: &str) -> anyhow::Result<Option<PairingDesired>> {
            anyhow::bail!("simulated desired-state read failure")
        }
        async fn load_applied(&self, _peer_id: &str) -> anyhow::Result<LoadedPairingApplied> {
            Ok(LoadedPairingApplied::default())
        }
        async fn persist_applied(
            &self,
            _peer_id: &str,
            _applied: &LoadedPairingApplied,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn list_peer_ids(&self) -> anyhow::Result<BTreeSet<String>> {
            Ok(BTreeSet::new())
        }
    }

    let admin = EmbeddedRemoteP2pAdmin::new(node);
    let store = FailingDesiredStore;
    match reconcile_peer_tick(&admin, &store, "peer-a").await {
        Ok(outcome) => outcome.desired_read_failed && outcome.ops_applied.is_empty(),
        Err(_) => false,
    }
}

#[tokio::test]
async fn reconcile_install_applies_added_agent() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_config = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-install"));
    let current_resolved = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_node(stub_runtime_node());
    let proposed =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(agent_config)]).await;
    let current = current_resolved.activate(1, HashMap::new());
    let diff = diff_counts(&current, &proposed);
    let applied = proposed.clone().activate(2, HashMap::new());
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
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_config = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-teardown"));
    let current_resolved =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(agent_config)]).await;
    let proposed = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_node(stub_runtime_node());
    let current = current_resolved.activate(1, HashMap::new());
    let diff = diff_counts(&current, &proposed);
    let applied = proposed.clone().activate(2, HashMap::new());
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
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_config = Arc::new(
        PendingAgent::new("general")
            .build_with_identity_for_test(test_identity("pairing-contract-slot-crash")),
    );
    let tool_surface = Arc::new(
        agent_config
            .tools
            .resolve(node.as_ref(), agent_config.node_did(), &Default::default())
            .await
            .unwrap(),
    );
    let starts = Arc::new(AtomicUsize::new(0));
    let (starts_tx, mut starts_rx) = watch::channel(0usize);
    let runner = {
        let starts = starts.clone();
        let starts_tx = starts_tx.clone();
        move |_agent: Arc<ResolvedAgent>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              _generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let starts = starts.clone();
            let starts_tx = starts_tx.clone();
            async move {
                let attempt = starts.fetch_add(1, Ordering::SeqCst) + 1;
                starts_tx.send_replace(attempt);
                if attempt == 1 {
                    panic!("contract probe panic");
                }
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
        }
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let slot = spawn_slot(
        agent_config,
        tool_surface,
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 1,
        },
        runner,
        shutdown_rx,
    );

    // One initial start per worker task does not demonstrate that the
    // panicked runner restarted.
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

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_config = Arc::new(
        PendingAgent::new("general")
            .build_with_identity_for_test(test_identity("slot-abort-on-owner-drop")),
    );
    let tool_surface = Arc::new(
        agent_config
            .tools
            .resolve(node.as_ref(), agent_config.node_did(), &Default::default())
            .await
            .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let runner = {
        let entered = entered.clone();
        let dropped = dropped.clone();
        move |_, _, _, _, _| {
            let entered = entered.clone();
            let dropped = dropped.clone();
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
        crate::retry::RetryPolicy::default(),
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
    node.shutdown().await;
}

#[tokio::test]
async fn agent_slot_fans_out_background_children_to_backend_capacity() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let mut agent_config = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("background-fanout"));
    agent_config.backend_id = Some("backend-wide".to_string());
    let snapshot = snapshot_for_agents_with_admission(
        node.as_ref(),
        "general",
        vec![Arc::new(agent_config)],
        HashMap::from([(
            "backend-wide".to_string(),
            backend_admission_config("backend-wide", 3, 100),
        )]),
    )
    .await;

    let (started_tx, mut started_rx) = mpsc::channel::<String>(8);
    let (dequeued_tx, mut dequeued_rx) = mpsc::channel::<String>(8);
    let release = Arc::new(Semaphore::new(0));
    let runner = {
        let release = release.clone();
        move |_agent: Arc<ResolvedAgent>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              _generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let started_tx = started_tx.clone();
            let dequeued_tx = dequeued_tx.clone();
            let release = release.clone();
            async move {
                loop {
                    let message = tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        message = async {
                            let mut receiver = request_rx.lock().await;
                            receiver.recv().await
                        } => message,
                    };
                    let Some(request) = message else {
                        return Ok(());
                    };
                    dequeued_tx.send(request.request_id.clone()).await.unwrap();
                    let capacity = crate::agent::worker_capacity::current_slot_capacity()
                        .expect("test runner has slot generation capacity");
                    let cancellation = tokio_util::sync::CancellationToken::new();
                    let unbound = tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        guard = capacity.acquire_unbound(&cancellation) => guard.unwrap(),
                    };
                    let _active = unbound
                        .bind(crate::agent::worker_capacity::WorkerTicket::new(
                            request.doc_id,
                            format!("fixture-generation-{_generation}"),
                        ))
                        .unwrap_or_else(|_| panic!("bind exact fixture request ticket"));
                    started_tx
                        .send(request.request_id)
                        .await
                        .expect("test receiver should stay open");
                    tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        permit = release.acquire() => permit.expect("release semaphore open").forget(),
                    }
                }
            }
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let slots = spawn_slots(
        &snapshot,
        1,
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 1,
        },
        runner,
        shutdown_rx,
        None,
    );
    let dispatcher = slots
        .get("general")
        .expect("general slot")
        .dispatcher
        .clone();

    for index in 0..4 {
        dispatcher
            .send(background_child_request(index, "general"))
            .await
            .unwrap();
    }

    // One fixed worker per active permit: the fourth request stays queued
    // until a worker frees, with no extra task holding it.
    let started = tokio::time::timeout(Duration::from_secs(1), async {
        let mut request_ids = BTreeSet::new();
        while request_ids.len() < 3 {
            let request_id = started_rx
                .recv()
                .await
                .expect("runner should report started requests");
            request_ids.insert(request_id);
        }
        request_ids
    })
    .await
    .expect("executor should start all same-agent background children concurrently");
    assert_eq!(
        started.len(),
        3,
        "logical active work is bounded by backend capacity"
    );
    let mut dequeued = BTreeSet::new();
    while let Ok(request_id) = dequeued_rx.try_recv() {
        dequeued.insert(request_id);
    }
    assert_eq!(dequeued, started, "only the three workers dequeued");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), started_rx.recv())
            .await
            .is_err(),
        "fourth request must wait for an active worker"
    );

    release.add_permits(3);
    let fourth = tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
        .await
        .expect("freed worker admits queued child")
        .expect("fourth child is reported");
    assert!(!started.contains(&fourth));
    let _ = shutdown_tx.send(true);
    for slot in slots.into_values() {
        retire_slot(slot);
    }
}

#[tokio::test]
async fn generation_supervisor_rotates_dispatcher_on_backend_capacity_change() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:test:reconcile-capacity-test";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did);

    let mut agent_config = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("capacity-general"));
    agent_config.backend_id = Some("backend-general".to_string());
    let agent_config = Arc::new(agent_config);
    let initial_snapshot = snapshot_for_agents_with_admission(
        node.as_ref(),
        "general",
        vec![agent_config.clone()],
        HashMap::from([(
            "backend-general".to_string(),
            backend_admission_config("backend-general", 1, 100),
        )]),
    )
    .await;
    let updated_snapshot = snapshot_for_agents_with_admission(
        node.as_ref(),
        "general",
        vec![agent_config],
        HashMap::from([(
            "backend-general".to_string(),
            backend_admission_config("backend-general", 3, 100),
        )]),
    )
    .await;

    let runner = move |_agent: Arc<ResolvedAgent>,
                       _tool_surface: Arc<ToolSurface>,
                       request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
                       _generation: u64,
                       mut shutdown: watch::Receiver<bool>| async move {
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
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node.clone()),
        crate::retry::RetryPolicy {
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
    let active_snapshot = supervisor.current_snapshot();
    let initial_dispatcher = active_snapshot
        .dispatchers
        .get("general")
        .expect("initial general dispatcher")
        .clone();
    assert_eq!(
        active_snapshot
            .agent_executor_capacities
            .get("general")
            .copied(),
        Some(1)
    );
    let (active_tx, mut active_rx) = watch::channel(active_snapshot);
    let (proposal_tx, proposal_rx) = mpsc::channel(4);

    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));

    proposal_tx.send(updated_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), active_rx.changed())
        .await
        .expect("capacity update should publish")
        .unwrap();
    let updated_active = active_rx.borrow().clone();
    let updated_dispatcher = updated_active
        .dispatchers
        .get("general")
        .expect("updated general dispatcher");
    assert!(!initial_dispatcher.same_channel(updated_dispatcher));
    assert_eq!(
        updated_active
            .agent_executor_capacities
            .get("general")
            .copied(),
        Some(3)
    );

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

/// N1: rotating only a backend API key must restage the agent_config slot.
/// `ResolvedAgent`'s Debug redacts the key, so without the keyed
/// connection identity the old slot would keep its old client forever.
#[tokio::test]
async fn generation_supervisor_restages_slot_on_api_key_rotation() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:test:reconcile-key-rotation";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did);

    let mut agent_config = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("rotation-general"));
    agent_config.backend_id = Some("backend-general".to_string());
    agent_config.backend_auth = crate::document_config::BackendAuth::ApiKey {
        key: "fixture-old-key".into(),
    };
    let mut rotated = agent_config.clone();
    rotated.backend_auth = crate::document_config::BackendAuth::ApiKey {
        key: "fixture-new-key".into(),
    };
    assert_eq!(format!("{agent_config:?}"), format!("{rotated:?}"));
    assert!(!format!("{rotated:?}").contains("fixture-new-key"));
    let new_connection = crate::completion_factory::agent_connection_fingerprint(&rotated);
    assert_ne!(
        crate::completion_factory::agent_connection_fingerprint(&agent_config),
        new_connection
    );
    let admission = |connection: String| {
        let mut config = backend_admission_config("backend-general", 1, 100);
        // Production admission configs key their fingerprint on the
        // credentials; mirror that so the rotated snapshot differs.
        config.config_fingerprint = connection;
        HashMap::from([("backend-general".to_string(), config)])
    };
    let initial_snapshot = snapshot_for_agents_with_admission(
        node.as_ref(),
        "general",
        vec![Arc::new(agent_config.clone())],
        admission(crate::completion_factory::agent_connection_fingerprint(
            &agent_config,
        )),
    )
    .await;
    let rotated_snapshot = snapshot_for_agents_with_admission(
        node.as_ref(),
        "general",
        vec![Arc::new(rotated)],
        admission(new_connection.clone()),
    )
    .await;

    let (built_tx, mut built_rx) = mpsc::unbounded_channel::<(u64, String)>();
    let runner = move |agent_config: Arc<ResolvedAgent>,
                       _tool_surface: Arc<ToolSurface>,
                       request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
                       generation: u64,
                       mut shutdown: watch::Receiver<bool>| {
        let built_tx = built_tx.clone();
        async move {
            let key = match &agent_config.backend_auth {
                crate::document_config::BackendAuth::ApiKey { key } => key.clone(),
                _ => String::new(),
            };
            let _ = built_tx.send((generation, key));
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
    };

    let registry = crate::admission::AdmissionRegistry::new(node.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        registry.clone(),
        crate::retry::RetryPolicy {
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
    let initial_dispatcher = supervisor.current_snapshot().dispatchers["general"].clone();
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
    let updated_dispatcher = active_rx.borrow().dispatchers["general"].clone();
    assert!(
        !initial_dispatcher.same_channel(&updated_dispatcher),
        "a key rotation must restage the slot"
    );
    // Each slot starts several workers; skip the first generation's reports.
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
        .acquire_with_connection_for_test(
            "req-rotated-slot",
            "backend-general",
            "general",
            node_did,
            crate::admission::CallKind::Inference,
            &new_connection,
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
    use crate::document_config::BackendAuth;

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:test:reconcile-backend-change";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did);

    let mut agent_config =
        PendingAgent::new("general").build_with_identity_for_test(test_identity("switch-general"));
    agent_config.backend_id = Some("claude".to_string());
    agent_config.backend_auth = BackendAuth::NodeOAuth { account_ref: None };
    let mut switched = agent_config.clone();
    switched.backend_id = Some("claude-subscription-acct-b".to_string());
    switched.backend_auth = BackendAuth::NodeOAuth {
        account_ref: Some("acct-b".to_string()),
    };
    let runtime_node = stub_runtime_node();
    let initial_snapshot = snapshot_for_agents_with_node(
        node.as_ref(),
        "general",
        vec![Arc::new(agent_config)],
        runtime_node.clone(),
    )
    .await;
    let switched_snapshot = snapshot_for_agents_with_node(
        node.as_ref(),
        "general",
        vec![Arc::new(switched)],
        runtime_node,
    )
    .await;

    // (request id, generation, backend id, account ref) of each finished request.
    let (done_tx, mut done_rx) =
        mpsc::unbounded_channel::<(String, u64, Option<String>, Option<String>)>();
    let (held_tx, mut held_rx) = mpsc::unbounded_channel::<String>();
    let release = Arc::new(Notify::new());
    let runner = {
        let release = release.clone();
        move |agent_config: Arc<ResolvedAgent>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let (done_tx, held_tx, release) = (done_tx.clone(), held_tx.clone(), release.clone());
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
                        agent_config.backend_id.clone(),
                        agent_config
                            .backend_auth
                            .oauth_account_ref()
                            .map(str::to_owned),
                    ));
                }
            }
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node.clone()),
        crate::retry::RetryPolicy {
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
    let mut held = background_child_request(0, "general");
    held.request_id = "held".to_string();
    initial.dispatchers["general"].send(held).await.unwrap();
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
        .expect("a backend change publishes a generation while a call is in flight")
        .unwrap();
    let updated = active_rx.borrow().clone();
    assert!(
        !initial.dispatchers["general"].same_channel(&updated.dispatchers["general"]),
        "a backend change must restage the slot"
    );
    drop(initial);

    let mut queued = background_child_request(1, "general");
    queued.request_id = "queued".to_string();
    updated.dispatchers["general"].send(queued).await.unwrap();
    let next = tokio::time::timeout(Duration::from_secs(5), done_rx.recv())
        .await
        .expect("the queued request runs on the new slot")
        .unwrap();
    assert_eq!(
        next,
        (
            "queued".to_string(),
            updated.generation,
            Some("claude-subscription-acct-b".to_string()),
            Some("acct-b".to_string()),
        )
    );

    release.notify_one();
    let drained = tokio::time::timeout(Duration::from_secs(5), done_rx.recv())
        .await
        .expect("the call in flight finishes on the retired slot")
        .unwrap();
    assert_eq!(drained.0, "held");
    assert_ne!(drained.1, updated.generation);
    assert_eq!(
        (drained.2, drained.3),
        (Some("claude".to_string()), None),
        "the call in flight keeps the old account"
    );

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

#[derive(Debug, serde::Deserialize)]
struct RuntimeStatusRow {
    reconcile_phase: String,
    last_reconcile_result: String,
    last_reconcile_error: String,
}

async fn fetch_runtime_status(node: &defra_node::EmbeddedNode, node_did: &str) -> RuntimeStatusRow {
    let node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeRuntime(filter: {{ node_did: {{ _eq: "{node_did}" }} }}, limit: 1) {{
                reconcile_phase
                last_reconcile_result
                last_reconcile_error
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "NodeRuntime query failed: {:?}",
        response.errors
    );
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("NodeRuntime"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("NodeRuntime row");
    serde_json::from_value(value).expect("decode NodeRuntime row")
}

#[tokio::test]
async fn generation_supervisor_rotates_dispatcher_on_agent_change() {
    let publish = lean_runtime_reconcile_case("publish_changed_snapshot");
    assert!(publish.legal);

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:test:reconcile-test";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did);

    let starts = Arc::new(StdMutex::new(HashMap::<String, usize>::new()));
    let mut initial_agent =
        PendingAgent::new("general").build_with_identity_for_test(test_identity("general"));
    initial_agent.system_prompt = "initial prompt".to_string();
    let mut updated_agent =
        PendingAgent::new("general").build_with_identity_for_test(test_identity("general"));
    updated_agent.system_prompt = "updated prompt".to_string();

    let initial_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(initial_agent)]).await;
    let updated_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(updated_agent)]).await;

    let runner = {
        let starts = starts.clone();
        move |agent_config: Arc<ResolvedAgent>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              _generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let starts = starts.clone();
            async move {
                *starts
                    .lock()
                    .unwrap()
                    .entry(agent_config.agent_id.clone())
                    .or_default() += 1;
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
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node.clone()),
        crate::retry::RetryPolicy {
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
    let active_snapshot = supervisor.current_snapshot();
    assert_eq!(
        active_snapshot.generation,
        publish.pre_active_generation as u64
    );
    assert!(active_snapshot.dispatchers.contains_key("general"));
    let (active_tx, mut active_rx) = watch::channel(active_snapshot);
    let (proposal_tx, proposal_rx) = mpsc::channel(4);

    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if starts
                .lock()
                .unwrap()
                .get("general")
                .copied()
                .unwrap_or_default()
                >= 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial agent slot should start");

    proposal_tx.send(updated_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), active_rx.changed())
        .await
        .expect("generation update should publish")
        .unwrap();
    let updated_active = active_rx.borrow().clone();
    assert_eq!(
        updated_active.generation,
        publish.post_active_generation as u64
    );
    assert_eq!(
        updated_active
            .agents
            .get("general")
            .expect("updated agent")
            .system_prompt,
        "updated prompt"
    );
    let status = fetch_runtime_status(node.as_ref(), node_did).await;
    assert_eq!(status.reconcile_phase, publish.post_phase.as_str());
    assert_eq!(status.last_reconcile_result, "applied");
    assert!(status.last_reconcile_error.is_empty());

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if starts
                .lock()
                .unwrap()
                .get("general")
                .copied()
                .unwrap_or_default()
                >= 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement agent slot should start");

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn generation_supervisor_keeps_previous_generation_after_failed_apply() {
    let apply_failed = lean_runtime_reconcile_case("apply_failed_clears_pending");
    assert!(apply_failed.legal);

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:test:reconcile-failure-test";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did);

    let initial_agent =
        PendingAgent::new("general").build_with_identity_for_test(test_identity("general-initial"));
    let mut updated_agent =
        PendingAgent::new("general").build_with_identity_for_test(test_identity("general-updated"));
    updated_agent.system_prompt = "updated prompt".to_string();

    let initial_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(initial_agent)]).await;
    let valid_updated_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(updated_agent)]).await;
    let mut extra_tool_surface_snapshot = valid_updated_snapshot.clone();
    extra_tool_surface_snapshot.tool_surfaces.insert(
        "extra".to_string(),
        extra_tool_surface_snapshot
            .tool_surfaces
            .get("general")
            .expect("general tool surface")
            .clone(),
    );
    assert!(extra_tool_surface_snapshot
        .validate_node_readiness_source()
        .expect_err("extra tool surface must violate exact keyset parity")
        .to_string()
        .contains("keysets differ"));
    let invalid_snapshot = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        valid_updated_snapshot.agents.values().cloned().collect(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_node(stub_runtime_node());

    let runner = move |_agent: Arc<ResolvedAgent>,
                       _tool_surface: Arc<ToolSurface>,
                       request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
                       _generation: u64,
                       mut shutdown: watch::Receiver<bool>| async move {
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
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node.clone()),
        crate::retry::RetryPolicy {
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
    let initial_active = supervisor.current_snapshot();
    let (active_tx, mut active_rx) = watch::channel(initial_active.clone());
    let (proposal_tx, proposal_rx) = mpsc::channel(4);

    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));

    proposal_tx.send(invalid_snapshot).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!active_rx.has_changed().unwrap());
    assert_eq!(
        active_rx.borrow().generation,
        apply_failed.post_active_generation as u64
    );
    let failed_status = fetch_runtime_status(node.as_ref(), node_did).await;
    assert_eq!(
        failed_status.reconcile_phase,
        apply_failed.post_phase.as_str()
    );
    assert_eq!(failed_status.last_reconcile_result, "error");
    assert!(!failed_status.last_reconcile_error.is_empty());

    proposal_tx.send(valid_updated_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), active_rx.changed())
        .await
        .expect("valid update should publish after failed apply")
        .unwrap();
    assert_eq!(active_rx.borrow().generation, 2);
    let recovered_status = fetch_runtime_status(node.as_ref(), node_did).await;
    assert_eq!(recovered_status.reconcile_phase, "idle");
    assert_eq!(recovered_status.last_reconcile_result, "applied");
    assert!(recovered_status.last_reconcile_error.is_empty());

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

struct FailGenerationSourceWriter {
    fatal_attempted: Arc<Notify>,
    fatal_release: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl crate::node_readiness_publisher::NodeReadinessWriter for FailGenerationSourceWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &gents_protocol::node_readiness::NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        if snapshot.active_generation == 2 {
            self.fatal_attempted.notify_one();
            self.fatal_release.acquire().await.unwrap().forget();
            return Err(crate::node_readiness_publisher::FatalNodeReadinessWrite.into());
        }
        Ok(())
    }
}

struct GateGenerationSourceWriter {
    attempted: Arc<Notify>,
    release: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl crate::node_readiness_publisher::NodeReadinessWriter for GateGenerationSourceWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &gents_protocol::node_readiness::NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        if snapshot.active_generation == 2 {
            self.attempted.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        Ok(())
    }
}

struct PublisherBackedSlotFailurePolicy {
    runtime_status: RuntimeStatusHandle,
}

#[async_trait::async_trait]
impl SlotFailurePolicy for PublisherBackedSlotFailurePolicy {
    fn build_failure_budget(&self) -> u32 {
        1
    }

    async fn on_slot_created(&self, agent_id: &str, generation: u64) -> Result<()> {
        self.runtime_status
            .readiness()
            .register_slot(agent_id, generation)
            .await
            .context("register test slot standing")?;
        Ok(())
    }

    async fn try_demote(&self, agent_id: &str, generation: u64, error: &str) -> Result<bool> {
        self.runtime_status
            .readiness()
            .demote_slot(agent_id, generation, error.to_string())
            .await
            .context("demote test slot")
    }

    async fn on_slot_retired(&self, agent_id: &str, generation: u64, _recreated: bool) {
        self.runtime_status
            .readiness()
            .retire_slot(agent_id, generation)
            .await
            .expect("retire test slot");
    }
}

struct FailSecondRegistrationPolicy {
    registrations: AtomicUsize,
    retired: StdMutex<Vec<(String, u64)>>,
}

#[async_trait::async_trait]
impl SlotFailurePolicy for FailSecondRegistrationPolicy {
    fn build_failure_budget(&self) -> u32 {
        1
    }

    async fn on_slot_created(&self, _agent_id: &str, _generation: u64) -> Result<()> {
        if self.registrations.fetch_add(1, Ordering::SeqCst) == 1 {
            anyhow::bail!("injected slot registration failure");
        }
        Ok(())
    }

    async fn try_demote(&self, _agent_id: &str, _generation: u64, _error: &str) -> Result<bool> {
        Ok(false)
    }

    async fn on_slot_retired(&self, agent_id: &str, generation: u64, _recreated: bool) {
        self.retired
            .lock()
            .unwrap()
            .push((agent_id.to_string(), generation));
    }
}

#[tokio::test]
async fn registration_failure_rolls_back_standing_before_any_staged_slot_spawns() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let initial = Arc::new(
        PendingAgent::new("general")
            .build_with_identity_for_test(test_identity("register-failure-initial")),
    );
    let mut changed = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("register-failure-changed"));
    changed.system_prompt = "changed".to_string();
    let added = Arc::new(
        PendingAgent::new("second")
            .build_with_identity_for_test(test_identity("register-failure-second")),
    );
    let initial_snapshot = snapshot_for_agents(node.as_ref(), "general", vec![initial]).await;
    let replacement_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(changed), added]).await;
    let started_generations = Arc::new(StdMutex::new(Vec::new()));
    let runner = {
        let started_generations = started_generations.clone();
        move |_agent: Arc<ResolvedAgent>,
              _tool_surface: Arc<ToolSurface>,
              _request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            started_generations.lock().unwrap().push(generation);
            async move {
                let _ = shutdown.changed().await;
                Ok(())
            }
        }
    };
    let (runtime_status_owner, runtime_status) =
        RuntimeStatusHandle::start(node.clone(), "did:test:registration-failure");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node),
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
        runner,
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let policy = Arc::new(FailSecondRegistrationPolicy {
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
    assert_eq!(supervisor.active_slots["general"].generation, 1);
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
            .contains(&("general".to_string(), 1)),
        "supervisor shutdown must retire the active generation standing"
    );
    runtime_status_owner.close().await.unwrap();
}

#[tokio::test]
async fn retired_slot_drain_failure_reaches_supervisor_shutdown() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let runtime_node = stub_runtime_node();
    let initial = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("retired-drain-failure"));
    let mut replacement = initial.clone();
    replacement.system_prompt = "replacement generation".to_string();
    let initial_snapshot = snapshot_for_agents_with_node(
        node.as_ref(),
        "general",
        vec![Arc::new(initial)],
        runtime_node.clone(),
    )
    .await;
    let replacement_snapshot = snapshot_for_agents_with_node(
        node.as_ref(),
        "general",
        vec![Arc::new(replacement)],
        runtime_node,
    )
    .await;
    let (runtime_status_owner, runtime_status) =
        RuntimeStatusHandle::start(node.clone(), "did:test:retired-drain-failure");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let runner = move |_agent: Arc<ResolvedAgent>,
                       _tool_surface: Arc<ToolSurface>,
                       _request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
                       generation: u64,
                       mut shutdown: watch::Receiver<bool>| {
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
        crate::admission::AdmissionRegistry::new(node.clone()),
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
        runner,
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let worker_count = supervisor.active_slots["general"].worker_task_count;
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
    node.shutdown().await;
}

#[tokio::test]
async fn source_publish_failure_rolls_back_and_joins_staged_slots() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let initial = Arc::new(
        PendingAgent::new("general")
            .build_with_identity_for_test(test_identity("source-failure-initial")),
    );
    let mut replacement = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("source-failure-replacement"));
    replacement.system_prompt = "replacement".to_string();
    let initial_snapshot = snapshot_for_agents(node.as_ref(), "general", vec![initial]).await;
    let replacement_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(replacement)]).await;

    let fatal_attempted = Arc::new(Notify::new());
    let fatal_release = Arc::new(Semaphore::new(0));
    let generation_one_exit = Arc::new(Semaphore::new(0));
    let generation_two_exit = Arc::new(Semaphore::new(0));
    let generation_two_waiting_exit = Arc::new(Notify::new());
    let writer = Arc::new(FailGenerationSourceWriter {
        fatal_attempted: fatal_attempted.clone(),
        fatal_release: fatal_release.clone(),
    });
    let (runtime_status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        "did:test:source-failure-rollback",
        writer,
        Duration::from_millis(1),
    );
    runtime_status.initialize_startup("general").await.unwrap();
    let slot_failure_policy = Arc::new(PublisherBackedSlotFailurePolicy {
        runtime_status: runtime_status.clone(),
    });
    let runner = {
        let generation_one_exit = generation_one_exit.clone();
        let generation_two_exit = generation_two_exit.clone();
        let generation_two_waiting_exit = generation_two_waiting_exit.clone();
        move |_agent: Arc<ResolvedAgent>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let generation_one_exit = generation_one_exit.clone();
            let generation_two_exit = generation_two_exit.clone();
            let generation_two_waiting_exit = generation_two_waiting_exit.clone();
            async move {
                tokio::select! {
                    _ = shutdown.changed() => {}
                    _ = async {
                        let mut receiver = request_rx.lock().await;
                        while receiver.recv().await.is_some() {}
                    } => {}
                }
                if generation == 2 {
                    generation_two_waiting_exit.notify_one();
                }
                let permit = if generation == 1 {
                    generation_one_exit.acquire().await.unwrap()
                } else {
                    generation_two_exit.acquire().await.unwrap()
                };
                permit.forget();
                Ok(())
            }
        }
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node),
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
        runner,
        runtime_status.clone(),
        shutdown_rx.clone(),
        Some(slot_failure_policy),
    )
    .await
    .unwrap();
    let workers_per_slot = supervisor.active_slots["general"].worker_task_count;
    runtime_status
        .readiness()
        .publish_snapshot(supervisor.current_snapshot().as_ref())
        .await
        .unwrap();
    assert!(runtime_status
        .readiness()
        .demote_slot("general", 1, "generation one demoted".to_string())
        .await
        .unwrap());
    assert_eq!(
        runtime_status
            .readiness()
            .observation()
            .demotion_reason("general"),
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
    generation_two_waiting_exit.notified().await;
    assert!(
        !apply.is_finished(),
        "failed source publication detached the staged generation"
    );
    generation_two_exit.add_permits(workers_per_slot);
    let (supervisor, result) = apply.await.unwrap();
    let error = result.expect_err("injected source publication must fail apply");
    assert!(error.to_string().contains("node readiness"));
    assert_eq!(supervisor.current_snapshot.generation, 1);
    assert_eq!(supervisor.active_slots["general"].generation, 1);
    let rolled_back = runtime_status.readiness().observation();
    assert_eq!(rolled_back.source_generation(), 1);
    assert_eq!(
        rolled_back.demotion_reason("general"),
        Some("generation one demoted"),
        "failed source publication must preserve the old slot's demotion"
    );
    assert!(
        runtime_status
            .readiness()
            .mark_slot_ready("general", 1)
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
    generation_one_exit.add_permits(workers_per_slot);
    shutdown.await.unwrap().unwrap();
    runtime_status_owner.close().await.unwrap();
}

#[tokio::test]
async fn closed_snapshot_receiver_does_not_detach_retired_or_active_slots() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let source_attempted = Arc::new(Notify::new());
    let source_release = Arc::new(Semaphore::new(0));
    let (runtime_status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        "did:test:closed-active-watch",
        Arc::new(GateGenerationSourceWriter {
            attempted: source_attempted.clone(),
            release: source_release.clone(),
        }),
        Duration::from_millis(1),
    );
    let initial = Arc::new(
        PendingAgent::new("general")
            .build_with_identity_for_test(test_identity("closed-watch-initial")),
    );
    let mut replacement = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("closed-watch-replacement"));
    replacement.system_prompt = "replacement".to_string();
    let initial_snapshot = snapshot_for_agents(node.as_ref(), "general", vec![initial]).await;
    let replacement_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![Arc::new(replacement)]).await;
    let exited = Arc::new(AtomicUsize::new(0));
    let generation_one_exit = Arc::new(Semaphore::new(0));
    let generation_two_exit = Arc::new(Semaphore::new(0));
    let generation_two_waiting_exit = Arc::new(Notify::new());
    let runner = {
        let exited = exited.clone();
        let generation_one_exit = generation_one_exit.clone();
        let generation_two_exit = generation_two_exit.clone();
        let generation_two_waiting_exit = generation_two_waiting_exit.clone();
        move |_agent: Arc<ResolvedAgent>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let exited = exited.clone();
            let generation_one_exit = generation_one_exit.clone();
            let generation_two_exit = generation_two_exit.clone();
            let generation_two_waiting_exit = generation_two_waiting_exit.clone();
            async move {
                loop {
                    let should_exit = tokio::select! {
                        _ = shutdown.changed() => true,
                        message = async {
                            let mut receiver = request_rx.lock().await;
                            receiver.recv().await
                        } => message.is_none(),
                    };
                    if should_exit {
                        if generation == 2 {
                            generation_two_waiting_exit.notify_one();
                        }
                        let permit = if generation == 1 {
                            generation_one_exit.acquire().await.unwrap()
                        } else {
                            generation_two_exit.acquire().await.unwrap()
                        };
                        permit.forget();
                        exited.fetch_add(1, Ordering::SeqCst);
                        return Ok(());
                    }
                }
            }
        }
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node),
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
        runner,
        runtime_status,
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap();
    let workers_per_slot = supervisor.active_slots["general"].worker_task_count;
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
    generation_two_waiting_exit.notified().await;
    assert!(
        !apply.is_finished(),
        "closed watch rollback must retain the staged generation until it joins"
    );
    generation_two_exit.add_permits(workers_per_slot);
    let (supervisor, result) = apply.await.unwrap();
    let error = result.expect_err("closed active snapshot receiver must reject publication");
    assert!(error.to_string().contains("receiver closed"));
    assert_eq!(supervisor.current_snapshot.generation, 1);
    assert_eq!(supervisor.active_slots["general"].generation, 1);
    shutdown_tx.send_replace(true);
    let mut shutdown_task = tokio::spawn(async move { supervisor.shutdown_slots().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown_task)
            .await
            .is_err(),
        "shutdown detached the blocked slot owners"
    );
    assert_eq!(exited.load(Ordering::SeqCst), workers_per_slot);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown_task)
            .await
            .is_err(),
        "shutdown completed before joining the retired generation-one slot"
    );
    generation_one_exit.add_permits(workers_per_slot);
    shutdown_task.await.unwrap().unwrap();
    runtime_status_owner.close().await.unwrap();
    assert_eq!(
        exited.load(Ordering::SeqCst),
        2 * workers_per_slot,
        "both the retired generation and replacement generation must be joined"
    );
}

#[tokio::test]
async fn generation_supervisor_rotates_dispatcher_on_tool_surface_change() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let node_did = "did:test:reconcile-tool-surface-test";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), node_did);
    let identity = Arc::new(test_identity("tool-surface-general"));
    let runtime_node = Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity: identity.clone(),
        default_agent_id: String::new(),
        display_name: None,
        enabled: true,
    });

    let initial_agent = Arc::new(ResolvedAgent {
        skills: Vec::new(),
        agent_id: "general".to_string(),
        node: runtime_node.clone(),
        backend_id: Some("backend-general".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
        backend_auth: crate::document_config::BackendAuth::Unauthenticated,
        model_name: "default".to_string(),
        resolved_reasoning_efforts: None,
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: crate::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: crate::config::DEFAULT_MAX_TURNS,
        max_turns_provenance: crate::config::MaxTurnsProvenance::Default,
        system_prompt: "initial".to_string(),
        tools: AgentToolSurfaceConfig::meta_only(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: crate::config::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(crate::config::DEFAULT_DEADLINE_DURATION_SECS),
        provider_idle_timeout: Duration::from_secs(
            crate::config::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry: crate::agent::completion_retry::CompletionRetryProfileFields::default(),
        sampling: crate::config::SamplingConfig::default(),
    });
    let updated_tools: crate::document_config::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "general-tools",
        "node_did": runtime_node.node_did,
        "host": {"files": {"mode": "ReadOnly"}},
        "built_ins": {"enable_context_budget": true}
    }))
    .unwrap();
    let updated_agent = Arc::new(ResolvedAgent {
        tools: AgentToolSurfaceConfig::from_tools_document(
            "general",
            &updated_tools,
            &ToolCeiling::readonly(),
            Vec::new(),
        )
        .unwrap(),
        ..initial_agent.as_ref().clone()
    });

    let initial_snapshot = snapshot_for_agents(node.as_ref(), "general", vec![initial_agent]).await;
    let updated_snapshot = snapshot_for_agents(node.as_ref(), "general", vec![updated_agent]).await;

    let observed_tool_names = Arc::new(StdMutex::new(Vec::<Vec<String>>::new()));
    let runner = {
        let observed_tool_names = observed_tool_names.clone();
        move |_agent: Arc<ResolvedAgent>,
              tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              _generation: u64,
              mut shutdown: watch::Receiver<bool>| {
            let observed_tool_names = observed_tool_names.clone();
            async move {
                observed_tool_names
                    .lock()
                    .unwrap()
                    .push(tool_surface.tool_names());
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
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node.clone()),
        crate::retry::RetryPolicy {
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
    let active_snapshot = supervisor.current_snapshot();
    let (active_tx, mut active_rx) = watch::channel(active_snapshot);
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
            if observed_tool_names
                .lock()
                .unwrap()
                .iter()
                .any(|tool_names| tool_names.contains(&"read_file".to_string()))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
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

/// #559: a slot retired by a generation change must release its agent_config from
/// the startup barrier (superseded) instead of orphaning the pending entry —
/// the policy's retirement hook is the only path that knowledge can take.
#[tokio::test]
async fn retiring_a_slot_notifies_the_failure_policy() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingPolicy {
        retired: std::sync::Mutex<Vec<(String, bool)>>,
        demote_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl slot::SlotFailurePolicy for RecordingPolicy {
        fn build_failure_budget(&self) -> u32 {
            3
        }
        async fn on_slot_created(&self, _agent_id: &str, _generation: u64) -> Result<()> {
            Ok(())
        }

        async fn try_demote(
            &self,
            _agent_id: &str,
            _generation: u64,
            _error: &str,
        ) -> Result<bool> {
            self.demote_calls.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        }
        async fn on_slot_retired(&self, agent_id: &str, _generation: u64, recreated: bool) {
            self.retired
                .lock()
                .expect("retired mutex")
                .push((agent_id.to_string(), recreated));
        }
    }

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let runtime_status =
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), "did:test:policy");
    let agent_config = Arc::new(
        PendingAgent::new("general")
            .build_with_identity_for_test(test_identity("policy-retirement-559")),
    );
    let initial_snapshot =
        snapshot_for_agents(node.as_ref(), "general", vec![agent_config.clone()]).await;
    // A runner that parks until shutdown: the agent_config never "starts", exactly
    // the mid-startup window the retirement release exists for.
    let runner = |_agent: Arc<ResolvedAgent>,
                  _tool_surface: Arc<ToolSurface>,
                  _request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
                  _generation: u64,
                  mut shutdown: watch::Receiver<bool>| async move {
        let _ = shutdown.changed().await;
        Ok(())
    };

    let policy = Arc::new(RecordingPolicy {
        retired: std::sync::Mutex::new(Vec::new()),
        demote_calls: AtomicUsize::new(0),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let supervisor = GenerationSupervisor::bootstrap(
        initial_snapshot,
        crate::admission::AdmissionRegistry::new(node.clone()),
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
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

    // A valid generation that replaces the default agent_config retires the old
    // slot outright without manufacturing an unassigned default snapshot.
    let replacement = Arc::new(
        PendingAgent::new("replacement")
            .build_with_identity_for_test(test_identity("policy-replacement-559")),
    );
    let replacement_snapshot =
        snapshot_for_agents(node.as_ref(), "replacement", vec![replacement]).await;
    proposal_tx.send(replacement_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), active_rx.changed())
        .await
        .expect("removal generation should publish")
        .unwrap();

    // The retirement callback is part of the ordered generation apply.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if policy
            .retired
            .lock()
            .expect("retired mutex")
            .contains(&("general".to_string(), false))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "retirement must notify the policy so the barrier is released"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

/// One Agent whose Tools selection names `fixture/list_files`, resolved
/// against two plugin homes: one without the record, one with it.
async fn plugin_resolution_surfaces(
    node: &defra_node::EmbeddedNode,
) -> (Arc<ResolvedAgent>, Arc<ToolSurface>, Arc<ToolSurface>) {
    let mut agent_config = PendingAgent::new("general")
        .build_with_identity_for_test(test_identity("plugin-resolution-fingerprint"));
    let tools: crate::document_config::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": format!("{}:tools", agent_config.agent_id),
        "node_did": agent_config.node_did(),
        "integrations": {"plugins": [{"plugin": "fixture/list_files"}]},
    }))
    .unwrap();
    agent_config.tools = AgentToolSurfaceConfig::from_tools_document(
        &agent_config.agent_id.clone(),
        &tools,
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();
    let agent_config = Arc::new(agent_config);

    let absent_home = tempfile::tempdir().unwrap();
    let absent_plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        absent_home.path().to_path_buf(),
    )));
    let installed_home = tempfile::tempdir().unwrap();
    let record: crate::plugin::store::InstalledPlugin = serde_json::from_value(serde_json::json!({
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
    crate::plugin::store::write_record(installed_home.path(), &record).unwrap();
    let installed_plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        installed_home.path().to_path_buf(),
    )));

    let absent = Arc::new(
        agent_config
            .tools
            .resolve(node, agent_config.node_did(), &absent_plugins)
            .await
            .unwrap(),
    );
    let installed = Arc::new(
        agent_config
            .tools
            .resolve(node, agent_config.node_did(), &installed_plugins)
            .await
            .unwrap(),
    );
    let plugin_ref = crate::document_config::PluginToolRef {
        tool_calls: false,
        plugin: "fixture/list_files".to_string(),
        digest: None,
        input_fields: Vec::new(),
    };
    assert_eq!(
        absent.plugin_resolutions(),
        &[(plugin_ref.clone(), None)],
        "a missing plugin record resolves to no identity, never an error"
    );
    assert_eq!(
        installed.plugin_resolutions(),
        &[(plugin_ref, Some(record.clone()))],
        "an installed plugin records its installed record"
    );
    (agent_config, absent, installed)
}

#[tokio::test]
async fn a_changed_plugin_resolution_changes_the_fingerprint_and_recreates_the_slot() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let runtime_node = stub_runtime_node();
    let (agent_config, absent_surface, installed_surface) =
        plugin_resolution_surfaces(node.as_ref()).await;

    let snapshot_for = |surface: Arc<ToolSurface>| {
        ResolvedRuntimeSnapshot::from_parts(
            "general".to_string(),
            vec![Arc::clone(&agent_config)],
            HashMap::from([("general".to_string(), surface)]),
            HashMap::new(),
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
    let active_absent = absent_snapshot.activate(1, HashMap::new());
    let diff = diff_counts(&active_absent, &installed_snapshot);
    assert_eq!(diff.updated, 1);
    assert_eq!(diff.added, 0);
    assert_eq!(diff.removed, 0);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let slot = spawn_slot(
        Arc::clone(&agent_config),
        Arc::clone(&absent_surface),
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 1,
        },
        |_, _, _, _, _| async { Ok(()) },
        shutdown_rx,
    );
    assert!(
        slot.matches(&agent_config, &absent_surface, 1),
        "the slot keeps its own surface"
    );
    assert!(
        !slot.matches(&agent_config, &installed_surface, 1),
        "a changed plugin resolution must recreate the slot so the Agent \
         re-admits with a fresh build budget"
    );
    let _ = shutdown_tx.send(true);
    retire_slot(slot);

    // A same-artifact reinstall with a changed declaration is a change too:
    // the built tool renders and enforces it.
    let mut reinstalled = installed_surface
        .plugin_resolutions()
        .first()
        .and_then(|(_, record)| record.clone())
        .expect("installed record");
    reinstalled.declaration.description = "lists files, reworded".to_owned();
    let home = tempfile::tempdir().unwrap();
    crate::plugin::store::write_record(home.path(), &reinstalled).unwrap();
    let plugins = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_path_buf()));
    let reinstalled_surface = Arc::new(
        agent_config
            .tools
            .resolve(node.as_ref(), agent_config.node_did(), &plugins)
            .await
            .unwrap(),
    );
    assert_ne!(
        snapshot_for(reinstalled_surface).configuration_fingerprint(),
        installed_snapshot.configuration_fingerprint(),
        "a same-artifact reinstall that changes the declaration must refresh the slot"
    );
}
