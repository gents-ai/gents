use std::collections::VecDeque;
use std::sync::{Arc, OnceLock, RwLock};

use chrono::{DateTime, Duration, Utc};
use tracing::Level;

use super::{classify_log_entry, layer::DesktopLogLayer, DesktopLogEntry, DesktopLogField};

const DEFAULT_LOG_CAPACITY: usize = 4096;
const EVENTS_PER_SECOND_WINDOW_SECS: i64 = 30;

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

    pub(crate) fn record_entry(
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

pub fn global_log_store() -> Arc<DesktopLogStore> {
    static GLOBAL_LOG_STORE: OnceLock<Arc<DesktopLogStore>> = OnceLock::new();
    GLOBAL_LOG_STORE
        .get_or_init(|| Arc::new(DesktopLogStore::default()))
        .clone()
}

pub fn global_log_layer() -> DesktopLogLayer {
    DesktopLogLayer::new(global_log_store())
}

fn prune_recent_event_window(window: &mut VecDeque<DateTime<Utc>>, now: DateTime<Utc>) {
    let cutoff = now - Duration::seconds(EVENTS_PER_SECOND_WINDOW_SECS);
    while window.front().is_some_and(|timestamp| *timestamp < cutoff) {
        window.pop_front();
    }
}
