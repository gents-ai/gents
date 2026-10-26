use gents_protocol::error_chain::ErrorChain;
use rapidhash::{HashSetExt, RapidHashSet};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type Result<T = (), E = TestError> = std::result::Result<T, E>;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{Barrier, Notify};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::{
    ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry, TransactionOutcome, TxnBackend,
};
use crate::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET;
use crate::identity::NodeIdentity;

fn transaction_id(txn: &ConfigApplyTxn<'_>) -> String {
    match &txn.backend {
        TxnBackend::Http { id, .. } => id.clone(),
        TxnBackend::Embedded { handle, .. } => handle.as_str().to_owned(),
    }
}

#[tokio::test]
async fn http_transaction_cursors_keep_all_pages() {
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("cursor.key");
    let identity =
        tokio::task::spawn_blocking(move || crate::KeyIdentity::load_or_create(key, None))
            .await
            .unwrap()
            .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap())
            .build()
            .await
            .unwrap(),
    );
    ConfigAccess::local(node.clone())
        .add_schema("type CursorTransportProbe { value: Int }")
        .await
        .unwrap();
    let rows = (0..66)
        .map(|value| format!("{{value:{value}}}"))
        .collect::<Vec<_>>()
        .join(",");
    ConfigAccess::write_local(
        &node,
        "test.cursor_transport.seed",
        &format!("mutation {{ create_CursorTransportProbe(input:[{rows}]) {{ _docID }} }}",),
    )
    .await
    .unwrap();
    let access = ConfigAccess::Graphql(super::GraphqlEndpoint::as_principal(
        endpoint,
        identity.did(),
    ));
    let rows = access
        .transact_readonly("test.cursor_transport.read", |txn| {
            Box::pin(async move {
                txn.cursor_rows("CursorTransportProbe", "_docID value", "")
                    .await
            })
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 66);
    let mut values: Vec<_> = rows
        .iter()
        .map(|row| row["value"].as_i64().unwrap())
        .collect();
    values.sort();
    assert_eq!(values, (0..66).collect::<Vec<_>>());
    node.shutdown().await;
}

#[tokio::test]
async fn config_access_readonly_does_not_wait_for_the_embedded_mutation_gate() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ConfigAccess::local(Arc::clone(&node))
        .add_schema("type ReadGateProbe { value: String }")
        .await
        .unwrap();
    ConfigAccess::write_local(
        &node,
        "test.read_gate_seed",
        r#"mutation { create_ReadGateProbe(input: {value: "ready"}) {_docID} }"#,
    )
    .await
    .unwrap();

    let gate = super::mutation_write_gate(&node);
    let held = fill_write_gate(&gate, "test.hold_mutation_gate_for_read").await;
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        ConfigAccess::local(Arc::clone(&node))
            .transact_readonly("test.config_access_readonly", |txn| {
                Box::pin(async move { txn.execute("{ ReadGateProbe { value } }").await })
            }),
    )
    .await
    .expect("read-only transaction waited on the mutation gate")
    .unwrap();
    assert_eq!(response["data"]["ReadGateProbe"][0]["value"], "ready");
    drop(held);
    node.shutdown().await;
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn fill_write_gate(
    gate: &Arc<super::MutationWriteGate>,
    first_operation: &'static str,
) -> Vec<super::MutationWriteGuard> {
    let mut guards = Vec::with_capacity(super::MUTATION_WRITE_GATE_CAPACITY);
    for index in 0..super::MUTATION_WRITE_GATE_CAPACITY {
        let operation = if index == 0 {
            first_operation
        } else {
            "test.additional_gate_holder"
        };
        guards.push(
            gate.acquire(
                crate::config_client::write_telemetry::WriteOperation::new(operation).unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    guards
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
        fields.0.insert(
            "level".to_owned(),
            event.metadata().level().as_str().to_owned(),
        );
        lock(&self.events).push(fields.0);
    }
}

#[tokio::test]
async fn clean_known_noop_commit_is_debug_but_mutation_is_info() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type WriteTelemetryLevelProbe { value: String }")
        .await
        .unwrap();
    let telemetry = EventCapture::default();
    let events = Arc::clone(&telemetry.events);
    let subscriber = tracing::Dispatch::new(Registry::default().with(telemetry));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);

    ConfigAccess::transact_local_write(&node, None, "test.clean_noop", |_| {
        Box::pin(async { Ok::<_, TestError>(()) })
    })
    .await
    .unwrap();
    ConfigAccess::transact_local_write(&node, None, "test.non_noop", |txn| {
        Box::pin(async move {
            txn.execute(r#"mutation { create_WriteTelemetryLevelProbe(input: { value: "changed" }) { _docID } }"#)
                .await?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();

    let events = std::mem::take(&mut *lock(&events));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].get("level").map(String::as_str), Some("DEBUG"));
    assert_eq!(
        events[0].get("outcome").map(String::as_str),
        Some("committed")
    );
    assert_eq!(
        events[0].get("affected_documents").map(String::as_str),
        Some("0")
    );
    assert_eq!(events[1].get("level").map(String::as_str), Some("INFO"));
    assert_eq!(
        events[1].get("affected_documents").map(String::as_str),
        Some("1")
    );
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn queued_auto_commit_cancellation_is_not_reported_as_native_pending() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let gate = super::mutation_write_gate(&node);
    let held = fill_write_gate(&gate, "test.hold_auto_commit_gate").await;
    let telemetry = EventCapture::default();
    let events = Arc::clone(&telemetry.events);
    let subscriber = tracing::Dispatch::new(Registry::default().with(telemetry));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        ConfigAccess::write_local(
            &node,
            "test.queued_auto_commit_cancelled",
            r#"mutation { create_WriteTelemetryLevelProbe(input: { value: "queued" }) { _docID } }"#,
        ),
    )
    .await;
    assert!(
        result.is_err(),
        "the write should remain queued behind the held gate"
    );
    drop(held);

    let events = std::mem::take(&mut *lock(&events));
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].get("outcome").map(String::as_str),
        Some("cancelled")
    );
    assert_eq!(events[0].get("settled").map(String::as_str), Some("true"));
    assert_eq!(
        events[0].get("affected_documents").map(String::as_str),
        Some("0")
    );
    node.shutdown().await;
}

#[tokio::test]
async fn successful_mutation_pause_is_one_shot_and_task_scoped() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type MutationPauseProbe { value: String }")
        .await
        .unwrap();
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicUsize::new(0));
    let completed_for_callback = Arc::clone(&completed);
    let mut paused = Box::pin(ConfigApplyTxn::with_successful_mutation_pause_at(
        2,
        Arc::clone(&reached),
        Arc::clone(&release),
        ConfigAccess::transact_local_write(&node, None, "test.mutation_pause", move |txn| {
            let completed = Arc::clone(&completed_for_callback);
            Box::pin(async move {
                for value in ["first", "second", "third"] {
                    txn.execute(&format!(
                        "mutation {{ create_MutationPauseProbe(input: {{ value: \"{}\" }}) {{ _docID }} }}",
                        crate::graphql::escape_graphql_string(value),
                    ))
                    .await?;
                    completed.fetch_add(1, Ordering::Relaxed);
                }
                Ok::<_, TestError>(())
            })
        }),
    ));

    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            () = reached.notified() => {}
            result = &mut paused => panic!("transaction finished before selected pause: {result:?}"),
        }
    })
    .await
    .expect("selected mutation did not reach the pause");
    assert_eq!(
        completed.load(Ordering::Relaxed),
        1,
        "the second successful mutation must pause before its callback resumes"
    );
    let capacity_fillers = {
        let gate = super::mutation_write_gate(&node);
        let mut fillers = Vec::new();
        for _ in 1..super::MUTATION_WRITE_GATE_CAPACITY {
            fillers.push(
                gate.acquire(super::WriteOperation::new("test.mutation_pause_filler").unwrap())
                    .await
                    .unwrap(),
            );
        }
        fillers
    };
    let queued = Arc::new(Notify::new());
    let acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut contender = Box::pin(ConfigApplyTxn::with_write_gate_observation(
        Arc::clone(&queued),
        Arc::clone(&acquired),
        ConfigAccess::transact_local_write(&node, None, "test.waiting_for_mutation_gate", |txn| {
            Box::pin(async move {
                txn.execute(r#"mutation { create_MutationPauseProbe(input: { value: "contender" }) { _docID } }"#)
                    .await?;
                Ok::<_, TestError>(())
            })
        }),
    ));
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            () = queued.notified() => {}
            result = &mut contender => panic!("contender acquired a held write gate: {result:?}"),
        }
    })
    .await
    .expect("contender did not poll the write-gate lock Pending");
    assert!(
        !acquired.load(Ordering::Acquire),
        "contender acquired the gate before release"
    );
    release.notify_one();
    let (result, fired) = tokio::time::timeout(Duration::from_secs(30), paused)
        .await
        .expect("paused transaction did not resume after release");
    result.expect("paused transaction commits after release");
    assert!(fired, "selected pause never fired");
    assert_eq!(completed.load(Ordering::Relaxed), 3);
    tokio::time::timeout(Duration::from_secs(30), contender)
        .await
        .expect("queued contender did not resume")
        .expect("queued contender committed after gate release");
    assert!(
        acquired.load(Ordering::Acquire),
        "actual write-gate lock did not report Ready after release"
    );
    drop(capacity_fillers);

    let dormant_queued = Arc::new(Notify::new());
    let dormant_acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    ConfigApplyTxn::with_write_gate_observation(
        dormant_queued,
        Arc::clone(&dormant_acquired),
        async {},
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        ConfigAccess::transact_local_write(&node, None, "test.after_mutation_pause", |txn| {
            Box::pin(async move {
                txn.execute(r#"mutation { create_MutationPauseProbe(input: { value: "outside" }) { _docID } }"#)
                    .await?;
                Ok::<_, TestError>(())
            })
        }),
    )
    .await
    .expect("an out-of-scope write must not wait on the old release")
    .expect("out-of-scope write commits");
    assert!(
        !dormant_acquired.load(Ordering::Acquire),
        "completed observer scope leaked into a later write"
    );

    let (failure, count) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        ConfigAccess::transact_local_write(&node, None, "test.existing_mutation_failure", |txn| {
            Box::pin(async move {
                txn.execute(r#"mutation { create_MutationPauseProbe(input: { value: "fault" }) { _docID } }"#)
                    .await?;
                Ok::<_, TestError>(())
            })
        }),
    )
    .await;
    assert_eq!(count, 1);
    let error = failure.expect_err("existing mutation fault still fires");
    assert!(
        format!("{}", ErrorChain(error.as_ref()))
            .contains("injected failure after successful transaction mutation 1"),
        "{}",
        ErrorChain(error.as_ref())
    );
    node.shutdown().await;
}

#[tokio::test]
async fn armed_mutation_pause_does_not_consume_an_out_of_scope_write() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type ArmedMutationPauseProbe { value: String }")
        .await
        .unwrap();
    let armed = Arc::new(Notify::new());
    let trigger = Arc::new(Notify::new());
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let armed_for_scope = Arc::clone(&armed);
    let trigger_for_scope = Arc::clone(&trigger);
    let mut scoped = Box::pin(ConfigApplyTxn::with_successful_mutation_pause_at(
        1,
        Arc::clone(&reached),
        Arc::clone(&release),
        async {
            armed_for_scope.notify_one();
            trigger_for_scope.notified().await;
            ConfigAccess::transact_local_write(&node, None, "test.armed_mutation_pause", |txn| {
                Box::pin(async move {
                    txn.execute(r#"mutation { create_ArmedMutationPauseProbe(input: { value: "scoped" }) { _docID } }"#)
                        .await?;
                    Ok::<_, TestError>(())
                })
            })
            .await
        },
    ));

    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            () = armed.notified() => {}
            result = &mut scoped => panic!("armed scope completed before its trigger: {result:?}"),
        }
    })
    .await
    .expect("pause scope was not armed");
    tokio::time::timeout(
        Duration::from_secs(30),
        ConfigAccess::transact_local_write(&node, None, "test.outside_armed_mutation_pause", |txn| {
            Box::pin(async move {
                txn.execute(r#"mutation { create_ArmedMutationPauseProbe(input: { value: "outside" }) { _docID } }"#)
                    .await?;
                Ok::<_, TestError>(())
            })
        }),
    )
    .await
    .expect("out-of-scope write was intercepted by the armed pause")
    .expect("out-of-scope write committed");

    trigger.notify_one();
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            () = reached.notified() => {}
            result = &mut scoped => panic!("scoped mutation bypassed its armed pause: {result:?}"),
        }
    })
    .await
    .expect("scoped first mutation did not reach its pause");
    release.notify_one();
    let (result, fired) = tokio::time::timeout(Duration::from_secs(30), scoped)
        .await
        .expect("scoped transaction did not resume after release");
    result.expect("scoped transaction committed after release");
    assert!(
        fired,
        "outside write consumed the armed first-mutation pause"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn cancellation_after_embedded_begin_abandons_before_releasing_the_gate() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let node_ref = Arc::clone(&node);
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
            let node = Arc::clone(&node_ref);
            let gate = Arc::clone(&gate);
            let registered = Arc::clone(&registered_for_begin);
            let registered_notify = Arc::clone(&registered_notify_for_begin);
            let release = Arc::clone(&release_for_begin);
            Box::pin(async move {
                let write_guard = gate.acquire(operation).await?;
                let native_operations = Arc::new(super::NativeOperationTracker::default());
                let (rollback_on_drop, handle) = super::begin_embedded_owned(
                    node.clone(),
                    Arc::clone(&gate),
                    false,
                    operation,
                    Some(write_guard),
                    Arc::clone(&native_operations),
                    rollback_scheduled,
                    move |handle| async move {
                        *lock(&registered) = Some(handle);
                        registered_notify.notify_one();
                        release.notified().await;
                    },
                )
                .await?;
                Ok(ConfigApplyTxn {
                    backend: TxnBackend::Embedded {
                        node,
                        handle,
                        identity: None,
                        native_operations,
                    },
                    rollback_on_drop: Some(rollback_on_drop),
                    affected_documents: std::sync::atomic::AtomicU64::new(0),
                    operation: Some(operation.as_str()),
                    contends_with_request_siblings: std::sync::atomic::AtomicBool::new(false),
                })
            })
        },
        |_| Box::pin(async { Ok::<_, TestError>(()) }),
    ));

    tokio::select! {
        () = registered_notify.notified() => {}
        result = &mut transaction => panic!("transaction finished before cancellation: {result:?}"),
    }
    let handle = lock(&registered)
        .clone()
        .expect("registered transaction handle was observed");
    drop(transaction);
    assert_eq!(
        gate_for_assert.permits.available_permits(),
        super::MUTATION_WRITE_GATE_CAPACITY
    );
    assert_native_transaction_abandoned(&node, &handle).await;

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
        .rollback_transaction(&handle)
        .await
        .expect_err("cancelled begin already removed the registered transaction");
    assert!(matches!(error, query::TransactionError::NotFound(_)));

    let events = std::mem::take(&mut *lock(&events));
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

#[tokio::test]
async fn abandoned_signed_statement_abandons_registry_but_holds_gate_until_drained() {
    let dir = tempfile::tempdir().unwrap();
    let identity = crate::identity::KeyIdentity::load_or_create(dir.path().join("node.key"), None)
        .expect("signing identity");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    node.add_schema("type NativeCompletionProbe { value: String }")
        .await
        .unwrap();

    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let registered = Arc::new(Mutex::new(None));
    let registered_for_callback = Arc::clone(&registered);
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_callback = Arc::clone(&attempts);
    let node_for_transaction = Arc::clone(&node);
    let reached_for_task = Arc::clone(&reached);
    let release_for_task = Arc::clone(&release);
    let transaction = tokio::spawn(async move {
        ConfigApplyTxn::with_native_operation_pause(
            reached_for_task,
            release_for_task,
            ConfigAccess::transact_local_idempotent(
                &node_for_transaction,
                None,
                IdempotentTransactionRetry::FiveAttempts,
                "test.native_completion_abandonment",
                move |txn| {
                    *lock(&registered_for_callback) = Some(transaction_handle(txn));
                    attempts_for_callback.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        txn.execute(
                            r#"mutation { create_NativeCompletionProbe(input: { value: "abandoned" }) { _docID } }"#,
                        )
                        .await?;
                        Ok::<_, TestError>(())
                    })
                },
            ),
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .expect("signed native-operation owner reached its test pause");
    tokio::time::pause();
    tokio::time::advance(super::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT + Duration::from_secs(1))
        .await;
    let error = transaction
        .await
        .expect("transaction caller task")
        .expect_err("abandoned statement must fail its transaction");
    assert!(
        crate::config_client::is_transaction_step_unavailable(&error),
        "abandoned native work remains a storage-unavailable result: {}",
        ErrorChain(error.as_ref())
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "poisoned attempt replayed"
    );
    tokio::time::resume();
    let handle = lock(&registered)
        .clone()
        .expect("native operation has a transaction");
    assert_native_transaction_abandoned(&node, &handle).await;
    let gate = super::mutation_write_gate(&node);
    assert!(
        gate.owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .any(|owner| owner.operation.as_str() == "test.native_completion_abandonment"),
        "the native transaction lost the mutation gate before its statement returned"
    );
    let mut other_owners = Vec::with_capacity(super::MUTATION_WRITE_GATE_CAPACITY - 1);
    for _ in 1..super::MUTATION_WRITE_GATE_CAPACITY {
        other_owners.push(
            gate.acquire(
                crate::config_client::write_telemetry::WriteOperation::new(
                    "test.native_completion_gate_filler",
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    assert_eq!(gate.permits.available_permits(), 0);

    let node_for_writer = Arc::clone(&node);
    let writer = tokio::spawn(async move {
        ConfigAccess::write_local(
            &node_for_writer,
            "test.native_completion_after_abandonment",
            r#"mutation { create_NativeCompletionProbe(input: { value: "next" }) { _docID } }"#,
        )
        .await
    });
    wait_for_gate_waiters(&gate, 1).await;
    assert!(
        !writer.is_finished(),
        "a later writer passed the gate while native work was still owned"
    );

    release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), writer)
        .await
        .expect("later writer remained blocked after native operation returned")
        .expect("writer task")
        .expect("later write succeeds after native work drains");
    drop(other_owners);

    let result = ConfigAccess::local(Arc::clone(&node))
        .execute("{ NativeCompletionProbe { value } }")
        .await
        .unwrap();
    let values: Vec<&str> = result["data"]["NativeCompletionProbe"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["value"].as_str())
        .collect();
    assert_eq!(values, ["next"], "abandoned transaction leaked a write");
    node.shutdown().await;
}

#[tokio::test]
async fn swallowed_abandoned_statement_cannot_start_another_statement_or_commit() {
    let dir = tempfile::tempdir().unwrap();
    let identity = crate::identity::KeyIdentity::load_or_create(dir.path().join("node.key"), None)
        .expect("signing identity");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    node.add_schema("type NativeCompletionSwallowProbe { value: String }")
        .await
        .unwrap();

    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let attempts = Arc::new(AtomicUsize::new(0));
    let reached_for_task = Arc::clone(&reached);
    let release_for_task = Arc::clone(&release);
    let attempts_for_task = Arc::clone(&attempts);
    let node_for_transaction = Arc::clone(&node);
    let transaction = tokio::spawn(async move {
        ConfigApplyTxn::with_native_operation_pause(
            reached_for_task,
            release_for_task,
            ConfigAccess::transact_local_idempotent(
                &node_for_transaction,
                None,
                IdempotentTransactionRetry::FiveAttempts,
                "test.native_completion_swallowed_error",
                move |txn| {
                    attempts_for_task.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        let first = txn
                            .execute(
                                r#"mutation { create_NativeCompletionSwallowProbe(input: { value: "first" }) { _docID } }"#,
                            )
                            .await;
                        assert!(first.is_err(), "the timed-out statement must fail its caller");
                        let second = txn
                            .execute("{ NativeCompletionSwallowProbe { value } }")
                            .await;
                        assert!(
                            second
                                .as_ref()
                                .is_err_and(crate::config_client::is_transaction_step_unavailable),
                            "a poisoned transaction admitted a later statement: {second:?}"
                        );
                        Ok::<_, TestError>(())
                    })
                },
            ),
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .expect("signed native operation reached its pause");
    tokio::time::pause();
    tokio::time::advance(super::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT + Duration::from_secs(1))
        .await;
    let error = transaction
        .await
        .expect("transaction task")
        .expect_err("callback cannot commit after swallowing an abandoned operation");
    assert!(crate::config_client::is_transaction_step_unavailable(
        &error
    ));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "poisoned callback replayed"
    );
    tokio::time::resume();
    release.notify_one();

    ConfigAccess::write_local(
        &node,
        "test.native_completion_after_swallow",
        r#"mutation { create_NativeCompletionSwallowProbe(input: { value: "after" }) { _docID } }"#,
    )
    .await
    .expect("deferred rollback releases the gate after native completion");
    let read = node
        .execute("{ NativeCompletionSwallowProbe { value } }")
        .await;
    assert!(!read.has_errors(), "{:?}", read.errors);
    assert_eq!(
        read.data.unwrap()["NativeCompletionSwallowProbe"],
        json!([{"value":"after"}]),
        "the swallowed abandoned transaction committed"
    );
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn cancellation_before_begin_reports_no_scheduled_rollback() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let gate = super::mutation_write_gate(&node);
    let held = fill_write_gate(&gate, "test.cancel_before_begin").await;
    let telemetry = EventCapture::default();
    let events = Arc::clone(&telemetry.events);
    let subscriber = tracing::Dispatch::new(Registry::default().with(telemetry));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    let mut transaction = Box::pin(ConfigAccess::transact_local_write(
        &node,
        None,
        "test.cancel_before_begin",
        |_| Box::pin(async { Ok::<_, TestError>(()) }),
    ));

    tokio::time::timeout(Duration::from_millis(10), &mut transaction)
        .await
        .expect_err("transaction remains blocked before begin");
    drop(transaction);
    drop(held);

    let events = std::mem::take(&mut *lock(&events));
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

#[tokio::test]
async fn stalled_callback_releases_the_embedded_write_gate() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type WriteAfterStall { value: String }")
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let entered_for_callback = Arc::clone(&entered);
    let mut stalled = Box::pin(ConfigAccess::transact_local_write(
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
    tokio::time::pause();
    tokio::time::advance(super::EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT).await;
    let error = stalled.await.expect_err("stalled transaction times out");
    tokio::time::resume();
    assert!(
        error.to_string().contains("callback timed out"),
        "{}",
        ErrorChain(error.as_ref())
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
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type NestedWriteProbe { value: String }")
        .await
        .unwrap();

    let error = ConfigAccess::transact_local_write(&node, None, "test.outer_transaction", |_| {
        Box::pin(async {
            ConfigAccess::write_local(
                &node,
                "test.inner_write",
                r#"mutation { create_NestedWriteProbe(input: {value: "nested"}) { _docID } }"#,
            )
            .await?;
            Ok::<_, crate::config_client::ConfigClientError>(())
        })
    })
    .await
    .expect_err("a canonical write cannot recursively acquire its transaction's gate");
    assert!(matches!(
        error,
        crate::config_client::ConfigClientError::Reentrant(_)
    ));
    let diagnostic = format!("{}", ErrorChain(&error));
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

#[tokio::test]
async fn nested_idempotent_write_does_not_replay_the_ownership_violation() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let outer_attempts = Arc::new(AtomicUsize::new(0));
    let inner_attempts = Arc::new(AtomicUsize::new(0));
    let outer_attempts_for_callback = Arc::clone(&outer_attempts);
    let inner_attempts_for_callback = Arc::clone(&inner_attempts);
    let node_for_callback = Arc::clone(&node);
    let telemetry = EventCapture::default();
    let events = Arc::clone(&telemetry.events);
    let subscriber = tracing::Dispatch::new(Registry::default().with(telemetry));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);

    let error =
        ConfigAccess::transact_local_write(&node, None, "test.outer_transaction", move |_| {
            outer_attempts_for_callback.fetch_add(1, Ordering::SeqCst);
            let inner_attempts = Arc::clone(&inner_attempts_for_callback);
            let node = Arc::clone(&node_for_callback);
            Box::pin(async move {
                ConfigAccess::transact_local_idempotent(
                    &node,
                    None,
                    super::IdempotentTransactionRetry::Standard,
                    "test.inner_idempotent_write",
                    move |_| {
                        inner_attempts.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Ok::<_, crate::config_client::ConfigClientError>(()) })
                    },
                )
                .await
            })
        })
        .await
        .expect_err("a nested idempotent write remains an ownership violation");

    let diagnostic = format!("{}", ErrorChain(&error));
    assert!(
        diagnostic.contains("test.inner_idempotent_write"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("test.outer_transaction"),
        "{diagnostic}"
    );
    assert_eq!(outer_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        inner_attempts.load(Ordering::SeqCst),
        0,
        "ownership rejection must happen before an inner attempt or retry backoff"
    );
    let events = std::mem::take(&mut *lock(&events));
    let inner_events: Vec<_> = events
        .iter()
        .filter(|event| {
            event.get("operation").map(String::as_str) == Some("test.inner_idempotent_write")
        })
        .collect();
    assert_eq!(
        inner_events.len(),
        1,
        "nested ownership failure was retried"
    );
    assert_eq!(
        inner_events[0].get("outcome").map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        inner_events[0].get("attempt").map(String::as_str),
        Some("1")
    );
    drop(events);
    node.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn write_gate_timeout_names_waiter_and_current_owner() {
    let gate = Arc::new(super::MutationWriteGate::new());
    let owners = fill_write_gate(&gate, "test.current_owner").await;
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
    let diagnostic = format!("{}", ErrorChain(&error));
    assert!(
        diagnostic.contains("test.waiting_operation"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("test.current_owner"), "{diagnostic}");
    assert!(
        super::retry::is_transaction_storage_failure(&error),
        "gate wait timeouts use the standard storage-failure retry classification"
    );
    assert!(gate
        .pending_acquires
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_empty());
    drop(owners);
}

#[tokio::test]
async fn stream_owned_gate_waiter_is_copolled_with_flush() {
    use std::future::Future;
    use std::task::Context;

    use futures::StreamExt;

    let gate = Arc::new(super::MutationWriteGate::new());
    let owner = gate
        .acquire(
            crate::config_client::write_telemetry::WriteOperation::new(
                "test.stream_gate_initial_owner",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let mut additional_owners = Vec::new();
    for _ in 1..super::MUTATION_WRITE_GATE_CAPACITY {
        additional_owners.push(
            gate.acquire(
                crate::config_client::write_telemetry::WriteOperation::new(
                    "test.stream_gate_additional_owner",
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    tokio::task::yield_now().await;

    let stream_gate = Arc::clone(&gate);
    let mut stream = Box::pin(async_stream::stream! {
        let guard = stream_gate
            .acquire(
                crate::config_client::write_telemetry::WriteOperation::new(
                    "test.stream_gate_capture",
                )
                .unwrap(),
            )
            .await
            .expect("stream capture acquires the gate");
        drop(guard);
        yield ();
    });
    let mut next = Box::pin(stream.next());
    let waker = futures::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    assert!(Future::poll(next.as_mut(), &mut context).is_pending());
    assert_eq!(
        gate.pending_acquires
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        1,
        "the actual gate acquisition is registered after its first pending poll"
    );
    let pending = gate
        .oldest_pending_acquire()
        .expect("pending gate acquisition");
    assert_eq!(pending.count, 1);
    assert_eq!(pending.operation.as_str(), "test.stream_gate_capture");
    assert!(pending.last_poll_age < Duration::from_secs(1));
    drop(next);
    assert_eq!(
        gate.pending_acquires
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        1,
        "dropping only StreamExt::next must retain the generator's queued acquire"
    );
    drop(owner);

    assert_eq!(
        gate.owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        super::MUTATION_WRITE_GATE_CAPACITY - 1,
        "the queued stream future has a permit reservation before it creates an owner"
    );
    assert_eq!(gate.permits.available_permits(), 0);

    let flush_gate = Arc::clone(&gate);
    let flush = flush_gate.acquire(
        crate::config_client::write_telemetry::WriteOperation::new("test.stream_gate_flush")
            .unwrap(),
    );
    let (flush_result, item) = tokio::time::timeout(
        Duration::from_secs(1),
        gents_loop::stream_processor::flush_while_polling(flush, stream.next()),
    )
    .await
    .expect("co-polling must release the retained stream waiter");

    assert_eq!(item, Some(Some(())));
    let flush_guard = flush_result.expect("flush acquires the gate");
    assert!(gate
        .pending_acquires
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_empty());
    assert_eq!(
        gate.owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        super::MUTATION_WRITE_GATE_CAPACITY
    );
    drop(flush_guard);
    assert_eq!(
        gate.owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        super::MUTATION_WRITE_GATE_CAPACITY - 1
    );
    drop(additional_owners);
}

#[tokio::test]
async fn failed_rollback_abandons_before_transaction_retry_backoff() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let node_for_begin = &node;
    let registered = Arc::new(Mutex::new(None));
    let registered_for_callback = Arc::clone(&registered);
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_transaction = Arc::clone(&attempts);
    let cleanup_scheduled = Arc::new(AtomicBool::new(false));
    let retry_boundary = Arc::new(Notify::new());
    let cancellation_scheduled = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));
    let cancellation_scheduled_for_begin = Arc::clone(&cancellation_scheduled);

    let mut transaction = Box::pin(super::ConfigApplyTxn::with_rollback_failure_retry_boundary(
        Arc::clone(&retry_boundary),
        Arc::clone(&cleanup_scheduled),
        super::transact_owned(
            "test.rollback_before_retry_backoff",
            crate::config_client::write_telemetry::WriteBackend::Embedded,
            super::TransactionMode::Idempotent(IdempotentTransactionRetry::Standard),
            move |operation, cancellation_rollback_scheduled| {
                let node = node_for_begin;
                let attempts = Arc::clone(&attempts_for_transaction);
                let retained = Arc::clone(&cancellation_scheduled_for_begin);
                Box::pin(async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    *lock(&retained) = Some(Arc::clone(&cancellation_rollback_scheduled));
                    ConfigApplyTxn::begin_local_owned(
                        node,
                        None,
                        operation,
                        cancellation_rollback_scheduled,
                        super::EmbeddedTransactionPurpose::Mutation,
                    )
                    .await
                })
            },
            move |txn| {
                *lock(&registered_for_callback) = Some(transaction_handle(txn));
                Box::pin(async {
                    Err::<(), _>(super::retry::transaction_storage_failure(
                        crate::config_client::ConfigClientError::InjectedReadStorageFailure,
                    ))
                })
            },
        ),
    ));

    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            () = retry_boundary.notified() => {}
            result = &mut transaction => panic!("transaction completed before retry backoff: {result:?}"),
        }
    })
    .await
    .expect("transaction did not reach its retry boundary");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(
        !lock(&cancellation_scheduled)
            .as_ref()
            .is_some_and(|scheduled| scheduled.load(Ordering::Acquire)),
        "native abandonment does not schedule rollback"
    );
    assert!(
        !cleanup_scheduled.load(Ordering::Acquire),
        "failed attempt is abandoned synchronously, not by a detached rollback"
    );
    let handle = lock(&registered)
        .clone()
        .expect("callback observed the transaction");
    assert_native_transaction_abandoned(&node, &handle).await;
    assert_eq!(
        super::mutation_write_gate(&node)
            .permits
            .available_permits(),
        super::MUTATION_WRITE_GATE_CAPACITY
    );
    drop(transaction);
    node.shutdown().await;
}

#[tokio::test]
async fn dropping_stream_clears_its_pending_gate_acquire() {
    use std::future::Future;
    use std::task::Context;

    use futures::StreamExt;

    let gate = Arc::new(super::MutationWriteGate::new());
    let owners = fill_write_gate(&gate, "test.stream_drop_owner").await;
    let stream_gate = Arc::clone(&gate);
    let mut stream = Box::pin(async_stream::stream! {
        let guard = stream_gate
            .acquire(
                crate::config_client::write_telemetry::WriteOperation::new(
                    "test.stream_drop_waiter",
                )
                .unwrap(),
            )
            .await
            .expect("stream acquisition follows owner release");
        drop(guard);
        yield ();
    });
    let mut next = Box::pin(stream.next());
    let waker = futures::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    assert!(Future::poll(next.as_mut(), &mut context).is_pending());
    drop(next);
    assert_eq!(
        gate.pending_acquires
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        1
    );

    drop(stream);
    assert!(gate
        .pending_acquires
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_empty());
    drop(owners);
}

#[tokio::test(start_paused = true)]
async fn stalled_cancellation_cleanup_releases_the_embedded_write_gate() {
    let gate = Arc::new(super::MutationWriteGate::new());
    let mut owners = fill_write_gate(&gate, "test.stalled_cleanup_owner").await;
    let guard = owners.remove(0);
    let cleanup = tokio::spawn(super::cleanup_while_holding_write_gate(
        Some(guard),
        std::future::pending::<()>(),
    ));

    assert_eq!(gate.permits.available_permits(), 0);
    tokio::time::advance(super::EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT).await;
    cleanup
        .await
        .expect("cleanup task joins")
        .expect_err("stalled cleanup times out");
    assert_eq!(
        gate.permits.available_permits(),
        1,
        "timed-out cleanup releases exactly its owned permit while other leases remain"
    );
    drop(owners);
    assert_eq!(
        gate.permits.available_permits(),
        super::MUTATION_WRITE_GATE_CAPACITY,
        "remaining leases release their permits"
    );
}

#[derive(Deserialize)]
struct StorageWriteGateCase {
    name: String,
    independently_scheduled: bool,
    rollback_owner_scheduled: bool,
    cleanup_times_out: bool,
    expected_gate_held_after_owner_handoff: bool,
    expected_gate_held: bool,
}

/// Embedded workers run outside the paused Tokio clock; real setup and shutdown
/// must complete before and after the modeled cleanup-timeout interval.
#[tokio::test]
async fn lean_cancelled_rollback_timeout_releases_actual_transaction_gate() {
    let cases: Vec<StorageWriteGateCase> =
        serde_json::from_str(include_str!("../../tests/fixtures/storage_write_gate.json"))
            .expect("Lean storage write-gate fixture");
    let scenario = cases
        .iter()
        .find(|case| case.name == "cancelled_rollback_timeout_releases_canonical_gate")
        .expect("generated cancellation and rollback-timeout scenario");
    assert!(!scenario.independently_scheduled);
    assert!(scenario.rollback_owner_scheduled && scenario.cleanup_times_out);
    assert!(!scenario.expected_gate_held);

    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    txn.pause_drop_rollback_for_test(Arc::new(super::RollbackDropCleanupPause {
        started: started.clone(),
        release,
    }));
    tokio::time::pause();
    drop(txn);

    let gate = super::mutation_write_gate(&node);
    started.notified().await;
    let gate_held = gate.permits.available_permits() < super::MUTATION_WRITE_GATE_CAPACITY;
    assert_eq!(
        gate_held, scenario.expected_gate_held_after_owner_handoff,
        "native gate observation must match the Lean handoff state"
    );
    if scenario.cleanup_times_out {
        tokio::time::advance(super::EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT).await;
    }
    tokio::time::resume();
    if !scenario.expected_gate_held {
        tokio::time::timeout(Duration::from_secs(1), async {
            while gate.permits.available_permits() < super::MUTATION_WRITE_GATE_CAPACITY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cleanup timeout releases all modeled permits");
    }
    let gate_held = gate.permits.available_permits() < super::MUTATION_WRITE_GATE_CAPACITY;
    assert_eq!(
        gate_held, scenario.expected_gate_held,
        "native gate observation must match the Lean cleanup-timeout state"
    );
    node.shutdown().await;
}

#[derive(Deserialize)]
struct WriteAdmissionScenario {
    name: String,
    capacity: usize,
    initial_claimed: bool,
    actions: Vec<WriteAdmissionAction>,
    checkpoints: Vec<WriteAdmissionCheckpoint>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WriteAdmissionAction {
    Request { owner: usize },
    Observe { owner: usize, event: String },
    Release { owner: usize },
}

#[derive(Deserialize)]
struct WriteAdmissionCheckpoint {
    after_actions: usize,
    active_count: usize,
    active_owners: Vec<usize>,
    waiting: Vec<usize>,
    leases: Vec<WriteAdmissionLease>,
    store_claim_held: bool,
}

#[derive(Deserialize)]
struct WriteAdmissionLease {
    owner: usize,
    phase: String,
}

async fn wait_for_gate_waiters(gate: &super::MutationWriteGate, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if lock(&gate.pending_acquires).len() == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native gate waiter registrations match the Lean trace");
}

#[tokio::test]
async fn lean_write_admission_traces_match_native_independent_leases_and_capacity() {
    let scenarios: Vec<WriteAdmissionScenario> = serde_json::from_str(include_str!(
        "../../tests/fixtures/storage_write_gate_admission.json"
    ))
    .expect("Lean write-admission traces");

    for scenario in scenarios {
        assert_eq!(
            scenario.capacity,
            super::MUTATION_WRITE_GATE_CAPACITY,
            "{}: model and native gate capacity",
            scenario.name
        );
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(&data)
                .build()
                .await
                .unwrap(),
        );
        let access = if scenario.initial_claimed {
            Some(
                ConfigAccess::claimed_local(
                    Arc::clone(&node),
                    crate::home::lock_store(home.path(), &data).unwrap(),
                )
                .unwrap(),
            )
        } else {
            None
        };
        let gate = super::mutation_write_gate(&node);
        let mut held = BTreeMap::new();
        let mut acquiring = tokio::task::JoinSet::new();

        for (index, action) in scenario.actions.into_iter().enumerate() {
            let checkpoint = scenario
                .checkpoints
                .get(index)
                .expect("Lean emits one checkpoint per action");
            assert_eq!(checkpoint.after_actions, index + 1, "{}", scenario.name);

            match action {
                WriteAdmissionAction::Request { owner } => {
                    let gate = Arc::clone(&gate);
                    acquiring.spawn(async move {
                        let operation = crate::config_client::write_telemetry::WriteOperation::new(
                            "test.write_admission.trace",
                        )?;
                        Ok::<_, TestError>((owner, gate.acquire(operation).await?))
                    });
                }
                WriteAdmissionAction::Observe { owner, event } => {
                    let phase = checkpoint
                        .leases
                        .iter()
                        .find(|lease| lease.owner == owner)
                        .map(|lease| lease.phase.as_str());
                    assert!(
                        phase.is_some(),
                        "{}: observed lease disappeared",
                        scenario.name
                    );
                    assert!(
                        matches!(event.as_str(), "storage_returned" | "release"),
                        "{}: unexpected modeled event {event}",
                        scenario.name
                    );
                }
                WriteAdmissionAction::Release { owner } => {
                    drop(
                        held.remove(&owner)
                            .expect("Lean only releases an admitted lease"),
                    );
                }
            }

            while held.len() < checkpoint.active_count {
                let acquired = tokio::time::timeout(Duration::from_secs(5), acquiring.join_next())
                    .await
                    .expect("native write permit admission made progress")
                    .expect("one lease acquisition remains to be joined")
                    .expect("lease task did not panic")
                    .expect("native lease acquisition succeeded");
                assert!(held.insert(acquired.0, acquired.1).is_none());
            }
            wait_for_gate_waiters(&gate, checkpoint.waiting.len()).await;

            let held_owners: Vec<_> = held.keys().copied().collect();
            assert_eq!(held_owners, checkpoint.active_owners, "{}", scenario.name);
            assert_eq!(
                lock(&gate.owners).len(),
                checkpoint.active_count,
                "{}: native owner metadata",
                scenario.name
            );
            assert_eq!(
                gate.permits.available_permits(),
                scenario.capacity.saturating_sub(checkpoint.active_count),
                "{}: native permits",
                scenario.name
            );
            assert_eq!(
                lock(&gate.pending_acquires).len(),
                checkpoint.waiting.len(),
                "{}: native waiters",
                scenario.name
            );
            assert_eq!(gate.store_claim().is_some(), checkpoint.store_claim_held);
            for lease in &checkpoint.leases {
                assert!(held.contains_key(&lease.owner), "{}", scenario.name);
                assert!(
                    matches!(
                        lease.phase.as_str(),
                        "storage" | "cleanup" | "dropping" | "releasable" | "released"
                    ),
                    "{}: unknown model lifecycle phase {}",
                    scenario.name,
                    lease.phase
                );
            }
        }

        acquiring.abort_all();
        while acquiring.join_next().await.is_some() {}
        held.clear();
        drop(access);
        drop(gate);
        node.shutdown().await;
    }
}

#[tokio::test]
async fn embedded_conflict_replays_complete_callback_with_fresh_snapshot() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
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

    ConfigAccess::transact_local_write(&node, None, "test.embedded_replay", move |txn| {
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
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let handles = std::mem::take(&mut *lock(&handles));
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
        &node,
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
                &node,
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
            &node,
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

    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let policy_id = node.add_dac_policy(OWNER, POLICY).await.unwrap();
    node.add_schema(&format!(
        "type IdentityFact @policy(id: \"{policy_id}\", resource: \"facts\") {{ value: String }}"
    ))
    .await
    .unwrap();
    let owner = identity::Did::new(OWNER).unwrap();
    let doc_id = ConfigAccess::transact_local_write(
        &node,
        Some(owner),
        "test.identity_owner_create",
        |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(
                        r#"mutation { add_IdentityFact(input: {value: "secret"}) { _docID } }"#,
                    )
                    .await?;
                Ok::<_, TestError>(
                    response["data"]["add_IdentityFact"][0]["_docID"]
                        .as_str()
                        .expect("created document id")
                        .to_owned(),
                )
            })
        },
    )
    .await
    .unwrap();
    node.add_dac_actor_relationship(OWNER, "IdentityFact", &doc_id, "reader", READER)
        .await
        .unwrap();

    fn count_rows<'txn, 'node>(
        txn: &'txn ConfigApplyTxn<'node>,
    ) -> futures::future::BoxFuture<'txn, Result<usize>> {
        Box::pin(async move {
            Ok(
                txn.execute("{ IdentityFact { value } }").await?["data"]["IdentityFact"]
                    .as_array()
                    .map_or(0, Vec::len),
            )
        })
    }
    for read_only in [false, true] {
        for (identity, expected) in [(Some(identity::Did::new(READER).unwrap()), 1), (None, 0)] {
            let visible = if read_only {
                ConfigAccess::transact_local_readonly(
                    &node,
                    identity,
                    "test.identity_readonly_query",
                    count_rows,
                )
                .await
            } else {
                ConfigAccess::transact_local_write(
                    &node,
                    identity,
                    "test.identity_query",
                    count_rows,
                )
                .await
            }
            .unwrap();
            assert_eq!(visible, expected, "read_only={read_only}");
        }
    }
    for (actor, expected_owner) in [(Some(READER), Some(OWNER)), (None, None)] {
        let ids = vec![doc_id.clone()];
        let observations = ConfigAccess::transact_local_readonly(
            &node,
            actor.map(|did| identity::Did::new(did).unwrap()),
            "test.identity_owner_observation",
            move |txn| {
                let ids = ids.clone();
                Box::pin(async move {
                    let rows = txn.execute("{ IdentityFact { _docID } }").await?;
                    let owners = txn.document_owners("IdentityFact", &ids).await?;
                    assert_eq!(owners.len(), 1);
                    assert_eq!(owners[0].doc_id, ids[0]);
                    assert_eq!(
                        rows["data"]["IdentityFact"].as_array().unwrap().len(),
                        usize::from(expected_owner.is_some())
                    );
                    Ok::<_, anyhow::Error>(owners)
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(
            observations[0].owner.as_ref().map(identity::Did::as_str),
            expected_owner
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn readonly_transaction_preserves_snapshot_and_rejects_native_mutation() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ConfigAccess::local(Arc::clone(&node))
        .add_schema("type ReadSnapshotProbe { value: String }")
        .await
        .unwrap();
    ConfigAccess::write_local(
        &node,
        "test.readonly_seed",
        r#"mutation { create_ReadSnapshotProbe(input: {value: "before"}) { _docID } }"#,
    )
    .await
    .unwrap();
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let reading_node = Arc::clone(&node);
    let reading_entered = Arc::clone(&entered);
    let reading_resume = Arc::clone(&resume);
    let reader = tokio::spawn(async move {
        ConfigAccess::transact_local_readonly(
            &reading_node,
            None,
            "test.readonly_snapshot",
            |txn| {
                let entered = Arc::clone(&reading_entered);
                let resume = Arc::clone(&reading_resume);
                Box::pin(async move {
                    let first = txn.execute("{ ReadSnapshotProbe { value } }").await?;
                    entered.notify_one();
                    resume.notified().await;
                    let second = txn.execute("{ ReadSnapshotProbe { value } }").await?;
                    Ok::<_, crate::config_client::ConfigClientError>((first, second))
                })
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    ConfigAccess::write_local(
        &node,
        "test.readonly_concurrent_update",
        r#"mutation { update_ReadSnapshotProbe(input: {value: "after"}) { _docID } }"#,
    )
    .await
    .unwrap();
    resume.notify_one();
    let (first, second) = tokio::time::timeout(Duration::from_secs(10), reader)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first["data"]["ReadSnapshotProbe"][0]["value"], "before");
    assert_eq!(second["data"]["ReadSnapshotProbe"][0]["value"], "before");
    let error = ConfigAccess::transact_local_readonly(
        &node,
        None,
        "test.readonly_mutation_rejected",
        |txn| {
            Box::pin(async move {
                txn.execute(r#"mutation { update_ReadSnapshotProbe(input: {value: "forbidden"}) { _docID } }"#)
                    .await
            })
        },
    )
    .await
    .unwrap_err();
    assert!(
        format!("{}", ErrorChain(&error))
            .contains("cannot execute mutation in read-only transaction"),
        "mutation must reach DefraDB's native read-only transaction guard: {}",
        ErrorChain(&error)
    );
    let after =
        ConfigAccess::transact_local_readonly(&node, None, "test.readonly_new_snapshot", |txn| {
            Box::pin(async move { txn.execute("{ ReadSnapshotProbe { value } }").await })
        })
        .await
        .unwrap();
    assert_eq!(after["data"]["ReadSnapshotProbe"][0]["value"], "after");
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
    active_ids: Arc<Mutex<RapidHashSet<String>>>,
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
            active_ids: Arc::new(Mutex::new(RapidHashSet::new())),
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
    let access = ConfigAccess::graphql(endpoint);
    let callback_ids = Arc::new(Mutex::new(Vec::new()));
    let callback_ids_for_attempt = Arc::clone(&callback_ids);

    let committed_id = access
        .transact_write("test.http_replay", move |txn| {
            let callback_ids = Arc::clone(&callback_ids_for_attempt);
            Box::pin(async move {
                let id = transaction_id(txn);
                lock(&callback_ids).push(id.clone());
                txn.execute(
                    r#"mutation { update_OwnedWriteFact(input: {value: "canonical"}) { _docID } }"#,
                )
                .await?;
                Ok::<_, TestError>(id)
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
    let access = ConfigAccess::graphql(endpoint);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_for_attempt = Arc::clone(&callbacks);

    let error = access
        .transact_write("test.http_no_replay", move |txn| {
            callbacks_for_attempt.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                txn.execute(
                    r#"mutation { update_OwnedWriteFact(input: {value: "canonical"}) { _docID } }"#,
                )
                .await?;
                Ok::<_, crate::config_client::ConfigClientError>(())
            })
        })
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("HTTP 500"),
        "{}",
        ErrorChain(&error)
    );
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
    let access = ConfigAccess::graphql(endpoint);
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
                    Ok::<_, TestError>(id)
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
    let access = ConfigAccess::graphql(endpoint);
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
                        return Err(super::retry::transaction_storage_failure(
                            crate::config_client::ConfigClientError::InjectedReadStorageFailure,
                        ));
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
    let access = ConfigAccess::graphql(endpoint);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callbacks_for_attempt = Arc::clone(&callbacks);

    let error = access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "test.http_idempotent_domain_rejection",
            move |_| {
                callbacks_for_attempt.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Err::<(), _>(TestError::from("stale domain state")) })
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
    let access = ConfigAccess::graphql(endpoint);
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
                Ok::<_, TestError>(())
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
    const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(10);

    let (endpoint, state, server) = fake_transaction_server(FirstCommit::Succeed).await;
    let stage_observed = Arc::clone(&state.stage_observed);
    let access = ConfigAccess::graphql(endpoint);
    let mut transaction = Box::pin(access.transact_write("test.http_cancel", |txn| {
        Box::pin(async move {
            txn.execute(
                r#"mutation { update_OwnedWriteFact(input: {value: "pending"}) { _docID } }"#,
            )
            .await?;
            std::future::pending::<Result<()>>().await
        })
    }));

    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::select! {
            _ = stage_observed.notified() => {}
            result = &mut transaction => panic!("transaction finished before cancellation: {result:?}"),
        }
    })
        .await
        .expect("callback should stage its mutation");
    drop(transaction);
    tokio::time::timeout(OBSERVATION_TIMEOUT, state.discard_observed.notified())
        .await
        .expect("cancellation should discard the open transaction");

    assert_eq!(&*lock(&state.staged_ids), &["0"]);
    assert!(lock(&state.committed_ids).is_empty());
    assert_eq!(&*lock(&state.discarded_ids), &["0"]);
    assert!(lock(&state.active_ids).is_empty());
    server.abort();
}

fn transaction_handle(txn: &ConfigApplyTxn<'_>) -> query::TransactionHandle {
    let TxnBackend::Embedded { handle, .. } = &txn.backend else {
        panic!("embedded transaction required")
    };
    handle.clone()
}

async fn assert_native_transaction_abandoned(
    node: &EmbeddedNode,
    handle: &query::TransactionHandle,
) {
    let error = node
        .rollback_transaction(handle)
        .await
        .expect_err("native owner already removed the registered transaction");
    assert!(
        matches!(error, query::TransactionError::NotFound(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn aborting_embedded_callback_discards_writes_and_releases_the_gate() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ConfigAccess::local(Arc::clone(&node))
        .add_schema("type AbortedTransactionProbe { value: String }")
        .await
        .unwrap();
    let telemetry = EventCapture::default();
    let events = Arc::clone(&telemetry.events);
    let subscriber = tracing::Dispatch::new(Registry::default().with(telemetry));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    let registered = Arc::new(Mutex::new(None));
    let registered_for_callback = Arc::clone(&registered);
    let reached = Arc::new(Notify::new());
    let reached_for_callback = Arc::clone(&reached);
    let task_node = Arc::clone(&node);
    let task = tokio::spawn(tracing::instrument::WithSubscriber::with_subscriber(
        async move {
            ConfigAccess::transact_local_write(&task_node, None, "test.abort_embedded_callback", |txn| {
            let registered = Arc::clone(&registered_for_callback);
            let reached = Arc::clone(&reached_for_callback);
            Box::pin(async move {
                txn.execute(r#"mutation { create_AbortedTransactionProbe(input: {value: "uncommitted"}) {_docID} }"#).await?;
                *lock(&registered) = Some(transaction_handle(txn));
                reached.notify_one();
                std::future::pending::<Result<()>>().await
            })
        })
        .await
        },
        subscriber.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .expect("callback staged its write");
    let handle = lock(&registered).clone().unwrap();
    let gate = super::mutation_write_gate(&node);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    {
        let events = lock(&events);
        let event = events
            .iter()
            .find(|event| event.get("outcome").map(String::as_str) == Some("cancelled"))
            .expect("cancelled callback emits transaction telemetry");
        assert_eq!(
            event.get("rollback").map(String::as_str),
            Some("not_needed")
        );
    }
    assert_eq!(
        gate.permits.available_permits(),
        super::MUTATION_WRITE_GATE_CAPACITY
    );
    assert_native_transaction_abandoned(&node, &handle).await;
    let response = ConfigAccess::local(Arc::clone(&node))
        .execute("{ AbortedTransactionProbe { value } }")
        .await
        .unwrap();
    assert_eq!(response["data"]["AbortedTransactionProbe"], json!([]));
    ConfigAccess::write_local(
        &node,
        "test.write_after_aborted_callback",
        r#"mutation { create_AbortedTransactionProbe(input: {value: "committed"}) {_docID} }"#,
    )
    .await
    .unwrap();
    node.shutdown().await;
}

#[test]
fn dropping_embedded_transaction_after_runtime_shutdown_abandons_synchronously() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let node = runtime.block_on(async {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_schema("type RuntimeShutdownProbe { value: String }")
            .await
            .unwrap();
        node
    });
    let txn = runtime.block_on(async {
        let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        txn.execute(
            r#"mutation { create_RuntimeShutdownProbe(input: {value: "uncommitted"}) {_docID} }"#,
        )
        .await
        .unwrap();
        txn
    });
    let handle = transaction_handle(&txn);
    let gate = super::mutation_write_gate(&node);
    runtime.shutdown_background();
    drop(txn);
    assert_eq!(
        gate.permits.available_permits(),
        super::MUTATION_WRITE_GATE_CAPACITY
    );

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert_native_transaction_abandoned(&node, &handle).await;
        let response = crate::graphql::graphql_with_transaction_retry(
            &node,
            "{ RuntimeShutdownProbe { value } }",
            "test.read_after_runtime_shutdown",
        )
        .await
        .unwrap();
        assert_eq!(response.data.unwrap()["RuntimeShutdownProbe"], json!([]));
        ConfigAccess::write_local(
            &node,
            "test.write_after_runtime_shutdown",
            r#"mutation { create_RuntimeShutdownProbe(input: {value: "committed"}) {_docID} }"#,
        )
        .await
        .unwrap();
        node.shutdown().await;
    });
}

#[tokio::test]
async fn cancellation_at_embedded_finalization_boundaries_preserves_commit_outcome() {
    use super::EmbeddedFinalizationBoundary::{AfterCommit, BeforeCommit, BeforeRollback};

    for boundary in [BeforeCommit, BeforeRollback, AfterCommit] {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        ConfigAccess::local(Arc::clone(&node))
            .add_schema("type FinalizationCancellationProbe { value: String }")
            .await
            .unwrap();
        let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        txn.execute(r#"mutation { create_FinalizationCancellationProbe(input: {value: "original"}) {_docID} }"#)
            .await
            .unwrap();
        let handle = transaction_handle(&txn);
        let gate = super::mutation_write_gate(&node);
        let reached = Arc::new(Notify::new());
        let mut finalization = Box::pin(super::EMBEDDED_FINALIZATION_PAUSE.scope(
            (boundary, Arc::clone(&reached)),
            async move {
                if boundary == BeforeRollback {
                    txn.discard().await
                } else {
                    txn.commit().await
                }
            },
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = reached.notified() => {}
                result = &mut finalization => panic!("finalizer skipped its boundary: {result:?}"),
            }
        })
        .await
        .expect("finalizer reached the selected boundary");
        assert!(
            gate.permits.available_permits() < super::MUTATION_WRITE_GATE_CAPACITY,
            "finalization must retain its write gate"
        );
        drop(finalization);
        assert!(
            gate.permits.available_permits() == super::MUTATION_WRITE_GATE_CAPACITY,
            "cancellation must release the write gate"
        );
        assert_native_transaction_abandoned(&node, &handle).await;
        let response = ConfigAccess::local(Arc::clone(&node))
            .execute("{ FinalizationCancellationProbe { value } }")
            .await
            .unwrap();
        let expected = if boundary == AfterCommit {
            json!([{"value": "original"}])
        } else {
            json!([])
        };
        assert_eq!(response["data"]["FinalizationCancellationProbe"], expected);
        ConfigAccess::write_local(
            &node,
            "test.write_after_cancelled_finalizer",
            r#"mutation { create_FinalizationCancellationProbe(input: {value: "next"}) {_docID} }"#,
        )
        .await
        .unwrap();
        node.shutdown().await;
    }
}
