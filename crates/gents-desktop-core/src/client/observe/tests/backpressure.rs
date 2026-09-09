use super::*;

#[tokio::test(start_paused = true)]
async fn idle_observer_publishes_without_advancing_a_batch_timer() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let mut changes = store.subscribe();
    let started = tokio::time::Instant::now();
    seed_principal(node.as_ref(), "did:immediate").await;
    tokio::time::timeout(Duration::from_millis(100), changes.changed())
        .await
        .expect("idle observer must not wait 150ms")
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    handle.shutdown().await;
}

#[tokio::test]
async fn committed_history_burst_does_not_force_a_snapshot_reload() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let mut raw = node.subscribe(&[EventName::Update]);
    let mut changes = store.subscribe();
    seed_principal(node.as_ref(), "did:history").await;
    let update = raw.recv().await.expect("committed update");
    node.event_bus().unsubscribe(raw.id());
    tokio::time::timeout(Duration::from_secs(5), changes.changed())
        .await
        .expect("initial projection deadline")
        .expect("projection alive");
    let before = handle.metrics_snapshot();

    // Match the merge boundary: all revision events are published synchronously
    // after commit, before a single-threaded mobile executor can drain them.
    for _ in 0..12_000 {
        node.event_bus().publish(update.clone());
    }
    tokio::time::timeout(Duration::from_secs(5), changes.changed())
        .await
        .expect("history projection deadline")
        .expect("projection alive");
    let after = handle.metrics_snapshot();
    assert_eq!(after.drop_recoveries, before.drop_recoveries);
    assert_eq!(after.scope_reloads, before.scope_reloads);
    assert!(after.docs_fetched - before.docs_fetched <= 1);
    assert_eq!(
        after.document_change_batches - before.document_change_batches,
        1
    );
    assert_eq!(after.coalesced_updates - before.coalesced_updates, 11_999);
    assert!(store
        .snapshot()
        .agent_principals
        .iter()
        .any(|p| p.agent_did == "did:history"));
    handle.shutdown().await;
}

#[tokio::test]
async fn change_after_drain_is_not_hidden_by_the_previous_snapshot() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let mut changes = store.subscribe();
    seed_principal(node.as_ref(), "did:first").await;
    tokio::time::timeout(Duration::from_secs(5), changes.changed())
        .await
        .unwrap()
        .unwrap();
    seed_principal(node.as_ref(), "did:next").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .snapshot()
                .agent_principals
                .iter()
                .any(|p| p.agent_did == "did:next")
            {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("subsequent change must be visible without another write");
    assert_eq!(handle.metrics_snapshot().drop_recoveries, 0);
    handle.shutdown().await;
}
