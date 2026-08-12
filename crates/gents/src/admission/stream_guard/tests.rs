use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{future::BoxFuture, StreamExt};
use rig::completion::{CompletionError, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};

use super::hold_stream_guard;

struct DropProbe {
    drops: Arc<AtomicUsize>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl super::StreamGuardLifecycle for DropProbe {}

#[tokio::test]
async fn holds_guard_until_stream_eof_and_preserves_final_response_metadata() {
    let drops = Arc::new(AtomicUsize::new(0));
    let inner = StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![
        Ok(RawStreamingChoice::Message("hello".to_string())),
        Ok(RawStreamingChoice::MessageId("msg_123".to_string())),
        Ok(RawStreamingChoice::FinalResponse(())),
    ])));
    let mut guarded = hold_stream_guard(
        inner,
        DropProbe {
            drops: drops.clone(),
        },
    );

    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(guarded.next().await.is_some());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    while guarded.next().await.is_some() {}

    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let completed: CompletionResponse<Option<()>> = guarded.into();
    assert_eq!(completed.raw_response, Some(()));
    assert_eq!(completed.message_id.as_deref(), Some("msg_123"));
}

struct AsyncFinalizeProbe {
    started: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    release: tokio::sync::oneshot::Receiver<()>,
    error: Option<&'static str>,
}

impl super::StreamGuardLifecycle for AsyncFinalizeProbe {
    fn finish_stream(self) -> BoxFuture<'static, Result<(), CompletionError>> {
        Box::pin(async move {
            self.started.store(true, Ordering::SeqCst);
            let _ = self.release.await;
            self.finished.store(true, Ordering::SeqCst);
            match self.error {
                Some(error) => Err(CompletionError::ProviderError(error.to_string())),
                None => Ok(()),
            }
        })
    }
}

#[tokio::test]
async fn awaits_finalization_before_publishing_final_response() {
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let inner = StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![Ok(
        RawStreamingChoice::FinalResponse(()),
    )])));
    let mut guarded = hold_stream_guard(
        inner,
        AsyncFinalizeProbe {
            started: started.clone(),
            finished: finished.clone(),
            release: release_rx,
            error: None,
        },
    );

    assert!(
        tokio::time::timeout(Duration::from_millis(20), guarded.next())
            .await
            .is_err(),
        "terminal response must wait for durable finalization"
    );
    assert!(started.load(Ordering::SeqCst));
    assert!(!finished.load(Ordering::SeqCst));

    release_tx.send(()).unwrap();
    assert!(matches!(
        guarded.next().await,
        Some(Ok(rig::streaming::StreamedAssistantContent::Final(_)))
    ));
    assert!(finished.load(Ordering::SeqCst));
}

#[tokio::test]
async fn finalization_failure_replaces_terminal_response_with_error() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let inner = StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![Ok(
        RawStreamingChoice::FinalResponse(()),
    )])));
    let mut guarded = hold_stream_guard(
        inner,
        AsyncFinalizeProbe {
            started: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            release: release_rx,
            error: Some("persist failed"),
        },
    );
    release_tx.send(()).unwrap();

    let error = guarded
        .next()
        .await
        .expect("error item")
        .expect_err("terminal response must not escape after persistence failure");
    assert!(error.to_string().contains("persist failed"));
    assert!(guarded.next().await.is_none());
}

#[tokio::test]
async fn drops_guard_when_caller_drops_stream_before_eof() {
    let drops = Arc::new(AtomicUsize::new(0));
    let inner: StreamingCompletionResponse<()> =
        StreamingCompletionResponse::stream(Box::pin(futures::stream::pending()));
    let guarded = hold_stream_guard(
        inner,
        DropProbe {
            drops: drops.clone(),
        },
    );

    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(guarded);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn drops_guard_when_inner_stream_errors() {
    let drops = Arc::new(AtomicUsize::new(0));
    let inner: StreamingCompletionResponse<()> =
        StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![Err(
            rig::completion::CompletionError::ProviderError("boom".to_string()),
        )])));
    let mut guarded = hold_stream_guard(
        inner,
        DropProbe {
            drops: drops.clone(),
        },
    );

    let item = guarded.next().await.expect("error item");
    assert!(item.is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
