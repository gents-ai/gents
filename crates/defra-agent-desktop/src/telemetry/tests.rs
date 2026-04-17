use chrono::{Duration, Utc};
use tracing::Level;

use super::{classify_log_entry, DesktopLogCategory, DesktopLogStore};

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
