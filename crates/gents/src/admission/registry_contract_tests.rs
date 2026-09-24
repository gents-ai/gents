use super::*;

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{bail, ensure, Result};
use serde::Deserialize;
use tokio::task::JoinHandle;

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
    Acquire { slot: u64 },
    Release,
}

#[derive(Deserialize)]
struct Desired {
    connection: u64,
    generation: u64,
    capacity: usize,
    queue_depth: usize,
    available: bool,
    name: String,
    catalog: String,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct Observation {
    admitting_generation: Option<u64>,
    capacity: usize,
    held: usize,
    queued: usize,
    admitted: usize,
    queue_full: usize,
    gone: usize,
    connection_changed: usize,
}

fn backend_document(
    backend_id: &str,
    connection: u64,
    capacity: usize,
    queue_depth: usize,
    name: &str,
) -> Result<crate::document_config::InferenceBackend> {
    Ok(serde_json::from_value(serde_json::json!({
        "agent_did":"did:test:registry", "backend_id":backend_id, "name":name,
        "provider_kind":"OpenAiCompatible",
        "endpoint":format!("http://127.0.0.1/resource-{connection}/v1"),
        "auth":{"kind":"unauthenticated"}, "max_concurrent":i64::try_from(capacity)?,
        "max_queue_depth":i64::try_from(queue_depth)?
    }))?)
}

/// The connection fingerprint a behavior slot built against `connection`
/// carries, derived the way `build_admitted_model` derives it.
fn slot_connection(backend_id: &str, connection: u64) -> Result<String> {
    let backend = backend_document(backend_id, connection, 1, 0, "slot")?;
    Ok(super::super::config::backend_connection_fingerprint(
        &backend.backend_fields(),
    ))
}

fn config_from_case(backend_id: &str, desired: &Desired) -> Result<BackendAdmissionConfig> {
    // Go through the real backend mapping: otherwise a test-only fingerprint
    // would hide metadata-triggered controller replacement.
    let backend = backend_document(
        backend_id,
        desired.connection,
        desired.capacity,
        desired.queue_depth,
        &desired.name,
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

/// Drives real acquisitions as spawned calls and observes the registry only
/// once every call is either finished or parked on the pool.
struct Harness {
    registry: AdmissionRegistry,
    backend_id: String,
    calls: Vec<JoinHandle<Result<AdmissionPermit, CompletionError>>>,
    held: VecDeque<AdmissionPermit>,
    outcomes: Observation,
    started: usize,
}

impl Harness {
    fn acquire(&mut self, connection: String) {
        let registry = self.registry.clone();
        let backend_id = self.backend_id.clone();
        let request_id = format!("{}-{}", self.backend_id, self.started);
        self.started += 1;
        self.calls.push(tokio::spawn(async move {
            registry
                .acquire_with_connection_for_test(
                    request_id,
                    backend_id,
                    "default",
                    "did:test:registry",
                    CallKind::Inference,
                    &connection,
                )
                .await
        }));
    }

    async fn settle(&mut self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let mut pending = Vec::new();
            for call in self.calls.drain(..) {
                if !call.is_finished() {
                    pending.push(call);
                    continue;
                }
                match call.await? {
                    Ok(permit) => {
                        self.outcomes.admitted += 1;
                        self.held.push_back(permit);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if message.contains("QueueFull") {
                            self.outcomes.queue_full += 1;
                        } else if message.contains(crate::error::BACKEND_CONNECTION_CHANGED) {
                            self.outcomes.connection_changed += 1;
                        } else if message.contains("BackendGone") {
                            self.outcomes.gone += 1;
                        } else {
                            bail!("unexpected admission error: {message}");
                        }
                    }
                }
            }
            self.calls = pending;
            let pool = self.registry.pool_for_test(&self.backend_id);
            let (waiters, transit, available) = pool.as_ref().map_or((0, 0, 0), |pool| {
                (
                    pool.queue_waiters_for_test(),
                    pool.in_transit_for_test(),
                    pool.available_permits_for_test(),
                )
            });
            // Parked calls exist only while admission is open and no permit
            // is free; closing admission wakes and fails every one of them.
            let open = self.registry.active_for_test(&self.backend_id).is_some();
            let parked_only = waiters == 0 || (open && available == 0);
            if self.calls.len() == waiters && transit == 0 && parked_only {
                return Ok(());
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "admission never settled: {} calls, {waiters} waiters, {transit} in transit",
                self.calls.len()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn observe(&self) -> Observation {
        let admitting = self.registry.active_for_test(&self.backend_id);
        let pool = self.registry.pool_for_test(&self.backend_id);
        Observation {
            admitting_generation: admitting.as_ref().map(|c| c.generation),
            capacity: admitting.as_ref().map_or(0, |c| c.config.max_concurrent),
            held: pool.as_ref().map_or(0, |pool| pool.held_for_test()),
            queued: pool
                .as_ref()
                .map_or(0, |pool| pool.queue_waiters_for_test()),
            ..self.outcomes
        }
    }
}

async fn run_case(node: Arc<EmbeddedNode>, case: &Case) -> Result<()> {
    ensure!(
        case.actions.len() == case.expected.len(),
        "missing step expectations"
    );
    let backend_id = format!("registry-{}", case.name);
    let mut harness = Harness {
        registry: AdmissionRegistry::new(node),
        backend_id: backend_id.clone(),
        calls: Vec::new(),
        held: VecDeque::new(),
        outcomes: Observation::default(),
        started: 0,
    };
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
                harness
                    .registry
                    .reconcile(desired.as_ref().map_or(0, |d| d.generation), &configs);
            }
            Action::Acquire { slot } => {
                harness.acquire(slot_connection(&backend_id, *slot)?);
            }
            Action::Release => {
                let Some(mut permit) = harness.held.pop_front() else {
                    bail!("trace released without an admitted call");
                };
                permit.finish_success(None).await?;
                drop(permit);
            }
        }
        harness.settle().await?;
        let actual = harness.observe();
        ensure!(
            actual == *expected,
            "step {index}: expected {expected:?}, got {actual:?}"
        );
    }
    for mut permit in harness.held.drain(..) {
        permit.finish_success(None).await?;
    }
    for call in harness.calls.drain(..) {
        call.abort();
    }
    Ok(())
}

#[tokio::test]
async fn generated_inference_registry_cases_drive_real_permits() {
    let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(snapshot.inference_registry_cases.len(), 10);
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
                connection: 7,
                generation,
                capacity,
                queue_depth: 0,
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
