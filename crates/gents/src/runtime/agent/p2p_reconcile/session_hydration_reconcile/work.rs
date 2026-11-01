use super::*;
use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use std::collections::VecDeque;

/// Bounds application request preparation across peers; native P2P owns transport concurrency.
const MAX_ACTIVE_PEERS: usize = 8;

/// The three deferred partitions, in serve order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RequestKind {
    Transcript,
    Inventory,
    Image,
}

/// Typed identity of one deferred request. Each kind keeps its own canonical
/// identity, so identical strings in different kinds never collide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RequestKey {
    Transcript {
        request_key: String,
        requester_did: String,
        node_did: String,
        session_id: String,
    },
    Inventory {
        document_id: String,
        request_key: String,
    },
    Image {
        collection_name: &'static str,
        document_id: String,
    },
}

/// One peer's deferred work. The batch's full admitted identity set is recorded
/// on `PeerWork::active` when it starts, including members not yet served.
struct PeerBatch {
    peer: String,
    work: BatchWork,
}

enum BatchWork {
    Requests(PeerRequests),
    #[cfg(test)]
    Future(BoxFuture<'static, ()>),
}

/// Deferred rows per kind, in serve order: transcripts, then inventory, then
/// images, and store order within each kind.
#[derive(Default)]
struct PeerRequests {
    transcripts: Vec<Request>,
    inventory: Vec<Request>,
    images: Vec<Request>,
}

impl PeerRequests {
    fn is_empty(&self) -> bool {
        self.transcripts.is_empty() && self.inventory.is_empty() && self.images.is_empty()
    }

    fn keys(&self) -> BTreeSet<RequestKey> {
        self.transcripts
            .iter()
            .chain(&self.inventory)
            .chain(&self.images)
            .map(Request::key)
            .collect()
    }

    fn clear(&mut self, kind: RequestKind) {
        match kind {
            RequestKind::Transcript => self.transcripts.clear(),
            RequestKind::Inventory => self.inventory.clear(),
            RequestKind::Image => self.images.clear(),
        }
    }

    fn set(&mut self, kind: RequestKind, requests: Vec<Request>) {
        match kind {
            RequestKind::Transcript => self.transcripts = requests,
            RequestKind::Inventory => self.inventory = requests,
            RequestKind::Image => self.images = requests,
        }
    }

    fn into_requests(self) -> impl Iterator<Item = Request> {
        self.transcripts
            .into_iter()
            .chain(self.inventory)
            .chain(self.images)
    }
}

impl PeerBatch {
    fn empty(peer: String) -> Self {
        Self {
            peer,
            work: BatchWork::Requests(PeerRequests::default()),
        }
    }

    fn is_empty(&self) -> bool {
        match &self.work {
            BatchWork::Requests(requests) => requests.is_empty(),
            #[cfg(test)]
            BatchWork::Future(_) => false,
        }
    }
}

#[derive(Default)]
pub(super) struct PeerWork {
    store: Option<Arc<GraphqlHydrationStore>>,
    delivery: Option<Arc<dyn HydrationDelivery>>,
    /// Peer -> every key in its started batch, including not-yet-served members.
    active: BTreeMap<String, BTreeSet<RequestKey>>,
    running: FuturesUnordered<BoxFuture<'static, String>>,
    /// At most one deferred batch per peer.
    queued: VecDeque<PeerBatch>,
    last_started: Option<String>,
    /// Test-only: the release half of each `start_test_batch` future. Holding it
    /// here keeps the future pending until the test releases that peer. Shared so
    /// a test can release a peer while `discover` borrows `work`.
    #[cfg(test)]
    test_releases: Arc<std::sync::Mutex<BTreeMap<String, tokio::sync::oneshot::Sender<()>>>>,
    /// Test-only: peers whose completion the production select arm consumed, in
    /// order, plus a signal. Shared so a test can observe consumption while
    /// `discover` still borrows `work`, without polling or sleeping.
    #[cfg(test)]
    consumed: Arc<std::sync::Mutex<Vec<String>>>,
    #[cfg(test)]
    consumed_signal: Option<Arc<tokio::sync::Notify>>,
    /// Test-only: parks the discovery loads so a completion can arrive while
    /// `discover` is inside its select loop. `discovery_reached` fires when the
    /// loads are about to park.
    #[cfg(test)]
    discovery_gate: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg(test)]
    discovery_reached: Option<Arc<tokio::sync::Notify>>,
}

impl PeerWork {
    pub(super) fn is_empty(&self) -> bool {
        self.running.is_empty() && self.queued.is_empty()
    }

    /// Starts one batch. The future is built here, never while the batch waits.
    /// Returns the batch back when the peer is already active or the cap is full.
    fn start(&mut self, batch: PeerBatch) -> std::result::Result<(), PeerBatch> {
        if self.active.contains_key(&batch.peer) || self.running.len() >= MAX_ACTIVE_PEERS {
            return Err(batch);
        }
        let PeerBatch { peer, work } = batch;
        let keys = match &work {
            BatchWork::Requests(requests) => requests.keys(),
            #[cfg(test)]
            BatchWork::Future(_) => BTreeSet::new(),
        };
        let work: BoxFuture<'static, ()> = match work {
            BatchWork::Requests(requests) => match (self.store.clone(), self.delivery.clone()) {
                (Some(store), Some(delivery)) => {
                    let log_peer = peer.clone();
                    async move {
                        for request in requests.into_requests() {
                            if let Err(error) = request.serve(&store, delivery.as_ref()).await {
                                tracing::warn!(peer = %log_peer, %error, "hydration request remains pending");
                            }
                        }
                    }
                    .boxed()
                }
                // `discover` binds both before anything can be queued, so this
                // is only reachable from a test that queues a real batch without
                // a store. It is a no-op there, and a debug-build tripwire in
                // production.
                _ => {
                    debug_assert!(
                        cfg!(test),
                        "a deferred batch requires the reconciler's store and delivery"
                    );
                    async {}.boxed()
                }
            },
            #[cfg(test)]
            BatchWork::Future(work) => work,
        };
        self.last_started = Some(peer.clone());
        self.active.insert(peer.clone(), keys);
        self.running.push(
            async move {
                work.await;
                peer
            }
            .boxed(),
        );
        Ok(())
    }

    /// One queue-length-bounded pass. A peer that is already active is skipped
    /// and restored at the front in its original order, so a full batch never
    /// loses or reorders the work behind it. A failed start restores that batch
    /// at the front and stops the pass.
    fn fill_slots(&mut self) {
        let mut skipped: VecDeque<PeerBatch> = VecDeque::new();
        for _ in 0..self.queued.len() {
            let Some(batch) = self.queued.pop_front() else {
                break;
            };
            if self.active.contains_key(&batch.peer) || self.running.len() >= MAX_ACTIVE_PEERS {
                skipped.push_back(batch);
                continue;
            }
            if let Err(batch) = self.start(batch) {
                self.queued.push_front(batch);
                break;
            }
        }
        while let Some(batch) = skipped.pop_back() {
            self.queued.push_front(batch);
        }
    }

    /// Releases a completed peer's ownership and records its batch keys in the
    /// caller's discovery-local suppression set. Called synchronously in the
    /// select arm that observed the completion, before any further await: the
    /// yielded peer is already gone from `running`, so leaving it in `active`
    /// would orphan it and `enqueue` would refuse it forever.
    fn consume_completion(&mut self, peer: &str, completed_suppress: &mut BTreeSet<RequestKey>) {
        if let Some(keys) = self.active.remove(peer) {
            completed_suppress.extend(keys);
        }
        #[cfg(test)]
        {
            self.consumed
                .lock()
                .expect("consumed lock")
                .push(peer.to_owned());
            if let Some(signal) = &self.consumed_signal {
                signal.notify_one();
            }
        }
    }

    /// Applies one observation synchronously, in memory, with no await.
    ///
    /// A successful kind replaces its partition, including an empty one; a kind
    /// that failed to load is absent from `observed` and keeps the rows it
    /// already had. Every identity already attempted by a running batch, plus
    /// the ones this observation consumed, is excluded globally across peers.
    /// Vec order and duplicates are preserved, empty batches are dropped, and
    /// the queue is rotated after `last_started` when any kind succeeded.
    fn apply_observation(
        &mut self,
        observed: BTreeMap<RequestKind, BTreeMap<String, Vec<Request>>>,
        completed_suppress: &BTreeSet<RequestKey>,
    ) {
        if observed.is_empty() {
            return;
        }
        let excluded: BTreeSet<RequestKey> = completed_suppress
            .iter()
            .cloned()
            .chain(self.active.values().flatten().cloned())
            .collect();
        let mut batches: BTreeMap<String, PeerBatch> = self
            .queued
            .drain(..)
            .map(|batch| (batch.peer.clone(), batch))
            .collect();
        for (kind, by_peer) in observed {
            for batch in batches.values_mut() {
                match &mut batch.work {
                    BatchWork::Requests(requests) => requests.clear(kind),
                    #[cfg(test)]
                    BatchWork::Future(_) => {}
                }
            }
            for (peer, requests) in by_peer {
                let requests: Vec<Request> = requests
                    .into_iter()
                    .filter(|request| !excluded.contains(&request.key()))
                    .collect();
                if requests.is_empty() {
                    continue;
                }
                let batch = batches
                    .entry(peer.clone())
                    .or_insert_with(|| PeerBatch::empty(peer));
                match &mut batch.work {
                    BatchWork::Requests(queued) => queued.set(kind, requests),
                    #[cfg(test)]
                    BatchWork::Future(_) => {}
                }
            }
        }
        batches.retain(|_, batch| !batch.is_empty());
        let mut ordered: Vec<PeerBatch> = batches.into_values().collect();
        if let Some(last) = self.last_started.as_deref() {
            let offset = ordered.partition_point(|batch| batch.peer.as_str() <= last);
            ordered.rotate_left(offset);
        }
        self.queued = ordered.into();
    }

    pub(super) async fn next(&mut self) {
        self.fill_slots();
        if let Some(peer) = self.running.next().await {
            self.active.remove(&peer);
            self.fill_slots();
        }
    }

    #[cfg(test)]
    fn start_test_future<F>(&mut self, peer: String, work: F) -> bool
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.start(PeerBatch {
            peer,
            work: BatchWork::Future(work.boxed()),
        })
        .is_ok()
    }

    #[cfg(test)]
    fn enqueue_test_future<F>(&mut self, peer: String, work: F) -> bool
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if self.active.contains_key(&peer) || self.queued.iter().any(|queued| queued.peer == peer) {
            return false;
        }
        self.queued.push_back(PeerBatch {
            peer,
            work: BatchWork::Future(work.boxed()),
        });
        true
    }

    #[cfg(test)]
    fn active_peers(&self) -> BTreeSet<String> {
        self.active.keys().cloned().collect()
    }

    #[cfg(test)]
    fn queued_is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    /// Peers whose deferred batch is waiting in the queue, in queue order.
    #[cfg(test)]
    fn queued_peers(&self) -> Vec<String> {
        self.queued.iter().map(|batch| batch.peer.clone()).collect()
    }

    /// The keys deferred for one peer, in serve order, for identity assertions.
    #[cfg(test)]
    fn queued_keys(&self, peer: &str) -> Vec<RequestKey> {
        self.queued
            .iter()
            .find(|batch| batch.peer == peer)
            .map(|batch| match &batch.work {
                BatchWork::Requests(requests) => requests
                    .transcripts
                    .iter()
                    .chain(&requests.inventory)
                    .chain(&requests.images)
                    .map(Request::key)
                    .collect(),
                BatchWork::Future(_) => Vec::new(),
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn last_started(&self) -> Option<&str> {
        self.last_started.as_deref()
    }

    /// Starts a batch that owns `keys` without a store behind it, so identity and
    /// fairness tests can occupy `active` deterministically. The future stays
    /// pending until `release_test_peer` fires, which is what a real completion
    /// looks like to `next`.
    #[cfg(test)]
    fn start_test_batch(&mut self, peer: String, keys: BTreeSet<RequestKey>) -> bool {
        if self.active.contains_key(&peer) || self.running.len() >= MAX_ACTIVE_PEERS {
            return false;
        }
        self.last_started = Some(peer.clone());
        self.active.insert(peer.clone(), keys);
        let (release, released) = tokio::sync::oneshot::channel();
        self.test_releases
            .lock()
            .expect("test releases lock")
            .insert(peer.clone(), release);
        let returned = peer;
        self.running.push(
            async move {
                let _ = released.await;
                returned
            }
            .boxed(),
        );
        true
    }

    /// Completes one test batch, so `next` can drain it exactly as it drains a
    /// real completion.
    #[cfg(test)]
    fn release_test_peer(&mut self, peer: &str) -> bool {
        release_test_peer(&self.test_releases, peer)
    }
}

/// Releases one test batch through a shared handle, so a test can complete a
/// peer while `discover` still borrows `PeerWork`.
#[cfg(test)]
fn release_test_peer(
    releases: &Arc<std::sync::Mutex<BTreeMap<String, tokio::sync::oneshot::Sender<()>>>>,
    peer: &str,
) -> bool {
    releases
        .lock()
        .expect("test releases lock")
        .remove(peer)
        .is_some_and(|release| release.send(()).is_ok())
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

    /// The kind-tagged canonical identity of this request. Transcript identity is
    /// the full four-field tuple the terminal compare-and-set uses; inventory is
    /// its physical document plus immutable request key; image is its collection
    /// plus physical document.
    fn key(&self) -> RequestKey {
        match self {
            Self::Transcript(row) => RequestKey::Transcript {
                request_key: row.request_key.clone(),
                requester_did: row.requester_did.clone(),
                node_did: row.node_did.clone(),
                session_id: row.session_id.clone(),
            },
            Self::Inventory(row) => RequestKey::Inventory {
                document_id: row.document_id.clone(),
                request_key: row.request_key.clone(),
            },
            Self::Image(row) => RequestKey::Image {
                collection_name: row.demand.collection_name(),
                document_id: row.document_id().to_owned(),
            },
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
    // Deferred batches build their future only when they start, so the
    // reconciler's store and delivery are bound here, once per observation.
    work.store = Some(store.clone());
    work.delivery = Some(delivery.clone());
    // Test-only: park the loads so a completion can arrive while this
    // observation is inside its select loop. Production never sets these.
    #[cfg(test)]
    let (gate, reached) = (work.discovery_gate.take(), work.discovery_reached.clone());
    let discovery = async {
        #[cfg(test)]
        {
            if let Some(reached) = reached {
                reached.notify_one();
            }
            if let Some(gate) = gate {
                let _ = gate.await;
            }
        }
        tokio::join!(
            store.load_pending_requests(),
            inventory_requests::pending_requests(store),
            file_images::pending_requests(store),
        )
    };
    tokio::pin!(discovery);
    // Keys of batches whose completion this observation consumed. Local to this
    // invocation: a cancel or drop discards it, and every consumed peer is
    // already out of `active`, so no peer is orphaned. Not-yet-polled peers stay
    // paired in `running` and `active`.
    let mut completed_suppress: BTreeSet<RequestKey> = BTreeSet::new();
    let (transcripts, inventory, images) = loop {
        let completed = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            result = &mut discovery => break result,
            peer = work.running.next(), if !work.running.is_empty() => peer,
        };
        if let Some(peer) = completed {
            work.consume_completion(&peer, &mut completed_suppress);
        }
    };
    // A successful kind replaces its partition, including an empty one; a failed
    // kind keeps the rows it already had. The three loads are not one snapshot.
    let mut observed: BTreeMap<RequestKind, BTreeMap<String, Vec<Request>>> = BTreeMap::new();
    let mut collect = |kind: RequestKind, requests: Vec<Request>| {
        let by_peer = observed.entry(kind).or_default();
        for request in requests {
            by_peer.entry(request.peer()).or_default().push(request);
        }
    };
    match transcripts {
        Ok(rows) => collect(
            RequestKind::Transcript,
            rows.into_iter()
                .map(Box::new)
                .map(Request::Transcript)
                .collect(),
        ),
        Err(error) => tracing::warn!(%error, "load pending transcript requests"),
    }
    match inventory {
        Ok(rows) => collect(
            RequestKind::Inventory,
            rows.into_iter()
                .map(Box::new)
                .map(Request::Inventory)
                .collect(),
        ),
        Err(error) => tracing::warn!(%error, "load pending inventory requests"),
    }
    match images {
        Ok(rows) => collect(
            RequestKind::Image,
            rows.into_iter().map(Box::new).map(Request::Image).collect(),
        ),
        Err(error) => tracing::warn!(%error, "load pending image requests"),
    }
    work.apply_observation(observed, &completed_suppress);
    // Suppression covers this observation only; a later discovery may retry an
    // unchanged failed request as today.
    drop(completed_suppress);
    work.fill_slots();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::agent::p2p_reconcile::file_image_hydration::FileImageDemand;
    use gents_protocol::file_image_hydration::FileImageInventoryReference;
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
        assert!(work.start_test_future("peer-a".into(), std::future::pending()));
        assert!(!work.start_test_future("peer-a".into(), async {}));
        for index in 1..MAX_ACTIVE_PEERS {
            assert!(work.start_test_future(format!("peer-{index}"), std::future::pending()));
        }
        assert!(!work.start_test_future("overflow".into(), async {}));
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
            assert!(
                work.enqueue_test_future(format!("peer-{index}"), async move {
                    attempts[index].fetch_add(1, Ordering::SeqCst);
                    started_tx.send(index).unwrap();
                    if index < MAX_ACTIVE_PEERS {
                        let _ = release_rx.await;
                    }
                    let result: Result<()> = Err(anyhow::anyhow!("indeterminate delivery"));
                    assert!(result.is_err());
                })
            );
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
        work.start_test_future("peer-a".into(), async move {
            let _guard = guard;
            ready.notify_one();
            std::future::pending::<()>().await;
        });
        tokio::select! {
            _ = work.next() => panic!("blocked peer completed"),
            _ = started.notified() => {},
        }
        work.start_test_future("peer-b".into(), async {});
        tokio::time::timeout(std::time::Duration::from_secs(1), work.next())
            .await
            .unwrap();
        assert_eq!(work.active_peers(), BTreeSet::from(["peer-a".to_string()]));
        assert!(!dropped.load(Ordering::SeqCst));
        drop(work);
        assert!(dropped.load(Ordering::SeqCst));
    }

    // --- Real-store characterization of the active-peer deferral (T2) --------
    //
    // These tests characterize the CURRENT behavior of `discover` when a new
    // request for a peer arrives while an earlier attempt for that peer is
    // still `active` (`work.rs:173-176`). They are not a claim that the current
    // latency is desired, and not a claim about the original 819 failure. The
    // store is real; only delivery is faked and gated.

    use crate::runtime::agent::p2p_reconcile::enrollment_reconcile::test_enrollment_authority;
    use crate::runtime::agent::p2p_reconcile::templates::{combine_filters, equality_filter};
    use crate::store::config_client::ConfigAccess;
    use crate::store::identity::KeyIdentity;
    use crate::tools::call::lifecycle::admission_fixture::ensure_fixture_session;
    use std::time::Duration;

    const FIXTURE_CREATED_AT: &str = "2026-10-28T00:00:00Z";

    struct SchedulerFixture {
        _directory: tempfile::TempDir,
        node: Arc<EmbeddedNode>,
        store: Arc<GraphqlHydrationStore>,
        peer: String,
        requester: String,
        node_did: String,
    }

    impl SchedulerFixture {
        fn request_key(&self, session_id: &str) -> String {
            format!("{}:{session_id}", self.peer)
        }
    }

    /// A gated delivery: every push records itself, signals `started`, and then
    /// parks until the test releases it. `fail_first` makes the first push
    /// return an error, which is the indeterminate-delivery path that writes no
    /// terminal state (`session_hydration_reconcile.rs:285-295`).
    struct GatedDelivery {
        started: tokio::sync::Notify,
        releases: std::sync::Mutex<Vec<tokio::sync::oneshot::Sender<()>>>,
        pushes: std::sync::Mutex<Vec<(String, BTreeSet<HydrationDocument>)>>,
        fail_first: bool,
    }

    impl GatedDelivery {
        fn new(fail_first: bool) -> Self {
            Self {
                started: tokio::sync::Notify::new(),
                releases: std::sync::Mutex::new(Vec::new()),
                pushes: std::sync::Mutex::new(Vec::new()),
                fail_first,
            }
        }

        fn pushes(&self) -> Vec<(String, BTreeSet<HydrationDocument>)> {
            self.pushes.lock().expect("pushes lock").clone()
        }

        /// The session whose selected document each push carried. Each fixture
        /// session owns exactly one selected document, so this is the exact
        /// request identity of the attempt. Every push must be a singleton set
        /// for this peer, so a push can never be attributed to the wrong request.
        fn pushed_sessions(&self, peer: &str) -> Vec<String> {
            self.pushes()
                .into_iter()
                .map(|(pushed_peer, documents)| {
                    assert_eq!(pushed_peer, peer, "every push targets the fixture peer");
                    assert_eq!(
                        documents.len(),
                        1,
                        "each fixture session selects exactly one document"
                    );
                    let session = documents
                        .iter()
                        .next()
                        .expect("length checked")
                        .session_id
                        .clone();
                    assert!(
                        documents
                            .iter()
                            .all(|document| document.session_id == session),
                        "a push carries one request's documents"
                    );
                    session
                })
                .collect()
        }

        fn release_next(&self) {
            let release = self
                .releases
                .lock()
                .expect("releases lock")
                .pop()
                .expect("a gated push is waiting for release");
            release.send(()).expect("gated delivery still waiting");
        }
    }

    #[async_trait]
    impl HydrationDelivery for GatedDelivery {
        async fn push_documents_to_peer(
            &self,
            peer_id: &str,
            documents: DeliveryTarget<'_>,
            _fence: &EnrollmentAuthorizationFence,
        ) -> Result<()> {
            let documents = documents.transcript();
            let attempt = {
                let mut pushes = self.pushes.lock().expect("pushes lock");
                pushes.push((peer_id.to_string(), documents));
                pushes.len()
            };
            let (release, released) = tokio::sync::oneshot::channel();
            self.releases.lock().expect("releases lock").push(release);
            self.started.notify_one();
            let _ = released.await;
            if self.fail_first && attempt == 1 {
                anyhow::bail!("injected indeterminate delivery");
            }
            Ok(())
        }
    }

    /// Every case is wrapped in this bound so a stuck scheduler fails the test
    /// instead of hanging it. It is a termination guard, never the ordering
    /// mechanism: ordering comes from the explicit `Notify`/oneshot barriers.
    ///
    /// Limitation: the bound ends the case's future, not the work it started.
    /// A `spawn_blocking` fixture task or a spawned enrollment-authority task
    /// outlives an expired case, so a timeout reports a failure without
    /// guaranteeing that everything the case started has been cleaned up.
    const CASE_TIMEOUT: Duration = Duration::from_secs(60);
    const STEP_TIMEOUT: Duration = Duration::from_secs(5);

    async fn bounded<F: std::future::Future>(label: &str, future: F) -> F::Output {
        tokio::time::timeout(CASE_TIMEOUT, future)
            .await
            .unwrap_or_else(|_| panic!("{label} exceeded {CASE_TIMEOUT:?}"))
    }

    /// Poll the scheduler until the gated delivery signals that an attempt has
    /// actually started. `discover` only enqueues; `fill_slots` pushes the
    /// future into `running`, and `FuturesUnordered` does not poll it until the
    /// scheduler is polled again.
    async fn drive_until_started(work: &mut PeerWork, delivery: &GatedDelivery) {
        tokio::time::timeout(STEP_TIMEOUT, async {
            tokio::select! {
                _ = work.next() => panic!("attempt completed before signalling start"),
                _ = delivery.started.notified() => {}
            }
        })
        .await
        .expect("the gated attempt starts within the bound");
    }

    /// One scheduler step that must complete: a completion, or an idle `next`.
    async fn next_within(work: &mut PeerWork, label: &str) {
        tokio::time::timeout(STEP_TIMEOUT, work.next())
            .await
            .unwrap_or_else(|_| panic!("{label} did not return within the bound"));
    }

    /// Drain one coalesced subscription batch, so a later "no wake" assertion
    /// is about the write under test and not about an earlier one.
    async fn drain_batch(changes: &mut events::DocumentChangeSubscription, label: &str) {
        tokio::time::timeout(STEP_TIMEOUT, changes.recv())
            .await
            .unwrap_or_else(|_| panic!("{label} did not wake the subscription"))
            .expect("subscription stays open");
    }

    /// Observation, not proof: `try_recv` reports only that no batch is pending
    /// right now. A notification published later would not be seen here.
    fn assert_no_pending_wake(changes: &mut events::DocumentChangeSubscription, label: &str) {
        assert!(
            matches!(changes.try_recv(), Err(events::TryRecvError::Empty)),
            "{label} left a subscription wake pending at this observation"
        );
    }

    async fn scheduler_fixture() -> SchedulerFixture {
        let (directory, identity, requester_identity) =
            tokio::task::spawn_blocking(|| -> Result<_> {
                let directory = tempfile::tempdir()?;
                let identity: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
                    directory.path().join("node.key"),
                    None,
                )?);
                let requester =
                    KeyIdentity::load_or_create(directory.path().join("requester.key"), None)?;
                Ok((directory, identity, requester))
            })
            .await
            .expect("fixture identity task")
            .expect("fixture identity");
        let node_did = identity.did().to_owned();
        let requester = requester_identity.did().to_owned();
        let peer = "peer-1".to_owned();
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(directory.path().join("store"))
                .with_node_identity_did(identity.did())
                .build()
                .await
                .expect("build fixture node"),
        );
        crate::ensure_runtime_schemas(&node)
            .await
            .expect("runtime schemas");
        for session in ["session-a", "session-b"] {
            ensure_fixture_session(
                &node,
                session,
                &node_did,
                Some(&requester),
                FIXTURE_CREATED_AT,
            )
            .await;
        }
        let fence = EnrollmentAuthorizationFence {
            network_id: "network-1".into(),
            request_id: "request-1".into(),
            admin_did: "did:key:admin-1".into(),
            member_did: requester.clone(),
            member_peer: peer.clone(),
            member_ticket: "ticket-1".into(),
            owner_node: node_did.clone(),
            profile: crate::runtime::agent::p2p_reconcile::templates::CLIENT_TEMPLATE.into(),
            request_digest: "digest-1".into(),
            authorization_sequence: 1,
            authorization_expires_at: "2099-09-29T00:00:00Z".into(),
        };
        let peer_literal = escape_graphql_string(&peer);
        let node_literal = escape_graphql_string(&node_did);
        let digest = escape_graphql_string(&fence.request_digest);
        let expires = escape_graphql_string(&fence.authorization_expires_at);
        let fields = format!(
            r#"node_did: "{node_literal}", enrollment_request_digest: "{digest}", enrollment_authorization_sequence: 1, enrollment_authorization_expires_at: "{expires}""#
        );
        ConfigAccess::write_local(
            &node,
            "test.hydration.desired_generation",
            &format!(
                r#"mutation {{ upsert_PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_literal}" }} }}, add: {{ peer_id: "{peer_literal}", source: "enrollment", collections: ["SessionHydrationRequest"], replicator_addresses: ["test-address"], {fields} }}, update: {{ source: "enrollment", {fields} }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .expect("seed desired pairing route");
        let filters = BTreeMap::from([(
            "SessionHydrationRequest".to_owned(),
            combine_filters(
                equality_filter("requester_did", requester.clone()),
                equality_filter("node_did", node_did.clone()),
            ),
        )]);
        let filter =
            escape_graphql_string(&serde_json::to_string(&filters).expect("pairing filter json"));
        ConfigAccess::write_local(
            &node,
            "test.hydration.applied_generation",
            &format!(
                r#"mutation {{ create_PeerPairingApplied(input: {{ peer_id: "{peer_literal}", replicator_filter: "{filter}", enrollment_request_digest: "{digest}", enrollment_authorization_sequence: 1, enrollment_authorization_expires_at: "{expires}" }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .expect("seed applied pairing route");
        let (_authority, enrollment) = test_enrollment_authority(Some(fence));
        let store = Arc::new(GraphqlHydrationStore {
            node: node.clone(),
            identity,
            enrollment,
        });
        let fixture = SchedulerFixture {
            _directory: directory,
            node,
            store,
            peer,
            requester,
            node_did,
        };
        for session in ["session-a", "session-b"] {
            seed_selected_document(&fixture, session).await;
        }
        fixture
    }

    /// One selected document per session, so a push identifies its request.
    async fn seed_selected_document(fixture: &SchedulerFixture, session_id: &str) {
        let key = escape_graphql_string(&format!("compaction-{session_id}"));
        let session = escape_graphql_string(session_id);
        let node_did = escape_graphql_string(&fixture.node_did);
        let requester = escape_graphql_string(&fixture.requester);
        ConfigAccess::write_local(
            &fixture.node,
            "test.hydration.seed_selected_document",
            &format!(
                r#"mutation {{ create_CompactionEntry(input: {{ compaction_key: "{key}", session_id: "{session}", node_did: "{node_did}", requester_did: "{requester}", sequence: 1 }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .expect("seed selected document");
    }

    async fn seed_pending_request(fixture: &SchedulerFixture, session_id: &str) {
        let request_key = escape_graphql_string(&fixture.request_key(session_id));
        let session = escape_graphql_string(session_id);
        let node_did = escape_graphql_string(&fixture.node_did);
        let requester = escape_graphql_string(&fixture.requester);
        ConfigAccess::write_local(
            &fixture.node,
            "test.hydration.seed_pending_request",
            &format!(
                r#"mutation {{ create_SessionHydrationRequest(input: {{ request_key: "{request_key}", requester_did: "{requester}", node_did: "{node_did}", session_id: "{session}", created_at: "{FIXTURE_CREATED_AT}", status: "pending" }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .expect("seed pending hydration request");
    }

    async fn request_status(fixture: &SchedulerFixture, session_id: &str) -> String {
        let request_key = escape_graphql_string(&fixture.request_key(session_id));
        let response = ConfigAccess::local(fixture.node.clone())
            .execute(&format!(
                r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key}" }} }}) {{ status }} }}"#,
            ))
            .await
            .expect("read hydration request status");
        let rows = response["data"]["SessionHydrationRequest"]
            .as_array()
            .expect("SessionHydrationRequest rows");
        assert_eq!(rows.len(), 1, "one row per request key");
        rows[0]["status"]
            .as_str()
            .expect("hydration request status")
            .to_owned()
    }

    /// Read-only precondition: the seeded request is admitted by the real store
    /// before the scheduler observes it, so a deferral case cannot pass on a
    /// request the store would have rejected. It writes nothing.
    async fn assert_initial_admission(store: &GraphqlHydrationStore) {
        let (request, admission) = tokio::time::timeout(STEP_TIMEOUT, async {
            let rows = store
                .load_pending_requests()
                .await
                .expect("fixture pending requests");
            assert_eq!(rows.len(), 1, "one pending request before discovery");
            let row = rows.into_iter().next().expect("length checked");
            let request = HydrationRequest::from_row(
                row.request_key.clone(),
                row.requester_did.clone(),
                row.node_did.clone(),
                row.session_id.clone(),
            )
            .expect("fixture request key names its peer and session");
            let admission = store
                .load_admission(&request)
                .await
                .expect("fixture admission query");
            (request, admission)
        })
        .await
        .expect("fixture admission within the bound");
        assert!(
            matches!(
                decide_hydration(&request, &admission.catalog),
                HydrationVerdict::Admit(_)
            ),
            "the seeded request must be admitted before the scheduler observes it"
        );
        assert!(
            admission.authorization.is_some(),
            "the seeded request must carry a current enrollment authorization"
        );
    }

    /// MIGRATED from `a_new_request_for_an_active_peer_is_deferred_until_the_next_discovery`
    /// (a442 characterization, kept as a historical receipt). A confirmed attempt
    /// for A, then a distinct request B for the same peer committed while A is
    /// still active. The discovery during A's attempt defers B; completing A now
    /// serves B with no further discovery, and A is not retried.
    ///
    /// Assertion changes from the characterization: the post-completion
    /// `queued.is_empty()` / `active.is_empty()` / "B stays pending" / "an
    /// explicit discovery is required" block is replaced by "B starts on
    /// completion" and "B is served". The pre-completion deferral assertions
    /// (no enqueue while the peer is active, no second concurrent attempt) are
    /// unchanged.
    #[tokio::test]
    async fn a_new_request_for_an_active_peer_is_served_after_the_active_attempt_completes() {
        bounded(
            "a_new_request_for_an_active_peer_is_served_after_the_active_attempt_completes",
            async {
                let fixture = scheduler_fixture().await;
                let store = fixture.store.clone();
                let delivery = Arc::new(GatedDelivery::new(false));
                let delivery_trait: Arc<dyn HydrationDelivery> = delivery.clone();
                let cancel = CancellationToken::new();
                let mut work = PeerWork::default();

                seed_pending_request(&fixture, "session-a").await;
                assert_initial_admission(&store).await;
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                drive_until_started(&mut work, &delivery).await;
                assert_eq!(work.active_peers(), BTreeSet::from([fixture.peer.clone()]));
                assert_eq!(delivery.pushes().len(), 1);
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string()]
                );

                // A distinct request for the same peer arrives while A is active.
                seed_pending_request(&fixture, "session-b").await;
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                assert_eq!(
                    work.queued_peers(),
                    vec![fixture.peer.clone()],
                    "the new row is deferred, not started, while its peer is active"
                );
                assert_eq!(work.active_peers(), BTreeSet::from([fixture.peer.clone()]));
                assert_eq!(
                    delivery.pushes().len(),
                    1,
                    "no second attempt for the same peer"
                );

                // Complete A explicitly: the delivery confirms and the terminal write lands.
                delivery.release_next();
                next_within(&mut work, "A completes").await;
                assert_eq!(request_status(&fixture, "session-a").await, "served");
                assert_eq!(
                    request_status(&fixture, "session-b").await,
                    "pending",
                    "B is deferred, not served, while A's attempt is in flight"
                );

                // Completion alone starts the deferred B: no discovery, no store
                // write, no retry of A.
                drive_until_started(&mut work, &delivery).await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string(), "session-b".to_string()],
                    "the deferred attempt serves B, not a retry of A"
                );
                delivery.release_next();
                next_within(&mut work, "B completes").await;
                assert_eq!(request_status(&fixture, "session-b").await, "served");
                assert_eq!(delivery.pushes().len(), 2, "A was attempted exactly once");

                // Idle again: nothing left to start.
                next_within(&mut work, "idle next").await;
                assert_eq!(delivery.pushes().len(), 2, "no third attempt");

                fixture.node.shutdown().await;
            },
        )
        .await;
    }

    /// MIGRATED from `a_failed_attempt_without_a_terminal_write_still_defers_the_new_request`
    /// (a442 characterization, kept as a historical receipt). The same deferral
    /// when A's attempt writes no terminal state, now with the desired outcome:
    /// completing the failed A serves the deferred B without retrying A. A later
    /// explicit discovery may still retry the unchanged pending A, as today.
    ///
    /// Assertion changes from the characterization: the post-completion block
    /// that required "B stays pending until an explicit discovery" and "the
    /// discovery serves B and retries A in one batch" is replaced by "completion
    /// serves B alone" followed by "a later discovery retries A alone". The
    /// lifetime multiset is unchanged (A twice, B once) but the order is now
    /// asserted: A, B, then A again.
    ///
    /// A real subscription is created before the first discovery, and each
    /// insert's batch is drained explicitly before the discovery that follows
    /// it, so the test does not show the discovery consuming a wake. No further
    /// wake is pending at the observation after A fails. It does not claim the
    /// subscription is lossless.
    #[tokio::test]
    async fn a_failed_attempt_without_a_terminal_write_serves_the_new_request_after_completion() {
        bounded(
            "a_failed_attempt_without_a_terminal_write_serves_the_new_request_after_completion",
            async {
                let fixture = scheduler_fixture().await;
                let store = fixture.store.clone();
                let delivery = Arc::new(GatedDelivery::new(true));
                let delivery_trait: Arc<dyn HydrationDelivery> = delivery.clone();
                let cancel = CancellationToken::new();
                let mut work = PeerWork::default();
                let mut changes =
                    crate::subscribe_collections(&fixture.node, ["SessionHydrationRequest"])
                        .expect("subscribe to hydration requests");

                seed_pending_request(&fixture, "session-a").await;
                drain_batch(&mut changes, "A's insert").await;
                assert_initial_admission(&store).await;
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                drive_until_started(&mut work, &delivery).await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string()],
                    "the first attempt serves A"
                );

                seed_pending_request(&fixture, "session-b").await;
                drain_batch(&mut changes, "B's insert").await;
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                assert_eq!(
                    work.queued_peers(),
                    vec![fixture.peer.clone()],
                    "the new row is deferred, not started, while its peer is active"
                );
                assert_eq!(delivery.pushes().len(), 1);

                // A finishes with no terminal write and no new store write.
                delivery.release_next();
                next_within(&mut work, "A completes").await;
                assert_eq!(
                    request_status(&fixture, "session-a").await,
                    "pending",
                    "an indeterminate delivery writes no terminal state"
                );
                assert_eq!(request_status(&fixture, "session-b").await, "pending");
                assert_no_pending_wake(&mut changes, "A's failed attempt");

                // Completion alone serves the deferred B and does not retry A.
                drive_until_started(&mut work, &delivery).await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string(), "session-b".to_string()],
                    "completion serves B, not a retry of A"
                );
                delivery.release_next();
                next_within(&mut work, "B completes").await;
                assert_eq!(request_status(&fixture, "session-b").await, "served");
                assert_eq!(
                    request_status(&fixture, "session-a").await,
                    "pending",
                    "B's success does not serve A"
                );

                // Idle: no completion-driven retry of the still-pending A.
                next_within(&mut work, "idle next").await;
                assert_eq!(
                    delivery.pushes().len(),
                    2,
                    "A is not retried without a discovery"
                );

                // A later explicit discovery legitimately retries the unchanged
                // pending A. The lifetime multiset stays A twice and B once.
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                drive_until_started(&mut work, &delivery).await;
                delivery.release_next();
                next_within(&mut work, "the retried A completes").await;
                assert_eq!(request_status(&fixture, "session-a").await, "served");
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec![
                        "session-a".to_string(),
                        "session-b".to_string(),
                        "session-a".to_string()
                    ],
                    "A twice (initial attempt plus discovery retry), B once"
                );

                fixture.node.shutdown().await;
            },
        )
        .await;
    }

    /// Desired behavior, not current behavior. A request B observed during A's
    /// failed attempt must run once A's attempt completes, with no further
    /// discovery, store write, retry or other traffic. On the unchanged
    /// scheduler this is expected to fail: `discover` skips a peer that is
    /// already active, and a complete snapshot clears the queue, so B is
    /// dropped and completion alone never starts it. Neither the internal queue
    /// nor the active set is asserted, so a fix may hold B anywhere — including
    /// scheduling it from `next` before its future is polled — as long as it
    /// runs.
    #[tokio::test]
    async fn a_new_request_observed_during_a_failed_attempt_runs_after_completion_without_rediscovery(
    ) {
        bounded(
            "a_new_request_observed_during_a_failed_attempt_runs_after_completion_without_rediscovery",
            async {
                let fixture = scheduler_fixture().await;
                let store = fixture.store.clone();
                let delivery = Arc::new(GatedDelivery::new(true));
                let delivery_trait: Arc<dyn HydrationDelivery> = delivery.clone();
                let cancel = CancellationToken::new();
                let mut work = PeerWork::default();
                let mut changes =
                    crate::subscribe_collections(&fixture.node, ["SessionHydrationRequest"])
                        .expect("subscribe to hydration requests");

                seed_pending_request(&fixture, "session-a").await;
                drain_batch(&mut changes, "A's insert").await;
                assert_initial_admission(&store).await;
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                drive_until_started(&mut work, &delivery).await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string()],
                    "the first attempt serves A"
                );

                // B is observed while A is active.
                seed_pending_request(&fixture, "session-b").await;
                drain_batch(&mut changes, "B's insert").await;
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string()],
                    "the discovery during A's attempt pushes nothing new"
                );

                // A finishes with no terminal write and no new store write.
                delivery.release_next();
                next_within(&mut work, "A completes").await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string()],
                    "completion alone does not retry A"
                );
                assert_eq!(
                    request_status(&fixture, "session-a").await,
                    "pending",
                    "an indeterminate delivery writes no terminal state"
                );
                assert_eq!(request_status(&fixture, "session-b").await, "pending");
                assert_no_pending_wake(&mut changes, "A's failed attempt");

                // No discovery, store write, retry or other traffic from here:
                // the scheduler alone must start the request it already observed.
                drive_until_started(&mut work, &delivery).await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string(), "session-b".to_string()],
                    "B runs after A's completion, and A is not retried"
                );

                delivery.release_next();
                next_within(&mut work, "B completes").await;
                assert_eq!(request_status(&fixture, "session-b").await, "served");
                assert_eq!(
                    request_status(&fixture, "session-a").await,
                    "pending",
                    "B's success does not serve A"
                );
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string(), "session-b".to_string()],
                    "exactly two attempts: A once, B once"
                );

                // Idle again: no third attempt and no A retry.
                next_within(&mut work, "idle next").await;
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string(), "session-b".to_string()],
                    "no third attempt and no A retry after B completes"
                );

                fixture.node.shutdown().await;
            },
        )
        .await;
    }

    // --- Scheduler identity, per-kind replacement and fairness ----------------
    //
    // These drive `PeerWork` directly through the seams `discover` uses, so the
    // bookkeeping is asserted without a store round trip. They are not a claim
    // about the original 819 failure.

    fn transcript_key(request_key: &str, session_id: &str) -> RequestKey {
        RequestKey::Transcript {
            request_key: request_key.to_owned(),
            requester_did: "did:key:requester-1".into(),
            node_did: "did:key:agent-1".into(),
            session_id: session_id.to_owned(),
        }
    }

    fn transcript_request(request_key: &str, session_id: &str) -> Request {
        Request::Transcript(Box::new(HydrationRequestRow {
            request_key: request_key.to_owned(),
            requester_did: "did:key:requester-1".into(),
            node_did: "did:key:agent-1".into(),
            session_id: session_id.to_owned(),
        }))
    }

    fn inventory_request(document_id: &str, request_key: &str) -> Request {
        Request::Inventory(Box::new(
            crate::file_inventory::requests::InventoryRequestRow {
                document_id: document_id.to_owned(),
                request_key: request_key.to_owned(),
                peer_id: "peer-1".into(),
                requester_did: "did:key:requester-1".into(),
                node_did: "did:key:agent-1".into(),
                session_id: "session-1".into(),
                demand: inventory_demand(),
                status: "pending".into(),
                response: None,
            },
        ))
    }

    fn inventory_demand() -> gents_protocol::file_inventory::FileInventoryPageDemand {
        gents_protocol::file_inventory::FileInventoryPageDemand {
            admission: gents_protocol::file_inventory::FileInventoryAdmission {
                request: HydrationRequest {
                    request_key: "inventory-request".into(),
                    peer_id: "peer-1".into(),
                    requester_did: "did:key:requester-1".into(),
                    node_did: "did:key:agent-1".into(),
                    session_id: "session-1".into(),
                },
                session_doc_id: "session-doc".into(),
            },
            attempt_id: "attempt-1".into(),
            cursor: None,
        }
    }

    fn image_request(document_id: &str, bound: bool) -> Request {
        let mut demand = FileImageDemand {
            inventory_reference: None,
            admission: HydrationRequest {
                request_key: "image-request".into(),
                peer_id: "peer-1".into(),
                requester_did: "did:key:requester-1".into(),
                node_did: "did:key:agent-1".into(),
                session_id: "session-1".into(),
            },
            request_doc_id: "request-doc".into(),
            tool_call_doc_id: "tool-call-doc".into(),
        };
        if bound {
            demand.inventory_reference = Some(FileImageInventoryReference {
                page: inventory_demand(),
                snapshot_digest: "snapshot".into(),
                entry_index: 0,
            });
        }
        Request::Image(Box::new(file_images::ImageDemandRow::from_parts(
            document_id.to_owned(),
            demand,
        )))
    }

    fn observation(
        entries: Vec<(RequestKind, &str, Vec<Request>)>,
    ) -> BTreeMap<RequestKind, BTreeMap<String, Vec<Request>>> {
        let mut observed: BTreeMap<RequestKind, BTreeMap<String, Vec<Request>>> = BTreeMap::new();
        for (kind, peer, requests) in entries {
            observed
                .entry(kind)
                .or_default()
                .entry(peer.to_owned())
                .or_default()
                .extend(requests);
        }
        observed
    }

    /// Identical strings in different kinds are different identities: a
    /// transcript key never suppresses an inventory or image row that happens to
    /// carry the same text.
    #[test]
    fn identical_strings_in_different_kinds_do_not_collide() {
        let shared = "peer-1:session-1";
        let transcript = transcript_key(shared, "session-1");
        let inventory = RequestKey::Inventory {
            document_id: shared.into(),
            request_key: shared.into(),
        };
        let image = RequestKey::Image {
            collection_name: gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME,
            document_id: shared.into(),
        };
        assert_ne!(transcript, inventory);
        assert_ne!(transcript, image);
        assert_ne!(inventory, image);

        // An active transcript batch suppresses only the transcript identity.
        let mut work = PeerWork::default();
        assert!(work.start_test_batch("peer-1".into(), BTreeSet::from([transcript.clone()])));
        work.apply_observation(
            observation(vec![
                (
                    RequestKind::Transcript,
                    "peer-1",
                    vec![transcript_request(shared, "session-1")],
                ),
                (
                    RequestKind::Inventory,
                    "peer-1",
                    vec![inventory_request(shared, shared)],
                ),
                (
                    RequestKind::Image,
                    "peer-1",
                    vec![image_request(shared, false)],
                ),
            ]),
            &BTreeSet::new(),
        );
        assert_eq!(
            work.queued_keys("peer-1"),
            vec![inventory, image],
            "only the transcript identity was suppressed by the active batch"
        );
    }

    /// A batch's full admitted identity set is active, so a later member that has
    /// not started yet is not re-served merely because it is still pending.
    #[test]
    fn an_active_batch_excludes_its_not_yet_served_members() {
        let mut work = PeerWork::default();
        assert!(work.start_test_batch(
            "peer-1".into(),
            BTreeSet::from([
                transcript_key("peer-1:session-a", "session-a"),
                transcript_key("peer-1:session-b", "session-b"),
            ])
        ));
        work.apply_observation(
            observation(vec![(
                RequestKind::Transcript,
                "peer-1",
                vec![
                    transcript_request("peer-1:session-a", "session-a"),
                    transcript_request("peer-1:session-b", "session-b"),
                ],
            )]),
            &BTreeSet::new(),
        );
        assert!(
            work.queued_is_empty(),
            "both members of the running batch are excluded, not just the served one"
        );
    }

    /// A successful kind replaces its partition, including an empty result, so a
    /// staged row that the store no longer reports is dropped rather than kept.
    #[test]
    fn a_successful_empty_partition_removes_staged_work() {
        let mut work = PeerWork::default();
        work.apply_observation(
            observation(vec![(
                RequestKind::Transcript,
                "peer-1",
                vec![transcript_request("peer-1:session-a", "session-a")],
            )]),
            &BTreeSet::new(),
        );
        assert_eq!(work.queued_peers(), vec!["peer-1".to_string()]);

        // The next successful transcript observation reports nothing for peer-1.
        work.apply_observation(
            observation(vec![(RequestKind::Transcript, "peer-1", Vec::new())]),
            &BTreeSet::new(),
        );
        assert!(
            work.queued_is_empty(),
            "a successful empty transcript partition drops the staged row"
        );
    }

    /// A failed kind retains its partition while a successful kind replaces its
    /// own, so a partial observation never masquerades as an empty one.
    #[test]
    fn a_failed_kind_retains_its_partition_while_a_successful_kind_replaces() {
        let mut work = PeerWork::default();
        work.apply_observation(
            observation(vec![
                (
                    RequestKind::Transcript,
                    "peer-1",
                    vec![transcript_request("peer-1:session-a", "session-a")],
                ),
                (
                    RequestKind::Inventory,
                    "peer-1",
                    vec![inventory_request("inventory-doc", "inventory-key")],
                ),
            ]),
            &BTreeSet::new(),
        );
        assert_eq!(work.queued_keys("peer-1").len(), 2);

        // Only the inventory load succeeded, and it reports nothing.
        work.apply_observation(
            observation(vec![(RequestKind::Inventory, "peer-1", Vec::new())]),
            &BTreeSet::new(),
        );
        assert_eq!(
            work.queued_keys("peer-1"),
            vec![transcript_key("peer-1:session-a", "session-a")],
            "the failed transcript kind kept its row; the successful inventory kind replaced its own"
        );
    }

    /// Unit transformation, not cancellation coverage: `consume_completion` plus
    /// `apply_observation` suppress a consumed batch's own identities for that
    /// observation only, so a failed attempt is not retried by its own completion.
    #[test]
    fn a_consumed_completion_suppresses_its_own_batch_without_a_retry() {
        let key = transcript_key("peer-1:session-a", "session-a");
        let mut work = PeerWork::default();
        assert!(work.start_test_batch("peer-1".into(), BTreeSet::from([key.clone()])));
        let mut suppress = BTreeSet::new();
        work.consume_completion("peer-1", &mut suppress);
        assert_eq!(suppress, BTreeSet::from([key.clone()]));
        assert!(
            work.active_peers().is_empty(),
            "the completed peer is released"
        );

        work.apply_observation(
            observation(vec![(
                RequestKind::Transcript,
                "peer-1",
                vec![transcript_request("peer-1:session-a", "session-a")],
            )]),
            &suppress,
        );
        assert!(
            work.queued_is_empty(),
            "the still-pending row is not requeued by its own completion"
        );

        // A later observation with no suppression may retry it, as today.
        work.apply_observation(
            observation(vec![(
                RequestKind::Transcript,
                "peer-1",
                vec![transcript_request("peer-1:session-a", "session-a")],
            )]),
            &BTreeSet::new(),
        );
        assert_eq!(work.queued_keys("peer-1"), vec![key]);
    }

    /// Unit transformation: releasing one peer's ownership leaves another peer's
    /// ownership intact. The real cancel/drop path is covered by
    /// `cancelling_a_discovery_after_a_completion_leaves_no_orphan`.
    #[test]
    fn releasing_one_peer_keeps_another_peers_ownership() {
        let first = transcript_key("peer-1:session-a", "session-a");
        let second = transcript_key("peer-2:session-b", "session-b");
        let mut work = PeerWork::default();
        assert!(work.start_test_batch("peer-1".into(), BTreeSet::from([first])));
        assert!(work.start_test_batch("peer-2".into(), BTreeSet::from([second.clone()])));
        let mut suppress = BTreeSet::new();
        work.consume_completion("peer-1", &mut suppress);
        assert_eq!(
            work.active_peers(),
            BTreeSet::from(["peer-2".to_string()]),
            "the unpolled peer keeps its ownership"
        );
        assert_eq!(work.active.get("peer-2"), Some(&BTreeSet::from([second])));
    }

    /// Capacity 8 and fairness: active peers are skipped without spinning or
    /// loss, the skipped peers keep their original order at the front, and only a
    /// successful start updates `last_started`.
    #[test]
    fn fill_slots_skips_active_peers_without_loss_and_keeps_order() {
        let mut work = PeerWork::default();
        // peer-a is active, so its queued batch must be skipped, not dropped.
        assert!(work.start_test_batch("peer-a".into(), BTreeSet::new()));
        work.apply_observation(
            observation(vec![
                (
                    RequestKind::Transcript,
                    "peer-a",
                    vec![transcript_request("peer-a:session-1", "session-1")],
                ),
                (
                    RequestKind::Transcript,
                    "peer-b",
                    vec![transcript_request("peer-b:session-1", "session-1")],
                ),
                (
                    RequestKind::Transcript,
                    "peer-c",
                    vec![transcript_request("peer-c:session-1", "session-1")],
                ),
            ]),
            &BTreeSet::new(),
        );
        // The queue is rotated after `last_started` (peer-a), as today.
        assert_eq!(
            work.queued_peers(),
            vec!["peer-b".to_string(), "peer-c".into(), "peer-a".into()]
        );
        work.fill_slots();
        assert_eq!(
            work.queued_peers(),
            vec!["peer-a".to_string()],
            "the active peer is restored at the front; the others started"
        );
        assert_eq!(work.last_started(), Some("peer-c"));
        assert_eq!(work.running.len(), 3);
    }

    /// The cap holds: with 8 peers running, a ninth queued peer waits and starts
    /// only after a completion, without a new observation.
    #[tokio::test]
    async fn the_cap_holds_and_a_queued_peer_starts_after_a_completion() {
        let mut work = PeerWork::default();
        for index in 0..MAX_ACTIVE_PEERS {
            assert!(work.start_test_batch(format!("peer-{index}"), BTreeSet::new()));
        }
        work.apply_observation(
            observation(vec![(
                RequestKind::Transcript,
                "peer-9",
                vec![transcript_request("peer-9:session-1", "session-1")],
            )]),
            &BTreeSet::new(),
        );
        work.fill_slots();
        assert_eq!(work.running.len(), MAX_ACTIVE_PEERS);
        assert_eq!(work.queued_peers(), vec!["peer-9".to_string()]);
        assert_eq!(
            work.last_started(),
            Some("peer-7"),
            "a skipped start does not update last_started"
        );

        assert!(work.release_test_peer("peer-0"));
        tokio::time::timeout(std::time::Duration::from_secs(1), work.next())
            .await
            .expect("the released peer completes");
        assert_eq!(work.running.len(), MAX_ACTIVE_PEERS);
        assert!(work.queued_is_empty(), "the ninth peer started");
        assert_eq!(work.last_started(), Some("peer-9"));
    }

    /// Real cancellation coverage, driving the production `discover` loop.
    ///
    /// Two peers are running. The observation's loads are parked, so the select
    /// loop is genuinely inside its `select!`. Peer-a's completion is then
    /// released and consumed by the production arm, and the discovery future is
    /// cancelled before its loads return. Afterwards: peer-a is out of `active`
    /// (no orphan, so a later observation can start it again), peer-b is still
    /// running and still owns its keys, and a later observation makes progress.
    #[tokio::test]
    async fn cancelling_a_discovery_after_a_completion_leaves_no_orphan() {
        let fixture = scheduler_fixture().await;
        let store = fixture.store.clone();
        let delivery: Arc<dyn HydrationDelivery> = Arc::new(GatedDelivery::new(false));
        let mut work = PeerWork::default();

        let peer_a = transcript_key("peer-a:session-a", "session-a");
        let peer_b = transcript_key("peer-b:session-b", "session-b");
        assert!(work.start_test_batch("peer-a".into(), BTreeSet::from([peer_a.clone()])));
        assert!(work.start_test_batch("peer-b".into(), BTreeSet::from([peer_b.clone()])));

        // Park the loads so the completion arrives while `discover` is selecting.
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let reached = Arc::new(tokio::sync::Notify::new());
        let consumed = work.consumed.clone();
        let releases = work.test_releases.clone();
        let consumed_signal = Arc::new(tokio::sync::Notify::new());
        work.discovery_gate = Some(gate_rx);
        work.discovery_reached = Some(reached.clone());
        work.consumed_signal = Some(consumed_signal.clone());

        let cancel = CancellationToken::new();
        let discovery = discover(&store, &delivery, &mut work, &cancel);
        tokio::pin!(discovery);
        tokio::select! {
            _ = &mut discovery => panic!("discovery returned before its loads parked"),
            _ = reached.notified() => {}
        }

        // The production select arm consumes peer-a's real completion. The
        // barrier is the consumption signal, never a sleep.
        assert!(release_test_peer(&releases, "peer-a"));
        tokio::select! {
            _ = &mut discovery => panic!("discovery returned before it was cancelled"),
            _ = consumed_signal.notified() => {}
        }
        assert_eq!(
            *consumed.lock().expect("consumed lock"),
            vec!["peer-a".to_string()],
            "the production select arm consumed the completion"
        );

        // Cancel the real discovery future while its loads are still parked.
        // `discover` still borrows `work`, so the bookkeeping is asserted after
        // it returns.
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut discovery)
            .await
            .expect("the cancelled discovery returns");
        drop(gate_tx);

        assert_eq!(
            work.active_peers(),
            BTreeSet::from(["peer-b".to_string()]),
            "the consumed peer is released, the still-running peer keeps ownership,              and cancellation left no orphan"
        );
        assert_eq!(
            work.active.get("peer-b"),
            Some(&BTreeSet::from([peer_b.clone()])),
            "the still-running peer kept its full key set"
        );
        assert!(
            work.queued_is_empty(),
            "a cancelled observation applies nothing"
        );

        // A later observation makes progress: peer-a can be started again.
        assert!(
            work.start_test_batch("peer-a".into(), BTreeSet::from([peer_a])),
            "the released peer is startable by a later observation"
        );
        assert_eq!(work.running.len(), 3);

        fixture.node.shutdown().await;
    }

    /// A peer never runs twice at once: a queued batch for an active peer is
    /// skipped, and the same peer is not started twice in one pass.
    #[test]
    fn a_peer_never_runs_twice_at_once() {
        let mut work = PeerWork::default();
        assert!(work.start_test_batch("peer-1".into(), BTreeSet::new()));
        work.apply_observation(
            observation(vec![(
                RequestKind::Transcript,
                "peer-1",
                vec![transcript_request("peer-1:session-1", "session-1")],
            )]),
            &BTreeSet::new(),
        );
        work.fill_slots();
        assert_eq!(
            work.running.len(),
            1,
            "the active peer was not started again"
        );
        assert_eq!(work.queued_peers(), vec!["peer-1".to_string()]);
    }
}
