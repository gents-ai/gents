use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use defra_agent::{ActiveRuntimeSnapshot, EventSource, UpdateSubscriptionSource};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod support;

use support::mock_subscription::MockUpdateSubscriptionSource;
use support::test_db;

#[tokio::test]
async fn integration_can_construct_event_source_with_mock_subscription_source() {
    let db = test_db("event-source-subscription-factory-smoke").await;
    let snapshot = Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: String::new(),
        paired_peer_dids: HashSet::new(),
        default_behavior_id: "test".to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
    });
    let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);

    let mock = MockUpdateSubscriptionSource::new();
    let subs: Arc<dyn UpdateSubscriptionSource> = Arc::new(mock.clone());
    let _source = EventSource::with_subscription_source(
        subs,
        snapshot_rx,
        db.node.clone(),
        CancellationToken::new(),
    );

    mock.publish_update("collection-id", "doc-id");
}
