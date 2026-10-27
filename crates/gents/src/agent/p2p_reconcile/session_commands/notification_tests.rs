use super::*;
use futures::FutureExt;
use std::sync::Mutex;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

struct FailedCommands(Arc<Mutex<Vec<String>>>);

impl<S> tracing_subscriber::Layer<S> for FailedCommands
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct CommandId(Option<String>);
        impl tracing::field::Visit for CommandId {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "command_doc_id" {
                    self.0 = Some(value.to_owned());
                }
            }

            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "command_doc_id" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }
        let mut command = CommandId(None);
        event.record(&mut command);
        if let Some(command) = command.0 {
            self.0.lock().unwrap().push(command);
        }
    }
}

struct ObservedChanges {
    waiting: Arc<tokio::sync::Notify>,
    changes: futures::channel::mpsc::UnboundedReceiver<events::DocumentChangeBatch>,
}

impl Stream for ObservedChanges {
    type Item = events::DocumentChangeBatch;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let result = std::pin::Pin::new(&mut self.changes).poll_next(context);
        if result.is_pending() {
            self.waiting.notify_one();
        }
        result
    }
}

#[tokio::test]
async fn collection_block_notifications_never_enter_the_command_retry_queue() {
    let temp = tempfile::tempdir().unwrap();
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        crate::identity::KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap(),
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let changes = CommandKind::ALL
        .into_iter()
        .flat_map(|kind| {
            let collection_id = node
                .get_collection(kind.collection())
                .unwrap()
                .unwrap()
                .collection_id;
            ["", "not-yet-replicated-command"]
                .into_iter()
                .map(move |doc_id| events::DocumentChange {
                    collection_id: collection_id.clone(),
                    doc_id: doc_id.into(),
                    has_local_write: false,
                })
        })
        .collect::<Vec<_>>();
    let (changes_tx, changes_rx) = futures::channel::mpsc::unbounded();
    let (ticks_tx, ticks) = futures::channel::mpsc::unbounded();
    let waiting = Arc::new(tokio::sync::Notify::new());
    let failures = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancellationToken::new();
    let run = run_session_command_reconciler_observing(
        node.clone(),
        identity,
        cancel.clone(),
        ObservedChanges {
            waiting: waiting.clone(),
            changes: changes_rx,
        },
        ticks,
    )
    .with_subscriber(tracing_subscriber::registry().with(FailedCommands(failures.clone())));
    tokio::pin!(run);
    for expected_count in [0, 2, 4] {
        let _ = waiting.notified().now_or_never();
        if expected_count == 2 {
            changes_tx
                .unbounded_send(events::DocumentChangeBatch {
                    updates: changes.len() as u64,
                    changes: changes.clone(),
                    resync_required: false,
                })
                .unwrap();
        } else if expected_count == 4 {
            ticks_tx.unbounded_send(()).unwrap();
        }
        tokio::select! {
            result = &mut run => panic!("reconciler stopped before waiting: {result:?}"),
            () = waiting.notified() => {}
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("reconciler did not finish the notification batch"),
        }
        assert_eq!(
            *failures.lock().unwrap(),
            vec!["not-yet-replicated-command"; expected_count],
            "only real document ids may be processed or retried"
        );
    }
    cancel.cancel();
    run.await.unwrap();
    node.shutdown().await;
}
