use crate::client_projection::observe::{ObservedStores, RuntimeConnectionStatus};
use std::time::Duration;

pub async fn wait_for_name(store: &ObservedStores, expected: Option<&str>) {
    let mut notices = store.subscribe();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let name = store.node_dids().into_iter().find_map(|node| {
                store
                    .snapshot(&node)
                    .nodes
                    .first()
                    .and_then(|row| row.display_name.clone())
            });
            if name.as_deref() == expected {
                break;
            }
            notices.changed().await.expect("observer alive");
        }
    })
    .await
    .expect("authenticated runtime observation");
}

pub async fn wait_for_connection(
    store: &ObservedStores,
    node: &str,
    expected: RuntimeConnectionStatus,
) {
    let mut notices = store.subscribe();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store.runtime_connection(node) == Some(expected) {
                break;
            }
            notices.changed().await.expect("observer alive");
        }
    })
    .await
    .expect("runtime connection status");
}

pub fn resync_notices(stores: &ObservedStores) -> tokio::sync::watch::Receiver<u64> {
    stores.subscribe_resync_requests()
}
