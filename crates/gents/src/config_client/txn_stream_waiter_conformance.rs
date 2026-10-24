use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use futures::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::oneshot;

use super::{MutationWriteGate, MutationWriteGuard};
use crate::config_client::write_telemetry::WriteOperation;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Event {
    PollNext,
    ConsumerWait,
    DropBorrowedNext,
    ReleaseInitialGuard,
    FlushPoll,
    ProgressPoll,
    ProgressFlush,
    StreamWriteReturned,
    FlushWriteReturned,
    DropGenerator,
    FlushOnlyPoll,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    events: Vec<Event>,
    flush_waiter: String,
    gate_held: bool,
    initial_guard_held: bool,
    owner_operation: String,
    stream_acquired: bool,
    flush_acquire_ready: bool,
    buffered_item_count: usize,
    generator_dropped: bool,
}

type AcquireFuture = Pin<Box<dyn Future<Output = super::Result<MutationWriteGuard>>>>;
struct GeneratorDropProbe(Arc<AtomicBool>);

impl Drop for GeneratorDropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn poll_once<F>(future: Pin<&mut F>) -> Poll<F::Output>
where
    F: Future + ?Sized,
{
    let waker = futures::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    future.poll(&mut context)
}

fn write_operation(name: &'static str) -> WriteOperation {
    WriteOperation::new(name).expect("static test operation")
}

fn poll_flush(future: &mut Option<AcquireFuture>, guard: &mut Option<MutationWriteGuard>) {
    let future = future.as_mut().expect("flush acquisition future exists");
    match poll_once(future.as_mut()) {
        Poll::Pending => {}
        Poll::Ready(result) => {
            *guard = Some(result.expect("flush acquires the canonical mutation gate"));
        }
    }
}

fn actual_owner(gate: &MutationWriteGate) -> String {
    gate.owners
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .find_map(|owner| {
            let label = match owner.operation.as_str() {
                "test.stream_waiter.initial" => "initial_guard",
                "test.stream_waiter.stream" => "stream",
                "test.stream_waiter.flush" => "flush",
                _ => return None,
            };
            Some(label.to_owned())
        })
        .unwrap_or_else(|| "none".to_owned())
}

#[tokio::test]
async fn lean_stream_waiter_schedules_match_the_canonical_write_gate() {
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../tests/fixtures/storage_write_gate_stream_waiter.json"
    ))
    .expect("Lean stream waiter fixture");
    assert_eq!(cases.len(), 4);

    for case in cases {
        let gate = Arc::new(MutationWriteGate::new());
        let mut initial_guard = Some(
            gate.acquire(write_operation("test.stream_waiter.initial"))
                .await
                .expect("initial mutation gate owner"),
        );
        let mut capacity_fillers = Vec::new();
        for _ in 1..super::MUTATION_WRITE_GATE_CAPACITY {
            capacity_fillers.push(
                gate.acquire(write_operation("test.stream_waiter.capacity_filler"))
                    .await
                    .expect("fill remaining permits so the modeled waiter is next"),
            );
        }
        tokio::task::yield_now().await;
        let stream_acquired = Arc::new(AtomicBool::new(false));
        let generator_dropped = Arc::new(AtomicBool::new(false));
        let yielded = Arc::new(AtomicUsize::new(0));
        let stream_gate = Arc::clone(&gate);
        let stream_acquired_by_generator = Arc::clone(&stream_acquired);
        let generator_dropped_by_generator = Arc::clone(&generator_dropped);
        let yielded_by_generator = Arc::clone(&yielded);
        let (release_stream_tx, release_stream_rx) = oneshot::channel();
        let mut release_stream_tx = Some(release_stream_tx);
        let mut stream: Option<Pin<Box<dyn Stream<Item = usize>>>> =
            Some(Box::pin(async_stream::stream! {
                let _drop_probe = GeneratorDropProbe(generator_dropped_by_generator);
                let stream_guard = stream_gate
                    .acquire(write_operation("test.stream_waiter.stream"))
                    .await
                    .expect("stream mutation gate acquisition");
                stream_acquired_by_generator.store(true, Ordering::Release);
                let _ = release_stream_rx.await;
                drop(stream_guard);
                yielded_by_generator.fetch_add(1, Ordering::SeqCst);
                yield 1;
                yielded_by_generator.fetch_add(1, Ordering::SeqCst);
                yield 2;
            }));
        let mut flush_future: Option<AcquireFuture> = None;
        let mut flush_guard: Option<MutationWriteGuard> = None;
        let mut buffered_item_count = 0;

        let mut events = case.events.into_iter();
        {
            let stream_ref = stream.as_mut().expect("stream generator is alive");
            let mut borrowed_next = Box::pin(stream_ref.next());
            for event in events.by_ref() {
                match event {
                    Event::PollNext => assert!(poll_once(borrowed_next.as_mut()).is_pending()),
                    Event::ConsumerWait => {}
                    Event::DropBorrowedNext => {
                        break;
                    }
                    Event::ReleaseInitialGuard => drop(initial_guard.take()),
                    Event::FlushPoll => {
                        assert!(flush_future.is_none());
                        let flush_gate = Arc::clone(&gate);
                        flush_future = Some(Box::pin(async move {
                            flush_gate
                                .acquire(write_operation("test.stream_waiter.flush"))
                                .await
                        }));
                        poll_flush(&mut flush_future, &mut flush_guard);
                        assert!(
                            flush_guard.is_none(),
                            "flush queues behind the initial owner"
                        );
                    }
                    Event::FlushOnlyPoll => poll_flush(&mut flush_future, &mut flush_guard),
                    _ => panic!("stream progress/drop actions follow borrowed-next release"),
                }
            }
        }

        {
            while let Some(event) = events.next() {
                match event {
                    Event::PollNext
                    | Event::ConsumerWait
                    | Event::DropBorrowedNext
                    | Event::ReleaseInitialGuard
                    | Event::FlushPoll => {
                        panic!("pre-progress action appears after borrowed-next release")
                    }
                    Event::FlushOnlyPoll => poll_flush(&mut flush_future, &mut flush_guard),
                    Event::ProgressPoll => {
                        let flush = flush_future.take().expect("queued flush future");
                        let stream_ref = stream.as_mut().expect("stream generator is alive");
                        let progress = stream_ref.next();
                        let mut combined = Box::pin(
                            gents_loop::stream_processor::flush_while_polling(flush, progress),
                        );
                        assert!(poll_once(combined.as_mut()).is_pending());
                        assert!(
                            stream_acquired.load(Ordering::Acquire),
                            "co-poll advances the retained generator through gate acquisition"
                        );
                        assert_eq!(actual_owner(&gate), "stream", "{}", case.name);
                        assert!(matches!(events.next(), Some(Event::StreamWriteReturned)));
                        release_stream_tx
                            .take()
                            .expect("stream write release is sent once")
                            .send(())
                            .expect("stream generator is waiting before yielding");
                        match poll_once(combined.as_mut()) {
                            Poll::Pending => {
                                panic!("co-polled flush acquires after stream returns")
                            }
                            Poll::Ready((result, item)) => {
                                flush_guard = Some(result.expect("flush gate acquisition"));
                                buffered_item_count = usize::from(item.flatten().is_some());
                            }
                        }
                        assert_eq!(yielded.load(Ordering::SeqCst), 1);
                        assert!(matches!(events.next(), Some(Event::ProgressFlush)));
                        assert!(flush_guard.is_some());
                    }
                    Event::ProgressFlush => poll_flush(&mut flush_future, &mut flush_guard),
                    Event::FlushWriteReturned => drop(flush_guard.take()),
                    Event::DropGenerator => drop(stream.take()),
                    Event::StreamWriteReturned => {
                        panic!("stream-write return is part of a co-poll progress schedule")
                    }
                }
            }
        }

        let owner_operation = actual_owner(&gate);
        let tracked_operations: Vec<_> = gate
            .owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|owner| owner.operation.as_str())
            .collect();
        assert_eq!(
            case.gate_held,
            owner_operation != "none",
            "{}: mapped owner={owner_operation}, tracked operations={tracked_operations:?}, \
             available permits={}, flush guard={}, stream acquired={}",
            case.name,
            gate.permits.available_permits(),
            flush_guard.is_some(),
            stream_acquired.load(Ordering::Acquire)
        );
        assert_eq!(
            case.initial_guard_held,
            actual_owner(&gate) == "initial_guard",
            "{}",
            case.name
        );
        assert_eq!(case.owner_operation, actual_owner(&gate), "{}", case.name);
        assert_eq!(
            case.flush_acquire_ready,
            flush_guard.is_some(),
            "{}",
            case.name
        );
        assert_eq!(
            case.stream_acquired,
            stream_acquired.load(Ordering::Acquire),
            "{}",
            case.name
        );
        assert_eq!(
            case.buffered_item_count, buffered_item_count,
            "{}",
            case.name
        );
        assert_eq!(
            case.generator_dropped,
            generator_dropped.load(Ordering::Acquire),
            "{}",
            case.name
        );
        assert_eq!(
            case.flush_waiter == "acquired",
            flush_guard.is_some(),
            "{}",
            case.name
        );

        drop(stream.take());
        drop(flush_guard.take());
        drop(flush_future.take());
        drop(initial_guard.take());
        drop(capacity_fillers);
    }
}
