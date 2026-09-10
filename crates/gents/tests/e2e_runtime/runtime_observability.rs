use std::sync::Arc;
use std::time::Duration;

use gents::{
    AgentIdentity, DocumentRuntimeOptions, Gents, KeyIdentity, ProcessLifecycleObserver,
    ProcessLifecycleState, RuntimeSnapshotObserver, ToolCeiling,
};
use tokio::sync::watch;

use crate::support::fixtures::bind_default_behavior_backend;
use crate::support::interrupt::TEST_RUNTIME_READY_TIMEOUT;
use crate::support::snapshots::{RuntimeSnapshot, fetch_runtime_snapshot};
use crate::support::test_db;

const UNUSED_BACKEND_ENDPOINT: &str = "http://127.0.0.1:9/v1";

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

struct RuntimeEventObserver {
    process_state_tx: watch::Sender<ProcessLifecycleState>,
    generation_tx: watch::Sender<u64>,
}

impl ProcessLifecycleObserver for RuntimeEventObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        self.process_state_tx.send_replace(state);
    }
}

impl RuntimeSnapshotObserver for RuntimeEventObserver {
    fn on_generation_published(
        &self,
        generation: u64,
        _configuration_fingerprint: &str,
        _runnable_behavior_ids: &[String],
    ) {
        self.generation_tx.send_replace(generation);
    }
}

async fn wait_for_observed<T>(
    receiver: &mut watch::Receiver<T>,
    description: &str,
    predicate: impl Fn(&T) -> bool,
) where
    T: Clone + std::fmt::Debug,
{
    let wait = async {
        loop {
            if predicate(&receiver.borrow_and_update()) {
                return Ok::<(), watch::error::RecvError>(());
            }
            receiver.changed().await?;
        }
    };
    match tokio::time::timeout(TEST_RUNTIME_READY_TIMEOUT, wait).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            panic!("runtime observer closed while waiting for {description}: {error}")
        }
        Err(error) => panic!(
            "timed out after {TEST_RUNTIME_READY_TIMEOUT:?} waiting for {description}; \
             last observed value: {:?}: {error}",
            receiver.borrow().clone()
        ),
    }
}

async fn wait_for_runtime_snapshot<F>(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + TEST_RUNTIME_READY_TIMEOUT;
    loop {
        let last_snapshot = fetch_runtime_snapshot(node, agent_did).await;
        if let Some(snapshot) = last_snapshot.as_ref() {
            if predicate(&snapshot) {
                return snapshot.clone();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {TEST_RUNTIME_READY_TIMEOUT:?} waiting for runtime snapshot for \
             {agent_did}; last observed snapshot: {last_snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_status_surfaces_startup_reconcile_and_shutdown() {
    let db = test_db("runtime-observability").await;
    let identity = Arc::new(test_identity("runtime-observability"));
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-runtime-observability",
        UNUSED_BACKEND_ENDPOINT,
    )
    .await;
    let (process_state_tx, mut process_state_rx) =
        watch::channel(ProcessLifecycleState::Uninitialized);
    let (generation_tx, mut generation_rx) = watch::channel(0);
    let observer = Arc::new(RuntimeEventObserver {
        process_state_tx,
        generation_tx,
    });
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            runtime_snapshot_observer: Some(observer),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let default_behavior_id = agent.default_behavior_id().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_observed(
        &mut process_state_rx,
        "runtime process state Ready",
        |state| *state == ProcessLifecycleState::Ready,
    )
    .await;
    let startup = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation == 1
            && snapshot.router_generation == 1
            && snapshot.last_reconcile_result == "startup"
    })
    .await;
    assert_eq!(startup.default_behavior_id, default_behavior_id);
    assert!(startup.last_reconcile_error.is_empty());

    gents::config_client::ConfigAccess::transact_local(
        db.node.as_ref(),
        None,
        "test.update_context",
        |txn| {
            let agent_did = &agent_did;
            let behavior_id = &default_behavior_id;
            Box::pin(async move {
                use gents::Collection;
                use gents::config_client::{
                    DesiredStateApplyDocument, DesiredStateApplyPlan,
                    read_desired_state_record_in_txn as read,
                };
                let (_, mut behavior) =
                    read(txn, Collection::AgentBehavior, agent_did, behavior_id)
                        .await?
                        .expect("behavior");
                let context_id = behavior["context_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{behavior_id}:context"));
                let mut context = read(txn, Collection::AgentContext, agent_did, &context_id)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(
                        || serde_json::json!({"agent_did":agent_did,"context_id":context_id}),
                    );
                context["system_prompt"] = "runtime observability update".into();
                behavior["context_id"] = context_id.into();
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::AgentBehavior, behavior),
                        (Collection::AgentContext, context),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                gents::config_client::apply_desired_state_plan(txn, &plan).await
            })
        },
    )
    .await
    .unwrap();

    wait_for_observed(&mut generation_rx, "runtime generation 2", |generation| {
        *generation >= 2
    })
    .await;
    let reconciled = wait_for_runtime_snapshot(db.node.as_ref(), &agent_did, |snapshot| {
        snapshot.process_state == "ready"
            && snapshot.reconcile_phase == "idle"
            && snapshot.active_generation == 2
            && snapshot.router_generation == 2
            && snapshot.last_reconcile_result == "applied"
    })
    .await;
    assert_eq!(reconciled.default_behavior_id, default_behavior_id);
    assert!(reconciled.last_reconcile_error.is_empty());

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();

    let shutdown = fetch_runtime_snapshot(db.node.as_ref(), &agent_did)
        .await
        .expect("shutdown runtime snapshot");
    assert_eq!(shutdown.process_state, "shutdown");
    assert_eq!(shutdown.reconcile_phase, "idle");
    assert_eq!(shutdown.router_generation, 2);
}
