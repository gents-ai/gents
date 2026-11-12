use std::{sync::Arc, time::Duration};

use futures::{future::BoxFuture, StreamExt};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use tokio::sync::{oneshot, Mutex};

use super::StreamGuardLifecycle;
use crate::llm::rig_compat::hold_stream_guard;
use gents_lean_vocab::families::storage_write_gate_contracts_stream_finalization as family;
use gents_store::config_client::ConfigAccess;

struct Finalizer {
    gate: Arc<Mutex<()>>,
    entered: oneshot::Sender<()>,
    storage_return: oneshot::Receiver<()>,
}

impl StreamGuardLifecycle for Finalizer {
    fn finish_stream(self) -> BoxFuture<'static, Result<(), crate::admission::AdmissionError>> {
        Box::pin(async move {
            let _guard = self.gate.lock().await;
            let _ = self.entered.send(());
            let _ = self.storage_return.await;
            Ok(())
        })
    }
}

async fn stream_guard_gate_held(case: &family::FinalizationInput) -> bool {
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
        Box::pin(async move { finalizer.finish_stream().await.unwrap() })
    };
    tokio::select! {
        result = &mut entered_rx => result.expect("finalizer entered"),
        () = &mut owner => panic!("{}: finalizer completed before storage return", case.name),
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("{}: finalizer never started", case.name),
    }
    let pending_return = if case.storage_returns {
        return_tx.send(()).unwrap();
        None
    } else {
        Some(return_tx)
    };
    let sibling = tokio::time::timeout(Duration::from_millis(250), gate.lock()).await;
    let gate_held = sibling.is_err();
    drop(sibling);
    drop(owner);
    drop(pending_return);
    gate_held
}

struct NativeFinalizer {
    node: Arc<defra_node::Node>,
    entered: Arc<tokio::sync::Notify>,
    storage_return: Arc<tokio::sync::Notify>,
}

impl StreamGuardLifecycle for NativeFinalizer {
    fn finish_stream(self) -> BoxFuture<'static, Result<(), crate::admission::AdmissionError>> {
        Box::pin(async move {
            ConfigAccess::transact_local_write(
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
                        Ok::<_, anyhow::Error>(())
                    })
                },
            )
            .await
            .map_err(|error| crate::admission::AdmissionError(error.to_string()))
        })
    }
}

async fn native_gate_held_after_independent_completion() -> bool {
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    node.add_collection("type StreamGateFact { value: String }")
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
    let sibling = tokio::time::timeout(
        Duration::from_secs(10),
        ConfigAccess::write_local_response(
            &node,
            "test.stream_finalization.native_sibling",
            "mutation { create_StreamGateFact(input: {value: \"sibling\"}) { _docID } }",
        ),
    )
    .await;
    let Ok(sibling) = sibling else {
        return true;
    };
    sibling.expect("sibling canonical write");
    assert!(matches!(
        guarded.next().await,
        Some(Ok(rig::streaming::StreamedAssistantContent::Final(_)))
    ));
    let response = node.execute("{ StreamGateFact { value } }").await;
    gents_store::graphql::ensure_no_errors(&response, "read stream gate facts").unwrap();
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
    node.shutdown().await.expect("node shutdown");
    false
}

#[tokio::test]
async fn lean_storage_gate_scenarios_drive_stream_finalization() {
    for case in family::cases() {
        case.check(stream_guard_gate_held(&case.input).await);
        if case.input.independently_scheduled && case.input.storage_returns {
            case.check(native_gate_held_after_independent_completion().await);
        }
    }
}
