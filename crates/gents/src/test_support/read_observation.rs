use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Clone)]
pub(crate) struct ReadCapture {
    operation: &'static str,
    count: Arc<AtomicUsize>,
}

impl ReadCapture {
    pub(crate) fn new(operation: &'static str) -> Self {
        Self {
            operation,
            count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn dispatcher(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(Registry::default().with(self.clone()))
    }

    pub(crate) fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
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

impl<S> Layer<S> for ReadCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() != crate::config_client::TXN_TRACING_TARGET {
            return;
        }
        let mut operation = Operation {
            expected: self.operation,
            matched: false,
        };
        event.record(&mut operation);
        if operation.matched {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }
}
