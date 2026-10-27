use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crypto::keys::PrivateKey;
use defra_core::signing::{RemoteSigner, SigningAuthorization, SigningConfig, SigningKeyType};
use defra_node::EmbeddedNode;
use identity::{Identity as _, RawIdentity};
use serde::Deserialize;
use tokio::sync::Notify;

use super::{
    mutation_write_gate, ConfigAccess, ConfigApplyTxn, NativeOperationTracker, TxnBackend,
    EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
};
use crate::config_client::write_telemetry::WriteOperation;

fn gate_owns_operation(gate: &super::MutationWriteGate, operation: &str) -> bool {
    gate.owners
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .any(|owner| owner.operation.as_str() == operation)
}

struct BlockingSigner {
    private_key: Vec<u8>,
    armed: AtomicBool,
    entered: Notify,
    released: Mutex<bool>,
    resume: Condvar,
}

impl BlockingSigner {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.resume.notify_all();
    }
}

impl RemoteSigner for BlockingSigner {
    fn sign_sync(
        &self,
        data: &[u8],
        _authorization: Option<&SigningAuthorization>,
    ) -> Result<Vec<u8>, String> {
        if self.armed.swap(false, Ordering::AcqRel) {
            self.entered.notify_one();
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.resume.wait(released).unwrap();
            }
        }
        let key = crypto::Secp256k1PrivateKey::from_bytes(&self.private_key)
            .map_err(|error| error.to_string())?;
        key.sign(data).map_err(|error| error.to_string())
    }
}

/// An assertion failure must also unblock DefraDB's real signing thread before
/// the test runtime shuts down and joins its workers.
struct ReleaseSignerOnDrop(Arc<BlockingSigner>);

impl Drop for ReleaseSignerOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[derive(Deserialize)]
struct Initial {
    cleanup_required: bool,
    native_outstanding: usize,
    gate_held: bool,
}

#[derive(Deserialize)]
struct Observation {
    gate_held: bool,
    native_outstanding: usize,
    registry_abandoned: bool,
    rollback_returned: bool,
    retry_started: bool,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    initial: Initial,
    events: Vec<String>,
    observations: Vec<Observation>,
}

async fn signed_node() -> (Arc<EmbeddedNode>, Arc<BlockingSigner>, ReleaseSignerOnDrop) {
    let identity = RawIdentity::from_secp256k1(crypto::generate_secp256k1().unwrap()).unwrap();
    let did = identity.did().unwrap().to_string();
    let signer = Arc::new(BlockingSigner {
        private_key: identity.private_key_bytes(),
        armed: AtomicBool::new(false),
        entered: Notify::new(),
        released: Mutex::new(false),
        resume: Condvar::new(),
    });
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Secp256k1,
            private_key_bytes: Vec::new(),
            public_key_bytes: identity.public_key_bytes(),
            public_key_hex: String::new(),
            remote_signer: Some(signer.clone()),
            signing_authorization: None,
        },
    );
    let release = ReleaseSignerOnDrop(signer.clone());
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(did)
            .build()
            .await
            .unwrap(),
    );
    ConfigAccess::local(node.clone())
        .add_collection("type NativeCompletionProbe { value: String }")
        .await
        .unwrap();
    (node, signer, release)
}

#[tokio::test]
async fn lean_auto_commit_completion_owns_the_real_signed_worker() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../tests/fixtures/native_completion.json")).unwrap();
    let mut exercised = 0;
    for case in cases.into_iter().filter(|case| {
        !case.initial.cleanup_required && case.events.iter().any(|event| event == "native_returned")
    }) {
        exercised += 1;
        assert_eq!(case.initial.native_outstanding, 0);
        assert!(case.initial.gate_held);
        assert_eq!(case.events.len(), case.observations.len());
        let (node, signer, _release_on_panic) = signed_node().await;
        let gate = mutation_write_gate(&node);
        let mut capacity_fillers = Vec::new();
        let mut caller = None;
        for (event, expected) in case.events.iter().zip(&case.observations) {
            match event.as_str() {
                "native_started" => {
                    signer.armed.store(true, Ordering::Release);
                    let node = node.clone();
                    caller = Some(tokio::spawn(async move {
                        ConfigAccess::write_local(
                            &node,
                            "test.native_completion.actual_signer",
                            "mutation { create_NativeCompletionProbe(input: {value: \"persisted\"}) {_docID} }",
                        )
                        .await
                    }));
                    tokio::time::timeout(Duration::from_secs(10), signer.entered.notified())
                        .await
                        .expect("the real signed database worker reaches the signer");
                    for _ in 1..super::MUTATION_WRITE_GATE_CAPACITY {
                        capacity_fillers.push(
                            gate.acquire(
                                WriteOperation::new("test.native_completion.capacity_filler")
                                    .unwrap(),
                            )
                            .await
                            .expect("fill the admission pool while the modeled owner is blocked"),
                        );
                    }
                }
                "caller_timed_out" => {
                    tokio::time::pause();
                    tokio::time::advance(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT).await;
                    let result = caller.take().unwrap().await.unwrap();
                    tokio::time::resume();
                    let error = result.expect_err("caller must observe timeout");
                    assert!(
                        crate::config_client::is_transaction_step_unavailable(&error),
                        "{}: unfinished storage work remains an availability failure",
                        case.name
                    );
                }
                "caller_cancelled" => {
                    let task = caller.take().unwrap();
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                }
                "native_returned" => {
                    signer.release();
                    let acquired = tokio::time::timeout(
                        Duration::from_secs(10),
                        gate.acquire(
                            WriteOperation::new("test.native_completion.drained").unwrap(),
                        ),
                    )
                    .await
                    .expect("completed native worker releases its gate")
                    .unwrap();
                    drop(acquired);
                    capacity_fillers.clear();
                    continue;
                }
                "gate_release_requested" => {}
                other => panic!("unsupported native auto-commit action {other}"),
            }
            let held = gate_owns_operation(&gate, "test.native_completion.actual_signer");
            assert_eq!(held, expected.gate_held, "{} after {event}", case.name);
        }
        assert!(
            caller.is_none(),
            "{} must account for its caller",
            case.name
        );
        let data = ConfigAccess::local(node.clone())
            .execute("{ NativeCompletionProbe { value } }")
            .await
            .unwrap();
        assert_eq!(
            data["data"]["NativeCompletionProbe"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            data["data"]["NativeCompletionProbe"][0]["value"],
            "persisted"
        );
        node.shutdown().await;
    }
    assert!(exercised >= 2, "Lean must cover timeout and cancellation");
}

async fn wait_for_native_count(tracker: &NativeOperationTracker, expected: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count = tracker.state.lock().unwrap().in_flight;
            if count == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native operation count reaches the modeled observation");
}

#[tokio::test]
async fn lean_abandoned_registry_retains_gate_until_every_native_operation_drains() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../tests/fixtures/native_completion.json")).unwrap();
    let concurrent = cases
        .iter()
        .find(|case| case.name == "registry_abandonment_retains_gate_until_all_native_returns")
        .unwrap();
    let expected = &concurrent.observations[6];
    let operation_count = concurrent
        .events
        .iter()
        .filter(|event| event.as_str() == "native_started")
        .count();
    let (node, _signer, _release_on_panic) = signed_node().await;
    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    txn.execute(
        r#"mutation { create_NativeCompletionProbe(input: { value: "uncommitted" }) { _docID } }"#,
    )
    .await
    .unwrap();
    let (tracker, handle) = match &txn.backend {
        TxnBackend::Embedded {
            native_operations,
            handle,
            ..
        } => (native_operations.clone(), handle.clone()),
        _ => unreachable!(),
    };
    let gate = mutation_write_gate(&node);
    let mut capacity_fillers = Vec::new();
    for _ in 1..super::MUTATION_WRITE_GATE_CAPACITY {
        capacity_fillers.push(
            gate.acquire(WriteOperation::new("test.native_completion.capacity_filler").unwrap())
                .await
                .expect("fill admission capacity while the modeled transaction owns its lease"),
        );
    }
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let worker_release = release.clone();
    let caller = tokio::spawn(async move {
        ConfigApplyTxn::with_native_operation_pause(entered, worker_release, async {
            futures::future::join_all(
                (0..operation_count).map(|_| txn.execute("{ NativeCompletionProbe { value } }")),
            )
            .await
        })
        .await
    });
    wait_for_native_count(&tracker, operation_count).await;
    tokio::time::pause();
    tokio::time::advance(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT).await;
    let results = caller.await.unwrap();
    tokio::time::resume();
    assert!(results.iter().all(Result::is_err));

    let abandoned = &concurrent.observations[4];
    assert!(abandoned.registry_abandoned);
    assert!(!abandoned.rollback_returned);
    assert_eq!(
        tracker.state.lock().unwrap().in_flight,
        abandoned.native_outstanding
    );
    assert_eq!(
        gate_owns_operation(&gate, "test.begin_local"),
        abandoned.gate_held
    );
    let response = node
        .execute_request_in_txn(
            defra_node::QueryRequest::new("{ NativeCompletionProbe { value } }"),
            &handle,
        )
        .await;
    assert_eq!(response.has_errors(), abandoned.registry_abandoned);

    release.notify_one();
    wait_for_native_count(&tracker, expected.native_outstanding).await;
    assert_eq!(
        gate_owns_operation(&gate, "test.begin_local"),
        expected.gate_held
    );
    let response = node
        .execute_request_in_txn(
            defra_node::QueryRequest::new("{ NativeCompletionProbe { value } }"),
            &handle,
        )
        .await;
    assert_eq!(response.has_errors(), expected.registry_abandoned);

    release.notify_one();
    let acquire_after_drain =
        gate.acquire(WriteOperation::new("test.native_completion.all_drained").unwrap());
    let acquired = tokio::time::timeout(Duration::from_secs(10), acquire_after_drain)
        .await
        .unwrap()
        .unwrap();
    drop(acquired);
    assert!(!gate_owns_operation(&gate, "test.begin_local"));
    drop(capacity_fillers);
    assert_eq!(
        gate.permits.available_permits() < super::MUTATION_WRITE_GATE_CAPACITY,
        concurrent.observations.last().unwrap().gate_held
    );
    let drained = concurrent.observations.last().unwrap();
    assert_eq!(
        tracker.state.lock().unwrap().in_flight,
        drained.native_outstanding
    );
    assert!(drained.registry_abandoned);
    assert!(!drained.rollback_returned);
    assert!(!drained.retry_started);
    let response = ConfigAccess::local(node.clone())
        .execute("{ NativeCompletionProbe { value } }")
        .await
        .unwrap();
    assert_eq!(
        response["data"]["NativeCompletionProbe"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    node.shutdown().await;
}
