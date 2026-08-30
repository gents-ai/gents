use std::collections::VecDeque;

use tokio::sync::Semaphore;

use super::*;

#[derive(Clone, Copy)]
enum WritePlan {
    Success,
    Fail,
    BlockForever,
    BlockThenFail,
    BlockThenSucceed,
}

struct ControlledWriter {
    plans: std::sync::Mutex<VecDeque<WritePlan>>,
    attempts: mpsc::UnboundedSender<BehaviorReadinessProcessState>,
    release: Semaphore,
    persisted: tokio::sync::Mutex<Vec<BehaviorReadinessProcessState>>,
}

#[async_trait::async_trait]
impl BehaviorReadinessWriter for ControlledWriter {
    async fn upsert(
        &self,
        _agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        _updated_at: &str,
    ) -> Result<()> {
        let plan = self
            .plans
            .lock()
            .expect("writer plans mutex")
            .pop_front()
            .expect("test supplied a write plan");
        self.attempts
            .send(snapshot.process_state)
            .expect("attempt observer remains alive");
        match plan {
            WritePlan::Success => {}
            WritePlan::Fail => anyhow::bail!("injected readiness persistence failure"),
            WritePlan::BlockForever => std::future::pending().await,
            WritePlan::BlockThenFail => {
                self.release.acquire().await.unwrap().forget();
                anyhow::bail!("injected readiness persistence failure");
            }
            WritePlan::BlockThenSucceed => {
                self.release.acquire().await.unwrap().forget();
            }
        }
        self.persisted.lock().await.push(snapshot.process_state);
        Ok(())
    }
}

#[tokio::test]
async fn persistence_retry_is_bounded_and_leaves_committed_state_unchanged() {
    let (attempts_tx, mut attempts_rx) = mpsc::unbounded_channel();
    let mut plans = vec![WritePlan::Fail; MAX_PERSIST_ATTEMPTS];
    plans.push(WritePlan::Success);
    let writer = Arc::new(ControlledWriter {
        plans: std::sync::Mutex::new(VecDeque::from(plans)),
        attempts: attempts_tx,
        release: Semaphore::new(0),
        persisted: tokio::sync::Mutex::new(Vec::new()),
    });
    let (owner, publisher) = BehaviorReadinessPublisherHandle::start_with_writer(
        writer.clone(),
        "did:test:bounded-readiness-writer",
        Duration::from_millis(1),
    );

    assert!(
        tokio::time::timeout(Duration::from_secs(1), publisher.initialize("general"))
            .await
            .expect("bounded retry must complete")
            .is_err()
    );
    let mut attempts = 0;
    while attempts_rx.try_recv().is_ok() {
        attempts += 1;
    }
    assert_eq!(attempts, MAX_PERSIST_ATTEMPTS);
    assert!(writer.persisted.lock().await.is_empty());
    assert_eq!(
        publisher.observation(),
        BehaviorAdmissionObservation::default()
    );
    publisher.initialize("general").await.unwrap();
    assert_eq!(
        *writer.persisted.lock().await,
        vec![BehaviorReadinessProcessState::Recovering],
        "a later valid command must recover from a bounded write failure"
    );
    owner.close().await.unwrap();
}

#[tokio::test]
async fn close_remains_bounded_when_write_attempts_timeout() {
    let (attempts_tx, mut attempts_rx) = mpsc::unbounded_channel();
    let mut plans = vec![WritePlan::Success];
    plans.extend(std::iter::repeat_n(
        WritePlan::BlockForever,
        MAX_PERSIST_ATTEMPTS,
    ));
    let writer = Arc::new(ControlledWriter {
        plans: std::sync::Mutex::new(VecDeque::from(plans)),
        attempts: attempts_tx,
        release: Semaphore::new(0),
        persisted: tokio::sync::Mutex::new(Vec::new()),
    });
    let (owner, publisher) = BehaviorReadinessPublisherHandle::start_with_writer(
        writer,
        "did:test:cancellable-readiness-writer",
        Duration::from_millis(1),
    );
    publisher.initialize("general").await.unwrap();
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Recovering)
    );

    let ready = tokio::spawn(async move {
        publisher
            .set_process_state(ProcessLifecycleState::Ready)
            .await
    });
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Ready)
    );
    tokio::time::timeout(Duration::from_secs(3), owner.close())
        .await
        .expect("close must remain bounded around a stuck writer")
        .unwrap();
    assert!(ready.await.unwrap().is_err());
}

#[tokio::test]
async fn saturated_command_queue_cannot_block_owner_close_forever() {
    let (attempts_tx, mut attempts_rx) = mpsc::unbounded_channel();
    let mut plans = vec![WritePlan::Success];
    plans.extend(std::iter::repeat_n(
        WritePlan::BlockForever,
        MAX_PERSIST_ATTEMPTS,
    ));
    plans.extend(std::iter::repeat_n(
        WritePlan::BlockForever,
        MAX_PERSIST_ATTEMPTS,
    ));
    let writer = Arc::new(ControlledWriter {
        plans: std::sync::Mutex::new(VecDeque::from(plans)),
        attempts: attempts_tx,
        release: Semaphore::new(0),
        persisted: tokio::sync::Mutex::new(Vec::new()),
    });
    let (owner, publisher) = BehaviorReadinessPublisherHandle::start_with_writer(
        writer,
        "did:test:saturated-readiness-writer",
        Duration::from_millis(1),
    );
    publisher.initialize("general").await.unwrap();
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Recovering)
    );

    let blocked = {
        let publisher = publisher.clone();
        tokio::spawn(async move {
            publisher
                .set_process_state(ProcessLifecycleState::Ready)
                .await
        })
    };
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Ready)
    );
    let queued = (0..64)
        .map(|generation| {
            let publisher = publisher.clone();
            tokio::spawn(async move { publisher.set_router_generation(generation).await })
        })
        .collect::<Vec<_>>();
    tokio::time::timeout(Duration::from_secs(1), async {
        while publisher.commands.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("test must deterministically saturate the publisher queue");

    let close_result = tokio::time::timeout(Duration::from_secs(3), owner.close())
        .await
        .expect("a saturated queue and stuck writer must not make close immortal");
    assert!(
        close_result.is_err(),
        "saturated close must report forced cancellation"
    );
    assert!(blocked.await.unwrap().is_err());
    let mut failed = 0;
    for queued in queued {
        if queued.await.unwrap().is_err() {
            failed += 1;
        }
    }
    assert!(
        failed > 0,
        "publisher cancellation did not reject queued work"
    );
}

async fn test_publisher(
    agent_did: &str,
) -> (
    BehaviorReadinessPublisherOwner,
    BehaviorReadinessPublisherHandle,
) {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    BehaviorReadinessPublisherHandle::start(node, agent_did)
}

#[tokio::test]
async fn replacement_ready_cannot_clear_demotion_until_source_generation_advances() {
    let (owner, publisher) = test_publisher("did:test:readiness-slot-cas").await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 1).await.unwrap();
    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 1,
                default_behavior_id: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    BehaviorReadinessSourceEntry {
                        behavior_id: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert!(publisher
        .demote_slot("general", 1, "generation one failed".to_string())
        .await
        .unwrap());

    publisher.register_slot("general", 2).await.unwrap();
    assert!(!publisher.mark_slot_ready("general", 0).await.unwrap());
    assert_eq!(
        publisher.observation().demotion_reason("general"),
        Some("generation one failed")
    );

    assert!(publisher.mark_slot_ready("general", 2).await.unwrap());
    assert_eq!(
        publisher.observation().demotion_reason("general"),
        Some("generation one failed"),
        "new slot readiness must not admit the still-published old source"
    );

    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 2,
                default_behavior_id: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    BehaviorReadinessSourceEntry {
                        behavior_id: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert_eq!(publisher.observation().demotion_reason("general"), None);
    assert!(publisher.retire_slot("general", 1).await.unwrap());
    owner.close().await.unwrap();
}

#[tokio::test]
async fn invalid_source_leaves_committed_standing_and_observation_usable() {
    let (owner, publisher) = test_publisher("did:test:readiness-invalid-source").await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 1).await.unwrap();
    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 1,
                default_behavior_id: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    BehaviorReadinessSourceEntry {
                        behavior_id: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert!(publisher
        .demote_slot("general", 1, "standing demotion".to_string())
        .await
        .unwrap());
    let committed = publisher.observation();

    let invalid = publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 2,
                default_behavior_id: "missing".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    BehaviorReadinessSourceEntry {
                        behavior_id: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await;
    assert!(invalid.is_err());
    assert_eq!(publisher.observation(), committed);
    assert!(
        publisher.mark_slot_ready("general", 1).await.unwrap(),
        "projection failure must not discard the committed slot standing"
    );
    assert_eq!(publisher.observation().demotion_reason("general"), None);

    publisher
        .send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: 2,
                default_behavior_id: "general".to_string(),
                entries: BTreeMap::from([(
                    "general".to_string(),
                    BehaviorReadinessSourceEntry {
                        behavior_id: "general".to_string(),
                        dispatcher_present: true,
                        unavailable_reason: None,
                        startup_demoted: false,
                    },
                )]),
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
        .unwrap();
    assert_eq!(publisher.observation().source_generation(), 2);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn unchanged_demoted_slot_stays_demoted_across_unrelated_global_generation() {
    let (owner, publisher) = test_publisher("did:test:readiness-reused-slot").await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 1).await.unwrap();

    for active_generation in [1, 2] {
        publisher
            .send(|ack| Command::PublishSource {
                source: ReadinessSource {
                    active_generation,
                    default_behavior_id: "general".to_string(),
                    entries: BTreeMap::from([(
                        "general".to_string(),
                        BehaviorReadinessSourceEntry {
                            behavior_id: "general".to_string(),
                            dispatcher_present: true,
                            unavailable_reason: None,
                            startup_demoted: false,
                        },
                    )]),
                    slot_generations: BTreeMap::new(),
                },
                ack,
            })
            .await
            .unwrap();
        if active_generation == 1 {
            assert!(publisher
                .demote_slot("general", 1, "generation one failed".to_string())
                .await
                .unwrap());
        }
    }

    let observation = publisher.observation();
    assert_eq!(observation.source_generation(), 2);
    assert_eq!(
        observation.demotion_reason("general"),
        Some("generation one failed"),
        "an unrelated source generation must not clear a reused slot's demotion"
    );
    owner.close().await.unwrap();
}

#[tokio::test]
async fn semantic_noop_and_stale_commands_do_not_emit_watch_revisions() {
    let (owner, publisher) = test_publisher("did:test:readiness-noop-watch").await;
    publisher.initialize("general").await.unwrap();
    publisher.register_slot("general", 2).await.unwrap();
    let mut observation = publisher.observation.clone();
    observation.borrow_and_update();

    publisher.register_slot("general", 2).await.unwrap();
    assert!(!publisher.retire_slot("general", 1).await.unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observation.changed())
            .await
            .is_err()
    );
    owner.close().await.unwrap();
}

#[tokio::test]
async fn ordered_writer_retries_without_overtake_and_flushes_terminal_state() {
    let (attempts_tx, mut attempts_rx) = mpsc::unbounded_channel();
    let writer = Arc::new(ControlledWriter {
        plans: std::sync::Mutex::new(VecDeque::from([
            WritePlan::Success,
            WritePlan::BlockThenFail,
            WritePlan::Success,
            WritePlan::BlockThenSucceed,
        ])),
        attempts: attempts_tx,
        release: Semaphore::new(0),
        persisted: tokio::sync::Mutex::new(Vec::new()),
    });
    let (owner, publisher) = BehaviorReadinessPublisherHandle::start_with_writer(
        writer.clone(),
        "did:test:ordered-readiness-writer",
        Duration::from_millis(5),
    );
    publisher.initialize("general").await.unwrap();
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Recovering)
    );

    let ready = {
        let publisher = publisher.clone();
        tokio::spawn(async move {
            publisher
                .set_process_state(ProcessLifecycleState::Ready)
                .await
        })
    };
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Ready)
    );
    let shutdown = {
        let publisher = publisher.clone();
        tokio::spawn(async move {
            publisher
                .set_process_state(ProcessLifecycleState::Shutdown)
                .await
        })
    };
    tokio::task::yield_now().await;
    assert!(!ready.is_finished(), "Ready ack preceded durable success");
    assert!(
        !shutdown.is_finished(),
        "newer Shutdown overtook the failed Ready write"
    );

    writer.release.add_permits(1);
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Ready),
        "the failed Ready snapshot must retry before Shutdown"
    );
    assert_eq!(
        attempts_rx.recv().await,
        Some(BehaviorReadinessProcessState::Shutdown)
    );
    let close = tokio::spawn(owner.close());
    tokio::task::yield_now().await;
    ready.await.unwrap().unwrap();
    assert!(
        !shutdown.is_finished(),
        "Shutdown ack preceded durable success"
    );
    assert!(!close.is_finished(), "owner joined before terminal flush");

    writer.release.add_permits(1);
    shutdown.await.unwrap().unwrap();
    close.await.unwrap().unwrap();
    assert_eq!(
        *writer.persisted.lock().await,
        vec![
            BehaviorReadinessProcessState::Recovering,
            BehaviorReadinessProcessState::Ready,
            BehaviorReadinessProcessState::Shutdown,
        ]
    );
}
