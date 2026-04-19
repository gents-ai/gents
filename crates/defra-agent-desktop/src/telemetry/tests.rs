use chrono::{Duration, Utc};
use tracing::Instrument;
use tracing::Level;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

use super::{classify_log_entry, DesktopLogCategory, DesktopLogLayer, DesktopLogStore};

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

#[tokio::test]
async fn layer_captures_active_span_fields_on_events() {
    let store = std::sync::Arc::new(DesktopLogStore::new(8));
    let subscriber = Registry::default().with(DesktopLogLayer::new(store.clone()));
    let guard = tracing::subscriber::set_default(subscriber);

    async {
        tracing::info!(message = "span-scoped event", agent_did = "did:defra:test");
    }
    .instrument(tracing::info_span!(
        "live_remote_agent",
        deployment_label = "Alpha Server",
        agent_did = "did:defra:test"
    ))
    .await;

    drop(guard);

    let snapshot = store.snapshot();
    let entry = snapshot.entries.first().expect("entry captured");
    assert!(entry
        .fields
        .iter()
        .any(|field| field.name == "span.deployment_label" && field.value == "Alpha Server"));
    assert!(entry
        .fields
        .iter()
        .any(|field| field.name == "span.agent_did" && field.value == "did:defra:test"));
}
