use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use chrono::{DateTime, Duration, Utc};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

const DEFAULT_LOG_CAPACITY: usize = 512;
const EVENTS_PER_SECOND_WINDOW_SECS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopLogCategory {
    Replication,
    Peering,
    Turns,
    Writes,
    Warnings,
}

impl DesktopLogCategory {
    pub const ALL: [Self; 5] = [
        Self::Replication,
        Self::Peering,
        Self::Turns,
        Self::Writes,
        Self::Warnings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Replication => "Replication",
            Self::Peering => "Peering",
            Self::Turns => "Turns",
            Self::Writes => "Writes",
            Self::Warnings => "Warnings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLogField {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLogEntry {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub level: Level,
    pub target: String,
    pub category: DesktopLogCategory,
    pub message: String,
    pub fields: Vec<DesktopLogField>,
}

#[derive(Debug, Clone, Default)]
pub struct DesktopLogSnapshot {
    pub entries: Vec<DesktopLogEntry>,
    pub capacity: usize,
    pub total_events: u64,
    pub dropped_events: u64,
    pub events_per_second: f32,
}

#[derive(Debug)]
struct DesktopLogState {
    entries: VecDeque<DesktopLogEntry>,
    recent_event_times: VecDeque<DateTime<Utc>>,
    next_id: u64,
    total_events: u64,
    dropped_events: u64,
}

#[derive(Debug)]
pub struct DesktopLogStore {
    capacity: usize,
    state: RwLock<DesktopLogState>,
}

impl Default for DesktopLogStore {
    fn default() -> Self {
        Self::new(DEFAULT_LOG_CAPACITY)
    }
}

impl DesktopLogStore {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            state: RwLock::new(DesktopLogState {
                entries: VecDeque::with_capacity(capacity),
                recent_event_times: VecDeque::new(),
                next_id: 1,
                total_events: 0,
                dropped_events: 0,
            }),
        }
    }

    pub fn snapshot(&self) -> DesktopLogSnapshot {
        let cutoff = Utc::now() - Duration::seconds(EVENTS_PER_SECOND_WINDOW_SECS);
        let state = self.state.read().expect("desktop log lock poisoned");
        let recent_events = state
            .recent_event_times
            .iter()
            .filter(|timestamp| **timestamp >= cutoff)
            .count();

        DesktopLogSnapshot {
            entries: state.entries.iter().rev().cloned().collect(),
            capacity: self.capacity,
            total_events: state.total_events,
            dropped_events: state.dropped_events,
            events_per_second: recent_events as f32 / EVENTS_PER_SECOND_WINDOW_SECS as f32,
        }
    }

    pub fn record_manual(
        &self,
        timestamp: DateTime<Utc>,
        level: Level,
        target: impl Into<String>,
        message: impl Into<String>,
        fields: impl IntoIterator<Item = (&'static str, String)>,
    ) {
        let target = target.into();
        let message = message.into();
        let fields: Vec<_> = fields
            .into_iter()
            .map(|(name, value)| DesktopLogField {
                name: name.to_string(),
                value,
            })
            .collect();

        self.record_entry(timestamp, level, target, message, fields);
    }

    fn record_entry(
        &self,
        timestamp: DateTime<Utc>,
        level: Level,
        target: String,
        message: String,
        fields: Vec<DesktopLogField>,
    ) {
        let category = classify_log_entry(level, &target, &message, &fields);
        let mut state = self.state.write().expect("desktop log lock poisoned");

        prune_recent_event_window(&mut state.recent_event_times, timestamp);
        state.recent_event_times.push_back(timestamp);

        if state.entries.len() == self.capacity {
            state.entries.pop_front();
            state.dropped_events = state.dropped_events.saturating_add(1);
        }

        let id = state.next_id;
        state.next_id = state.next_id.saturating_add(1);
        state.total_events = state.total_events.saturating_add(1);
        state.entries.push_back(DesktopLogEntry {
            id,
            timestamp,
            level,
            target,
            category,
            message,
            fields,
        });
    }
}

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

pub fn global_log_store() -> Arc<DesktopLogStore> {
    static GLOBAL_LOG_STORE: OnceLock<Arc<DesktopLogStore>> = OnceLock::new();
    GLOBAL_LOG_STORE
        .get_or_init(|| Arc::new(DesktopLogStore::default()))
        .clone()
}

pub fn global_log_layer() -> DesktopLogLayer {
    DesktopLogLayer::new(global_log_store())
}

pub fn classify_log_entry(
    level: Level,
    target: &str,
    message: &str,
    fields: &[DesktopLogField],
) -> DesktopLogCategory {
    if is_warning_level(level) {
        return DesktopLogCategory::Warnings;
    }

    let mut haystack = String::with_capacity(target.len() + message.len() + 64);
    haystack.push_str(&target.to_ascii_lowercase());
    haystack.push(' ');
    haystack.push_str(&message.to_ascii_lowercase());
    for field in fields {
        haystack.push(' ');
        haystack.push_str(&field.name.to_ascii_lowercase());
        haystack.push('=');
        haystack.push_str(&field.value.to_ascii_lowercase());
    }

    if contains_any(
        &haystack,
        &[
            "peer",
            "peering",
            "p2p",
            "iroh",
            "relay",
            "dial",
            "replicator",
            "listen_addresses",
            "ticket",
            "node_id",
        ],
    ) {
        DesktopLogCategory::Peering
    } else if contains_any(
        &haystack,
        &[
            "replication",
            "replica",
            "schema",
            "collection",
            "subscribe",
            "subscription",
            "snapshot",
            "gossip",
            "observer",
            "update",
        ],
    ) {
        DesktopLogCategory::Replication
    } else if contains_any(
        &haystack,
        &[
            "request",
            "response",
            "turn",
            "session",
            "conversation",
            "tool_call",
            "tool result",
            "tool_result",
            "scheduler",
            "scheduled task",
        ],
    ) {
        DesktopLogCategory::Turns
    } else if contains_any(
        &haystack,
        &[
            "write", "mutation", "upsert", "save_", "saved", "persist", "apply", "create", "submit",
        ],
    ) {
        DesktopLogCategory::Writes
    } else {
        DesktopLogCategory::Replication
    }
}

fn is_warning_level(level: Level) -> bool {
    matches!(level, Level::WARN | Level::ERROR)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn prune_recent_event_window(window: &mut VecDeque<DateTime<Utc>>, now: DateTime<Utc>) {
    let cutoff = now - Duration::seconds(EVENTS_PER_SECOND_WINDOW_SECS);
    while window.front().is_some_and(|timestamp| *timestamp < cutoff) {
        window.pop_front();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_warns_as_warning_category() {
        let category = classify_log_entry(Level::WARN, "desktop::client", "dial failed", &[]);
        assert_eq!(category, DesktopLogCategory::Warnings);
    }

    #[test]
    fn classify_peering_terms_as_peering() {
        let category = classify_log_entry(
            Level::INFO,
            "defra_node::p2p",
            "connected peer over iroh",
            &[],
        );
        assert_eq!(category, DesktopLogCategory::Peering);
    }

    #[test]
    fn events_per_second_uses_rolling_window() {
        let store = DesktopLogStore::new(8);
        let now = Utc::now();
        store.record_manual(
            now - Duration::seconds(31),
            Level::INFO,
            "desktop::old",
            "stale event",
            [],
        );
        store.record_manual(
            now - Duration::seconds(5),
            Level::INFO,
            "desktop::fresh",
            "fresh event",
            [],
        );
        store.record_manual(
            now - Duration::seconds(1),
            Level::INFO,
            "desktop::fresh",
            "fresh event 2",
            [],
        );

        let snapshot = store.snapshot();

        assert_eq!(snapshot.entries.len(), 3);
        assert!((snapshot.events_per_second - (2.0 / 30.0)).abs() < f32::EPSILON);
    }
}
