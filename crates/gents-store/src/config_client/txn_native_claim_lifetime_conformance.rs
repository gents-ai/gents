use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crypto::keys::PrivateKey;
use defra_core::signing::{RemoteSigner, SigningAuthorization, SigningConfig, SigningKeyType};
use defra_node::Node as EmbeddedNode;
use identity::{Identity as _, RawIdentity};
use tokio::sync::Notify;

use super::{mutation_write_gate, ConfigAccess};

type TestError = Box<dyn std::error::Error + Send + Sync>;

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

struct ReleaseSignerOnDrop(Arc<BlockingSigner>);

impl Drop for ReleaseSignerOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn observe(home: &std::path::Path, data: &std::path::Path, node: &EmbeddedNode) -> (bool, bool) {
    let claim_held = crate::local::home::lock_store(home, data).is_err();
    let gate = mutation_write_gate(node);
    let mutation_gate_held = gate.permits.available_permits() < super::MUTATION_WRITE_GATE_CAPACITY;
    (claim_held, mutation_gate_held)
}

async fn signed_claimed_access(
    home: &std::path::Path,
    data: &std::path::Path,
) -> (
    Arc<EmbeddedNode>,
    ConfigAccess,
    Arc<BlockingSigner>,
    ReleaseSignerOnDrop,
) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::create_dir_all(data).unwrap();
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
    )
    .expect("the signing identity registers");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(data)
            .with_node_identity_did(did)
            .build()
            .await
            .unwrap(),
    );
    let claim = crate::local::home::lock_store(home, data).unwrap();
    let access = ConfigAccess::claimed_local(node.clone(), claim).unwrap();
    access
        .add_collection("type NativeClaimLifetimeProbe { value: String }")
        .await
        .unwrap();
    (node, access, signer.clone(), ReleaseSignerOnDrop(signer))
}

async fn shutdown_then_reacquire_store_lock(
    node: Arc<EmbeddedNode>,
    home: &std::path::Path,
    data: &std::path::Path,
) -> crate::local::home::StoreLock {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if super::existing_mutation_write_gate(&node).is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the Gents-owned native operation drops its claimed gate");
    node.shutdown().await.expect("node shutdown");
    drop(node);
    crate::local::home::lock_store(home, data)
        .expect("the store lock is reacquirable after the owned node shuts down")
}

async fn run_paused_owned_read(
    access: ConfigAccess,
    reached: Arc<Notify>,
    release: Arc<Notify>,
) -> Result<serde_json::Value, TestError> {
    super::ConfigApplyTxn::with_owned_local_operation_pause(reached, release, async move {
        tokio::time::timeout(
            Duration::from_millis(25),
            access.execute("{ NativeClaimLifetimeProbe { value } }"),
        )
        .await
        .map_err(TestError::from)?
        .map_err(TestError::from)
    })
    .await
}

async fn run_paused_owned_schema(
    access: ConfigAccess,
    reached: Arc<Notify>,
    release: Arc<Notify>,
    completed: Arc<Notify>,
) -> Result<(), TestError> {
    super::ConfigApplyTxn::with_owned_local_operation_completion(
        reached,
        release,
        completed,
        async move {
            tokio::time::timeout(
                Duration::from_millis(25),
                access.add_collection("type NativeClaimLifetimeSchemaProbe { value: String }"),
            )
            .await
            .map_err(TestError::from)?
            .map_err(TestError::from)
        },
    )
    .await
}

async fn auto_commit_cancel() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    signer.armed.store(true, Ordering::Release);

    let writer_access = access.clone();
    let writer = tokio::spawn(async move {
        writer_access
            .write(
                "test.native_claim_lifetime.cancelled_write",
                "mutation { create_NativeClaimLifetimeProbe(input: {value: \"signed\"}) {_docID} }",
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), signer.entered.notified())
        .await
        .expect("actual DefraDB signed write reaches the blocking signer");

    observed.push(observe(&home, &data, &node));
    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());

    observed.push(observe(&home, &data, &node));

    drop(access);
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));

    signer.release();
    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

async fn claimed_read_timeout() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, _signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let caller_access = access.clone();
    let reached_for_task = Arc::clone(&reached);
    let release_for_task = Arc::clone(&release);
    let caller = tokio::spawn(async move {
        run_paused_owned_read(caller_access, reached_for_task, release_for_task).await
    });
    tokio::time::timeout(Duration::from_secs(10), reached.notified())
        .await
        .expect("claimed read owner reaches its finite dispatch pause");
    observed.push(observe(&home, &data, &node));
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(50)).await;
    assert!(
        caller.await.unwrap().is_err(),
        "caller timeout must be observed"
    );
    tokio::time::resume();
    observed.push(observe(&home, &data, &node));
    drop(access);
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));

    release.notify_one();
    let response = ConfigAccess::local(Arc::clone(&node))
        .execute("{ NativeClaimLifetimeProbe { value } }")
        .await
        .unwrap();
    assert!(response.get("errors").is_none() || response["errors"].is_null());
    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

async fn claimed_schema_timeout() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, _signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let completed = Arc::new(Notify::new());
    let caller_access = access.clone();
    let reached_for_task = Arc::clone(&reached);
    let release_for_task = Arc::clone(&release);
    let completed_for_task = Arc::clone(&completed);
    let caller = tokio::spawn(async move {
        run_paused_owned_schema(
            caller_access,
            reached_for_task,
            release_for_task,
            completed_for_task,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), reached.notified())
        .await
        .expect("claimed schema owner reaches its finite dispatch pause");
    observed.push(observe(&home, &data, &node));
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(50)).await;
    assert!(
        caller.await.unwrap().is_err(),
        "caller timeout must be observed"
    );
    tokio::time::resume();
    observed.push(observe(&home, &data, &node));
    drop(access);
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));

    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), completed.notified())
        .await
        .expect("owned native schema operation returns before querying its schema");
    let response = ConfigAccess::local(Arc::clone(&node))
        .execute("{ NativeClaimLifetimeSchemaProbe { value } }")
        .await
        .unwrap();
    assert!(response.get("errors").is_none() || response["errors"].is_null());
    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

async fn readonly_transaction_cleanup() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, _signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let caller_access = access.clone();
    let reached_for_task = Arc::clone(&reached);
    let release_for_task = Arc::clone(&release);
    let caller = tokio::spawn(async move {
        super::ConfigApplyTxn::with_native_operation_pause(
            reached_for_task,
            release_for_task,
            caller_access.transact_readonly("test.native_claim_lifetime.readonly", |txn| {
                Box::pin(async move {
                    txn.execute("{ NativeClaimLifetimeProbe { value } }")
                        .await?;
                    Ok::<_, TestError>(())
                })
            }),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), reached.notified())
        .await
        .expect("read-only native query owner reaches its finite pause");
    observed.push(observe(&home, &data, &node));
    tokio::time::pause();
    tokio::time::advance(super::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT + Duration::from_secs(1))
        .await;
    let result = caller.await.unwrap();
    tokio::time::resume();
    let error = result.expect_err("the paused transaction query reaches its owner timeout");
    assert!(crate::config_client::is_transaction_step_unavailable(
        &error
    ));
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));
    drop(access);
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));

    release.notify_one();
    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

async fn mutation_transaction_cleanup() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    signer.armed.store(true, Ordering::Release);
    let caller_access = access.clone();
    let caller = tokio::spawn(async move {
        caller_access
            .transact_write("test.native_claim_lifetime.transaction", |txn| {
                Box::pin(async move {
                    txn.execute("mutation { create_NativeClaimLifetimeProbe(input: {value: \"txn\"}) {_docID} }")
                        .await?;
                    Ok::<_, TestError>(())
                })
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), signer.entered.notified())
        .await
        .expect("actual DefraDB transaction statement reaches the blocking signer");
    observed.push(observe(&home, &data, &node));
    tokio::time::pause();
    tokio::time::advance(super::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT + Duration::from_secs(1))
        .await;
    let result = caller.await.unwrap();
    tokio::time::resume();
    let error = result.expect_err("the blocked transaction statement reaches its timeout");
    assert!(crate::config_client::is_transaction_step_unavailable(
        &error
    ));
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));
    drop(access);
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));

    signer.release();
    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

async fn renewal_transaction() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, _signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let node_for_task = Arc::clone(&node);
    let reached_for_task = Arc::clone(&reached);
    let release_for_task = Arc::clone(&release);
    let renewal = tokio::spawn(async move {
        super::ConfigApplyTxn::with_native_operation_pause(
            reached_for_task,
            release_for_task,
            async move {
                let txn =
                    super::ConfigApplyTxn::begin_execution_renewal_for_test(&node_for_task).await?;
                txn.execute("{ NativeClaimLifetimeProbe { value } }").await
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), reached.notified())
        .await
        .expect("renewal native operation reaches its finite pause");
    observed.push(observe(&home, &data, &node));
    tokio::time::pause();
    tokio::time::advance(super::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT + Duration::from_secs(1))
        .await;
    let result = renewal.await.unwrap();
    tokio::time::resume();
    let error = result.expect_err("the paused renewal statement reaches its timeout");
    assert!(crate::config_client::is_transaction_step_unavailable(
        &error
    ));
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));
    drop(access);
    observed.push(observe(&home, &data, &node));
    release.notify_one();
    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

async fn committed_readonly_transaction() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, _signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    access
        .transact_readonly("test.native_claim_lifetime.committed", |txn| {
            Box::pin(async move {
                txn.execute("{ NativeClaimLifetimeProbe { value } }")
                    .await?;
                Ok::<_, TestError>(())
            })
        })
        .await
        .unwrap();
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));
    drop(access);
    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

async fn poisoned_transaction_thread_drop() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    let (node, access, signer, _release_on_panic) = signed_claimed_access(&home, &data).await;
    signer.armed.store(true, Ordering::Release);
    let mut txn = super::ConfigApplyTxn::begin_local(&node, None)
        .await
        .unwrap();
    let cleanup_pause = Arc::new(super::RollbackDropCleanupPause {
        started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    txn.pause_drop_rollback_for_test(Arc::clone(&cleanup_pause));

    {
        let execution = txn.execute(
            "mutation { create_NativeClaimLifetimeProbe(input: {value: \"thread-drop\"}) {_docID} }",
        );
        tokio::pin!(execution);
        tokio::select! {
            result = &mut execution => panic!("blocked signed statement returned early: {result:?}"),
            _ = signer.entered.notified() => {}
        }
        tokio::time::pause();
        tokio::time::advance(
            super::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT + Duration::from_secs(1),
        )
        .await;
        let error = execution
            .await
            .expect_err("the blocked statement reaches its owner timeout");
        assert!(crate::config_client::is_transaction_step_unavailable(
            &error
        ));
        tokio::time::resume();
    }

    observed.push(observe(&home, &data, &node));
    std::thread::spawn(move || drop(txn))
        .join()
        .expect("dropping the poisoned transaction from an OS thread");
    drop(access);
    observed.push(observe(&home, &data, &node));
    observed.push(observe(&home, &data, &node));

    signer.release();
    tokio::time::timeout(Duration::from_secs(10), cleanup_pause.started.notified())
        .await
        .expect("rollback owner waits for native completion before cleanup");
    observed.push(observe(&home, &data, &node));
    cleanup_pause.release.notify_one();

    let reacquired = shutdown_then_reacquire_store_lock(node, &home, &data).await;
    drop(reacquired);
    observed
}

#[tokio::test]
async fn claimed_store_owners_follow_the_lean_claim_lifetime() {
    for case in
        gents_lean_vocab::families::native_claim_lifetime_store_scenario::cases::<String, String>()
    {
        let observed = match case.input.lane.as_str() {
            "auto_commit_write" => auto_commit_cancel().await,
            "claimed_read" => claimed_read_timeout().await,
            "claimed_schema" => claimed_schema_timeout().await,
            "read_only_transaction"
                if case
                    .input
                    .events
                    .contains(&"transaction_committed".to_string()) =>
            {
                committed_readonly_transaction().await
            }
            "read_only_transaction" => readonly_transaction_cleanup().await,
            "mutation_transaction" => mutation_transaction_cleanup().await,
            "periodic_renewal" => renewal_transaction().await,
            other => panic!("{}: no store scenario drives lane {other}", case.input.name),
        };
        case.check(
            observed
                .into_iter()
                .map(|(claim_held, mutation_gate_held)| {
                    gents_lean_vocab::families::native_claim_lifetime_store_scenario::StoreView {
                        claim_held,
                        mutation_gate_held,
                    }
                })
                .collect(),
        );
    }
}

#[tokio::test]
async fn poisoned_transaction_dropped_from_a_thread_follows_the_lean_claim_lifetime() {
    for case in gents_lean_vocab::families::native_claim_lifetime_thread_drop_scenario::cases::<
        String,
        String,
    >() {
        let observed = poisoned_transaction_thread_drop().await;
        case.check(
            observed
                .into_iter()
                .map(|(claim_held, mutation_gate_held)| gents_lean_vocab::families::native_claim_lifetime_thread_drop_scenario::StoreView {
                    claim_held,
                    mutation_gate_held,
                })
                .collect(),
        );
    }
}
