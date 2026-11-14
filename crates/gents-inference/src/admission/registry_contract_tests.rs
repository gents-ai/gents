use super::*;

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{bail, ensure, Result};

use crate::backends::registry::backend_fields;
use tokio::task::JoinHandle;

use gents_lean_vocab::families::inference_registry_admission as family;

struct Desired {
    connection: u64,
    capacity: usize,
    queue_depth: usize,
    available: bool,
    name: String,
    catalog: String,
}

#[derive(Default)]
struct Counters {
    admitted: u64,
    queue_full: u64,
    gone: u64,
    attributed: Vec<u64>,
}

fn backend_document(
    backend_id: &str,
    connection: u64,
    capacity: usize,
    queue_depth: usize,
    name: &str,
) -> Result<gents_documents::InferenceBackend> {
    Ok(serde_json::from_value(serde_json::json!({
        "node_did":"did:test:registry", "backend_id":backend_id, "name":name,
        "provider_kind":"OpenAiCompatible",
        "endpoint":format!("http://127.0.0.1/resource-{connection}/v1"),
        "auth":{"kind":"unauthenticated"}, "max_concurrent":i64::try_from(capacity)?,
        "max_queue_depth":i64::try_from(queue_depth)?
    }))?)
}

fn slot_connection(backend_id: &str, connection: u64) -> Result<String> {
    let backend = backend_document(backend_id, connection, 1, 0, "slot")?;
    Ok(super::super::config::backend_connection_fingerprint(
        &backend_fields(&backend),
    ))
}

fn config_from_case(backend_id: &str, desired: &Desired) -> Result<BackendAdmissionConfig> {
    let backend = backend_document(
        backend_id,
        desired.connection,
        desired.capacity,
        desired.queue_depth,
        &desired.name,
    )?;
    let observation = serde_json::from_value::<gents_documents::InferenceBackendObservation>(
        serde_json::json!({
            "backend_id":backend_id, "probe_status":crate::backends::registry::HEALTHY_PROBE_STATUS,
            "catalogs":[{"node_did":null,"observed_at":"2026-01-01T00:00:00Z",
                "models":[{"model_name":desired.catalog}]}]
        }),
    )?;
    Ok(
        BackendAdmissionConfig::from_backend(&backend, &observation)?
            .with_measured_unhealthy(!desired.available),
    )
}

async fn acquire_with_signer(
    node: &Arc<EmbeddedNode>,
    registry: &AdmissionRegistry,
    request_id: &str,
    backend_id: &str,
    signer: &gents_store::identity::ManagedAgentSigner,
    connection: &str,
) -> Result<AdmissionPermit, AdmissionError> {
    let request = super::super::tests::seeded_request(node, signer, request_id).await;
    let context = AdmissionCallContext::for_request(&request, signer.did(), backend_id);
    scope_request(
        context,
        registry.acquire_current_call_with_signer(connection, None, None, Some(signer.clone())),
    )
    .await
}

struct Harness {
    registry: AdmissionRegistry,
    node: Arc<EmbeddedNode>,
    signer: gents_store::identity::ManagedAgentSigner,
    backend_id: String,
    calls: Vec<JoinHandle<Result<AdmissionPermit, AdmissionError>>>,
    held: VecDeque<AdmissionPermit>,
    outcomes: Counters,
    started: usize,
    slots: rapidhash::RapidHashMap<String, u64>,
}

impl Harness {
    fn acquire(&mut self, slot: u64, connection: String) {
        self.slots.insert(connection.clone(), slot);
        let registry = self.registry.clone();
        let node = self.node.clone();
        let backend_id = self.backend_id.clone();
        let signer = self.signer.clone();
        let request_id = format!("{}-{}", self.backend_id, self.started);
        self.started += 1;
        self.calls.push(tokio::spawn(async move {
            acquire_with_signer(
                &node,
                &registry,
                &request_id,
                &backend_id,
                &signer,
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
                        let Some(slot) = self.slots.get(permit.attribution_for_test()) else {
                            bail!("admitted call is not attributed to a caller's slot");
                        };
                        self.outcomes.attributed.push(*slot);
                        self.outcomes.attributed.sort_unstable();
                        self.held.push_back(permit);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if message.contains("QueueFull") {
                            self.outcomes.queue_full += 1;
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

    fn observe(&self) -> Result<family::Observation> {
        let admitting = self.registry.active_for_test(&self.backend_id);
        let pool = self.registry.pool_for_test(&self.backend_id);
        Ok(family::Observation {
            admitting_generation: admitting.as_ref().map(|c| c.generation),
            capacity: u64::try_from(admitting.as_ref().map_or(0, |c| c.config.max_concurrent))?,
            held: u64::try_from(pool.as_ref().map_or(0, |pool| pool.held_for_test()))?,
            queued: u64::try_from(
                pool.as_ref()
                    .map_or(0, |pool| pool.queue_waiters_for_test()),
            )?,
            tally: family::Tally {
                admitted: self.outcomes.admitted,
                queue_full: self.outcomes.queue_full,
                gone: self.outcomes.gone,
            },
            attributed: self.outcomes.attributed.clone(),
        })
    }
}

async fn run_case(
    node: Arc<EmbeddedNode>,
    signer: gents_store::identity::ManagedAgentSigner,
    trace: &family::Trace<String>,
) -> Result<family::Outcome> {
    let backend_id = format!("registry-{}", trace.name);
    let mut harness = Harness {
        registry: AdmissionRegistry::new(node.clone()),
        node,
        signer,
        backend_id: backend_id.clone(),
        calls: Vec::new(),
        held: VecDeque::new(),
        outcomes: Counters::default(),
        started: 0,
        slots: rapidhash::RapidHashMap::default(),
    };
    let mut observations = Vec::new();
    for step in &trace.steps {
        match step.kind.as_str() {
            "reconcile" => {
                let configs = step
                    .desired
                    .as_ref()
                    .map(|d| {
                        let desired = Desired {
                            connection: d.config.connection,
                            capacity: usize::try_from(d.config.capacity)?,
                            queue_depth: usize::try_from(d.config.queue_depth)?,
                            available: d.config.available,
                            name: d.name.clone(),
                            catalog: d.catalog.clone(),
                        };
                        Ok::<_, anyhow::Error>((
                            backend_id.clone(),
                            config_from_case(&backend_id, &desired)?,
                        ))
                    })
                    .transpose()?
                    .into_iter()
                    .collect();
                harness.registry.reconcile(
                    step.desired.as_ref().map_or(0, |d| d.config.generation),
                    &configs,
                );
            }
            "acquire" => {
                let Some(slot) = step.slot else {
                    bail!("acquire without a slot");
                };
                harness.acquire(slot, slot_connection(&backend_id, slot)?);
            }
            "release" => {
                let Some(mut permit) = harness.held.pop_front() else {
                    bail!("trace released without an admitted call");
                };
                permit.finish_success(None).await?;
                drop(permit);
            }
            other => bail!("unknown step kind {other}"),
        }
        harness.settle().await?;
        observations.push(harness.observe()?);
    }
    for mut permit in harness.held.drain(..) {
        permit.finish_success(None).await?;
    }
    for call in harness.calls.drain(..) {
        call.abort();
    }
    Ok(family::Outcome { observations })
}

#[tokio::test]
async fn generated_inference_registry_cases_drive_real_permits() {
    let (db, signer) = super::super::tests::test_node().await;
    let node = db.node.clone();
    let mut failures = Vec::new();
    for case in family::cases::<String>() {
        match run_case(node.clone(), signer.clone(), &case.input).await {
            Ok(outcome) => case.check(outcome),
            Err(error) => failures.push(format!("{}: {error:#}", case.input.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn rollback_reuses_epoch_with_distinct_controller_on_shared_pool() {
    let (db, signer) = super::super::tests::test_node().await;
    let node = db.node.clone();
    let registry = AdmissionRegistry::new(node.clone());
    let backend_id = "registry-rollback-epoch";
    let config = |capacity| {
        config_from_case(
            backend_id,
            &Desired {
                connection: 7,
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
        acquire_with_signer(
            &node,
            &registry,
            request_id,
            backend_id,
            &signer,
            TEST_SLOT_CONNECTION,
        )
    };
    registry.reconcile(
        1,
        &rapidhash::RapidHashMap::from_iter([(backend_id.to_owned(), config(1))]),
    );
    let old = registry.active_for_test(backend_id).unwrap();
    let mut first = acquire("rollback-old-owner").await.unwrap();
    registry.reconcile(
        2,
        &rapidhash::RapidHashMap::from_iter([(backend_id.to_owned(), config(2))]),
    );
    registry.reconcile(
        1,
        &rapidhash::RapidHashMap::from_iter([(backend_id.to_owned(), config(1))]),
    );
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
