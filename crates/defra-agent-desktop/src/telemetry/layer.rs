use std::fmt;
use std::sync::Arc;

use chrono::Utc;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
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
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };

        let mut visitor = EventFieldVisitor::default();
        attrs.record(&mut visitor);

        span.extensions_mut()
            .insert(RecordedSpanFields(visitor.fields));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };

        let mut visitor = EventFieldVisitor::default();
        values.record(&mut visitor);

        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<RecordedSpanFields>() {
            fields.0.extend(visitor.fields);
        } else {
            extensions.insert(RecordedSpanFields(visitor.fields));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = EventFieldVisitor::default();
        event.record(&mut visitor);

        let message = visitor
            .message
            .take()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| metadata.name().to_string());

        let mut fields = visitor.fields;
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                fields.push(DesktopLogField {
                    name: "span.name".to_string(),
                    value: span.metadata().name().to_string(),
                });
                if let Some(span_fields) = span.extensions().get::<RecordedSpanFields>() {
                    fields.extend(span_fields.0.iter().cloned().map(|field| DesktopLogField {
                        name: format!("span.{}", field.name),
                        value: field.value,
                    }));
                }
            }
        }

        self.store.record_entry(
            Utc::now(),
            *metadata.level(),
            metadata.target().to_string(),
            message,
            fields,
        );
    }
}

#[derive(Debug, Clone, Default)]
struct RecordedSpanFields(Vec<DesktopLogField>);

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
