use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Clone)]
pub(crate) struct ReadCapture {
    operation: &'static str,
    count: Arc<AtomicUsize>,
    changed: Arc<tokio::sync::Notify>,
}

impl ReadCapture {
    pub(crate) fn new(operation: &'static str) -> Self {
        Self {
            operation,
            count: Arc::new(AtomicUsize::new(0)),
            changed: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub(crate) fn dispatcher(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(Registry::default().with(self.clone()))
    }

    pub(crate) fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    pub(crate) async fn wait_for(&self, count: usize) {
        while self.count() < count {
            self.changed.notified().await;
        }
    }
}

struct Operation<'a> {
    expected: &'a str,
    matched: bool,
}

impl Visit for Operation<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "operation" && value == self.expected {
            self.matched = true;
        }
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S: tracing::Subscriber> Layer<S> for ReadCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() != "gents::config_client::txn" {
            return;
        }
        let mut operation = Operation {
            expected: self.operation,
            matched: false,
        };
        event.record(&mut operation);
        if operation.matched {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.changed.notify_one();
        }
    }
}
