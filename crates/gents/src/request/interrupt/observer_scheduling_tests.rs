use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use events::{DocumentChange, DocumentChangeBatch};
use tokio::sync::{oneshot, watch, Mutex};

use super::{
    observe_request_interrupt, InterruptChangeFeed, InterruptIntent, OBSERVER_POLL_INTERVAL,
};

const REQUEST: &str = "bae-request";
const AT: &str = "2026-09-23T03:03:34Z";

fn batch(doc_id: &str) -> DocumentChangeBatch {
    DocumentChangeBatch {
        changes: vec![DocumentChange {
            collection_id: "AgentRequest".into(),
            doc_id: doc_id.into(),
            has_local_write: true,
        }],
        resync_required: false,
        updates: 1,
    }
}

struct Flood {
    delivered: Arc<AtomicUsize>,
    limit: usize,
}

impl InterruptChangeFeed for Flood {
    async fn recv(&mut self) -> Option<DocumentChangeBatch> {
        if self.delivered.fetch_add(1, Ordering::SeqCst) >= self.limit {
            return None;
        }
        Some(batch("bae-unrelated"))
    }
}

struct AdvancingFlood;

impl InterruptChangeFeed for AdvancingFlood {
    async fn recv(&mut self) -> Option<DocumentChangeBatch> {
        tokio::time::advance(Duration::from_millis(100)).await;
        Some(batch("bae-unrelated"))
    }
}

struct Once(Option<oneshot::Receiver<DocumentChangeBatch>>);

impl InterruptChangeFeed for Once {
    async fn recv(&mut self) -> Option<DocumentChangeBatch> {
        match self.0.take() {
            Some(rx) => rx.await.ok(),
            None => std::future::pending().await,
        }
    }
}

#[tokio::test(start_paused = true)]
async fn initial_read_is_not_starved_by_an_always_ready_feed() {
    let delivered = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (_shutdown_tx, mut shutdown) = watch::channel(false);
    observe_request_interrupt(
        REQUEST,
        Some(Flood {
            delivered: delivered.clone(),
            limit: 1000,
        }),
        || async { Some(AT.to_string()) },
        &tx,
        &mut shutdown,
    )
    .await;
    assert!(rx.borrow().is_some());
    assert_eq!(delivered.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn fallback_tick_reads_while_unrelated_changes_keep_arriving() {
    let reads = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (_shutdown_tx, mut shutdown) = watch::channel(false);
    let start = tokio::time::Instant::now();
    let counter = reads.clone();
    observe_request_interrupt(
        REQUEST,
        Some(AdvancingFlood),
        move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            async move { (n > 0).then(|| AT.to_string()) }
        },
        &tx,
        &mut shutdown,
    )
    .await;
    assert!(rx.borrow().is_some());
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert!(start.elapsed() >= OBSERVER_POLL_INTERVAL);
}

#[tokio::test(start_paused = true)]
async fn a_change_to_this_request_is_read_before_the_fallback_tick() {
    let value = Arc::new(Mutex::new(None::<String>));
    let (change_tx, change_rx) = oneshot::channel();
    let (tx, mut rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (_shutdown_tx, mut shutdown) = watch::channel(false);
    let reader = value.clone();
    let observer = tokio::spawn(async move {
        observe_request_interrupt(
            REQUEST,
            Some(Once(Some(change_rx))),
            move || {
                let reader = reader.clone();
                async move { reader.lock().await.clone() }
            },
            &tx,
            &mut shutdown,
        )
        .await;
    });

    tokio::time::advance(Duration::from_millis(500)).await;
    assert!(rx.borrow().is_none());
    let start = tokio::time::Instant::now();
    *value.lock().await = Some(AT.to_string());
    change_tx.send(batch(REQUEST)).unwrap();
    rx.changed().await.unwrap();
    assert!(start.elapsed() < OBSERVER_POLL_INTERVAL);
    observer.await.unwrap();
}
