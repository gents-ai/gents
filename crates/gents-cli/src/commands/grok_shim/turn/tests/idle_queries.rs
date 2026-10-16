use super::*;
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

#[derive(Clone, Default)]
struct NativeReads(Arc<std::sync::Mutex<BTreeMap<String, u64>>>);

#[derive(Default)]
struct CompletedRead {
    operation: Option<String>,
    completed: bool,
}

impl Visit for CompletedRead {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "operation" {
            self.operation = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.completed = format!("{value:?}") == "DefraDB GraphQL completed";
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for NativeReads {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "gents::config_client::txn" {
            return;
        }
        let mut read = CompletedRead::default();
        event.record(&mut read);
        if let Some(operation) = read.operation.filter(|_| read.completed) {
            *self.0.lock().unwrap().entry(operation).or_default() += 1;
        }
    }
}

#[tokio::test]
#[ignore = "measures native idle reads; set GENTS_IDLE_QUERY_REPORT to a report path"]
async fn measure_idle_turn_native_reads() {
    let report_path = std::env::var_os("GENTS_IDLE_QUERY_REPORT")
        .expect("set GENTS_IDLE_QUERY_REPORT to a report path");
    let (_tempdir, node, node_did) = test_node().await;
    let request = seed_runtime_wake(&node, &node_did, "waiting for inference").await;
    let manager = TurnManager::new(node.clone(), test_config(String::new(), &node_did));
    let projections = test_engine(node);
    let (_buffer, sender) = buffer_sender();
    let generation = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
    let mut progress = ObservedRequest::new("prompt-1".into(), 0, true);
    progress.cursor.get_mut().request = Some(request.clone());
    let (_response_tx, response_rx) = oneshot::channel();
    let reads = NativeReads::default();
    let subscriber = tracing_subscriber::registry().with(reads.clone());
    let key = ("session-1".into(), "prompt-1".into());
    let watch = manager.watch_terminal(
        &key,
        &request,
        "session-1",
        "prompt-1",
        &generation,
        &sender,
        &projections,
        &mut progress,
        response_rx,
    );
    let mut watch = Box::pin(watch.with_subscriber(subscriber));
    let window = Duration::from_secs(10);
    let started = tokio::time::Instant::now();
    tokio::select! {
        result = &mut watch => panic!("idle request unexpectedly stopped: {result:?}"),
        _ = tokio::time::sleep(window) => {},
    }
    let elapsed = started.elapsed();
    drop(watch);
    let by_operation = reads.0.lock().unwrap().clone();
    let total: u64 = by_operation.values().sum();
    assert!(
        total > 0,
        "native query instrumentation must observe the initial read pass"
    );
    let report = json!({
        "active_sessions": 1,
        "request_state": "pending",
        "includes_initial_read": true,
        "elapsed_ms": elapsed.as_millis(),
        "native_reads": total,
        "native_reads_per_second": total as f64 / elapsed.as_secs_f64(),
        "by_operation": by_operation,
    });
    std::fs::write(report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
