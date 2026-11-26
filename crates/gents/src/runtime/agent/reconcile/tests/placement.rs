use std::collections::BTreeMap;

use defra_node::AssignmentPosition;
use gents_automation::triggers::snapshot_observer::RuntimeSnapshotObserver;
use gents_store::config_client::placement_fence::PlacementBasis;

use super::*;

const SCOPE: &str = "scope";

fn position(height: u64) -> AssignmentPosition {
    AssignmentPosition {
        term: 1,
        height,
        block_hash: format!("block-{height}"),
    }
}

struct Fixture {
    node: Arc<defra_node::Node>,
    runtime_status: RuntimeStatusHandle,
    _db: gents_store::test_support::TestStore,
    standing: AgentWithTools,
    withdrawn: AgentWithTools,
    documents: BTreeMap<String, String>,
}

impl Fixture {
    async fn new() -> Self {
        let db = test_node().await;
        let node = db.node.clone();
        let node_did = node
            .node_identity_did()
            .expect("fixture node has an identity");
        gents_documents::test_support::ensure_test_node(&node, node_did).await;
        let runtime_status = runtime_status_handle(&node, node_did);
        let standing = AgentLineage::provision("standing").revision("standing");
        let withdrawn = AgentLineage::provision("withdrawn").revision("withdrawn");
        let documents = BTreeMap::from([
            (standing.agent.agent_did.clone(), "standing-doc".to_owned()),
            (
                withdrawn.agent.agent_did.clone(),
                "withdrawn-doc".to_owned(),
            ),
        ]);
        Self {
            node,
            runtime_status,
            _db: db,
            standing,
            withdrawn,
            documents,
        }
    }

    fn withdrawn_did(&self) -> String {
        self.withdrawn.agent.agent_did.clone()
    }

    async fn snapshot(&self, agents: Vec<AgentWithTools>, floor: u64) -> ResolvedRuntimeSnapshot {
        snapshot_for_agents(&self.node, &self.standing.agent.agent_did, agents)
            .await
            .with_placement_basis(PlacementBasis {
                floors: BTreeMap::from([(SCOPE.to_owned(), position(floor))]),
                agent_documents: self.documents.clone(),
            })
    }
}

struct Joined(mpsc::UnboundedSender<(String, u64)>);

impl RuntimeSnapshotObserver for Joined {
    fn on_generation_published(&self, _: u64, _: &str, _: &[String]) {}

    fn on_event_sources_reconciled(&self, _: u64, _: &str, _: Result<(), &str>) {}

    fn on_retired_slot_joined(&self, agent_did: &str, generation: u64) {
        let _ = self.0.send((agent_did.to_owned(), generation));
    }
}

/// Counts slot starts per agent and holds `held`'s retired slot open until
/// `release` gets a permit.
#[derive(Clone)]
struct Slots {
    starts: Arc<StdMutex<RapidHashMap<String, usize>>>,
    held: String,
    release: Arc<Semaphore>,
}

impl Slots {
    fn new(held: String) -> Self {
        Self {
            starts: Arc::new(StdMutex::new(RapidHashMap::new())),
            held,
            release: Arc::new(Semaphore::new(0)),
        }
    }

    fn starts(&self, agent_did: &str) -> usize {
        self.starts
            .lock()
            .unwrap()
            .get(agent_did)
            .copied()
            .unwrap_or_default()
    }

    async fn run(
        self,
        agent_did: String,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        *self
            .starts
            .lock()
            .unwrap()
            .entry(agent_did.clone())
            .or_default() += 1;
        tokio::select! {
            _ = shutdown.changed() => {}
            _ = async {
                let mut receiver = request_rx.lock().await;
                while receiver.recv().await.is_some() {}
            } => {}
        }
        if agent_did == self.held {
            let _ = self.release.acquire().await;
        }
        Ok(())
    }
}

struct Running {
    placement: Arc<PlacementFence>,
    active_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    proposal_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    joined_rx: mpsc::UnboundedReceiver<(String, u64)>,
    shutdown_tx: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<()>>,
}

async fn start(fixture: &Fixture, slots: &Slots, initial: ResolvedRuntimeSnapshot) -> Running {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (joined_tx, joined_rx) = mpsc::unbounded_channel();
    let supervisor = GenerationSupervisor::bootstrap(
        initial,
        gents_inference::admission::AdmissionRegistry::new(fixture.node.clone()),
        crate::runtime::agent::retry::RetryPolicy {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
        {
            let slots = slots.clone();
            move |agent: AgentWithTools,
                  _surface: Arc<ToolSurface>,
                  request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
                  _capacity: Arc<crate::runtime::agent::worker_capacity::WorkerCapacity>,
                  _generation: u64,
                  shutdown: watch::Receiver<bool>| {
                slots
                    .clone()
                    .run(agent.agent.agent_did.clone(), request_rx, shutdown)
            }
        },
        fixture.runtime_status.clone(),
        shutdown_rx.clone(),
        None,
    )
    .await
    .unwrap()
    .with_observer(Some(Arc::new(Joined(joined_tx))));
    let placement = supervisor.placement();
    let (active_tx, active_rx) = watch::channel(supervisor.current_snapshot());
    let (proposal_tx, proposal_rx) = mpsc::channel(8);
    let task = tokio::spawn(supervisor.run(active_tx, proposal_rx, shutdown_rx));
    Running {
        placement,
        active_rx,
        proposal_tx,
        joined_rx,
        shutdown_tx,
        task,
    }
}

impl Running {
    async fn basis_reaches(&self, floor: u64) {
        let mut progress = self.placement.progress();
        tokio::time::timeout(
            Duration::from_secs(2),
            progress
                .wait_for(|progress| progress.basis.floors.get(SCOPE) == Some(&position(floor))),
        )
        .await
        .expect("the supervisor publishes the proposal's basis")
        .unwrap();
    }

    async fn stop(self, slots: &Slots) {
        slots.release.add_permits(8);
        let _ = self.shutdown_tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .expect("supervisor stops on shutdown")
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn a_proposal_queued_behind_the_fence_is_dropped_and_never_restarts_the_agent() {
    let fixture = Fixture::new().await;
    let withdrawn = fixture.withdrawn_did();
    let slots = Slots::new(withdrawn.clone());
    let initial = fixture.snapshot(vec![fixture.standing.clone()], 1).await;
    let stale = fixture
        .snapshot(vec![fixture.standing.clone(), fixture.withdrawn.clone()], 1)
        .await;
    let current = fixture.snapshot(vec![fixture.standing.clone()], 2).await;
    let mut running = start(&fixture, &slots, initial).await;
    let mut resolves = running.placement.resolve_requests();

    running.placement.raise(SCOPE, &position(2));
    resolves.borrow_and_update();
    running.proposal_tx.send(stale).await.unwrap();
    running.proposal_tx.send(current).await.unwrap();
    running.basis_reaches(2).await;
    tokio::time::timeout(
        Duration::from_secs(2),
        running
            .placement
            .stopped(SCOPE, &position(2), &["withdrawn-doc".to_owned()]),
    )
    .await
    .expect("the barrier passes once the current basis is processed");

    assert!(
        resolves.has_changed().unwrap(),
        "a dropped proposal asks the watcher to resolve again"
    );
    assert_eq!(running.active_rx.borrow_and_update().generation, 1);
    assert_eq!(slots.starts(&withdrawn), 0);
    running.stop(&slots).await;
}

#[tokio::test]
async fn an_unchanged_proposal_advances_the_published_basis() {
    let fixture = Fixture::new().await;
    let slots = Slots::new(fixture.withdrawn_did());
    let initial = fixture.snapshot(vec![fixture.standing.clone()], 1).await;
    let unchanged = fixture.snapshot(vec![fixture.standing.clone()], 2).await;
    let mut running = start(&fixture, &slots, initial).await;

    running.placement.raise(SCOPE, &position(2));
    let placement = running.placement.clone();
    let floor = position(2);
    let ids = ["withdrawn-doc".to_owned()];
    let barrier = placement.stopped(SCOPE, &floor, &ids);
    tokio::pin!(barrier);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), barrier.as_mut())
            .await
            .is_err(),
        "the barrier waits for a basis at the fence"
    );
    running.proposal_tx.send(unchanged).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), barrier)
        .await
        .expect("a no-op at the fence's basis releases the barrier");
    assert_eq!(running.active_rx.borrow_and_update().generation, 1);
    running.stop(&slots).await;
}

#[tokio::test]
async fn the_barrier_waits_for_the_retired_slot_to_join() {
    let fixture = Fixture::new().await;
    let withdrawn = fixture.withdrawn_did();
    let slots = Slots::new(withdrawn.clone());
    let initial = fixture
        .snapshot(vec![fixture.standing.clone(), fixture.withdrawn.clone()], 1)
        .await;
    let withdrawal = fixture.snapshot(vec![fixture.standing.clone()], 2).await;
    let mut running = start(&fixture, &slots, initial).await;

    running.placement.raise(SCOPE, &position(2));
    running.proposal_tx.send(withdrawal).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), running.active_rx.changed())
        .await
        .expect("the withdrawal publishes a generation")
        .unwrap();
    assert!(!running
        .active_rx
        .borrow_and_update()
        .agents
        .contains_key(&withdrawn));
    running.basis_reaches(2).await;
    let placement = running.placement.clone();
    let floor = position(2);
    let ids = ["withdrawn-doc".to_owned()];
    let barrier = placement.stopped(SCOPE, &floor, &ids);
    tokio::pin!(barrier);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), barrier.as_mut())
            .await
            .is_err(),
        "a retired slot that has not joined holds the barrier"
    );

    slots.release.add_permits(1);
    let joined = tokio::time::timeout(Duration::from_secs(2), running.joined_rx.recv())
        .await
        .expect("the retired slot joins once released")
        .unwrap();
    assert_eq!(joined, (withdrawn.clone(), 1));
    tokio::time::timeout(Duration::from_secs(2), barrier)
        .await
        .expect("the barrier passes once the retired slot joined");
    running.stop(&slots).await;
}

#[test]
fn the_proposal_outcome_replays_the_lean_owner() {
    gents_lean_vocab::families::runtime_reconcile_placement_proposal_outcome::replay::<
        ProposalOutcome,
    >(|input| proposal_outcome(input.admitted, input.fingerprint_changed, input.parked));
}
