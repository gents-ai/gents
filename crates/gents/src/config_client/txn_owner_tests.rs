use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use defra_node::EmbeddedNode;
use serde_json::{json, Value};
use tokio::sync::{Barrier, Notify};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::{
    ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry, TransactionOutcome, TxnBackend,
};
use crate::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET;

fn transaction_id(txn: &ConfigApplyTxn<'_>) -> String {
    match &txn.backend {
        TxnBackend::Http { id, .. } => id.clone(),
        TxnBackend::Embedded { handle, .. } => handle.as_str().to_owned(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Default)]
struct EventCapture {
    events: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
}

#[derive(Default)]
struct EventFields(BTreeMap<String, String>);

impl Visit for EventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl<S> Layer<S> for EventCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: LayerContext<'_, S>) {
        if event.metadata().target() != WRITE_ATTEMPT_EVENT_TARGET {
            return;
        }
        let mut fields = EventFields::default();
        event.record(&mut fields);
        lock(&self.events).push(fields.0);
    }
}

#[tokio::test]
async fn cancellation_after_embedded_begin_reports_and_completes_rollback() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let node_ref = &node;
    let runner = node.runner().clone();
    let gate = Arc::new(super::MutationWriteGate::new());
    let gate_for_assert = Arc::clone(&gate);
    let registered = Arc::new(Mutex::new(None));
    let registered_for_begin = Arc::clone(&registered);
    let registered_notify = Arc::new(Notify::new());
    let registered_notify_for_begin = Arc::clone(&registered_notify);
    let release = Arc::new(Notify::new());
    let release_for_begin = Arc::clone(&release);
    let telemetry = EventCapture::default();
    let events = Arc::clone(&telemetry.events);
    let subscriber = tracing::Dispatch::new(Registry::default().with(telemetry));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);

    let mut transaction = Box::pin(super::transact_owned(
        "test.cancel_after_embedded_begin",
        crate::config_client::write_telemetry::WriteBackend::Embedded,
        super::TransactionMode::ConflictRetry,
        move |operation, rollback_scheduled| {
            let runner = Arc::clone(&runner);
            let gate = Arc::clone(&gate);
            let registered = Arc::clone(&registered_for_begin);
            let registered_notify = Arc::clone(&registered_notify_for_begin);
            let release = Arc::clone(&release_for_begin);
            Box::pin(async move {
                let write_guard = gate.acquire(operation).await?;
                let (rollback_on_drop, handle) = tokio::spawn(super::begin_embedded_owned(
                    runner,
                    write_guard,
                    rollback_scheduled,
                    move |handle| async move {
                        *lock(&registered) = Some(handle);
                        registered_notify.notify_one();
                        release.notified().await;
                    },
                ))
                .await
                .expect("detached begin task")?;
                Ok(ConfigApplyTxn {
                    backend: TxnBackend::Embedded {
                        node: node_ref,
                        handle,
                        identity: None,
                    },
                    rollback_on_drop: Some(rollback_on_drop),
                    affected_documents: std::sync::atomic::AtomicU64::new(0),
                })
            })
        },
        |_| Box::pin(async { Ok::<_, anyhow::Error>(()) }),
    ));

    tokio::select! {
        () = registered_notify.notified() => {}
        result = &mut transaction => panic!("transaction finished before cancellation: {result:?}"),
    }
    let handle = lock(&registered)
        .clone()
        .expect("registered transaction handle was observed");
    drop(transaction);
    release.notify_one();

    let next_guard = tokio::time::timeout(
        Duration::from_secs(2),
        gate_for_assert.acquire(
            crate::config_client::write_telemetry::WriteOperation::new(
                "test.write_after_cancelled_begin",
            )
            .expect("valid test write operation"),
        ),
    )
    .await
    .expect("rollback finishes before releasing the write gate")
    .expect("write gate acquisition succeeds");
    drop(next_guard);
    let error = node
        .runner()
        .rollback_txn(&handle)
        .await
        .expect_err("cancelled begin already removed the registered transaction");
    assert!(matches!(error, query::TransactionError::NotFound(_)));

    let events = lock(&events);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].get("outcome").map(String::as_str),
        Some("cancelled")
    );
    assert_eq!(
        events[0].get("rollback").map(String::as_str),
        Some("scheduled")
    );
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn cancellation_before_begin_reports_no_scheduled_rollback() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let gate = super::mutation_write_gate(&node);
    let held = gate.lock.lock().await;
    let telemetry = EventCapture::default();
    let events = Arc::clone(&telemetry.events);
    let subscriber = tracing::Dispatch::new(Registry::default().with(telemetry));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    let mut transaction = Box::pin(ConfigAccess::transact_local(
        &node,
        None,
        "test.cancel_before_begin",
        |_| Box::pin(async { Ok::<_, anyhow::Error>(()) }),
    ));

    tokio::time::timeout(Duration::from_millis(10), &mut transaction)
        .await
        .expect_err("transaction remains blocked before begin");
    drop(transaction);
    drop(held);

    let events = lock(&events);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].get("outcome").map(String::as_str),
        Some("cancelled")
    );
    assert_eq!(
        events[0].get("rollback").map(String::as_str),
        Some("not_needed")
    );
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn stalled_callback_releases_the_embedded_write_gate() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    node.add_schema("type WriteAfterStall { value: String }")
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let entered_for_callback = Arc::clone(&entered);
    let mut stalled = Box::pin(ConfigAccess::transact_local(
        &node,
        None,
        "test.stalled_callback",
        move |_| {
            let entered = Arc::clone(&entered_for_callback);
            Box::pin(async move {
                entered.notify_one();
                std::future::pending::<Result<()>>().await
            })
        },
    ));

    tokio::select! {
        () = entered.notified() => {}
        result = &mut stalled => panic!("transaction finished before callback stalled: {result:?}"),
    }
    tokio::time::advance(super::EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT).await;
    let error = stalled.await.expect_err("stalled transaction times out");
    assert!(
        error.to_string().contains("callback timed out"),
        "{error:#}"
    );
    assert!(
        super::retry::is_transaction_storage_failure(&error),
        "callback timeout must use the standard storage-failure retry classification"
    );

    ConfigAccess::write_local(
        &node,
        "test.write_after_stall",
        r#"mutation { create_WriteAfterStall(input: {value: "committed"}) { _docID } }"#,
    )
    .await
    .expect("a later canonical write is not blocked by the stalled transaction");
    let response = node.execute("{ WriteAfterStall { value } }").await;
    assert_eq!(
        response.data.unwrap()["WriteAfterStall"][0]["value"],
        "committed"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn nested_canonical_write_fails_fast_and_releases_the_embedded_write_gate() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    node.add_schema("type NestedWriteProbe { value: String }")
        .await
        .unwrap();

    let error = ConfigAccess::transact_local(&node, None, "test.outer_transaction", |_| {
        Box::pin(async {
            ConfigAccess::write_local(
                &node,
                "test.inner_write",
                r#"mutation { create_NestedWriteProbe(input: {value: "nested"}) { _docID } }"#,
            )
            .await?;
            Ok(())
        })
    })
    .await
    .expect_err("a canonical write cannot recursively acquire its transaction's gate");
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("test.inner_write"), "{diagnostic}");
    assert!(
        diagnostic.contains("test.outer_transaction"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("use the supplied transaction owner"),
        "{diagnostic}"
    );
    assert!(
        !super::retry::is_transaction_storage_failure(&error),
        "an application ownership violation is not a retryable storage failure"
    );

    ConfigAccess::write_local(
        &node,
        "test.write_after_nested_rejection",
        r#"mutation { create_NestedWriteProbe(input: {value: "committed"}) { _docID } }"#,
    )
    .await
    .expect("rejecting the nested write releases the outer transaction gate");
    let response = node.execute("{ NestedWriteProbe { value } }").await;
    assert_eq!(
        response.data.unwrap()["NestedWriteProbe"][0]["value"],
        "committed"
    );
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn nested_idempotent_write_does_not_replay_the_ownership_violation() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let started = tokio::time::Instant::now();

    let error = ConfigAccess::transact_local(&node, None, "test.outer_transaction", |_| {
        Box::pin(async {
            ConfigAccess::transact_local_idempotent(
                &node,
                None,
                super::IdempotentTransactionRetry::Standard,
                "test.inner_idempotent_write",
                |_| Box::pin(async { Ok(()) }),
            )
            .await
        })
    })
    .await
    .expect_err("a nested idempotent write remains an ownership violation");

    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("test.inner_idempotent_write"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("test.outer_transaction"),
        "{diagnostic}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "ownership rejection must not consume idempotent retry backoff"
    );
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn write_gate_timeout_names_waiter_and_current_owner() {
    let gate = Arc::new(super::MutationWriteGate::new());
    let owner = gate
        .acquire(
            crate::config_client::write_telemetry::WriteOperation::new("test.current_owner")
                .unwrap(),
        )
        .await
        .unwrap();
    let mut waiting = Box::pin(
        gate.acquire(
            crate::config_client::write_telemetry::WriteOperation::new("test.waiting_operation")
                .unwrap(),
        ),
    );

    tokio::select! {
        _ = &mut waiting => panic!("waiter completed while the owner holds the gate"),
        _ = tokio::time::sleep(Duration::from_millis(1)) => {}
    }
    tokio::time::advance(super::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT).await;
    let error = match waiting.as_mut().await {
        Ok(_) => panic!("waiting operation acquired a gate that is still owned"),
        Err(error) => error,
    };
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("test.waiting_operation"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("test.current_owner"), "{diagnostic}");
    assert!(
        super::retry::is_transaction_storage_failure(&error),
        "gate wait timeouts use the standard storage-failure retry classification"
    );
    drop(owner);
}

#[tokio::test(start_paused = true)]
async fn stalled_cancellation_cleanup_releases_the_embedded_write_gate() {
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    let guard = Arc::clone(&gate).lock_owned().await;
    let cleanup = tokio::spawn(super::cleanup_while_holding_write_gate(
        Some(guard),
        std::future::pending::<()>(),
    ));

    tokio::time::advance(super::EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT).await;
    cleanup
        .await
        .expect("cleanup task joins")
        .expect_err("stalled cleanup times out");
    let _guard = tokio::time::timeout(Duration::from_millis(1), gate.lock())
        .await
        .expect("the embedded write gate is released after cleanup times out");
}

#[test]
fn embedded_timeout_diagnostics_preserve_phase_and_retry_classification() {
    for phase in [
        "write-gate acquisition",
        "begin",
        "execute",
        "callback",
        "commit",
        "rollback",
        "auto-commit",
    ] {
        let error = super::embedded_phase_timeout(phase, Duration::from_secs(1));
        assert!(error.to_string().contains(phase), "{error:#}");
        assert!(
            super::retry::is_transaction_storage_failure(&error),
            "{phase} timeout must use the standard storage-failure retry classification"
        );
    }
}

#[tokio::test]
async fn embedded_conflict_replays_complete_callback_with_fresh_snapshot() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    node.add_schema("type OwnedWriteFact { value: String }")
        .await
        .unwrap();
    ConfigAccess::write_local(
        &node,
        "test.seed_embedded_replay",
        r#"mutation { create_OwnedWriteFact(input: {value: "initial"}) { _docID } }"#,
    )
    .await
    .unwrap();

    let attempts = Arc::new(AtomicUsize::new(0));
    let handles = Arc::new(Mutex::new(Vec::new()));
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let attempts_for_callback = Arc::clone(&attempts);
    let handles_for_callback = Arc::clone(&handles);
    let snapshots_for_callback = Arc::clone(&snapshots);
    let node_ref = &node;

    ConfigAccess::transact_local(&node, None, "test.embedded_replay", move |txn| {
        let attempt = attempts_for_callback.fetch_add(1, Ordering::SeqCst);
        let handles = Arc::clone(&handles_for_callback);
        let snapshots = Arc::clone(&snapshots_for_callback);
        Box::pin(async move {
            lock(&handles).push(transaction_id(txn));
            let response = txn
                .execute("{ OwnedWriteFact { value } }")
                .await?;
            let observed = response["data"]["OwnedWriteFact"][0]["value"]
                .as_str()
                .expect("stored value")
                .to_owned();
            lock(&snapshots).push(observed.clone());

            if attempt == 0 {
                let competing = node_ref
                    .execute(
                        r#"mutation { update_OwnedWriteFact(filter: {value: {_eq: "initial"}}, input: {value: "racer"}) { _docID } }"#,
                    )
                    .await;
                assert!(!competing.has_errors(), "{:?}", competing.errors);
            }

            txn.execute(&format!(
                r#"mutation {{ update_OwnedWriteFact(filter: {{value: {{_eq: "{observed}"}}}}, input: {{value: "canonical"}}) {{ _docID }} }}"#,
            ))
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let handles = lock(&handles);
    assert_eq!(handles.len(), 2);
    assert_ne!(handles[0], handles[1], "replay reused a transaction handle");
    assert_eq!(&*lock(&snapshots), &["initial", "racer"]);

    let durable = node.execute("{ OwnedWriteFact { value } }").await;
    assert!(!durable.has_errors(), "{:?}", durable.errors);
    assert_eq!(
        durable.data.unwrap()["OwnedWriteFact"][0]["value"],
        "canonical"
    );
    node.shutdown().await;
}

/// Regression for the write/event feedback loop that wedged response
/// publication under concurrent request progress. All application writes enter
/// through the canonical owner, while a deliberately idle current-state
/// observer coalesces every revision of the hot document into one invalidation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_two_hot_document_writers_converge_without_event_loss() {
    const WORKERS: usize = 32;

    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema(
        "type HotMutationState { key: String @index(unique: true) @immutable value: Int }",
    )
    .await
    .unwrap();
    ConfigAccess::write_local(
        node.as_ref(),
        "test.seed_hot_mutation_state",
        r#"mutation { create_HotMutationState(input: {key: "shared", value: 0}) { _docID } }"#,
    )
    .await
    .unwrap();
    let seed = node
        .execute(r#"{ HotMutationState(filter: {key: {_eq: "shared"}}) { _docID } }"#)
        .await;
    assert!(!seed.has_errors(), "{:?}", seed.errors);
    let doc_id = seed.data.unwrap()["HotMutationState"][0]["_docID"]
        .as_str()
        .expect("seeded document id")
        .to_owned();
    let collection_id = node
        .get_collection("HotMutationState")
        .unwrap()
        .expect("hot mutation collection")
        .collection_id;

    // Subscribe after the seed and intentionally do not drain until every
    // writer completes. A raw bounded event stream can overflow here; the
    // native document-change subscription must retain the invalidation.
    let mut changes = node.subscribe_document_changes();
    let start = Arc::new(Barrier::new(WORKERS + 1));
    let mut writers = Vec::with_capacity(WORKERS);
    for value in 1..=WORKERS {
        let node = Arc::clone(&node);
        let start = Arc::clone(&start);
        writers.push(tokio::spawn(async move {
            start.wait().await;
            let mutation = format!(
                r#"mutation {{ update_HotMutationState(filter: {{key: {{_eq: "shared"}}}}, input: {{value: {value}}}) {{ _docID }} }}"#
            );
            ConfigAccess::write_local(
                node.as_ref(),
                "test.concurrent_hot_document_update",
                &mutation,
            )
            .await
        }));
    }
    start.wait().await;

    let results = tokio::time::timeout(Duration::from_secs(30), futures::future::join_all(writers))
        .await
        .expect("32 canonical writers complete within the liveness bound");
    for result in results {
        result.expect("writer task joins").expect("writer commits");
    }

    let batch = tokio::time::timeout(Duration::from_secs(2), changes.recv())
        .await
        .expect("document observer is woken")
        .expect("document observer remains open");
    assert!(!batch.resync_required);
    assert_eq!(batch.updates, WORKERS as u64);
    assert_eq!(batch.changes.len(), 1);
    assert_eq!(batch.changes[0].collection_id, collection_id);
    assert_eq!(batch.changes[0].doc_id, doc_id);
    assert!(batch.changes[0].has_local_write);

    tokio::time::timeout(
        Duration::from_secs(2),
        ConfigAccess::write_local(
            node.as_ref(),
            "test.write_after_hot_document_burst",
            r#"mutation { update_HotMutationState(filter: {key: {_eq: "shared"}}, input: {value: 33}) { _docID } }"#,
        ),
    )
    .await
    .expect("a following canonical write is not blocked")
    .expect("the following canonical write commits");
    let durable = node.execute("{ HotMutationState { key value } }").await;
    assert!(!durable.has_errors(), "{:?}", durable.errors);
    assert_eq!(durable.data.unwrap()["HotMutationState"][0]["value"], 33);
    node.shutdown().await;
}

#[tokio::test]
async fn embedded_transaction_carries_the_supplied_did_to_document_acp() {
    const OWNER: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    const READER: &str = "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR";
    const POLICY: &str = r#"
name: Transaction identity test
resources:
  - name: facts
    relations:
      - name: reader
    permissions:
      - name: read
        expr: reader
      - name: update
      - name: delete
"#;

    let node = EmbeddedNode::builder().build().await.unwrap();
    let policy_id = node.add_dac_policy(OWNER, POLICY).await.unwrap();
    node.add_schema(&format!(
        "type IdentityFact @policy(id: \"{policy_id}\", resource: \"facts\") {{ value: String }}"
    ))
    .await
    .unwrap();
    let owner = identity::Did::new(OWNER).unwrap();
    let doc_id =
        ConfigAccess::transact_local(&node, Some(owner), "test.identity_owner_create", |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(
                        r#"mutation { add_IdentityFact(input: {value: "secret"}) { _docID } }"#,
                    )
                    .await?;
                Ok(response["data"]["add_IdentityFact"][0]["_docID"]
                    .as_str()
                    .expect("created document id")
                    .to_owned())
            })
        })
        .await
        .unwrap();
    node.add_dac_actor_relationship(OWNER, "IdentityFact", &doc_id, "reader", READER)
        .await
        .unwrap();

    let visible = ConfigAccess::transact_local(
        &node,
        Some(identity::Did::new(READER).unwrap()),
        "test.identity_reader_query",
        |txn| {
            Box::pin(async move {
                Ok(
                    txn.execute("{ IdentityFact { value } }").await?["data"]["IdentityFact"]
                        .as_array()
                        .map_or(0, Vec::len),
                )
            })
        },
    )
    .await
    .unwrap();
    let anonymous =
        ConfigAccess::transact_local(&node, None, "test.identity_anonymous_query", |txn| {
            Box::pin(async move {
                Ok(
                    txn.execute("{ IdentityFact { value } }").await?["data"]["IdentityFact"]
                        .as_array()
                        .map_or(0, Vec::len),
                )
            })
        })
        .await
        .unwrap();
    assert_eq!(visible, 1);
    assert_eq!(anonymous, 0);
    node.shutdown().await;
}

#[derive(Clone, Copy)]
enum FirstCommit {
    Conflict,
    ServerError,
    Succeed,
}

#[derive(Clone)]
struct FakeTxnState {
    next_id: Arc<AtomicUsize>,
    active_ids: Arc<Mutex<HashSet<String>>>,
    staged_ids: Arc<Mutex<Vec<String>>>,
    committed_ids: Arc<Mutex<Vec<String>>>,
    discarded_ids: Arc<Mutex<Vec<String>>>,
    stage_observed: Arc<Notify>,
    discard_observed: Arc<Notify>,
    first_commit: FirstCommit,
}

impl FakeTxnState {
    fn new(first_commit: FirstCommit) -> Self {
        Self {
            next_id: Arc::new(AtomicUsize::new(0)),
            active_ids: Arc::new(Mutex::new(HashSet::new())),
            staged_ids: Arc::new(Mutex::new(Vec::new())),
            committed_ids: Arc::new(Mutex::new(Vec::new())),
            discarded_ids: Arc::new(Mutex::new(Vec::new())),
            stage_observed: Arc::new(Notify::new()),
            discard_observed: Arc::new(Notify::new()),
            first_commit,
        }
    }
}

async fn begin_transaction(State(state): State<FakeTxnState>) -> Json<Value> {
    let id = state.next_id.fetch_add(1, Ordering::SeqCst);
    lock(&state.active_ids).insert(id.to_string());
    Json(json!({"id": id}))
}

async fn stage_transaction(
    State(state): State<FakeTxnState>,
    headers: HeaderMap,
    Json(_request): Json<Value>,
) -> Json<Value> {
    let id = headers
        .get("x-defradb-tx")
        .and_then(|value| value.to_str().ok())
        .expect("transaction header")
        .to_owned();
    lock(&state.staged_ids).push(id);
    state.stage_observed.notify_one();
    Json(json!({"data": {"update_OwnedWriteFact": [{"_docID": "doc"}]}}))
}

async fn commit_transaction(
    State(state): State<FakeTxnState>,
    Path(id): Path<String>,
) -> (StatusCode, &'static str) {
    if !lock(&state.active_ids).remove(&id) {
        return (StatusCode::NOT_FOUND, "transaction not found");
    }
    lock(&state.committed_ids).push(id.clone());
    if id == "0" {
        match state.first_commit {
            FirstCommit::Conflict => {
                return (
                    StatusCode::CONFLICT,
                    "transaction conflict; please retry the transaction",
                );
            }
            FirstCommit::ServerError => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "storage unavailable");
            }
            FirstCommit::Succeed => {}
        }
    }
    (StatusCode::OK, "")
}

async fn discard_transaction(
    State(state): State<FakeTxnState>,
    Path(id): Path<String>,
) -> StatusCode {
    let active = lock(&state.active_ids).remove(&id);
    lock(&state.discarded_ids).push(id);
    if active {
        state.discard_observed.notify_one();
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn fake_transaction_server(
    first_commit: FirstCommit,
) -> (String, FakeTxnState, tokio::task::JoinHandle<()>) {
    let state = FakeTxnState::new(first_commit);
    let app = Router::new()
        .route("/api/v0/tx", post(begin_transaction))
        .route("/api/v0/graphql", post(stage_transaction))
        .route(
            "/api/v0/tx/{id}",
            post(commit_transaction).delete(discard_transaction),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/api/v0/graphql"), state, server)
}

#[tokio::test]
async fn http_commit_conflict_replays_callback_under_new_transaction_id() {
    let (endpoint, state, server) = fake_transaction_server(FirstCommit::Conflict).await;
    let access = ConfigAccess::Graphql(endpoint);
    let callback_ids = Arc::new(Mutex::new(Vec::new()));
    let callback_ids_for_attempt = Arc::clone(&callback_ids);

    let committed_id = access
        .transact("test.http_replay", move |txn| {
            let callback_ids = Arc::clone(&callback_ids_for_attempt);
            Box::pin(async move {
                let id = transaction_id(txn);
                lock(&callback_ids).push(id.clone());
                txn.execute(
                    r#"mutation { update_OwnedWriteFact(input: {value: "canonical"}) { _docID } }"#,
                )
                .await?;
                Ok(id)
            })
        })
        .await
        .unwrap();

    assert_eq!(committed_id, "1");
    assert_eq!(&*lock(&callback_ids), &["0", "1"]);
    assert_eq!(&*lock(&state.staged_ids), &["0", "1"]);
    assert_eq!(&*lock(&state.committed_ids), &["0", "1"]);
    assert!(lock(&state.discarded_ids).is_empty());
    assert!(lock(&state.active_ids).is_empty());
    assert_eq!(state.next_id.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn non_conflict_commit_failure_does_not_replay_callback() {
    let (endpoint, state, server) = fake_transaction_server(FirstCommit::ServerError).await;
    let access = ConfigAccess::Graphql(endpoint);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_for_attempt = Arc::clone(&callbacks);

    let error = access
        .transact("test.http_no_replay", move |txn| {
            callbacks_for_attempt.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                txn.execute(
                    r#"mutation { update_OwnedWriteFact(input: {value: "canonical"}) { _docID } }"#,
                )
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("HTTP 500"), "{error:#}");
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    assert_eq!(&*lock(&state.committed_ids), &["0"]);
    assert!(lock(&state.discarded_ids).is_empty());
    assert!(lock(&state.active_ids).is_empty());
    assert_eq!(state.next_id.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn idempotent_transaction_replays_non_conflict_commit_failure() {
    let (endpoint, state, server) = fake_transaction_server(FirstCommit::ServerError).await;
    let access = ConfigAccess::Graphql(endpoint);
    let callback_ids = Arc::new(Mutex::new(Vec::new()));
    let callback_ids_for_attempt = Arc::clone(&callback_ids);

    let committed_id = access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "test.http_idempotent_commit_replay",
            move |txn| {
                let callback_ids = Arc::clone(&callback_ids_for_attempt);
                Box::pin(async move {
                    let id = transaction_id(txn);
                    lock(&callback_ids).push(id.clone());
                    txn.execute(
                        r#"mutation { update_OwnedWriteFact(input: {value: "canonical"}) { _docID } }"#,
                    )
                    .await?;
                    Ok(id)
                })
            },
        )
        .await
        .unwrap();

    assert_eq!(committed_id, "1");
    assert_eq!(&*lock(&callback_ids), &["0", "1"]);
    assert_eq!(&*lock(&state.staged_ids), &["0", "1"]);
    assert_eq!(&*lock(&state.committed_ids), &["0", "1"]);
    assert!(lock(&state.discarded_ids).is_empty());
    assert!(lock(&state.active_ids).is_empty());
    server.abort();
}

#[tokio::test]
async fn idempotent_transaction_replays_complete_callback_after_callback_error() {
    let (endpoint, state, server) = fake_transaction_server(FirstCommit::Succeed).await;
    let access = ConfigAccess::Graphql(endpoint);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_for_attempt = Arc::clone(&callbacks);

    let committed_id = access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "test.http_idempotent_callback_replay",
            move |txn| {
                let attempt = callbacks_for_attempt.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if attempt == 0 {
                        return Err(super::retry::transaction_storage_failure(anyhow::anyhow!(
                            "injected storage failure"
                        )));
                    }
                    let id = transaction_id(txn);
                    txn.execute(
                        r#"mutation { update_OwnedWriteFact(input: {value: "canonical"}) { _docID } }"#,
                    )
                    .await?;
                    Ok(id)
                })
            },
        )
        .await
        .unwrap();

    assert_eq!(committed_id, "1");
    assert_eq!(callbacks.load(Ordering::SeqCst), 2);
    assert_eq!(&*lock(&state.discarded_ids), &["0"]);
    assert_eq!(&*lock(&state.committed_ids), &["1"]);
    assert!(lock(&state.active_ids).is_empty());
    server.abort();
}

#[tokio::test]
async fn idempotent_transaction_does_not_replay_domain_rejection() {
    let (endpoint, state, server) = fake_transaction_server(FirstCommit::Succeed).await;
    let access = ConfigAccess::Graphql(endpoint);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_for_attempt = Arc::clone(&callbacks);

    let error = access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "test.http_idempotent_domain_rejection",
            move |_| {
                callbacks_for_attempt.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Err::<(), _>(anyhow::anyhow!("stale domain state")) })
            },
        )
        .await
        .expect_err("domain rejection is terminal");

    assert!(error.to_string().contains("stale domain state"));
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    assert_eq!(&*lock(&state.discarded_ids), &["0"]);
    assert!(lock(&state.committed_ids).is_empty());
    assert!(lock(&state.active_ids).is_empty());
    server.abort();
}

#[tokio::test]
async fn observing_transaction_reports_conflict_without_replay() {
    let (endpoint, state, server) = fake_transaction_server(FirstCommit::Conflict).await;
    let access = ConfigAccess::Graphql(endpoint);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_for_attempt = Arc::clone(&callbacks);

    let outcome = access
        .transact_observing_conflict("test.http_observe_conflict", move |txn| {
            callbacks_for_attempt.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                txn.execute(
                    r#"mutation { update_OwnedWriteFact(input: {value: "canonical"}) { _docID } }"#,
                )
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

    assert!(matches!(outcome, TransactionOutcome::ConflictObserved));
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    assert_eq!(&*lock(&state.committed_ids), &["0"]);
    assert!(lock(&state.discarded_ids).is_empty());
    assert!(lock(&state.active_ids).is_empty());
    server.abort();
}

#[tokio::test]
async fn cancelling_callback_schedules_http_transaction_discard() {
    let (endpoint, state, server) = fake_transaction_server(FirstCommit::Succeed).await;
    let stage_observed = Arc::clone(&state.stage_observed);
    let access = ConfigAccess::Graphql(endpoint);
    let mut transaction = Box::pin(access.transact("test.http_cancel", |txn| {
        Box::pin(async move {
            txn.execute(
                r#"mutation { update_OwnedWriteFact(input: {value: "pending"}) { _docID } }"#,
            )
            .await?;
            std::future::pending::<Result<()>>().await
        })
    }));

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            _ = stage_observed.notified() => {}
            result = &mut transaction => panic!("transaction finished before cancellation: {result:?}"),
        }
    })
        .await
        .expect("callback should stage its mutation");
    drop(transaction);
    tokio::time::timeout(Duration::from_secs(2), state.discard_observed.notified())
        .await
        .expect("cancellation should discard the open transaction");

    assert_eq!(&*lock(&state.staged_ids), &["0"]);
    assert!(lock(&state.committed_ids).is_empty());
    assert_eq!(&*lock(&state.discarded_ids), &["0"]);
    assert!(lock(&state.active_ids).is_empty());
    server.abort();
}
