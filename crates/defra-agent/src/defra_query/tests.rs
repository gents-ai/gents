use std::sync::Arc;

use rig::tool::Tool;
use serde_json::json;

use super::*;

async fn seeded_node() -> Arc<defra_node::EmbeddedNode> {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    for (request_id, status) in [
        ("req-pending-1", "pending"),
        ("req-pending-2", "pending"),
        ("req-completed-1", "completed"),
    ] {
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "did:key:z-test",
                    status: "{status}",
                    content: "hello"
                }}) {{ _docID }}
            }}"#
        );
        let resp = node.execute(&mutation).await;
        assert!(!resp.has_errors(), "seed insert failed: {:?}", resp.errors);
    }

    node
}

#[tokio::test]
async fn returns_only_rows_matching_the_filter() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let output = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentRequest".to_string(),
            filter: Some(json!({ "status": { "_eq": "pending" } })),
            fields: vec!["request_id".to_string(), "status".to_string()],
            limit: None,
        },
    )
    .await
    .expect("filtered query should succeed");

    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(parsed["collection"], "AgentRequest");
    assert_eq!(parsed["count"], 2);

    let results = parsed["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for row in results {
        assert_eq!(row["status"], "pending");
        assert!(row["request_id"]
            .as_str()
            .unwrap()
            .starts_with("req-pending-"));
    }
}

#[tokio::test]
async fn rejects_query_against_collection_outside_scope() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(
        node,
        CollectionScope::restricted(vec!["AgentSession".to_string()]),
    );

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentRequest".to_string(),
            filter: None,
            fields: vec!["request_id".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("querying outside the allowed scope must fail");

    assert!(
        err.to_string()
            .contains("not within the allowed query scope"),
        "{err}"
    );
}
