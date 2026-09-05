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
    Reconcile {
        desired: Option<Desired>,
    },
    Acquire,
    Release {
        generation: u64,
        returned_permit: bool,
    },
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
    pending_generation: Option<u64>,
    controller_generation: Option<u64>,
    capacity: usize,
    in_flight: usize,
    permits: usize,
    is_open: bool,
}

fn observe(registry: &AdmissionRegistry, backend_id: &str) -> Result<Observation> {
    let state = registry.inner.state.lock().unwrap();
    let controllers = state
        .active
        .get(backend_id)
        .into_iter()
        .chain(state.draining.get(backend_id).into_iter().flatten())
        .collect::<Vec<_>>();
    ensure!(
        controllers.len() <= 1,
        "replacement overlapped retiring ownership"
    );
    let controller = controllers.first();
    Ok(Observation {
        pending_generation: state.pending.get(backend_id).map(|p| p.generation),
        controller_generation: controller.map(|c| c.generation),
        capacity: controller.map_or(0, |c| c.config.max_concurrent),
        in_flight: controller.map_or(0, |c| c.in_flight_for_test()),
        permits: controller.map_or(0, |c| {
            c.config.max_concurrent - c.available_permits_for_test()
        }),
        is_open: controller.is_some_and(|c| !c.is_closed()),
    })
}

fn config_from_case(backend_id: &str, desired: &Desired) -> Result<BackendAdmissionConfig> {
    // Go through the real backend mapping: otherwise a test-only fingerprint
    // would hide metadata-triggered controller replacement.
    let backend = crate::backend_registry::InferenceBackend {
        backend_id: backend_id.to_owned(),
        name: desired.name.clone(),
        provider_kind: crate::backend_provider::BackendProviderKind::OpenAiCompatible,
        openai_wire_api: None,
        endpoint: format!("http://127.0.0.1/resource-{}/v1", desired.key),
        api_key: None,
        api_key_env_var: None,
        max_concurrent: desired.capacity.try_into()?,
        max_queue_depth: 0,
        enabled: true,
        models: vec![desired.catalog.clone()],
        probe_status: crate::backend_registry::HEALTHY_PROBE_STATUS.to_owned(),
    };
    Ok(BackendAdmissionConfig::from_backend(&backend)?.with_measured_unhealthy(!desired.available))
}

async fn run_case(node: Arc<EmbeddedNode>, case: &Case) -> Result<()> {
    ensure!(
        case.actions.len() == case.expected.len(),
        "missing step expectations"
    );
    let backend_id = format!("registry-{}", case.name);
    let registry = AdmissionRegistry::new(node);
    let mut held: Vec<(u64, AdmissionPermit)> = Vec::new();
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
                let generation = observe(&registry, &backend_id)?.controller_generation;
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
                    held.push((generation.context("admitted without a controller")?, permit));
                }
            }
            Action::Release {
                generation,
                returned_permit,
            } => {
                ensure!(
                    *returned_permit,
                    "queue releases belong to ControllerBookkeeping coverage"
                );
                let position = held
                    .iter()
                    .position(|(g, _)| g == generation)
                    .context("trace must release an actual owned permit")?;
                let (_, mut permit) = held.swap_remove(position);
                permit.finish_success(None).await?;
                drop(permit);
            }
        }
        let actual = observe(&registry, &backend_id)?;
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

#[tokio::test]
async fn deferred_drain_callback_cannot_leave_stale_pending_configuration() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();
    let registry = AdmissionRegistry::new(node);
    let backend_id = "registry-deferred-drain";
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

    // Defer only the registry notification, not real permit bookkeeping.
    // In production this gap occurs after release_in_flight decrements to
    // zero while controller_drained is waiting for the registry mutex.
    let retiring = BackendAdmissionController::new(1, config(1, 1), std::sync::Weak::new());
    registry
        .inner
        .state
        .lock()
        .unwrap()
        .active
        .insert(backend_id.to_owned(), retiring.clone());
    let mut permit = registry
        .acquire_for_test(
            "deferred-drain-owner",
            backend_id,
            "default",
            "did:test:registry",
            CallKind::Inference,
        )
        .await
        .unwrap();
    registry.reconcile(2, &[(backend_id.to_owned(), config(2, 2))].into());
    assert_eq!(
        observe(&registry, backend_id).unwrap(),
        Observation {
            pending_generation: Some(2),
            controller_generation: Some(1),
            capacity: 1,
            in_flight: 1,
            permits: 1,
            is_open: false,
        }
    );

    permit.finish_success(None).await.unwrap();
    drop(permit);
    assert!(retiring.is_drained());
    assert_eq!(retiring.available_permits_for_test(), 1);

    // Reconciliation wins the mutex before the deferred callback. It must
    // replace pending generation 2 with generation 3 and consume that entry
    // when installing, rather than leave an active controller plus stale work.
    registry.reconcile(3, &[(backend_id.to_owned(), config(3, 3))].into());
    let installed = Observation {
        pending_generation: None,
        controller_generation: Some(3),
        capacity: 3,
        in_flight: 0,
        permits: 0,
        is_open: true,
    };
    assert_eq!(observe(&registry, backend_id).unwrap(), installed);
    registry
        .inner
        .clone()
        .controller_drained(backend_id.to_owned());
    assert_eq!(observe(&registry, backend_id).unwrap(), installed);
}

#[tokio::test]
async fn rollback_reuses_epoch_with_distinct_controller_ownership() {
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
    registry.reconcile(1, &[(backend_id.to_owned(), config(1, 1))].into());
    let old = registry.inner.state.lock().unwrap().active[backend_id].clone();
    let mut first = registry
        .acquire_for_test(
            "rollback-old-owner",
            backend_id,
            "default",
            "did:test:registry",
            CallKind::Inference,
        )
        .await
        .unwrap();
    registry.reconcile(2, &[(backend_id.to_owned(), config(2, 2))].into());
    // Failed snapshot publication restores the prior full configuration and
    // epoch. The closed controller must still drain; rollback cannot reopen it.
    registry.reconcile(1, &[(backend_id.to_owned(), config(1, 1))].into());
    assert_eq!(
        observe(&registry, backend_id).unwrap(),
        Observation {
            pending_generation: Some(1),
            controller_generation: Some(1),
            capacity: 1,
            in_flight: 1,
            permits: 1,
            is_open: false,
        }
    );
    first.finish_success(None).await.unwrap();
    drop(first);
    let replacement = registry.inner.state.lock().unwrap().active[backend_id].clone();
    assert!(!Arc::ptr_eq(&old, &replacement));
    assert_eq!(old.generation, replacement.generation);
    assert!(old.is_closed());
    assert!(old.is_drained());
    let mut second = registry
        .acquire_for_test(
            "rollback-new-owner",
            backend_id,
            "default",
            "did:test:registry",
            CallKind::Inference,
        )
        .await
        .unwrap();
    let held = Observation {
        pending_generation: None,
        controller_generation: Some(1),
        capacity: 1,
        in_flight: 1,
        permits: 1,
        is_open: true,
    };
    assert_eq!(observe(&registry, backend_id).unwrap(), held);
    // Deliver a late notification for the retired incarnation. Epoch equality
    // does not authorize it to release the replacement's real permit.
    registry
        .inner
        .clone()
        .controller_drained(backend_id.to_owned());
    assert_eq!(observe(&registry, backend_id).unwrap(), held);
    assert_eq!(replacement.available_permits_for_test(), 0);
    second.finish_success(None).await.unwrap();
    drop(second);
    assert!(replacement.is_drained());
    assert_eq!(replacement.available_permits_for_test(), 1);
}
