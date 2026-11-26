use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::SubscriberExt as _;

use super::*;

#[derive(Clone, Debug)]
struct StageRecord {
    level: tracing::Level,
    message: String,
    stage: String,
    elapsed_ms: u64,
}

#[derive(Clone, Default)]
struct StageRecords {
    entries: Arc<Mutex<Vec<StageRecord>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl StageRecords {
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    async fn wait_for_len(&self, target: usize) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.len() >= target {
                return;
            }
            notified.await;
        }
    }

    fn matching(&self, message: &str) -> Vec<StageRecord> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|record| record.message.contains(message))
            .cloned()
            .collect()
    }
}

impl<S> tracing_subscriber::Layer<S> for StageRecords
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != SHUTDOWN_EVENT_TARGET {
            return;
        }
        #[derive(Default)]
        struct Fields {
            message: String,
            stage: String,
            elapsed_ms: u64,
        }
        impl tracing::field::Visit for Fields {
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                if field.name() == "elapsed_ms" {
                    self.elapsed_ms = value;
                }
            }

            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "stage" {
                    self.stage = value.to_owned();
                }
            }

            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                match field.name() {
                    "message" => self.message = format!("{value:?}"),
                    "stage" => self.stage = format!("{value:?}").trim_matches('"').to_owned(),
                    _ => {}
                }
            }
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.entries.lock().unwrap().push(StageRecord {
            level: *event.metadata().level(),
            message: fields.message,
            stage: fields.stage,
            elapsed_ms: fields.elapsed_ms,
        });
        self.notify.notify_waiters();
    }
}

fn captured() -> (StageRecords, tracing::subscriber::DefaultGuard) {
    let records = StageRecords::default();
    let subscriber = tracing_subscriber::Registry::default().with(records.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    (records, guard)
}

async fn drive_stage<T>(
    records: &StageRecords,
    stage_name: &'static str,
    budget: Duration,
    ticks: usize,
    output: T,
) -> T
where
    T: Send + 'static,
{
    let base = records.len();
    let done = Arc::new(tokio::sync::Notify::new());
    let done_wait = Arc::clone(&done);
    let task = tokio::spawn(async move {
        observe_stage(stage_name, budget, async move {
            done_wait.notified().await;
            output
        })
        .await
    });
    records.wait_for_len(base + 1).await;
    for tick in 1..=ticks {
        tokio::time::advance(STAGE_OBSERVATION_INTERVAL).await;
        records.wait_for_len(base + 1 + tick).await;
    }
    done.notify_one();
    task.await.expect("observed stage task should join")
}

#[tokio::test(start_paused = true)]
async fn an_outstanding_stage_names_itself_while_it_still_runs() {
    let (records, _guard) = captured();

    drive_stage(&records, "node_close", Duration::from_secs(600), 3, ()).await;

    let outstanding = records.matching("still outstanding");
    assert!(
        outstanding.len() >= 2,
        "an outstanding stage must keep naming itself, saw {outstanding:#?}"
    );
    for record in &outstanding {
        assert_eq!(record.stage, "node_close");
        assert_eq!(record.level, tracing::Level::WARN);
    }
    assert!(
        outstanding
            .windows(2)
            .all(|pair| pair[1].elapsed_ms > pair[0].elapsed_ms),
        "each record must carry how long the stage has been outstanding, saw {outstanding:#?}"
    );
    assert!(
        records.matching("exceeded its budget").is_empty(),
        "a stage within its budget is not over it"
    );
    let completed = records.matching("completed");
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].stage, "node_close");
}

#[tokio::test(start_paused = true)]
async fn a_stage_past_its_budget_is_reported_against_that_budget() {
    let (records, _guard) = captured();

    drive_stage(&records, "agent_drain", STAGE_OBSERVATION_INTERVAL, 2, ()).await;

    let exceeded = records.matching("exceeded its budget");
    assert!(
        !exceeded.is_empty(),
        "a stage past its budget must say so, saw {:#?}",
        records.matching("")
    );
    for record in &exceeded {
        assert_eq!(record.stage, "agent_drain");
        assert_eq!(record.level, tracing::Level::ERROR);
        assert!(record.elapsed_ms >= STAGE_OBSERVATION_INTERVAL.as_millis() as u64);
    }
}

/// Observation never shortens a stage: the durable completion writes and the
/// store release both require the work under them to have finished.
#[tokio::test(start_paused = true)]
async fn observation_leaves_the_stage_to_finish_on_its_own() {
    let (records, _guard) = captured();

    let settled = drive_stage(&records, "node_close", Duration::ZERO, 4, "settled").await;

    assert_eq!(settled, "settled");
}

#[test]
fn the_consumer_budget_covers_a_whole_node_close_and_the_agent_drain() {
    let node_close_stage = storage::RegolithStoreOptions::default().close_timeout;

    let sequential_stages = [
        "request drain",
        "network drain",
        "worker pool stop",
        "store close",
    ];

    assert!(
        node_close_budget() >= node_close_stage * u32::try_from(sequential_stages.len()).unwrap(),
        "every sequential stage of the node close has to fit inside the stage budget"
    );
    assert_eq!(
        shutdown_budget(),
        agent_drain_budget() + node_close_budget(),
        "a consumer waits out the drain and the node close in sequence"
    );
    assert!(
        shutdown_budget() > node_close_stage,
        "a budget no longer than one node close stage cannot tell slow from stalled"
    );
}
