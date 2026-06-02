use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch, Mutex};

use super::*;
use crate::agent::PendingAgentBehavior;
use crate::backend_provider::BackendProviderKind;
use crate::config::AgentBehavior;
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::identity::{AgentIdentity as _, AgentPrincipal, KeyIdentity};
use crate::lean_vocab_test::{
    assert_state_machine_contract_is_complete, lean_runtime_reconcile_case,
    lean_state_machine_contract,
};
use crate::runtime_status::RuntimeStatusHandle;
use crate::tool_surface::{
    BehaviorToolConfig, FileToolMode, ToolCeiling, ToolSelection, ToolSurface,
};
use crate::watcher::AgentRequest;

#[derive(Debug)]
struct PairingReconcileRuntimeProbes {
    operator_write_diverges: bool,
    install_converges: bool,
    teardown_converges: bool,
    crash_restarts_slot: bool,
}

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

fn stub_principal() -> Arc<AgentPrincipal> {
    let identity: Arc<dyn crate::identity::AgentIdentity> = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("stub-principal-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    Arc::new(AgentPrincipal {
        agent_did: identity.did().to_string(),
        identity,
        default_behavior_id: String::new(),
        display_name: None,
        enabled: true,
    })
}

async fn snapshot_for_behaviors(
    node: &defra_node::EmbeddedNode,
    default_behavior_id: &str,
    behaviors: Vec<Arc<AgentBehavior>>,
) -> ResolvedRuntimeSnapshot {
    let mut tool_surfaces = HashMap::new();
    for behavior in &behaviors {
        let tool_surface = behavior.tools.resolve(node).await.unwrap();
        tool_surfaces.insert(behavior.behavior_id.clone(), Arc::new(tool_surface));
    }
    ResolvedRuntimeSnapshot::from_parts(
        default_behavior_id.to_string(),
        behaviors,
        tool_surfaces,
        HashMap::new(),
    )
    .with_principal(stub_principal())
}

#[tokio::test]
async fn pairing_reconcile_state_machine_contract_is_complete() {
    assert_state_machine_contract_is_complete("PairingReconcile");
    let machine = lean_state_machine_contract("PairingReconcile");
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let probes = PairingReconcileRuntimeProbes {
        operator_write_diverges: operator_write_changes_snapshot_fingerprint(node.as_ref()).await,
        install_converges: reconcile_install_applies_added_behavior(node.as_ref()).await,
        teardown_converges: reconcile_teardown_applies_removed_behavior(node.as_ref()).await,
        crash_restarts_slot: slot_panic_restarts_behavior(node.as_ref()).await,
    };

    let mut rust_legal_pairs = BTreeSet::new();
    for from in &machine.states {
        for action in &machine.actions {
            if let Some(post) = rust_pairing_reconcile_step(from, action, &probes) {
                rust_legal_pairs.insert((from.clone(), post.to_string()));
            }
        }
    }

    let lean_legal_pairs = machine
        .legal_transitions
        .iter()
        .map(|pair| (pair.from.clone(), pair.to.clone()))
        .collect::<BTreeSet<_>>();
    let lean_illegal_pairs = machine
        .illegal_transitions
        .iter()
        .map(|pair| (pair.from.clone(), pair.to.clone()))
        .collect::<BTreeSet<_>>();

    assert_eq!(
        rust_legal_pairs, lean_legal_pairs,
        "PairingReconcile Lean legal transitions drifted from Rust diff/slot behavior"
    );
    assert!(
        rust_legal_pairs.is_disjoint(&lean_illegal_pairs),
        "PairingReconcile Rust transitions overlap Lean illegal transitions"
    );
}

fn rust_pairing_reconcile_step(
    phase: &str,
    action: &str,
    probes: &PairingReconcileRuntimeProbes,
) -> Option<&'static str> {
    match (phase, action) {
        ("idle" | "converged" | "crashed", "operatorWrite") if probes.operator_write_diverges => {
            Some("diverged")
        }
        ("diverged", "reconcileInstall") if probes.install_converges => Some("converged"),
        ("diverged", "reconcileTeardown") if probes.teardown_converges => Some("converged"),
        (_, "crash") if probes.crash_restarts_slot => Some("crashed"),
        _ => None,
    }
}

async fn operator_write_changes_snapshot_fingerprint(node: &defra_node::EmbeddedNode) -> bool {
    let mut initial_behavior = PendingAgentBehavior::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-initial"));
    initial_behavior.system_prompt = "before operator write".to_string();
    let mut updated_behavior = PendingAgentBehavior::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-updated"));
    updated_behavior.system_prompt = "after operator write".to_string();
    let current_resolved =
        snapshot_for_behaviors(node, "general", vec![Arc::new(initial_behavior)]).await;
    let proposed = snapshot_for_behaviors(node, "general", vec![Arc::new(updated_behavior)]).await;
    let current = current_resolved.activate(1, HashMap::new());
    let diff = diff_counts(&current, &proposed);

    current.configuration_fingerprint() != proposed.configuration_fingerprint()
        && diff.updated == 1
        && diff.added == 0
        && diff.removed == 0
}

async fn reconcile_install_applies_added_behavior(node: &defra_node::EmbeddedNode) -> bool {
    let behavior = PendingAgentBehavior::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-install"));
    let current_resolved = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_principal(stub_principal());
    let proposed = snapshot_for_behaviors(node, "general", vec![Arc::new(behavior)]).await;
    let current = current_resolved.activate(1, HashMap::new());
    let diff = diff_counts(&current, &proposed);
    let applied = proposed.clone().activate(2, HashMap::new());
    let rediff = diff_counts(&applied, &proposed);

    diff.added == 1
        && diff.updated == 0
        && diff.removed == 0
        && rediff.added == 0
        && rediff.updated == 0
        && rediff.removed == 0
}

async fn reconcile_teardown_applies_removed_behavior(node: &defra_node::EmbeddedNode) -> bool {
    let behavior = PendingAgentBehavior::new("general")
        .build_with_identity_for_test(test_identity("pairing-contract-teardown"));
    let current_resolved = snapshot_for_behaviors(node, "general", vec![Arc::new(behavior)]).await;
    let proposed = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_principal(stub_principal());
    let current = current_resolved.activate(1, HashMap::new());
    let diff = diff_counts(&current, &proposed);
    let applied = proposed.clone().activate(2, HashMap::new());
    let rediff = diff_counts(&applied, &proposed);

    diff.removed == 1
        && diff.added == 0
        && diff.updated == 0
        && rediff.added == 0
        && rediff.updated == 0
        && rediff.removed == 0
}

async fn slot_panic_restarts_behavior(node: &defra_node::EmbeddedNode) -> bool {
    let behavior = Arc::new(
        PendingAgentBehavior::new("general")
            .build_with_identity_for_test(test_identity("pairing-contract-slot-crash")),
    );
    let tool_surface = Arc::new(behavior.tools.resolve(node).await.unwrap());
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = {
        let starts = starts.clone();
        move |_behavior: Arc<AgentBehavior>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              mut shutdown: watch::Receiver<bool>| {
            let starts = starts.clone();
            async move {
                if starts.fetch_add(1, Ordering::SeqCst) == 0 {
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
        behavior,
        tool_surface,
        crate::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 1,
        },
        runner,
        shutdown_rx,
    );

    let restarted = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if starts.load(Ordering::SeqCst) >= 2 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or(false);
    let _ = shutdown_tx.send(true);
    retire_slot(slot);
    restarted
}

#[derive(Debug, serde::Deserialize)]
struct RuntimeStatusRow {
    reconcile_phase: String,
    active_generation: i64,
    last_reconcile_result: String,
    last_reconcile_error: String,
}

async fn fetch_runtime_status(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
) -> RuntimeStatusRow {
    let agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, limit: 1) {{
                reconcile_phase
                active_generation
                last_reconcile_result
                last_reconcile_error
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "AgentRuntime query failed: {:?}",
        response.errors
    );
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRuntime"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("AgentRuntime row");
    serde_json::from_value(value).expect("decode AgentRuntime row")
}

#[tokio::test]
async fn generation_supervisor_rotates_dispatcher_on_behavior_change() {
    let publish = lean_runtime_reconcile_case("publish_changed_snapshot");
    assert!(publish.legal);

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:defra-agent:reconcile-test";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent_did);

    let starts = Arc::new(StdMutex::new(HashMap::<String, usize>::new()));
    let mut initial_behavior =
        PendingAgentBehavior::new("general").build_with_identity_for_test(test_identity("general"));
    initial_behavior.system_prompt = "initial prompt".to_string();
    let mut updated_behavior =
        PendingAgentBehavior::new("general").build_with_identity_for_test(test_identity("general"));
    updated_behavior.system_prompt = "updated prompt".to_string();

    let initial_snapshot =
        snapshot_for_behaviors(node.as_ref(), "general", vec![Arc::new(initial_behavior)]).await;
    let updated_snapshot =
        snapshot_for_behaviors(node.as_ref(), "general", vec![Arc::new(updated_behavior)]).await;

    let runner = {
        let starts = starts.clone();
        move |behavior: Arc<AgentBehavior>,
              _tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
              mut shutdown: watch::Receiver<bool>| {
            let starts = starts.clone();
            async move {
                *starts
                    .lock()
                    .unwrap()
                    .entry(behavior.behavior_id.clone())
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
    )
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
            .behaviors
            .get("general")
            .expect("updated behavior")
            .system_prompt,
        "updated prompt"
    );
    let status = fetch_runtime_status(node.as_ref(), agent_did).await;
    assert_eq!(status.reconcile_phase, publish.post_phase.as_str());
    assert_eq!(
        status.active_generation,
        publish.post_active_generation as i64
    );
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
    .expect("replacement behavior slot should start");

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
    let agent_did = "did:defra-agent:reconcile-failure-test";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent_did);

    let initial_behavior = PendingAgentBehavior::new("general")
        .build_with_identity_for_test(test_identity("general-initial"));
    let mut updated_behavior = PendingAgentBehavior::new("general")
        .build_with_identity_for_test(test_identity("general-updated"));
    updated_behavior.system_prompt = "updated prompt".to_string();

    let initial_snapshot =
        snapshot_for_behaviors(node.as_ref(), "general", vec![Arc::new(initial_behavior)]).await;
    let valid_updated_snapshot =
        snapshot_for_behaviors(node.as_ref(), "general", vec![Arc::new(updated_behavior)]).await;
    let invalid_snapshot = ResolvedRuntimeSnapshot::from_parts(
        "general".to_string(),
        valid_updated_snapshot.behaviors.values().cloned().collect(),
        HashMap::new(),
        HashMap::new(),
    )
    .with_principal(stub_principal());

    let runner = move |_behavior: Arc<AgentBehavior>,
                       _tool_surface: Arc<ToolSurface>,
                       request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
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
    )
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
    let failed_status = fetch_runtime_status(node.as_ref(), agent_did).await;
    assert_eq!(
        failed_status.reconcile_phase,
        apply_failed.post_phase.as_str()
    );
    assert_eq!(failed_status.active_generation, 0);
    assert_eq!(failed_status.last_reconcile_result, "error");
    assert!(!failed_status.last_reconcile_error.is_empty());

    proposal_tx.send(valid_updated_snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), active_rx.changed())
        .await
        .expect("valid update should publish after failed apply")
        .unwrap();
    assert_eq!(active_rx.borrow().generation, 2);
    let recovered_status = fetch_runtime_status(node.as_ref(), agent_did).await;
    assert_eq!(recovered_status.reconcile_phase, "idle");
    assert_eq!(recovered_status.active_generation, 2);
    assert_eq!(recovered_status.last_reconcile_result, "applied");
    assert!(recovered_status.last_reconcile_error.is_empty());

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("supervisor should stop on shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn generation_supervisor_rotates_dispatcher_on_tool_surface_change() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let agent_did = "did:defra-agent:reconcile-tool-surface-test";
    let runtime_status = RuntimeStatusHandle::new(node.clone(), agent_did);
    let identity = Arc::new(test_identity("tool-surface-general"));
    let principal = Arc::new(AgentPrincipal {
        agent_did: identity.did().to_string(),
        identity: identity.clone(),
        default_behavior_id: String::new(),
        display_name: None,
        enabled: true,
    });

    let initial_behavior = Arc::new(AgentBehavior {
        skills: Vec::new(),
        behavior_id: "general".to_string(),
        principal: principal.clone(),
        backend_id: Some("backend-general".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "default".to_string(),
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: crate::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: crate::config::DEFAULT_MAX_TURNS,
        system_prompt: "initial".to_string(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: crate::config::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: crate::compaction::CompactionStrategy::StripThenSummarize,
        stream_batch_ms: crate::config::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(crate::config::DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    });
    let updated_behavior = Arc::new(AgentBehavior {
        skills: Vec::new(),
        behavior_id: "general".to_string(),
        principal: principal.clone(),
        backend_id: Some("backend-general".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "default".to_string(),
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: crate::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: crate::config::DEFAULT_MAX_TURNS,
        system_prompt: "initial".to_string(),
        tools: BehaviorToolConfig::from_selection(
            "general",
            ToolSelection {
                file_tools: FileToolMode::ReadOnly,
                file_tool_root: None,
                bash: crate::tool_surface::BashMode::Off,
                command_policy: None,
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                allowed_mcp_service_ids: Vec::new(),
                delegate_to: Vec::new(),
                backgroundable_tool_names: Vec::new(),
            },
            &ToolCeiling::readonly(),
            Vec::new(),
        )
        .unwrap(),
        compaction_threshold: crate::config::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: crate::compaction::CompactionStrategy::StripThenSummarize,
        stream_batch_ms: crate::config::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(crate::config::DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    });

    let initial_snapshot =
        snapshot_for_behaviors(node.as_ref(), "general", vec![initial_behavior]).await;
    let updated_snapshot =
        snapshot_for_behaviors(node.as_ref(), "general", vec![updated_behavior]).await;

    let observed_tool_names = Arc::new(StdMutex::new(Vec::<Vec<String>>::new()));
    let runner = {
        let observed_tool_names = observed_tool_names.clone();
        move |_behavior: Arc<AgentBehavior>,
              tool_surface: Arc<ToolSurface>,
              request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
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
    )
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
