//! Executes Lean-derived scheduling scenarios against the real stream wrapper.
//! The shared-task arm is the negative control, not a second runtime path.

use std::{sync::Arc, time::Duration};

use futures::{future::BoxFuture, StreamExt};
use rig::{
    completion::CompletionError,
    streaming::{RawStreamingChoice, StreamingCompletionResponse},
};
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex};

use super::{hold_stream_guard, StreamGuardLifecycle};
use crate::config_client::ConfigAccess;

#[derive(Deserialize)]
struct Case {
    name: String,
    independently_scheduled: bool,
    storage_returns: bool,
    expected_gate_held: bool,
}

struct Finalizer {
    gate: Arc<Mutex<()>>,
    entered: oneshot::Sender<()>,
    storage_return: oneshot::Receiver<()>,
}

impl StreamGuardLifecycle for Finalizer {
    fn finish_stream(self) -> BoxFuture<'static, Result<(), CompletionError>> {
        Box::pin(async move {
            let _guard = self.gate.lock().await;
            let _ = self.entered.send(());
            let _ = self.storage_return.await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn lean_storage_gate_scenarios_drive_stream_finalization() {
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/storage_write_gate.json"
    ))
    .expect("Lean storage write-gate fixtures");
    assert_eq!(cases.len(), 3);
    for case in cases {
        let gate = Arc::new(Mutex::new(()));
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let (return_tx, return_rx) = oneshot::channel();
        let finalizer = Finalizer {
            gate: gate.clone(),
            entered: entered_tx,
            storage_return: return_rx,
        };
        let mut owner: BoxFuture<'static, ()> = if case.independently_scheduled {
            Box::pin(async move {
                let inner = StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(
                    vec![Ok(RawStreamingChoice::FinalResponse(()))],
                )));
                let mut guarded = hold_stream_guard(inner, finalizer);
                assert!(guarded.next().await.is_some());
            })
        } else {
            // Reproduce a finalizer polled only by the consumer task. Once the
            // sibling branch wins below, this owner cannot make progress.
            Box::pin(async move { finalizer.finish_stream().await.unwrap() })
        };
        tokio::select! {
            result = &mut entered_rx => result.expect("finalizer entered"),
            () = &mut owner => panic!("{}: finalizer completed before storage return", case.name),
            () = tokio::time::sleep(Duration::from_secs(5)) => panic!("{}: finalizer never started", case.name),
        }
        // A real sibling awaits the held gate without polling `owner` again.
        let pending_return = if case.storage_returns {
            return_tx.send(()).unwrap();
            None
        } else {
            Some(return_tx)
        };
        let sibling = tokio::time::timeout(Duration::from_millis(250), gate.lock()).await;
        assert_eq!(
            sibling.is_err(),
            case.expected_gate_held,
            "{}: unexpected gate ownership while consumer polling is suspended",
            case.name,
        );
        drop(sibling);
        drop(owner);
        drop(pending_return);
    }
}

struct NativeFinalizer {
    node: Arc<defra_node::EmbeddedNode>,
    entered: Arc<tokio::sync::Notify>,
    storage_return: Arc<tokio::sync::Notify>,
}

impl StreamGuardLifecycle for NativeFinalizer {
    fn finish_stream(self) -> BoxFuture<'static, Result<(), CompletionError>> {
        Box::pin(async move {
            ConfigAccess::transact_local(
                &self.node,
                None,
                "test.stream_finalization.native_owner",
                move |txn| {
                    let entered = self.entered.clone();
                    let storage_return = self.storage_return.clone();
                    Box::pin(async move {
                        entered.notify_one();
                        storage_return.notified().await;
                        txn.execute(
                            "mutation { create_StreamGateFact(input: {value: \"finalizer\"}) { _docID } }",
                        )
                        .await?;
                        Ok(())
                    })
                },
            )
            .await
            .map_err(|error| CompletionError::ProviderError(error.to_string()))
        })
    }
}

/// The same positive model scenario through native transactions and the actual
/// canonical gate; no test-only access to gate internals or bypass writer.
#[tokio::test]
async fn lean_independent_completion_releases_native_canonical_gate() {
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/storage_write_gate.json"
    ))
    .unwrap();
    let scenario = cases
        .iter()
        .find(|case| case.name == "independent_completion")
        .expect("generated positive scheduling scenario");
    assert!(scenario.independently_scheduled && scenario.storage_returns);
    assert!(!scenario.expected_gate_held);

    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type StreamGateFact { value: String }")
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let storage_return = Arc::new(tokio::sync::Notify::new());
    let inner = StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![Ok(
        RawStreamingChoice::FinalResponse(()),
    )])));
    let mut guarded = hold_stream_guard(
        inner,
        NativeFinalizer {
            node: node.clone(),
            entered: entered.clone(),
            storage_return: storage_return.clone(),
        },
    );
    tokio::select! {
        () = entered.notified() => {},
        result = guarded.next() => panic!("terminal published before native finalizer was released: {result:?}"),
        () = tokio::time::sleep(Duration::from_secs(10)) => panic!("native finalizer never entered"),
    }
    storage_return.notify_one();
    // Do not poll guarded.next(): the consumer is occupied by response flush.
    tokio::time::timeout(
        Duration::from_secs(10),
        ConfigAccess::write_local_response(
            &node,
            "test.stream_finalization.native_sibling",
            "mutation { create_StreamGateFact(input: {value: \"sibling\"}) { _docID } }",
        ),
    )
    .await
    .expect("native gate must release without consumer stream polls")
    .expect("sibling canonical write");
    assert!(matches!(
        guarded.next().await,
        Some(Ok(rig::streaming::StreamedAssistantContent::Final(_)))
    ));
    let response = node.execute("{ StreamGateFact { value } }").await;
    crate::graphql::ensure_no_errors(&response, "read stream gate facts").unwrap();
    let data = response.data.unwrap();
    let mut values: Vec<_> = data["StreamGateFact"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["value"].as_str().unwrap())
        .collect();
    values.sort_unstable();
    assert_eq!(values, ["finalizer", "sibling"]);
    drop(guarded);
    node.shutdown().await;
}
