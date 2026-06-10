use std::sync::Arc;

use crate::llm::tool::Tool;
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

/// Seed a node with a row that has a deliberately long `content` field
/// (exceeding `MAX_FIELD_STRING_BYTES`) and confirm the tool output is:
///   - valid JSON (parseable)
///   - `truncated: true` in the envelope
///   - the oversized field contains the honest truncation marker
#[tokio::test]
async fn oversized_field_is_truncated_json_stays_valid() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    // Build a content string that clearly exceeds MAX_FIELD_STRING_BYTES.
    let big_content = "x".repeat(MAX_FIELD_STRING_BYTES + 5_000);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "req-big-content",
                agent_did: "did:key:z-test",
                status: "pending",
                content: "{big_content}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "seed insert failed: {:?}", resp.errors);

    let tool = DefraQueryTool::new(Arc::clone(&node), CollectionScope::all());
    let raw_output = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentRequest".to_string(),
            filter: Some(json!({ "request_id": { "_eq": "req-big-content" } })),
            fields: vec!["request_id".to_string(), "content".to_string()],
            limit: None,
        },
    )
    .await
    .expect("query must succeed");

    // Must be valid JSON.
    let parsed: serde_json::Value =
        serde_json::from_str(&raw_output).expect("output must be parseable JSON after truncation");

    // Envelope fields.
    assert_eq!(parsed["collection"], "AgentRequest");
    assert_eq!(parsed["truncated"], true, "truncated flag must be true");
    assert!(
        parsed["total_bytes"].as_u64().unwrap_or(0) > 0,
        "total_bytes must be reported"
    );

    // The `content` field in the result must carry the honest marker.
    let rows = parsed["results"].as_array().expect("results array");
    assert_eq!(rows.len(), 1, "exactly one matching row");
    let content = rows[0]["content"]
        .as_str()
        .expect("content must be a string");
    assert!(
        content.contains("[truncated: showed"),
        "honest truncation marker must be present in content field: {content}"
    );
    assert!(
        content.len() < big_content.len(),
        "returned content must be shorter than the original: returned={}, original={}",
        content.len(),
        big_content.len()
    );
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
