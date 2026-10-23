use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use futures::StreamExt;
use tokio::sync::{oneshot, Mutex};

use super::flush_while_polling as while_polling;

fn poll_once<F>(future: Pin<&mut F>) -> Poll<F::Output>
where
    F: Future,
{
    let waker = futures::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    future.poll(&mut context)
}

#[tokio::test]
async fn a_retained_generator_waiter_blocks_flush_until_the_generator_is_polled() {
    let gate = Arc::new(Mutex::new(()));
    let first_guard = gate.lock().await;
    let stream_gate = Arc::clone(&gate);
    let mut stream = Box::pin(async_stream::stream! {
        let guard = stream_gate.lock().await;
        drop(guard);
        yield ();
    });

    let mut next = Box::pin(stream.next());
    assert!(poll_once(next.as_mut()).is_pending());
    drop(next);

    drop(first_guard);
    let mut flush = Box::pin(gate.lock());
    assert!(poll_once(flush.as_mut()).is_pending());

    let mut resumed_next = Box::pin(stream.next());
    assert_eq!(poll_once(resumed_next.as_mut()), Poll::Ready(Some(())));
    assert!(poll_once(flush.as_mut()).is_ready());
}

#[tokio::test]
async fn while_polling_buffers_one_item_and_flushes_before_returning_it() {
    let gate = Arc::new(Mutex::new(()));
    let first_guard = gate.lock().await;
    let produced = Arc::new(AtomicUsize::new(0));
    let produced_by_stream = Arc::clone(&produced);
    let stream_gate = Arc::clone(&gate);
    let mut stream = Box::pin(async_stream::stream! {
        let guard = stream_gate.lock().await;
        drop(guard);
        produced_by_stream.fetch_add(1, Ordering::SeqCst);
        yield 1;
        produced_by_stream.fetch_add(1, Ordering::SeqCst);
        yield 2;
    });

    let mut first_next = Box::pin(stream.next());
    assert!(poll_once(first_next.as_mut()).is_pending());
    drop(first_next);
    drop(first_guard);

    let (release_flush, wait_for_release) = oneshot::channel();
    let flush_gate = Arc::clone(&gate);
    let flush_acquired = Arc::new(AtomicUsize::new(0));
    let flush_acquired_by_future = Arc::clone(&flush_acquired);
    let flush = async move {
        let _guard = flush_gate.lock().await;
        flush_acquired_by_future.store(1, Ordering::SeqCst);
        wait_for_release.await.expect("flush release");
        "flushed"
    };

    let mut combined = Box::pin(while_polling(flush, stream.next()));
    assert!(poll_once(combined.as_mut()).is_pending());
    assert_eq!(produced.load(Ordering::SeqCst), 1);
    assert_eq!(flush_acquired.load(Ordering::SeqCst), 1);

    release_flush.send(()).expect("release the flush");
    let (flushed, item) = combined.await;
    assert_eq!(flushed, "flushed");
    assert_eq!(item, Some(Some(1)));
    assert_eq!(produced.load(Ordering::SeqCst), 1);

    assert_eq!(stream.next().await, Some(2));
    assert_eq!(produced.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn while_polling_preserves_end_of_stream_after_flush() {
    let (release_flush, wait_for_release) = oneshot::channel();
    let flush = async move {
        wait_for_release.await.expect("flush release");
        "flushed"
    };
    let end_of_stream = futures::future::ready(None::<()>);
    let mut combined = Box::pin(while_polling(flush, end_of_stream));

    assert!(poll_once(combined.as_mut()).is_pending());
    release_flush.send(()).expect("release the flush");
    let (flushed, item) = combined.await;

    assert_eq!(flushed, "flushed");
    assert_eq!(item, Some(None));
}

#[tokio::test]
async fn dropping_the_generator_cancels_its_queued_gate_waiter() {
    let gate = Arc::new(Mutex::new(()));
    let first_guard = gate.lock().await;
    let stream_gate = Arc::clone(&gate);
    let mut stream = Box::pin(async_stream::stream! {
        let _guard = stream_gate.lock().await;
        yield ();
    });

    let mut next = Box::pin(stream.next());
    assert!(poll_once(next.as_mut()).is_pending());
    drop(next);
    drop(stream);
    drop(first_guard);

    let mut flush = Box::pin(gate.lock());
    assert!(poll_once(flush.as_mut()).is_ready());
}

#[tokio::test]
async fn while_polling_returns_a_buffered_stream_error_after_flush() {
    let (release_flush, wait_for_release) = oneshot::channel();
    let flush = async move {
        wait_for_release.await.expect("flush release");
        "flushed"
    };
    let stream_error = futures::future::ready(Some(Err::<(), _>("provider failed")));
    let mut combined = Box::pin(while_polling(flush, stream_error));

    assert!(poll_once(combined.as_mut()).is_pending());
    release_flush.send(()).expect("release the flush");
    let (flushed, item) = combined.await;

    assert_eq!(flushed, "flushed");
    assert_eq!(item, Some(Some(Err("provider failed"))));
}

#[tokio::test]
async fn while_polling_retains_owned_item_when_flush_fails() {
    let produced = Arc::new(AtomicUsize::new(0));
    let produced_by_stream = Arc::clone(&produced);
    let mut stream = Box::pin(async_stream::stream! {
        produced_by_stream.fetch_add(1, Ordering::SeqCst);
        yield String::from("first");
        produced_by_stream.fetch_add(1, Ordering::SeqCst);
        yield String::from("second");
    });

    let (release_flush, wait_for_release) = oneshot::channel();
    let flush = async move {
        wait_for_release.await.expect("flush release");
        Err::<(), _>("flush failed")
    };
    let mut combined = Box::pin(while_polling(flush, stream.next()));

    assert!(poll_once(combined.as_mut()).is_pending());
    assert_eq!(produced.load(Ordering::SeqCst), 1);

    release_flush.send(()).expect("release the flush");
    let (flush_result, item) = combined.await;

    assert_eq!(flush_result, Err("flush failed"));
    assert_eq!(item, Some(Some(String::from("first"))));
    assert_eq!(produced.load(Ordering::SeqCst), 1);
    assert_eq!(stream.next().await, Some(String::from("second")));
    assert_eq!(produced.load(Ordering::SeqCst), 2);
}
