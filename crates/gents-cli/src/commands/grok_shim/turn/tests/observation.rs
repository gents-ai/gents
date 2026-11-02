use super::*;
use events::{Bus, ChannelBus, ChannelBusConfig, Message, Update};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

fn invalidation(collection: &str, document: &str) -> Message {
    Message::update(Update::new(
        document.to_owned(),
        Default::default(),
        collection.to_owned(),
        Vec::new(),
        false,
        false,
    ))
}

#[tokio::test(start_paused = true)]
async fn quiet_or_missing_feed_keeps_a_bounded_durable_rescan() {
    let bus = ChannelBus::new();
    for mut changes in [None, Some(bus.subscribe_document_changes())] {
        let started = tokio::time::Instant::now();
        wait_for_projected_change(&mut changes).await;
        assert_eq!(started.elapsed(), PROJECTION_FALLBACK);
    }
}

#[tokio::test(start_paused = true)]
async fn closed_feed_invalidates_once_then_uses_the_fallback() {
    let bus = ChannelBus::new();
    let mut changes = Some(bus.subscribe_document_changes());
    bus.close();
    let started = tokio::time::Instant::now();
    wait_for_projected_change(&mut changes).await;
    assert!(changes.is_none());
    assert_eq!(started.elapsed(), Duration::ZERO);
    wait_for_projected_change(&mut changes).await;
    assert_eq!(started.elapsed(), PROJECTION_FALLBACK);
}

#[tokio::test(start_paused = true)]
async fn revision_bursts_and_overflow_each_request_one_durable_read() {
    let bus = ChannelBus::with_config(ChannelBusConfig::new().with_event_buffer_size(1));
    let mut changes = Some(bus.subscribe_document_changes());
    for overflow in [false, true] {
        bus.publish_batch(
            (0..100)
                .map(|index| {
                    invalidation(
                        "watched",
                        if overflow && index > 0 {
                            "second"
                        } else {
                            "first"
                        },
                    )
                })
                .collect(),
        );
        let started = tokio::time::Instant::now();
        wait_for_projected_change(&mut changes).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
        wait_for_projected_change(&mut changes).await;
        assert_eq!(started.elapsed(), PROJECTION_FALLBACK);
    }
}

#[tokio::test(start_paused = true)]
async fn unrelated_collection_traffic_cannot_postpone_the_fallback() {
    let bus = Arc::new(ChannelBus::new());
    let mut changes = Some(bus.subscribe_collection_changes(vec!["watched".into()]));
    let traffic = {
        let bus = bus.clone();
        tokio::spawn(async move {
            loop {
                bus.publish(invalidation("unrelated", "streaming"));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    };
    let started = tokio::time::Instant::now();
    wait_for_projected_change(&mut changes).await;
    traffic.abort();
    assert!(traffic.await.unwrap_err().is_cancelled());
    assert_eq!(started.elapsed(), PROJECTION_FALLBACK);
}

#[derive(Clone)]
struct Reads {
    operation: &'static str,
    count: Arc<AtomicUsize>,
    completed: Arc<tokio::sync::Notify>,
}

impl Reads {
    fn new(operation: &'static str) -> Self {
        Self {
            operation,
            count: Arc::default(),
            completed: Arc::default(),
        }
    }

    fn dispatcher(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(Registry::default().with(self.clone()))
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }
}

struct ReadOperation<'a> {
    expected: &'a str,
    matched: bool,
}

impl Visit for ReadOperation<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.matched |= field.name() == "operation" && value == self.expected;
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S> Layer<S> for Reads
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() != gents::__test_internals::TXN_TRACING_TARGET {
            return;
        }
        let mut operation = ReadOperation {
            expected: self.operation,
            matched: false,
        };
        event.record(&mut operation);
        if operation.matched {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.completed.notify_one();
        }
    }
}

#[tokio::test]
async fn missed_or_closed_feed_projects_final_output_before_returning_terminal() {
    for closed in [false, true] {
        let (_directory, node, node_did) = test_node().await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(String::new(), &node_did),
        ));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();
        let request = seed_runtime_wake(&node, &node_did, "opening").await;
        let bus = ChannelBus::new();
        let changes = closed.then(|| bus.subscribe_document_changes());
        bus.close();
        let reads = Reads::new("grok shim turn terminal query");
        let (_cancel, response) = oneshot::channel();
        let watch = {
            let manager = manager.clone();
            let request = request.clone();
            let subscriber = reads.dispatcher();
            tokio::spawn(
                async move {
                    let mut progress = ObservedRequest::new("prompt".into(), 0, true);
                    progress.cursor.get_mut().request = Some(request.clone());
                    manager
                        .watch_terminal(
                            &("session-1".into(), "prompt".into()),
                            &request,
                            "session-1",
                            "prompt",
                            &Arc::new(Mutex::new(CancelBeforeIdLatch::default())),
                            &sender,
                            &engine,
                            &mut progress,
                            response,
                            changes,
                        )
                        .await
                }
                .with_subscriber(subscriber),
            )
        };
        tokio::time::timeout(Duration::from_secs(10), reads.completed.notified())
            .await
            .expect("watcher has read the durable pending state");
        seed_assistant_message(&node, &request, 1, "Final durable output").await;
        terminalize_request(&node, &request, "completed").await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), watch)
            .await
            .expect("fallback observes terminal state without notifications")
            .unwrap()
            .unwrap();
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert!(
            reads.count() >= 2,
            "terminal state requires another durable read"
        );
        let updates = parse_buffered_updates(&buffer).await;
        assert_eq!(
            updates
                .iter()
                .filter(|row| {
                    row["params"]["update"]["content"]["text"] == "Final durable output"
                })
                .count(),
            1,
            "final output is visible exactly once before terminal returns"
        );
        drop(manager);
        node.shutdown().await;
    }
}

#[tokio::test]
#[ignore = "matched six-second native discovery samples; writes the requested profile artifact"]
async fn session_discovery_read_profile() {
    let output = std::env::var_os("GENTS_SESSION_DISCOVERY_PROFILE")
        .expect("set GENTS_SESSION_DISCOVERY_PROFILE to the measurement output path");
    let (_directory, node, node_did) = test_node().await;
    let manager = TurnManager::new(node.clone(), test_config(String::new(), &node_did));
    let engine = test_engine(node.clone());
    let (_buffer, sender) = buffer_sender();
    let unrelated = gents::collection_ids(&node, ["ToolServiceHealthState"])
        .unwrap()
        .remove(0);
    let mut samples = Vec::new();
    for timer_baseline in [true, false] {
        for noisy in [false, true] {
            let mut changes = subscribe_projected_changes(&node);
            assert!(changes.is_some());
            let traffic = noisy.then(|| {
                let node = node.clone();
                let unrelated = unrelated.clone();
                tokio::spawn(async move {
                    loop {
                        node.event_bus()
                            .publish(invalidation(&unrelated, "unrelated-stream"));
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
            });
            let reads = Reads::new("Grok shim session request discovery");
            let mut after = String::new();
            let mut delivery_after = String::new();
            let attached_at = chrono::Utc::now().to_rfc3339();
            let started = tokio::time::Instant::now();
            let sample = async {
                loop {
                    manager
                        .observe_session_tick(
                            "session-1",
                            &attached_at,
                            &mut after,
                            &mut delivery_after,
                            &sender,
                            &engine,
                        )
                        .await
                        .unwrap();
                    if timer_baseline {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    } else {
                        wait_for_projected_change(&mut changes).await;
                    }
                }
            }
            .with_subscriber(reads.dispatcher());
            assert!(tokio::time::timeout(Duration::from_secs(6), sample)
                .await
                .is_err());
            if let Some(traffic) = traffic {
                traffic.abort();
                assert!(traffic.await.unwrap_err().is_cancelled());
            }
            let count = reads.count();
            assert!(
                count > 0,
                "profile must observe actual completed discovery queries"
            );
            if !timer_baseline {
                assert!(count <= 4, "idle discovery is not bounded: {count}");
            }
            samples.push(json!({
                "wait": if timer_baseline { "200ms_timer_baseline" } else { "coalesced_feed" },
                "unrelated_batches_per_second": if noisy { 10 } else { 0 },
                "elapsed_ms": started.elapsed().as_millis(),
                "completed_discovery_reads": count,
            }));
        }
    }
    std::fs::write(output, serde_json::to_vec_pretty(&samples).unwrap()).unwrap();
    drop(manager);
    drop(engine);
    node.shutdown().await;
}
