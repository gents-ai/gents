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

    /// A confirmed attempt for A, then a distinct request B for the same peer
    /// committed while A is still active. The discovery during A's attempt
    /// defers B; completing A does not serve B; an explicit discovery does.
    #[tokio::test]
    async fn a_new_request_for_an_active_peer_is_deferred_until_the_next_discovery() {
        bounded(
            "a_new_request_for_an_active_peer_is_deferred_until_the_next_discovery",
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
                assert_eq!(work.active, BTreeSet::from([fixture.peer.clone()]));
                assert_eq!(delivery.pushes().len(), 1);
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string()]
                );

                // A distinct request for the same peer arrives while A is active.
                seed_pending_request(&fixture, "session-b").await;
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                assert!(
                    work.queued.is_empty(),
                    "the new row is not enqueued while its peer is active"
                );
                assert_eq!(work.active, BTreeSet::from([fixture.peer.clone()]));
                assert_eq!(
                    delivery.pushes().len(),
                    1,
                    "no second attempt for the same peer"
                );

                // Complete A explicitly: the delivery confirms and the terminal write lands.
                delivery.release_next();
                next_within(&mut work, "A completes").await;
                assert!(work.active.is_empty());
                assert!(work.queued.is_empty());
                assert_eq!(
                    delivery.pushes().len(),
                    1,
                    "completion alone does not retry the peer"
                );
                assert_eq!(request_status(&fixture, "session-a").await, "served");
                assert_eq!(request_status(&fixture, "session-b").await, "pending");

                // `work.next()` alone neither serves B nor retries A.
                next_within(&mut work, "idle next").await;
                assert_eq!(delivery.pushes().len(), 1);
                assert_eq!(request_status(&fixture, "session-b").await, "pending");

                // An explicit discovery serves the deferred row.
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                drive_until_started(&mut work, &delivery).await;
                assert_eq!(delivery.pushes().len(), 2);
                assert_eq!(
                    delivery.pushed_sessions(&fixture.peer),
                    vec!["session-a".to_string(), "session-b".to_string()],
                    "the second attempt serves B, not a retry of A"
                );
                delivery.release_next();
                next_within(&mut work, "B completes").await;
                assert_eq!(request_status(&fixture, "session-b").await, "served");
                assert_eq!(delivery.pushes().len(), 2, "A was attempted exactly once");

                fixture.node.shutdown().await;
            },
        )
        .await;
    }

    /// The same deferral when A's attempt writes no terminal state. A real
    /// subscription is created before the first discovery, and each insert's
    /// batch is drained explicitly before the discovery that follows it, so the
    /// test does not show the discovery consuming a wake. No further wake is
    /// pending at the observation after A fails. It does not claim the
    /// subscription is lossless.
    #[tokio::test]
    async fn a_failed_attempt_without_a_terminal_write_still_defers_the_new_request() {
        bounded(
            "a_failed_attempt_without_a_terminal_write_still_defers_the_new_request",
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
                assert!(
                    work.queued.is_empty(),
                    "the new row is not enqueued while its peer is active"
                );
                assert_eq!(delivery.pushes().len(), 1);

                // A finishes with no terminal write and no new store write.
                delivery.release_next();
                next_within(&mut work, "A completes").await;
                assert!(work.active.is_empty());
                assert_eq!(delivery.pushes().len(), 1);
                assert_eq!(
                    request_status(&fixture, "session-a").await,
                    "pending",
                    "an indeterminate delivery writes no terminal state"
                );
                assert_eq!(request_status(&fixture, "session-b").await, "pending");
                assert_no_pending_wake(&mut changes, "A's failed attempt");

                // Idle: no completion-driven retry and no service of B.
                next_within(&mut work, "idle next").await;
                assert_eq!(
                    delivery.pushes().len(),
                    1,
                    "B is not attempted without a discovery"
                );

                // An explicit discovery serves B and legitimately retries pending A.
                // The assertions below require exactly two A attempts and one B attempt.
                discover(&store, &delivery_trait, &mut work, &cancel).await;
                drive_until_started(&mut work, &delivery).await;
                delivery.release_next();
                drive_until_started(&mut work, &delivery).await;
                delivery.release_next();
                next_within(&mut work, "the deferred batch completes").await;
                assert_eq!(request_status(&fixture, "session-b").await, "served");
                assert_eq!(request_status(&fixture, "session-a").await, "served");
                // A is reattempted because its row is still pending, so the
                // lifetime multiset is A twice and B once. The order within the
                // second batch is not asserted.
                let mut served = delivery.pushed_sessions(&fixture.peer);
                assert_eq!(served.len(), 3, "three attempts in total");
                assert_eq!(
                    served
                        .iter()
                        .filter(|session| *session == "session-b")
                        .count(),
                    1,
                    "B is served exactly once"
                );
                served.sort();
                assert_eq!(
                    served,
                    vec![
                        "session-a".to_string(),
                        "session-a".to_string(),
                        "session-b".to_string()
                    ],
                    "A twice (initial attempt plus reattempt), B once"
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
}
