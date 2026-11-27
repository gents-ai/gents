use super::support::*;
use super::*;
use crate::runtime::agent::runtime::router::session_selection::UNRESOLVED_SESSION_ATTEMPT_BOUND;
use crate::runtime::agent::runtime::router::{RouterInputs, RuntimeAdmissionGate};
use crate::test_support::readiness::{
    GatedRouterGenerationDefraWriter, RejectRouterGenerationWriter,
};
use crate::test_support::running_runtime::{AgentRuntimeHome, BootedRuntime};
use crate::test_support::runtime::ScriptedWatcher;
use gents_inference::test_support::scripted_backend::{ScriptedBackend, Unscripted};
use gents_store::test_support::snapshot_reader::SnapshotReader as _;
use gents_store::test_support::{read_when_changed, STALL_CEILING};
use rapidhash::{HashMapExt, RapidHashMap};

fn routed_snapshot(
    generation: u64,
    agent_did: &str,
    dispatcher: mpsc::Sender<AgentRequest>,
) -> Arc<gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot> {
    Arc::new(
        gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot {
            placement_basis: Default::default(),
            placement_bodies: Default::default(),
            generation,
            node: None,
            local_did: String::new(),
            default_agent_did: agent_did.to_owned(),
            tool_configs: Default::default(),
            agents: RapidHashMap::new(),
            tool_surfaces: RapidHashMap::new(),
            backend_admission_configs: RapidHashMap::new(),
            unavailable_agents: RapidHashMap::new(),
            active_schedules: RapidHashMap::new(),
            unavailable_schedules: rapidhash::RapidHashSet::default(),
            active_event_triggers: RapidHashMap::new(),
            unavailable_event_triggers: rapidhash::RapidHashSet::default(),
            active_tasks: RapidHashMap::new(),
            dispatchers: RapidHashMap::from_iter([(
                agent_did.to_owned(),
                gents_automation::triggers::runtime_snapshot::AgentDispatcher::single(dispatcher),
            )]),
            agent_executor_capacities: RapidHashMap::new(),
            agent_executor_queue_capacities: RapidHashMap::new(),
        },
    )
}

#[tokio::test]
async fn invalid_execution_origin_route_rejection_terminalizes_without_stopping_router() {
    let db = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("invalid-origin-route").await,
        "invalid-origin-route",
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&db).await;
    let node = db.node.clone();
    let root = db.node_identity.clone();
    let agent = gents_store::identity::resolve_agent_identity(root.as_ref(), &binding).unwrap();
    grant_held_agent_operations(&node, root.as_ref(), agent.as_ref()).await;
    let node_did = root.did();
    let agent_did = agent.did();
    let doc_id = create_held_agent_request(
        &node,
        node_did,
        agent.as_ref(),
        "invalid-origin-route-request",
        "invalid-origin-route-session",
        "hostile",
    )
    .await;
    let mut malformed = request(Some(agent_did), "invalid-origin-route-session");
    malformed.doc_id = doc_id;
    malformed.request_id = "invalid-origin-route-request".to_string();
    malformed.node_did = node_did.to_string();

    super::super::router::fail_routed_request(
        node.clone(),
        node_did,
        malformed,
        agent_did,
        agent.as_ref(),
        "agent unavailable",
    )
    .await
    .expect("invalid origin must be terminalized, not escape the router");

    // The Agent-owned request row is read through the owning Agent's managed access.
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent_did.parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let response = access
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "invalid-origin-route-request" } }
                    limit: 1
                ) { lifecycle_state failure_reason }
            }"#,
        )
        .await
        .expect("reload terminal row");
    let row = response["data"]["AgentRequest"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("terminal request row");
    assert_eq!(
        row.get("lifecycle_state").and_then(Value::as_str),
        Some("failed")
    );
    assert!(row
        .get("failure_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| reason.contains("execution_origin")));
    node.shutdown().await.expect("node shutdown");
}

pub(super) fn routed_snapshot_with_agent(
    generation: u64,
    agent: Arc<gents_inference::resolved::ResolvedAgent>,
    dispatcher: mpsc::Sender<AgentRequest>,
) -> Arc<gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot> {
    let agent_did = agent.agent_did.clone();
    let Ok(mut snapshot) = Arc::try_unwrap(routed_snapshot(generation, &agent_did, dispatcher))
    else {
        panic!("the snapshot has a sole reference");
    };
    snapshot.agents = RapidHashMap::from_iter([(agent_did, agent)]);
    Arc::new(snapshot)
}

#[tokio::test]
async fn unresolvable_session_terminalizes_the_request_at_the_attempt_bound() {
    let signed_home = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("unresolved-session").await,
        "unresolved-session",
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, _user, _access) = signed_home.into_parts();
    let node = db.node.clone();
    let root = db.node_identity.clone();
    let agent = gents_store::identity::resolve_agent_identity(root.as_ref(), &binding).unwrap();
    grant_held_agent_operations(&node, root.as_ref(), agent.as_ref()).await;
    let node_did = root.did().to_owned();
    let agent_did = agent.did().to_owned();
    let doc_id = create_held_agent_request(
        &node,
        &node_did,
        agent.as_ref(),
        "unresolved-session-request",
        "unresolved-session",
        "hello",
    )
    .await;
    let resolved = Arc::new(
        crate::runtime::agent::PendingAgent::new("unresolved-session")
            .with_identity_binding_for_test(binding)
            .build_with_identity_arc_for_test(root.clone()),
    );

    let (dispatcher, mut executor_rx) = mpsc::channel(1);
    let snapshot = routed_snapshot_with_agent(1, resolved.agent.clone(), dispatcher);
    let (_active_tx, active_rx) = watch::channel(snapshot.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (request_tx, request_rx) = mpsc::channel(1);
    let (status_owner, status) =
        RuntimeStatusHandle::start_with_unbounded_test_clock(node.clone(), node_did.clone());
    status.initialize_startup(&agent_did).await.unwrap();
    status
        .readiness()
        .register_slot(&agent_did, 1)
        .await
        .unwrap();
    status
        .readiness()
        .publish_snapshot(snapshot.as_ref())
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::runtime::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let gate = RuntimeAdmissionGate::closed();
    gate.open().await;

    let mut routed = request(Some(&agent_did), "unresolved-session");
    routed.node_did = node_did.clone();
    routed.doc_id = doc_id;
    routed.request_id = "unresolved-session-request".to_owned();
    routed.requester_did = Some(agent_did.clone());
    routed.execution_origin = Some("interactive".to_owned());
    let mut updates = node.subscribe(&[defra_node::EventName::Update]);
    request_tx.send(Ok(routed)).await.unwrap();

    let router_node = node.clone();
    let router_node_did = node_did.clone();
    let router = tokio::spawn(async move {
        super::super::router::run_router_with_watcher(RouterInputs {
            node: router_node,
            node_did: router_node_did,
            watcher: ScriptedWatcher::new(request_rx),
            active_snapshot_rx: active_rx,
            shutdown: shutdown_rx,
            admission_gate: gate,
            runtime_status: status,
            runtime_snapshot_observer: None,
            unresolved_session_bound: 1,
        })
        .await
    });

    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent_did.parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let reason = loop {
        let response = access
            .execute(
                r#"{
                    AgentRequest(
                        filter: { request_id: { _eq: "unresolved-session-request" } }
                        limit: 1
                    ) { lifecycle_state failure_reason }
                }"#,
            )
            .await
            .expect("reload routed row");
        let row = response["data"]["AgentRequest"]
            .as_array()
            .and_then(|rows| rows.first())
            .cloned()
            .expect("routed request row");
        if row.get("lifecycle_state").and_then(Value::as_str) == Some("failed") {
            break row
                .get("failure_reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        tokio::time::timeout_at(deadline, updates.recv())
            .await
            .expect("router left an unresolvable request pending")
            .expect("request update subscription closed");
    };
    assert!(
        reason.contains("unresolved-session") && reason.contains("routing attempts"),
        "failure reason must name the session: {reason}"
    );
    assert!(
        executor_rx.try_recv().is_err(),
        "an unresolvable request must not dispatch"
    );

    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), router)
        .await
        .expect("router must stop")
        .unwrap()
        .unwrap();
    status_owner.close().await.unwrap();
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn closing_admission_cancels_a_send_to_a_full_executor_queue() {
    let signed_home = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("runtime-router").await,
        "runtime-router",
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, _user, _user_access) = signed_home.into_parts();
    let node = db.node.clone();
    let node_did = db.node_identity.did().to_owned();
    let agent_did = binding.agent_did;
    let (dispatcher, mut executor_rx) = mpsc::channel(1);
    dispatcher
        .send(request(Some(&agent_did), "already-queued"))
        .await
        .unwrap();
    let snapshot = routed_snapshot(1, &agent_did, dispatcher);
    let (_active_tx, active_rx) = watch::channel(snapshot.clone());
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let (request_tx, request_rx) = mpsc::channel(1);
    let (status_owner, status) =
        RuntimeStatusHandle::start_with_unbounded_test_clock(node.clone(), node_did.clone());
    status.initialize_startup(&agent_did).await.unwrap();
    status
        .readiness()
        .register_slot(&agent_did, 1)
        .await
        .unwrap();
    status
        .readiness()
        .publish_snapshot(snapshot.as_ref())
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::runtime::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let gate = RuntimeAdmissionGate::closed();
    gate.open().await;

    let mut routed = request(Some(&agent_did), "blocked-dispatch");
    routed.node_did = node_did.to_string();
    request_tx.send(Ok(routed)).await.unwrap();
    let gate_for_router = gate.clone();
    let router_node = node.clone();
    let router_node_did = node_did.clone();
    let router = tokio::spawn(async move {
        super::super::router::run_router_with_watcher(RouterInputs {
            node: router_node,
            node_did: router_node_did,
            watcher: ScriptedWatcher::new(request_rx),
            active_snapshot_rx: active_rx,
            shutdown: shutdown_rx,
            admission_gate: gate_for_router,
            runtime_status: status,
            runtime_snapshot_observer: None,
            unresolved_session_bound: UNRESOLVED_SESSION_ATTEMPT_BOUND,
        })
        .await
    });

    tokio::time::timeout(Duration::from_secs(1), gate.wait_for_entry_for_test())
        .await
        .expect("router must hold an admission lease before the close race");
    tokio::time::timeout(Duration::from_secs(1), gate.close())
        .await
        .expect("gate close must cancel the blocked executor send");
    tokio::time::timeout(Duration::from_secs(1), router)
        .await
        .expect("router must leave the blocked send after admission closes")
        .unwrap()
        .unwrap();
    assert_eq!(
        executor_rx.recv().await.unwrap().session_id,
        "already-queued"
    );
    assert!(
        executor_rx.try_recv().is_err(),
        "closed admission leaked work"
    );
    status_owner.close().await.unwrap();
    node.shutdown().await.expect("node shutdown");
}

const BLOCKED_LEVEL_WORKERS: usize = 8;
const BLOCKED_QUEUE_CAPACITY: usize = 32;

async fn read_request_state_and_generation(
    access: &gents_store::config_client::ConfigAccess,
    request_id: &str,
) -> (Option<String>, Option<i64>) {
    let escaped = escape_graphql_string(request_id);
    let data = access
        .read_data(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state executed_runtime_generation }} }}"#
        ))
        .await;
    let row = &data["AgentRequest"][0];
    (
        row["lifecycle_state"].as_str().map(str::to_owned),
        row["executed_runtime_generation"].as_i64(),
    )
}

async fn recv_next_dispatch(
    dispatch_rx: &mut mpsc::UnboundedReceiver<String>,
    what: &str,
) -> String {
    tokio::time::timeout(STALL_CEILING, dispatch_rx.recv())
        .await
        .unwrap_or_else(|_| panic!("{what}: timed out waiting for router dispatch"))
        .unwrap_or_else(|| panic!("{what}: router dispatch probe closed"))
}

async fn fill_generation_one_queue_and_block_target(
    home: &AgentRuntimeHome,
    held: &gents_inference::test_support::scripted_backend::HeldCall<'_>,
    dispatch_rx: &mut mpsc::UnboundedReceiver<String>,
    target_request_id: &str,
) {
    let agent_did = home.agent.did().to_owned();
    let hold_request_id = "blocked-a-00".to_owned();
    home.submit_request(
        &agent_did,
        &hold_request_id,
        "blocked-session-a-00",
        "blocked-hold-0",
    )
    .await;
    // Hold the backend's single max_concurrent=1 permit before submitting the
    // remaining worker requests so none of them can race ahead and complete.
    held.entered().await;

    let mut worker_ids = Vec::with_capacity(BLOCKED_LEVEL_WORKERS);
    worker_ids.push(hold_request_id);
    let rest_worker_specs: Vec<(String, String)> = (1..BLOCKED_LEVEL_WORKERS)
        .map(|index| {
            (
                format!("blocked-a-{index:02}"),
                format!("blocked-session-a-{index:02}"),
            )
        })
        .collect();
    futures::future::join_all(rest_worker_specs.iter().map(|(req_id, session_id)| {
        home.submit_request(&agent_did, req_id, session_id, "blocked-worker")
    }))
    .await;
    worker_ids.extend(rest_worker_specs.into_iter().map(|(req_id, _)| req_id));

    let user_access = home.home.user_access.clone();
    read_when_changed(
        &user_access,
        "AgentRequest",
        "all level-0 workers occupy their requests",
        async || {
            let data = user_access
                .read_data(r#"{ AgentRequest { request_id lifecycle_state } }"#)
                .await;
            let processing = data["AgentRequest"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| {
                    row["request_id"]
                        .as_str()
                        .is_some_and(|id| worker_ids.iter().any(|wanted| wanted == id))
                        && row["lifecycle_state"].as_str() == Some("processing")
                })
                .count();
            if processing == BLOCKED_LEVEL_WORKERS {
                Ok(())
            } else {
                Err(format!("processing {processing}/{BLOCKED_LEVEL_WORKERS}"))
            }
        },
    )
    .await;

    let mut seen_workers = rapidhash::RapidHashSet::default();
    let mut channel_entries = 0usize;
    while seen_workers.len() < BLOCKED_LEVEL_WORKERS {
        let dispatched = recv_next_dispatch(dispatch_rx, "worker requests").await;
        if worker_ids.iter().any(|wanted| wanted == &dispatched) && seen_workers.insert(dispatched)
        {
            continue;
        }
        channel_entries += 1;
    }

    let queue_needed = BLOCKED_QUEUE_CAPACITY.saturating_sub(channel_entries);
    let queue_specs: Vec<(String, String)> = (0..queue_needed)
        .map(|index| {
            (
                format!("blocked-b-{index:02}"),
                format!("blocked-session-b-{index:02}"),
            )
        })
        .collect();
    for chunk in queue_specs.chunks(BLOCKED_LEVEL_WORKERS) {
        futures::future::join_all(chunk.iter().map(|(req_id, session_id)| {
            home.submit_request(&agent_did, req_id, session_id, "blocked-queue")
        }))
        .await;
    }
    while channel_entries < BLOCKED_QUEUE_CAPACITY {
        let _dispatched = recv_next_dispatch(dispatch_rx, "queue fill requests").await;
        channel_entries += 1;
    }

    home.submit_request(
        &agent_did,
        target_request_id,
        "blocked-session-z-target",
        "blocked-target",
    )
    .await;
    let blocked = recv_next_dispatch(dispatch_rx, "blocked target request").await;
    assert_eq!(
        blocked, target_request_id,
        "router blocked on {blocked} instead of target {target_request_id}"
    );
}

/// Fills generation 1's real level-0 workers and bounded queue on a booted
/// runtime so the target request blocks on `dispatcher.reserve()`. Replays
/// each modeled wait event (`nothing`, `snapshot_changed`, `admission_closed`)
/// and observes which real executor generation claims the target request and
/// whether the router stops.
#[tokio::test]
async fn blocked_dispatch_follows_the_lean_model() {
    for case in
        gents_lean_vocab::families::router_dispatch_blocked_dispatch::cases::<String, String>()
    {
        let backend = ScriptedBackend::builder("default")
            .unscripted(Unscripted::Reply)
            .start()
            .unwrap();
        let held = backend.hold("blocked-hold-0");
        let home = AgentRuntimeHome::start(
            &format!("blocked-dispatch-{}", case.input.event),
            None,
            backend.endpoint(),
            1,
            gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
        )
        .await;
        let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
        let (router_stopped_tx, router_stopped_rx) = watch::channel(false);
        let runtime = BootedRuntime::boot(
            &home,
            crate::runtime::agent::DocumentRuntimeOptions {
                tool_ceiling: ToolCeiling::meta_only(),
                router_dispatch_probe: Some(dispatch_tx),
                router_stopped_probe: Some(router_stopped_tx),
                ..Default::default()
            },
        )
        .await;
        let initial_generation = runtime
            .readiness()
            .await
            .expect("booted runtime readiness")
            .active_generation;
        assert_eq!(initial_generation, 1);

        let target_id = "blocked-z-target";
        fill_generation_one_queue_and_block_target(&home, &held, &mut dispatch_rx, target_id).await;

        let user_access = home.home.user_access.clone();
        let (destination, router_stopped) = match case.input.event.as_str() {
            "snapshot_changed" => {
                home.bind_backend(backend.endpoint(), 2).await;
                let executed_generation = read_when_changed(
                    &user_access,
                    "AgentRequest",
                    "rerouted target request completes on generation 2",
                    async || {
                        let (state, generation) =
                            read_request_state_and_generation(&user_access, target_id).await;
                        if state.as_deref() == Some("completed") {
                            Ok(generation)
                        } else {
                            Err(format!("state={state:?} generation={generation:?}"))
                        }
                    },
                )
                .await;
                let router_stopped = *router_stopped_rx.borrow();
                held.release();
                runtime.shutdown().await.expect("the runtime stops cleanly");
                let destination = match executed_generation {
                    Some(1) => "routed_executor",
                    Some(generation) if generation > 1 => "newest_executor",
                    _ => "undispatched",
                };
                (destination, router_stopped)
            }
            "admission_closed" => {
                runtime
                    .shutdown()
                    .await
                    .expect("shutdown closes admission and stops the blocked router");
                let router_stopped = *router_stopped_rx.borrow();
                let (state, generation) =
                    read_request_state_and_generation(&user_access, target_id).await;
                let destination = match (state.as_deref(), generation) {
                    (Some("pending"), None) => "undispatched",
                    (_, Some(1)) => "routed_executor",
                    (_, Some(gen)) if gen > 1 => "newest_executor",
                    other => panic!("unexpected target state after shutdown: {other:?}"),
                };
                (destination, router_stopped)
            }
            "nothing" => {
                let (state, generation) =
                    read_request_state_and_generation(&user_access, target_id).await;
                let router_stopped = *router_stopped_rx.borrow();
                held.release();
                runtime.shutdown().await.expect("the runtime stops cleanly");
                let destination = match (state.as_deref(), generation) {
                    (Some("pending"), None) => "undispatched",
                    (_, Some(1)) => "routed_executor",
                    (_, Some(gen)) if gen > 1 => "newest_executor",
                    other => panic!("unexpected target state while blocked: {other:?}"),
                };
                (destination, router_stopped)
            }
            other => panic!("unmodeled blocked dispatch event {other}"),
        };
        case.check(
            gents_lean_vocab::families::router_dispatch_blocked_dispatch::BlockedDispatch {
                destination: destination.to_owned(),
                router_stopped,
            },
        );
    }
}

#[tokio::test]
async fn router_generation_write_failure_closes_admission_before_dequeue_or_dispatch() {
    let db = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("runtime-router").await,
        "runtime-router",
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&db).await;
    let node = db.node.clone();
    let node_did = db.node_identity.did().to_owned();
    let agent_did = binding.agent_did;
    let (dispatcher, mut executor_rx) = mpsc::channel(1);
    let snapshot = routed_snapshot(1, &agent_did, dispatcher);
    let (_active_tx, active_rx) = watch::channel(snapshot.clone());
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let (request_tx, request_rx) = mpsc::channel(1);
    let (status_owner, status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        node_did.clone(),
        Arc::new(RejectRouterGenerationWriter),
        Duration::from_millis(1),
        crate::runtime::storage_fault::StorageFaultObservation::new(),
    );
    status.initialize_startup(&agent_did).await.unwrap();
    status
        .readiness()
        .register_slot(&agent_did, 1)
        .await
        .unwrap();
    status
        .readiness()
        .publish_snapshot(snapshot.as_ref())
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::runtime::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let gate = RuntimeAdmissionGate::closed();
    gate.open().await;
    let mut routed = request(Some(&agent_did), "must-remain-pending");
    routed.node_did = node_did.to_string();
    request_tx.send(Ok(routed)).await.unwrap();
    let watcher_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let error = super::super::router::run_router_with_watcher(RouterInputs {
        node: node.clone(),
        node_did: node_did.to_string(),
        watcher: ScriptedWatcher::new(request_rx).with_calls(watcher_calls.clone()),
        active_snapshot_rx: active_rx,
        shutdown: shutdown_rx,
        admission_gate: gate.clone(),
        runtime_status: status,
        runtime_snapshot_observer: None,
        unresolved_session_bound: UNRESOLVED_SESSION_ATTEMPT_BOUND,
    })
    .await
    .expect_err("router generation persistence failure must stop the router");
    assert!(
        format!("{error:#}").contains("injected fatal agent readiness write"),
        "unexpected router error: {error:#}"
    );
    assert!(!gate.is_open().await);
    assert_eq!(
        watcher_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "router dequeued a request before its generation was durably acknowledged"
    );
    assert!(executor_rx.try_recv().is_err(), "request reached executor");
    status_owner.close().await.unwrap();
    node.shutdown().await.expect("node shutdown");
}

/// Drives `contracts_runtime_reconcile_router` through the real runtime:
/// generation 1 boots Ready, a real backend update publishes generation 2 while
/// `GatedRouterGenerationDefraWriter` holds the router's acknowledgement of
/// generation 2, exposing the window where `active_generation == 2` and
/// `router_generation == 1`. Releasing the gate lets the router persist
/// generation 2 and complete requests on generation 2.
async fn replay_router_reconcile_case(
    action: &str,
    pre_active_generation: u64,
    pre_router_generation: u64,
    name: &str,
) -> (bool, u64) {
    let backend = ScriptedBackend::builder("default")
        .unscripted(Unscripted::Reply)
        .start()
        .unwrap();
    let home = AgentRuntimeHome::start(
        &format!("router-reconcile-{name}"),
        None,
        backend.endpoint(),
        1,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
    )
    .await;
    let node = home.home.store.node.clone();
    let identity = home.home.store.node_identity.clone();
    let node_did = identity.did().to_owned();
    let agent_did = home.agent.did().to_owned();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let writer = Arc::new(GatedRouterGenerationDefraWriter {
        node: node.clone(),
        router_generation: pre_active_generation,
        gated: std::sync::atomic::AtomicBool::new(true),
        entered: entered.clone(),
        release: release.clone(),
    });
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(home.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(super::startup::run_agent_with_readiness_writer(
        agent,
        shutdown_rx,
        writer,
        Duration::from_secs(20),
    ));

    wait_for_runtime_process_state(node.as_ref(), &node_did, "ready").await;
    read_when_changed(
        node.as_ref(),
        "NodeReadiness",
        "generation 1 is ready and routed",
        async || {
            let readiness = fetch_node_readiness(node.as_ref(), &node_did).await;
            if readiness.process_state == NodeReadinessProcessState::Ready
                && readiness.active_generation == 1
                && readiness.router_generation == 1
            {
                Ok(())
            } else {
                Err(format!("{readiness:?}"))
            }
        },
    )
    .await;

    home.bind_backend(backend.endpoint(), 2).await;
    tokio::time::timeout(STALL_CEILING, entered.notified())
        .await
        .expect("router must attempt to acknowledge generation 2");

    let while_unobserved = fetch_node_readiness(node.as_ref(), &node_did).await;
    assert_eq!(while_unobserved.active_generation, pre_active_generation);
    assert_eq!(while_unobserved.router_generation, 1);

    let user_access = home.home.user_access.clone();
    let (legal, post_router_generation) = match (action, pre_router_generation) {
        ("routerObserve", _) => {
            release.add_permits(1);
            let observed = read_when_changed(
                node.as_ref(),
                "NodeReadiness",
                "router acknowledges generation 2",
                async || {
                    let readiness = fetch_node_readiness(node.as_ref(), &node_did).await;
                    if readiness.router_generation == pre_active_generation {
                        Ok(readiness.router_generation)
                    } else {
                        Err(format!("{readiness:?}"))
                    }
                },
            )
            .await;
            (observed != pre_router_generation, observed)
        }
        ("acceptRequest", router_gen) if router_gen < pre_active_generation => {
            let req_id = format!("router-unobserved-{name}");
            home.submit_request(&agent_did, &req_id, "router-unobserved-session", "held")
                .await;
            let (state, executed_gen) =
                read_request_state_and_generation(&user_access, &req_id).await;
            let readiness = fetch_node_readiness(node.as_ref(), &node_did).await;
            assert_eq!(state.as_deref(), Some("pending"));
            assert_eq!(executed_gen, None);
            let result = (
                state.as_deref() == Some("completed"),
                readiness.router_generation,
            );
            release.add_permits(1);
            read_when_changed(
                &user_access,
                "AgentRequest",
                "held request finishes after router observes generation 2",
                async || {
                    let (state, generation) =
                        read_request_state_and_generation(&user_access, &req_id).await;
                    if state.as_deref() == Some("completed")
                        && generation == Some(pre_active_generation as i64)
                    {
                        Ok(())
                    } else {
                        Err(format!("state={state:?} generation={generation:?}"))
                    }
                },
            )
            .await;
            result
        }
        ("acceptRequest", _) => {
            release.add_permits(1);
            read_when_changed(
                node.as_ref(),
                "NodeReadiness",
                "router acknowledges generation 2 before request submission",
                async || {
                    let readiness = fetch_node_readiness(node.as_ref(), &node_did).await;
                    if readiness.router_generation == pre_active_generation {
                        Ok(())
                    } else {
                        Err(format!("{readiness:?}"))
                    }
                },
            )
            .await;
            let req_id = format!("router-observed-{name}");
            home.submit_request(&agent_did, &req_id, "router-observed-session", "routed")
                .await;
            let executed_gen = read_when_changed(
                &user_access,
                "AgentRequest",
                "request completes on generation 2",
                async || {
                    let (state, generation) =
                        read_request_state_and_generation(&user_access, &req_id).await;
                    if state.as_deref() == Some("completed") {
                        Ok(generation)
                    } else {
                        Err(format!("state={state:?} generation={generation:?}"))
                    }
                },
            )
            .await;
            let readiness = fetch_node_readiness(node.as_ref(), &node_did).await;
            assert_eq!(executed_gen, Some(pre_active_generation as i64));
            (true, readiness.router_generation)
        }
        (other, _) => panic!("the router fixture cannot drive {other}"),
    };

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, run)
        .await
        .expect("runtime stops within ceiling")
        .expect("runtime task joins")
        .expect("runtime stops cleanly");
    (legal, post_router_generation)
}

#[tokio::test]
async fn router_generation_steps_match_lean_runtime_reconcile_cases() {
    for case in gents_lean_vocab::families::contracts_runtime_reconcile_router::cases() {
        let (legal, post_router_generation) = replay_router_reconcile_case(
            case.input.action.as_str(),
            case.input.pre_active_generation,
            case.input.pre_router_generation,
            case.input.name.as_str(),
        )
        .await;
        case.check(
            gents_lean_vocab::families::contracts_runtime_reconcile_router::ReconcileRouterOutput {
                legal,
                post_router_generation,
            },
        );
    }
}
