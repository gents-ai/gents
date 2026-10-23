use super::*;

#[tokio::test]
async fn canonical_selection_reuses_the_callers_read_snapshot() {
    let builder = tokio::task::spawn_blocking(crate::test_support::node_builder)
        .await
        .unwrap();
    let node = Arc::new(builder.build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let did = node.node_identity_did().unwrap().to_owned();
    let request = HydrationRequest {
        request_key: "atomic-peer:atomic-session".into(),
        peer_id: "atomic-peer".into(),
        requester_did: did.clone(),
        node_did: did,
        session_id: "atomic-session".into(),
    };
    ConfigAccess::transact_local_readonly(&node, None, "test.hydration.atomic_selection", |txn| {
        let request = &request;
        let writer = Arc::clone(&node);
        Box::pin(async move {
            let before = load_hydration_selection_in_txn(txn, request).await?;
            assert!(before.documents.is_empty());
            let did = escape_graphql_string(&request.node_did);
            tokio::spawn(async move {
                ConfigAccess::write_local(
                    &writer,
                    "test.hydration.concurrent_selection",
                    &format!(
                        r#"mutation {{ create_CompactionEntry(input: {{
                    compaction_key: "atomic-compaction", session_id: "atomic-session",
                    node_did: "{did}", requester_did: "{did}", sequence: 1
                    }}) {{ _docID }} }}"#,
                    ),
                )
                .await
            })
            .await??;
            let after = load_hydration_selection_in_txn(txn, request).await?;
            assert_eq!(
                after.documents, before.documents,
                "a concurrent commit must not change the selected read snapshot"
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    let fresh = load_hydration_selection(&node, &request).await.unwrap();
    assert_eq!(fresh.documents.len(), 1);
    assert!(fresh.documents.iter().all(|document| document.collection
        == gents_protocol::session_hydration::SessionHydrationCollection::CompactionEntry));
    node.shutdown().await;
}

#[test]
fn missing_authorized_roots_do_not_certify_empty_selection() {
    let omitted = defra_node::QueryResponse::success(serde_json::json!({}));
    assert!(required_rows::<serde_json::Value>(&omitted, "AgentToolCall").is_err());
    let explicit = defra_node::QueryResponse::success(serde_json::json!({"AgentToolCall": []}));
    assert!(
        required_rows::<serde_json::Value>(&explicit, "AgentToolCall")
            .unwrap()
            .is_empty()
    );
}
