use super::*;

use anyhow::{ensure, Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct Snapshot {
    inference_registry_cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    actions: Vec<Action>,
    expected: Vec<Observation>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Action {
    Reconcile { desired: Option<Desired> },
    Acquire,
    Release { pool: usize },
}

#[derive(Deserialize)]
struct Desired {
    key: u64,
    generation: u64,
    capacity: usize,
    available: bool,
    name: String,
    catalog: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Observation {
    admitting_generation: Option<u64>,
    capacity: usize,
    held: usize,
    pool: usize,
}

/// Numbers real pools in opening order, matching the model's pool counter.
#[derive(Default)]
struct PoolIds {
    opened: Vec<Arc<CapacityPool>>,
}

impl PoolIds {
    fn current(&mut self, controller: Option<&Arc<BackendAdmissionController>>) -> usize {
        if let Some(controller) = controller {
            if !self
                .opened
                .last()
                .is_some_and(|pool| Arc::ptr_eq(pool, &controller.pool))
            {
                self.opened.push(controller.pool.clone());
            }
        }
        self.opened.len()
    }
}

fn observe(
    registry: &AdmissionRegistry,
    backend_id: &str,
    pools: &mut PoolIds,
) -> Result<Observation> {
    let controller = registry.active_for_test(backend_id);
    if let Some(controller) = &controller {
        ensure!(
            controller.pool.capacity_for_test() == controller.config.max_concurrent,
            "admitting controller and its pool disagree on capacity"
        );
    }
    Ok(Observation {
        admitting_generation: controller.as_ref().map(|c| c.generation),
        capacity: controller.as_ref().map_or(0, |c| c.config.max_concurrent),
        held: controller.as_ref().map_or(0, |c| c.pool.held_for_test()),
        pool: pools.current(controller.as_ref()),
    })
}

fn config_from_case(backend_id: &str, desired: &Desired) -> Result<BackendAdmissionConfig> {
    // Go through the real backend mapping: otherwise a test-only fingerprint
    // would hide metadata-triggered controller replacement.
    let backend: crate::document_config::InferenceBackend = serde_json::from_value(
        serde_json::json!({
            "agent_did":"did:test:registry", "backend_id":backend_id, "name":desired.name,
            "provider_kind":"OpenAiCompatible", "endpoint":format!("http://127.0.0.1/resource-{}/v1", desired.key),
            "auth":{"kind":"unauthenticated"}, "max_concurrent":i64::try_from(desired.capacity)?,
            "max_queue_depth":0
        }),
    )?;
    let observation = serde_json::from_value::<crate::document_config::InferenceBackendObservation>(
        serde_json::json!({
            "backend_id":backend_id, "probe_status":crate::backend_registry::HEALTHY_PROBE_STATUS,
            "catalogs":[{"agent_did":null,"observed_at":"2026-01-01T00:00:00Z",
                "models":[{"model_name":desired.catalog}]}]
        }),
    )?;
    Ok(
        BackendAdmissionConfig::from_backend(&backend, &observation)?
            .with_measured_unhealthy(!desired.available),
    )
}

async fn run_case(node: Arc<EmbeddedNode>, case: &Case) -> Result<()> {
    ensure!(
        case.actions.len() == case.expected.len(),
        "missing step expectations"
    );
    let backend_id = format!("registry-{}", case.name);
    let registry = AdmissionRegistry::new(node);
    let mut pools = PoolIds::default();
    let mut held: Vec<(usize, AdmissionPermit)> = Vec::new();
    for (index, (action, expected)) in case.actions.iter().zip(&case.expected).enumerate() {
        match action {
            Action::Reconcile { desired } => {
                let configs = desired
                    .as_ref()
                    .map(|d| {
                        Ok::<_, anyhow::Error>((
                            backend_id.clone(),
                            config_from_case(&backend_id, d)?,
                        ))
                    })
                    .transpose()?
                    .into_iter()
                    .collect();
                registry.reconcile(desired.as_ref().map_or(0, |d| d.generation), &configs);
            }
            Action::Acquire => {
                let before = observe(&registry, &backend_id, &mut pools)?;
                if let Ok(permit) = registry
                    .acquire_for_test(
                        format!("{}-{index}", case.name),
                        &backend_id,
                        "default",
                        "did:test:registry",
                        CallKind::Inference,
                    )
                    .await
                {
                    ensure!(
                        Some(permit.controller_generation_for_test())
                            == before.admitting_generation,
                        "admitted under a controller other than the admitting one"
                    );
                    held.push((before.pool, permit));
                }
            }
            Action::Release { pool } => {
                let position = held
                    .iter()
                    .position(|(p, _)| p == pool)
                    .context("trace must release an actual owned permit")?;
                let (_, mut permit) = held.swap_remove(position);
                permit.finish_success(None).await?;
                drop(permit);
            }
        }
        let actual = observe(&registry, &backend_id, &mut pools)?;
        ensure!(
            actual == *expected,
            "step {index}: expected {expected:?}, got {actual:?}"
        );
    }
    for (_, mut permit) in held {
        permit.finish_success(None).await?;
    }
    Ok(())
}

#[tokio::test]
async fn generated_inference_registry_cases_drive_real_permits() {
    let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(snapshot.inference_registry_cases.len(), 8);
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();
    let mut failures = Vec::new();
    for case in &snapshot.inference_registry_cases {
        if let Err(error) = run_case(node.clone(), case).await {
            failures.push(format!("{}: {error:#}", case.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Rollback may reuse an epoch. The restored configuration is a distinct
/// incarnation on the same pool, so the replaced incarnation's permit still
/// counts against capacity and its release frees the shared slot.
#[tokio::test]
async fn rollback_reuses_epoch_with_distinct_controller_on_shared_pool() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();
    let registry = AdmissionRegistry::new(node);
    let backend_id = "registry-rollback-epoch";
    let config = |generation, capacity| {
        config_from_case(
            backend_id,
            &Desired {
                key: 7,
                generation,
                capacity,
                available: true,
                name: "backend".to_owned(),
                catalog: "model".to_owned(),
            },
        )
        .unwrap()
    };
    let acquire = |request_id: &'static str| {
        registry.acquire_for_test(
            request_id,
            backend_id,
            "default",
            "did:test:registry",
            CallKind::Inference,
        )
    };
    registry.reconcile(1, &[(backend_id.to_owned(), config(1, 1))].into());
    let old = registry.active_for_test(backend_id).unwrap();
    let mut first = acquire("rollback-old-owner").await.unwrap();
    registry.reconcile(2, &[(backend_id.to_owned(), config(2, 2))].into());
    // Failed snapshot publication restores the prior full configuration and
    // epoch.
    registry.reconcile(1, &[(backend_id.to_owned(), config(1, 1))].into());
    let restored = registry.active_for_test(backend_id).unwrap();
    assert!(!Arc::ptr_eq(&old, &restored));
    assert!(Arc::ptr_eq(&old.pool, &restored.pool));
    assert_eq!(old.generation, restored.generation);
    assert_eq!(restored.pool.held_for_test(), 1);
    let error = match acquire("rollback-over-capacity").await {
        Ok(_) => panic!("the replaced incarnation's permit must still count"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("QueueFull"), "{error}");

    first.finish_success(None).await.unwrap();
    drop(first);
    assert_eq!(restored.pool.held_for_test(), 0);
    let mut second = acquire("rollback-new-owner").await.unwrap();
    assert_eq!(restored.pool.held_for_test(), 1);
    second.finish_success(None).await.unwrap();
    drop(second);
    assert_eq!(restored.pool.held_for_test(), 0);
    assert_eq!(restored.pool.available_permits_for_test(), 1);
}
