use super::*;
use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use std::collections::VecDeque;

/// Bounds application request preparation across peers; native P2P owns transport concurrency.
const MAX_ACTIVE_PEERS: usize = 8;

#[derive(Default)]
pub(super) struct PeerWork {
    active: BTreeSet<String>,
    running: FuturesUnordered<BoxFuture<'static, String>>,
    queued: VecDeque<(String, BoxFuture<'static, ()>)>,
    last_started: Option<String>,
}

impl PeerWork {
    pub(super) fn is_empty(&self) -> bool {
        self.running.is_empty() && self.queued.is_empty()
    }

    fn start(
        &mut self,
        peer: String,
        work: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> bool {
        if self.active.contains(&peer) || self.running.len() >= MAX_ACTIVE_PEERS {
            return false;
        }
        self.last_started = Some(peer.clone());
        self.active.insert(peer.clone());
        self.running.push(
            async move {
                work.await;
                peer
            }
            .boxed(),
        );
        true
    }

    fn enqueue<F>(&mut self, peer: String, work: F) -> bool
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if self.active.contains(&peer) || self.queued.iter().any(|(queued, _)| queued == &peer) {
            return false;
        }
        self.queued.push_back((peer, work.boxed()));
        true
    }

    fn fill_slots(&mut self) {
        // Consume each discovered batch once. Completion cannot requeue a failed attempt.
        while self.running.len() < MAX_ACTIVE_PEERS {
            let Some((peer, work)) = self.queued.pop_front() else {
                break;
            };
            let started = self.start(peer, work);
            debug_assert!(started, "queued peer must not already be running");
        }
    }

    pub(super) async fn next(&mut self) {
        self.fill_slots();
        if let Some(peer) = self.running.next().await {
            self.active.remove(&peer);
            self.fill_slots();
        }
    }
}

enum Request {
    Transcript(Box<HydrationRequestRow>),
    Inventory(Box<crate::file_inventory::requests::InventoryRequestRow>),
    Image(Box<file_images::ImageDemandRow>),
}

impl Request {
    fn peer(&self) -> String {
        match self {
            Self::Transcript(row) => row
                .request_key
                .split_once(':')
                .map_or_else(|| row.request_key.clone(), |(peer, _)| peer.to_string()),
            Self::Inventory(row) => row.peer_id.clone(),
            Self::Image(row) => row.demand.admission.peer_id.clone(),
        }
    }

    async fn serve(
        self,
        store: &GraphqlHydrationStore,
        delivery: &dyn HydrationDelivery,
    ) -> Result<()> {
        match self {
            Self::Transcript(row) => {
                let mut outcome = HydrationTickOutcome::default();
                if let Some(admitted) = admit_or_reject(store, *row, &mut outcome).await? {
                    serve_admitted_request(store, delivery, admitted, &mut outcome).await?;
                }
                Ok(())
            }
            Self::Inventory(row) => {
                inventory_requests::serve_inventory(store, delivery, &row).await
            }
            Self::Image(row) => file_images::serve_image(store, delivery, &row).await,
        }
    }
}

pub(super) async fn discover(
    store: &Arc<GraphqlHydrationStore>,
    delivery: &Arc<dyn HydrationDelivery>,
    work: &mut PeerWork,
    cancel: &CancellationToken,
) {
    let mut pending: BTreeMap<String, Vec<Request>> = BTreeMap::new();
    let discovery = async {
        tokio::join!(
            store.load_pending_requests(),
            inventory_requests::pending_requests(store),
            file_images::pending_requests(store),
        )
    };
    tokio::pin!(discovery);
    let mut completed = Vec::new();
    let (transcripts, inventory, images) = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            result = &mut discovery => break result,
            peer = work.running.next(), if !work.running.is_empty() => {
                if let Some(peer) = peer { completed.push(peer); }
            }
        }
    };
    let complete_snapshot = transcripts.is_ok() && inventory.is_ok() && images.is_ok();
    if complete_snapshot {
        work.queued.clear();
    }
    let mut collect = |requests: Vec<Request>| {
        for request in requests {
            pending.entry(request.peer()).or_default().push(request);
        }
    };
    match transcripts {
        Ok(rows) => collect(
            rows.into_iter()
                .map(Box::new)
                .map(Request::Transcript)
                .collect(),
        ),
        Err(error) => tracing::warn!(%error, "load pending transcript requests"),
    }
    match inventory {
        Ok(rows) => collect(
            rows.into_iter()
                .map(Box::new)
                .map(Request::Inventory)
                .collect(),
        ),
        Err(error) => tracing::warn!(%error, "load pending inventory requests"),
    }
    match images {
        Ok(rows) => collect(rows.into_iter().map(Box::new).map(Request::Image).collect()),
        Err(error) => tracing::warn!(%error, "load pending image requests"),
    }
    let mut pending = pending.into_iter().collect::<Vec<_>>();
    if let Some(last) = work.last_started.as_deref() {
        let offset = pending.partition_point(|(peer, _)| peer.as_str() <= last);
        pending.rotate_left(offset);
    }
    for (peer, requests) in pending {
        if work.active.contains(&peer) {
            continue;
        }
        let store = store.clone();
        let delivery = delivery.clone();
        let log_peer = peer.clone();
        work.enqueue(peer, async move {
            for request in requests {
                if let Err(error) = request.serve(&store, delivery.as_ref()).await {
                    tracing::warn!(peer = %log_peer, %error, "hydration request remains pending");
                }
            }
        });
    }
    for peer in completed {
        work.active.remove(&peer);
    }
    work.fill_slots();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn one_job_per_peer_and_total_work_are_bounded() {
        let mut work = PeerWork::default();
        assert!(work.start("peer-a".into(), std::future::pending()));
        assert!(!work.start("peer-a".into(), async {}));
        for index in 1..MAX_ACTIVE_PEERS {
            assert!(work.start(format!("peer-{index}"), std::future::pending()));
        }
        assert!(!work.start("overflow".into(), async {}));
        assert_eq!(work.running.len(), MAX_ACTIVE_PEERS);
    }

    #[tokio::test]
    async fn queued_ninth_peer_starts_after_failure_without_a_new_discovery() {
        use std::sync::atomic::AtomicUsize;
        let mut work = PeerWork::default();
        let attempts = Arc::new(
            (0..=MAX_ACTIVE_PEERS)
                .map(|_| AtomicUsize::new(0))
                .collect::<Vec<_>>(),
        );
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut releases = Vec::new();
        for index in 0..=MAX_ACTIVE_PEERS {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            releases.push(Some(release_tx));
            let attempts = attempts.clone();
            let started_tx = started_tx.clone();
            assert!(work.enqueue(format!("peer-{index}"), async move {
                attempts[index].fetch_add(1, Ordering::SeqCst);
                started_tx.send(index).unwrap();
                if index < MAX_ACTIVE_PEERS {
                    let _ = release_rx.await;
                }
                let result: Result<()> = Err(anyhow::anyhow!("indeterminate delivery"));
                assert!(result.is_err());
            }));
        }
        work.fill_slots();
        assert_eq!(work.running.len(), MAX_ACTIVE_PEERS);
        assert_eq!(work.queued.len(), 1);
        {
            let first_completion = work.next();
            tokio::pin!(first_completion);
            let mut first_started = BTreeSet::new();
            for _ in 0..MAX_ACTIVE_PEERS {
                tokio::select! {
                    _ = &mut first_completion => panic!("delivery completed before release"),
                    peer = started_rx.recv() => { first_started.insert(peer.unwrap()); }
                }
            }
            assert_eq!(
                first_started,
                (0..MAX_ACTIVE_PEERS).collect::<BTreeSet<_>>()
            );
            releases[0].take().unwrap().send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut first_completion)
                .await
                .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), work.next())
            .await
            .unwrap();
        assert_eq!(started_rx.try_recv().unwrap(), MAX_ACTIVE_PEERS);
        assert!(work.queued.is_empty());
        assert!(attempts
            .iter()
            .all(|count| count.load(Ordering::SeqCst) == 1));
        assert!(
            work.next().now_or_never().is_none(),
            "failed peers must not busy-retry"
        );
        assert!(attempts
            .iter()
            .all(|count| count.load(Ordering::SeqCst) == 1));
        for release in releases.iter_mut().take(MAX_ACTIVE_PEERS).skip(1) {
            release.take().unwrap().send(()).unwrap();
        }
        for _ in 1..MAX_ACTIVE_PEERS {
            tokio::time::timeout(std::time::Duration::from_secs(1), work.next())
                .await
                .unwrap();
        }
        assert!(
            work.is_empty(),
            "all failed batches finish without retrying"
        );
        assert!(attempts
            .iter()
            .all(|count| count.load(Ordering::SeqCst) == 1));
    }

    #[tokio::test]
    async fn a_late_peer_completes_while_an_earlier_peer_is_blocked_and_drop_cancels_work() {
        let mut work = PeerWork::default();
        let started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let ready = started.clone();
        let guard = Dropped(dropped.clone());
        work.start("peer-a".into(), async move {
            let _guard = guard;
            ready.notify_one();
            std::future::pending::<()>().await;
        });
        tokio::select! {
            _ = work.next() => panic!("blocked peer completed"),
            _ = started.notified() => {},
        }
        work.start("peer-b".into(), async {});
        tokio::time::timeout(std::time::Duration::from_secs(1), work.next())
            .await
            .unwrap();
        assert_eq!(work.active, BTreeSet::from(["peer-a".to_string()]));
        assert!(!dropped.load(Ordering::SeqCst));
        drop(work);
        assert!(dropped.load(Ordering::SeqCst));
    }
}
