use super::*;
use crate::runtime::agent::reconcile::SlotFailurePolicy as _;
use crate::runtime::node_readiness_publisher::{FatalNodeReadinessWrite, NodeReadinessWriter};
use gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;

struct DemotionWriter {
    durable: bool,
}
#[async_trait::async_trait]
impl NodeReadinessWriter for DemotionWriter {
    async fn upsert(
        &self,
        _: &str,
        snapshot: &gents_protocol::node_readiness::NodeReadinessSnapshot,
        _: &str,
    ) -> Result<()> {
        if !self.durable && snapshot.agents.iter().any(|entry| entry.reason == Some(gents_protocol::node_readiness::AgentReadinessUnavailableReason::ExecutorStartFailed)) {
            return Err(FatalNodeReadinessWrite.into());
        }
        Ok(())
    }
}

#[tokio::test]
async fn generated_startup_demotions_release_only_durable_current_obligations() {
    use gents_lean_vocab::families::runtime_startup_admission_try_demote as family;
    for case in family::cases() {
        let db = gents_inference::test_support::test_db(
            "startup-demote-family",
            gents_automation::lifecycle_hooks::composition(),
        )
        .await;
        let (status_owner, runtime_status) = RuntimeStatusHandle::start_with_readiness_writer(
            db.node.clone(),
            "did:test:startup-demote",
            Arc::new(DemotionWriter {
                durable: case.input.durable,
            }),
            std::time::Duration::from_millis(1),
            crate::runtime::storage_fault::StorageFaultObservation::new(),
        );
        runtime_status.initialize_startup("general").await.unwrap();
        if case.input.registered {
            runtime_status
                .readiness()
                .register_slot("general", 1)
                .await
                .unwrap();
        }
        let (dispatcher, _requests) = mpsc::channel(1);
        runtime_status
            .readiness()
            .publish_snapshot(&ActiveRuntimeSnapshot {
                generation: 1,
                node: None,
                local_did: String::new(),
                default_agent_did: "general".into(),
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
                dispatchers: RapidHashMap::from_iter([("general".into(), dispatcher)]),
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
        if case.input.pending {
            barrier.register_agent("general", 1).await;
        }
        let gate = super::super::router::RuntimeAdmissionGate::closed();
        gate.open().await;
        let runtime_cancel = CancellationToken::new();
        let (fatal_error, mut fatal_errors) = mpsc::unbounded_channel();
        let policy = StartupSlotFailurePolicy {
            barrier: barrier.clone(),
            runtime_status: runtime_status.clone(),
            admission_gate: gate.clone(),
            runtime_cancel: runtime_cancel.clone(),
            fatal_error,
            observer: None,
            budget: 1,
        };
        let result = policy.try_demote("general", 1, "executor failed").await;
        case.check(family::DemoteObservation {
            applied: result.ok(),
            pending: barrier.is_pending("general", 1).await,
            gate_open: gate.is_open().await,
            cancelled: runtime_cancel.is_cancelled(),
            fatal: fatal_errors.try_recv().ok().map(|error| {
                if error.to_string() == FatalNodeReadinessWrite.to_string() {
                    "readinessWrite".to_owned()
                } else {
                    format!("{error:#}")
                }
            }),
            committed: runtime_status
                .readiness()
                .observation()
                .demotion_reason("general")
                .is_some(),
        });
        status_owner.close().await.unwrap();
        db.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn startup_generation_trace_matches_the_lean_model() {
    let cases = gents_lean_vocab::families::startup_readiness_generation_trace::cases::<String>();
    for (index, case) in cases.into_iter().enumerate() {
        let node_db = gents_inference::test_support::test_db(
            &format!("startup-generation-trace-{index}"),
            gents_automation::lifecycle_hooks::composition(),
        )
        .await;
        let node = node_db.node.clone();
        gents_store::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .unwrap();
        let node_did = node
            .node_identity_did()
            .expect("fixture node has an identity")
            .to_owned();
        gents_documents::test_support::ensure_test_node(&node, &node_did).await;
        let (status_owner, runtime_status) = RuntimeStatusHandle::start(
            node.clone(),
            node_did,
            crate::runtime::storage_fault::StorageFaultObservation::new(),
        );
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

        let mut observations = Vec::new();
        for step in &case.input.steps {
            match step.kind.as_str() {
                "register" => policy
                    .on_slot_created("general", step.generation)
                    .await
                    .unwrap(),
                "retire" => {
                    policy
                        .on_slot_retired("general", step.generation, true)
                        .await
                }
                other => panic!("unmodeled generation step {other:?}"),
            }
            observations.push(
                gents_lean_vocab::families::startup_readiness_generation_trace::GenerationObservation {
                    generation_one_pending: barrier.is_pending("general", 1).await,
                    generation_two_pending: barrier.is_pending("general", 2).await,
                    ready: tokio::time::timeout(
                        std::time::Duration::from_millis(20),
                        barrier.wait_ready(),
                    )
                    .await
                    .is_ok(),
                },
            );
        }
        case.check(observations);
        status_owner.close().await.unwrap();
        node.shutdown().await.expect("node shutdown");
    }
}
