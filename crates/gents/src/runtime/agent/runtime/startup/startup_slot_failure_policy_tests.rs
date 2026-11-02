use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;

use crate::runtime::agent::reconcile::SlotFailurePolicy as _;
use crate::runtime::node_readiness_publisher::{FatalNodeReadinessWrite, NodeReadinessWriter};
use crate::runtime::snapshot::ActiveRuntimeSnapshot;

use super::*;

struct FailDemotionWriter;

#[async_trait::async_trait]
impl NodeReadinessWriter for FailDemotionWriter {
    async fn upsert(
        &self,
        _node_did: &str,
        snapshot: &gents_protocol::node_readiness::NodeReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        if snapshot.agents.iter().any(|entry| {
            entry.reason
                == Some(gents_protocol::node_readiness::AgentReadinessUnavailableReason::ExecutorStartFailed)
        }) {
            return Err(FatalNodeReadinessWrite.into());
        }
        Ok(())
    }
}

#[tokio::test]
async fn aborted_staged_generation_does_not_release_active_startup_obligation() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let (status_owner, runtime_status) =
        RuntimeStatusHandle::start(node.clone(), "did:test:startup-generation-barrier");
    runtime_status.initialize_startup("general").await.unwrap();
    let barrier = Arc::new(StartupBarrier::ready_for_test());
    let (fatal_error, _fatal_errors) = mpsc::unbounded_channel();
    let policy = StartupSlotFailurePolicy {
        barrier: barrier.clone(),
        runtime_status,
        admission_gate: super::super::router::RuntimeAdmissionGate::closed(),
        runtime_cancel: CancellationToken::new(),
        fatal_error,
        observer: None,
        budget: 1,
    };

    policy.on_slot_created("general", 1).await.unwrap();
    policy.on_slot_created("general", 2).await.unwrap();
    assert!(barrier.is_pending("general", 1).await);
    assert!(barrier.is_pending("general", 2).await);

    policy.on_slot_retired("general", 2, true).await;
    assert!(barrier.is_pending("general", 1).await);
    assert!(!barrier.is_pending("general", 2).await);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), barrier.wait_ready())
            .await
            .is_err(),
        "aborting staged generation two released unresolved generation one"
    );

    policy.on_slot_retired("general", 1, true).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), barrier.wait_ready())
        .await
        .expect("exact generation-one retirement should release the final obligation");
    status_owner.close().await.unwrap();
    node.shutdown().await;
}

#[tokio::test]
async fn demotion_persistence_failure_closes_admission_and_cancels_runtime() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let (status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
        node.clone(),
        "did:test:demotion-fail-closed",
        Arc::new(FailDemotionWriter),
        std::time::Duration::from_millis(1),
    );
    runtime_status.initialize_startup("general").await.unwrap();
    runtime_status
        .readiness()
        .register_slot("general", 1)
        .await
        .unwrap();
    let (dispatcher, _requests) = mpsc::channel(1);
    runtime_status
        .readiness()
        .publish_snapshot(&ActiveRuntimeSnapshot {
            generation: 1,
            node: None,
            local_did: String::new(),
            default_agent_id: "general".to_string(),
            agents: RapidHashMap::new(),
            tool_surfaces: RapidHashMap::new(),
            backend_admission_configs: RapidHashMap::new(),
            unavailable_agents: RapidHashMap::new(),
            active_schedules: RapidHashMap::new(),
            unavailable_schedules: RapidHashSet::new(),
            active_event_triggers: RapidHashMap::new(),
            unavailable_event_triggers: RapidHashSet::new(),
            active_tasks: RapidHashMap::new(),
            dispatchers: RapidHashMap::from_iter([("general".to_string(), dispatcher)]),
            agent_executor_capacities: RapidHashMap::new(),
            agent_executor_queue_capacities: RapidHashMap::new(),
        })
        .await
        .unwrap();
    runtime_status
        .readiness()
        .set_router_generation(1)
        .await
        .unwrap();
    runtime_status
        .set_process_state_durable(ProcessLifecycleState::Ready)
        .await
        .unwrap();

    let barrier = Arc::new(StartupBarrier::ready_for_test());
    barrier.register_agent("general", 1).await;
    let gate = super::super::router::RuntimeAdmissionGate::closed();
    gate.open().await;
    let runtime_cancel = CancellationToken::new();
    let (fatal_error, mut fatal_errors) = mpsc::unbounded_channel();
    let policy = StartupSlotFailurePolicy {
        barrier,
        runtime_status: runtime_status.clone(),
        admission_gate: gate.clone(),
        runtime_cancel: runtime_cancel.clone(),
        fatal_error,
        observer: None,
        budget: 1,
    };

    assert!(policy
        .try_demote("general", 1, "executor failed")
        .await
        .is_err());
    assert!(!gate.is_open().await, "failed veto left routing open");
    assert!(
        runtime_cancel.is_cancelled(),
        "runtime teardown was not armed"
    );
    let fatal = fatal_errors
        .recv()
        .await
        .expect("runtime coordinator must receive demotion persistence failure");
    assert_eq!(fatal.to_string(), "injected fatal agent readiness write");
    assert_eq!(
        runtime_status
            .readiness()
            .observation()
            .demotion_reason("general"),
        None,
        "failed persistence must not forge a committed demotion"
    );

    status_owner.close().await.unwrap();
    node.shutdown().await;
}
