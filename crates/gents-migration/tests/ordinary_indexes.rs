use gents_migration::ensure_migrations;

mod common;
use common::fresh_node;

#[tokio::test]
async fn existing_request_indexes_reconcile_without_rewriting_documents_or_lineage() {
    let node = fresh_node().await;
    let sdl = gents_protocol::schemas::AGENT_REQUEST.replace(
        "@index(name: \"AgentRequest_node_session\", fields: [\"node_did\", \"session_id\"])",
        "",
    );
    node.add_schema(&sdl).await.unwrap();
    let before = node.get_collection("AgentRequest").unwrap().unwrap();
    let created = node.execute(r#"mutation {
        create_AgentRequest(input: [
            {request_id: "one", node_did: "node", session_id: "session", purpose: "normal", lifecycle_state: "completed", content: "one"},
            {request_id: "two", node_did: "node", session_id: "session", purpose: "normal", lifecycle_state: "completed", content: "two"},
            {request_id: "foreign", node_did: "foreign", session_id: "session", purpose: "normal", lifecycle_state: "completed", content: "foreign"}
        ]) { _docID }
    }"#).await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let query = "{ AgentRequest(order: {request_id: ASC}) { _docID request_id node_did session_id content lifecycle_state } }";
    let documents = node.execute(query).await.data.unwrap();
    ensure_migrations(&node).await.unwrap();
    let (left, right) = tokio::join!(ensure_migrations(&node), ensure_migrations(&node));
    left.unwrap();
    right.unwrap();
    let after = node.get_collection("AgentRequest").unwrap().unwrap();
    assert_eq!(after.version_id, before.version_id);
    assert_eq!(after.collection_id, before.collection_id);
    assert_eq!(node.execute(query).await.data.unwrap(), documents);
    for index in &before.indexes {
        assert!(after
            .indexes
            .iter()
            .any(|current| serde_json::to_value(current).unwrap()
                == serde_json::to_value(index).unwrap()));
    }
    assert_eq!(
        after
            .indexes
            .iter()
            .filter(|index| index.name == "AgentRequest_node_session")
            .count(),
        1
    );
    let id = documents["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["request_id"] == "one")
        .unwrap()["_docID"]
        .as_str()
        .unwrap();
    let id = gents_protocol::graphql::escape_graphql_string(id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "node"}}, session_id: {{_eq: "session"}}, _docID: {{_neq: "{id}"}}}}) {{ request_id }} }}"#
    );
    let result = node.execute(&query).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    assert_eq!(
        result.data.unwrap()["AgentRequest"],
        serde_json::json!([{"request_id": "two"}])
    );
    let plan = node.execute(&format!("query @explain {query}")).await;
    assert!(!plan.has_errors(), "{:?}", plan.errors);
    let plan = plan.data.unwrap().to_string();
    assert!(
        after.indexes.iter().any(|index| {
            index
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .take(2)
                .eq(["node_did", "session_id"])
                && plan.contains(&index.name)
        }),
        "{plan}"
    );
    node.shutdown().await;
}
