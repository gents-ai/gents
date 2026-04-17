use chrono::Utc;
use tracing::Level;

use crate::state::LogsFilter;
use crate::telemetry::{DesktopLogCategory, DesktopLogEntry};

use super::filtering::matches_filter;
use super::rail::format_bytes;
use super::summary_text;

#[test]
fn filter_matches_selected_category() {
    let entry = DesktopLogEntry {
        id: 1,
        timestamp: Utc::now(),
        level: Level::INFO,
        target: "desktop::observe".to_string(),
        category: DesktopLogCategory::Replication,
        message: "snapshot refreshed".to_string(),
        fields: Vec::new(),
    };

    assert!(matches_filter(&entry, LogsFilter::All));
    assert!(matches_filter(
        &entry,
        LogsFilter::Category(DesktopLogCategory::Replication)
    ));
    assert!(!matches_filter(
        &entry,
        LogsFilter::Category(DesktopLogCategory::Warnings)
    ));
}

#[test]
fn format_bytes_uses_human_units() {
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(2048), "2.0 KiB");
}

#[test]
fn summary_text_reports_filtered_and_total_counts() {
    assert_eq!(summary_text(3, 12), "3 shown · 12 captured");
}
