use std::fmt;
use std::sync::Arc;

use chrono::Utc;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use super::{DesktopLogField, DesktopLogStore};

#[derive(Debug, Clone)]
pub struct DesktopLogLayer {
    store: Arc<DesktopLogStore>,
}

impl DesktopLogLayer {
    pub fn new(store: Arc<DesktopLogStore>) -> Self {
        Self { store }
    }
}

impl<S> Layer<S> for DesktopLogLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = EventFieldVisitor::default();
        event.record(&mut visitor);

        let message = visitor
            .message
            .take()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| metadata.name().to_string());

        self.store.record_entry(
            Utc::now(),
            *metadata.level(),
            metadata.target().to_string(),
            message,
            visitor.fields,
        );
    }
}

#[derive(Debug, Default)]
struct EventFieldVisitor {
    message: Option<String>,
    fields: Vec<DesktopLogField>,
}

impl EventFieldVisitor {
    fn push_value(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
            return;
        }

        self.fields.push(DesktopLogField {
            name: field.name().to_string(),
            value,
        });
    }
}

impl Visit for EventFieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_value(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push_value(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push_value(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push_value(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push_value(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push_value(field, trim_debug_value(format!("{value:?}")));
    }
}

fn trim_debug_value(value: String) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value
    }
}
