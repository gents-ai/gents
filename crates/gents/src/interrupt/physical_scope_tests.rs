use super::*;
use serde_json::{json, Value};

async fn insert(node: &EmbeddedNode, collection: &str, value: Value) -> String {
    crate::config_client::ConfigAccess::transact_local(node, None, "test.physical_cancel_fixture", |txn| {
        let value = &value;
        Box::pin(async move {
            let query = format!("mutation($input: {collection}MutationInputArg!) {{create_{collection}(input:$input){{_docID}}}}");
            let response = txn.execute_with_variables(&query, &json!({"input":value})).await?;
            gents_protocol::graphql::extract_mutation_doc_id(&response, collection)
        })
    }).await.unwrap()
}

#[tokio::test]
async fn physical_interrupt_preserves_exact_scope_and_never_reaches_caused_requests() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let parent = insert(&node, "AgentRequest", json!({"request_id":"parent","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"same-session","agent_id":"configured","lifecycle_state":"processing"})).await;
    let foreign = insert(&node, "AgentRequest", json!({"request_id":"parent","purpose":"normal","node_did":"foreign","requester_did":null,"session_id":"same-session","agent_id":"configured","lifecycle_state":"processing"})).await;
    let bridge = insert(&node, "AgentToolCall", json!({"request_id":"parent","request_doc_id":parent,"node_did":"owner","requester_did":null,"session_id":"same-session","tool_call_id":"spawn","tool_name":"agent_new","lifecycle_state":"running","await_mode":"background"})).await;
    let child = insert(&node, "AgentRequest", json!({"request_id":"child","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"child-session","agent_id":"configured","lifecycle_state":"processing","caused_by_parent_request_id":"parent","caused_by_parent_request_doc_id":parent,"caused_by_parent_tool_call_id":"spawn","caused_by_parent_tool_call_doc_id":bridge})).await;
    for (owner, requester) in [
        ("foreign", None),
        ("owner", Some("requester")),
        ("owner", Some("")),
    ] {
        assert!(interrupt_request_by_doc_id_with_access(
            &crate::config_client::ConfigAccess::Local(node.clone()),
            &parent,
            owner,
            requester
        )
        .await
        .is_err());
    }
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    interrupt_request_by_doc_id_with_access(&access, &parent, "owner", None)
        .await
        .unwrap();
    let snapshot = node
        .execute("{AgentRequest { _docID interrupt_requested_at }}")
        .await;
    assert!(!snapshot.has_errors(), "{:?}", snapshot.errors);
    let data = snapshot.data.unwrap();
    let rows = data["AgentRequest"].as_array().unwrap();
    let first =
        rows.iter().find(|row| row["_docID"] == parent).unwrap()["interrupt_requested_at"].clone();
    assert!(first.is_string());
    assert!(
        rows.iter().find(|row| row["_docID"] == foreign).unwrap()["interrupt_requested_at"]
            .is_null()
    );
    assert!(
        rows.iter().find(|row| row["_docID"] == child).unwrap()["interrupt_requested_at"].is_null()
    );
    interrupt_request_by_doc_id_with_access(&access, &parent, "owner", None)
        .await
        .unwrap();
    let result = node
        .execute(&format!(
            "{{AgentRequest(filter:{{_docID:{{_eq:\"{}\"}}}}){{interrupt_requested_at}}}}",
            escape_graphql_string(&parent)
        ))
        .await;
    assert_eq!(
        result.data.unwrap()["AgentRequest"][0]["interrupt_requested_at"],
        first
    );
    node.shutdown().await;
}

#[tokio::test]
async fn colliding_logical_interrupt_fetch_rejects_ambiguity_and_binds_owner_physical() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let owner = insert(
        &node,
        "AgentRequest",
        json!({"request_id":"shared","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"s","agent_id":"configured","lifecycle_state":"processing"}),
    )
    .await;
    let foreign = insert(
        &node,
        "AgentRequest",
        json!({"request_id":"shared","purpose":"normal","node_did":"foreign","requester_did":null,"session_id":"s","agent_id":"configured","lifecycle_state":"processing"}),
    )
    .await;

    // The foreign principal latches only its own physical row.
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    interrupt_request_by_doc_id_with_access(&access, &foreign, "foreign", None)
        .await
        .unwrap();

    // The physical fetch binds exactly one physical row and never reads the
    // foreign principal's latch through the owner's key.
    assert_eq!(
        fetch_interrupt_requested_at_by_doc_id(&node, &owner)
            .await
            .unwrap(),
        None
    );
    assert!(fetch_interrupt_requested_at_by_doc_id(&node, &foreign)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        fetch_interrupt_requested_at_by_doc_id(&node, "missing-doc")
            .await
            .unwrap(),
        None
    );

    // The surviving logical reader fails closed on the colliding logical id
    // instead of silently reading one replica's row with `limit: 1`.
    assert!(
        fetch_interrupt_requested_at(&node, "shared").await.is_err(),
        "colliding logical id must be rejected, not resolved by limit 1"
    );
    assert_eq!(
        fetch_interrupt_requested_at_scoped(&node, "shared", "owner", None)
            .await
            .unwrap(),
        None,
        "owner-scoped fetch must not observe the foreign principal's latch"
    );
    assert!(
        fetch_interrupt_requested_at_scoped(&node, "shared", "foreign", None)
            .await
            .unwrap()
            .is_some()
    );
    node.shutdown().await;
}
