use super::*;
use serde_json::{json, Value};

async fn insert(node: &Arc<EmbeddedNode>, collection: &str, value: Value) -> String {
    gents_store::config_client::ConfigAccess::transact_local_write(node, None, "test.physical_cancel_fixture", |txn| {
        let value = &value;
        Box::pin(async move {
            let query = format!("mutation($input: {collection}MutationInputArg!) {{create_{collection}(input:$input){{_docID}}}}");
            let response = txn.execute_with_variables(&query, &json!({"input":value})).await?;
            Ok::<_, anyhow::Error>(gents_protocol::graphql::extract_mutation_doc_id(&response, collection)?)
        })
    }).await.unwrap()
}

/// A session and the interrupt marker created with it.
async fn session(node: &Arc<EmbeddedNode>, node_did: &str, session_id: &str) {
    gents_store::config_client::ConfigAccess::transact_local_write(node, None, "test.physical_cancel_session", |txn| {
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input:$input){_docID}}",
                &json!({"input": {"node_did": node_did, "session_id": session_id, "agent_did": "configured", "created_at": "2026-01-01T00:00:00Z"}}),
            ).await?;
            let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentSession")?;
            txn.execute_with_variables(
                "mutation($input: AgentSessionInterruptMutationInputArg!) {create_AgentSessionInterrupt(input:$input){_docID}}",
                &json!({"input": {"session_doc_id": doc, "node_did": node_did, "session_id": session_id, "generation": 0}}),
            ).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap()
}

async fn interrupted_at(node: &Arc<EmbeddedNode>, doc: &str) -> Option<String> {
    request_interrupted_at_by_doc_id_with_access(
        &gents_store::config_client::ConfigAccess::local(node.clone()),
        doc,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn physical_interrupt_preserves_exact_scope_and_never_reaches_caused_requests() {
    let node_db = gents_inference::test_support::test_db(
        "physical-interrupt-preserves-exact-scope",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    for (node_did, session_id) in [
        ("owner", "same-session"),
        ("foreign", "same-session"),
        ("owner", "child-session"),
    ] {
        session(&node, node_did, session_id).await;
    }
    let parent = insert(&node, "AgentRequest", json!({"request_id":"parent","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"same-session","agent_did":"configured","lifecycle_state":"processing","interrupt_generation":0})).await;
    let foreign = insert(&node, "AgentRequest", json!({"request_id":"parent","purpose":"normal","node_did":"foreign","requester_did":null,"session_id":"same-session","agent_did":"configured","lifecycle_state":"processing","interrupt_generation":0})).await;
    let bridge = insert(&node, "AgentToolCall", json!({"request_id":"parent","request_doc_id":parent,"node_did":"owner","requester_did":null,"session_id":"same-session","tool_call_id":"spawn","tool_name":"agent_new","lifecycle_state":"running","await_mode":"background"})).await;
    let child = insert(&node, "AgentRequest", json!({"request_id":"child","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"child-session","agent_did":"configured","lifecycle_state":"processing","interrupt_generation":0,"caused_by_parent_request_id":"parent","caused_by_parent_request_doc_id":parent,"caused_by_parent_tool_call_id":"spawn","caused_by_parent_tool_call_doc_id":bridge})).await;
    for (owner, requester) in [
        ("foreign", None),
        ("owner", Some("requester")),
        ("owner", Some("")),
    ] {
        assert!(interrupt_request_by_doc_id_with_access(
            &gents_store::config_client::ConfigAccess::local(node.clone()),
            &parent,
            owner,
            requester
        )
        .await
        .is_err());
    }
    let access = gents_store::config_client::ConfigAccess::local(node.clone());
    interrupt_request_by_doc_id_with_access(&access, &parent, "owner", None)
        .await
        .unwrap();
    let first = interrupted_at(&node, &parent).await;
    assert!(first.is_some());
    assert_eq!(interrupted_at(&node, &foreign).await, None);
    assert_eq!(interrupted_at(&node, &child).await, None);
    assert!(matches!(
        interrupt_request_by_doc_id_with_access(&access, &parent, "owner", None)
            .await
            .unwrap(),
        RequestInterrupt::Interrupted { already: true, .. }
    ));
    assert_eq!(interrupted_at(&node, &parent).await, first);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn colliding_logical_interrupt_fetch_binds_owner_physical() {
    let node_db = gents_inference::test_support::test_db(
        "colliding-logical-interrupt-fetch-reject",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    session(&node, "owner", "s").await;
    session(&node, "foreign", "s").await;
    let owner = insert(
        &node,
        "AgentRequest",
        json!({"request_id":"shared","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"s","agent_did":"configured","lifecycle_state":"processing","interrupt_generation":0}),
    )
    .await;
    let foreign = insert(
        &node,
        "AgentRequest",
        json!({"request_id":"shared","purpose":"normal","node_did":"foreign","requester_did":null,"session_id":"s","agent_did":"configured","lifecycle_state":"processing","interrupt_generation":0}),
    )
    .await;

    let access = gents_store::config_client::ConfigAccess::local(node.clone());
    interrupt_request_by_doc_id_with_access(&access, &foreign, "foreign", None)
        .await
        .unwrap();

    assert_eq!(interrupted_at(&node, &owner).await, None);
    assert!(interrupted_at(&node, &foreign).await.is_some());
    assert_eq!(interrupted_at(&node, "missing-doc").await, None);
    assert_eq!(
        observe_request_interrupted_at_with_access(&access, "shared", "owner", None)
            .await
            .unwrap()
            .interrupted_at,
        None,
        "owner-scoped fetch must not observe the foreign principal's interrupt"
    );
    assert!(
        observe_request_interrupted_at_with_access(&access, "shared", "foreign", None)
            .await
            .unwrap()
            .interrupted_at
            .is_some()
    );
    node.shutdown().await.expect("shut down test node");
}
