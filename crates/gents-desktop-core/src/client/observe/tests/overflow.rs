use super::*;

#[tokio::test]
async fn distinct_document_overflow_reloads_once_and_keeps_observing() {
    let (_tempdir, node, store, handle) = build_observer_fixture().await;
    let mut raw = node.subscribe(&[EventName::Update]);
    let mut changes = store.subscribe();
    seed_principal(node.as_ref(), "did:survives-overflow").await;
    let update = raw.recv().await.unwrap().as_update().unwrap().clone();
    node.event_bus().unsubscribe(raw.id());
    tokio::time::timeout(Duration::from_secs(5), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let before = handle.metrics_snapshot();
    for id in 0..12_000 {
        let mut changed = update.clone();
        changed.doc_id = format!("distinct-{id}");
        node.event_bus().publish(events::Message::update(changed));
    }
    tokio::time::timeout(Duration::from_secs(5), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let after = handle.metrics_snapshot();
    assert_eq!(after.drop_recoveries - before.drop_recoveries, 1);
    assert_eq!(after.scope_reloads - before.scope_reloads, 1);
    assert!(store
        .snapshot()
        .agent_principals
        .iter()
        .any(|p| p.agent_did == "did:survives-overflow"));

    seed_principal(node.as_ref(), "did:after-overflow").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !store
            .snapshot()
            .agent_principals
            .iter()
            .any(|p| p.agent_did == "did:after-overflow")
        {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("normal observation resumes after resync");
    handle.shutdown().await;
}
