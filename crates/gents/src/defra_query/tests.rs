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

/// Selecting a field that does not exist on the collection must produce an
/// agent-usable diagnostic: the collection name, the invalid field, close-match
/// suggestions, and the allowed field inventory — not just DefraDB's raw
/// "Cannot query field" error.
#[tokio::test]
async fn invalid_tool_call_created_at_suggests_started_and_completed_at() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentToolCall".to_string(),
            filter: None,
            fields: vec!["tool_name".to_string(), "created_at".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("invalid field must fail");

    let msg = err.to_string();
    assert!(msg.contains("AgentToolCall"), "{msg}");
    assert!(msg.contains("created_at"), "{msg}");
    assert!(msg.contains("started_at"), "suggestion missing: {msg}");
    assert!(msg.contains("completed_at"), "suggestion missing: {msg}");
    // Inventory: a valid field the caller did not mention must be listed.
    assert!(msg.contains("tool_call_key"), "inventory missing: {msg}");
}

/// `AgentRequest.agent_name` (lives on AgentConversation, not AgentRequest)
/// and `AgentRequest.updated_at` (only `created_at` exists) are the canonical
/// operator mistakes from #592 — both must get suggestions.
#[tokio::test]
async fn invalid_agent_request_fields_get_suggestions() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentRequest".to_string(),
            filter: None,
            fields: vec!["agent_name".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("invalid field must fail");
    let msg = err.to_string();
    assert!(msg.contains("agent_name"), "{msg}");
    assert!(msg.contains("agent_did"), "suggestion missing: {msg}");
    assert!(msg.contains("request_id"), "inventory missing: {msg}");

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentRequest".to_string(),
            filter: None,
            fields: vec!["request_id".to_string(), "updated_at".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("invalid field must fail");
    let msg = err.to_string();
    assert!(msg.contains("updated_at"), "{msg}");
    assert!(msg.contains("created_at"), "suggestion missing: {msg}");
}

/// An invalid field referenced only in the filter gets the same diagnostic.
#[tokio::test]
async fn invalid_filter_key_gets_diagnostics() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentToolCall".to_string(),
            filter: Some(json!({ "created_at": { "_gt": "2026-01-01" } })),
            fields: vec!["tool_name".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("invalid filter key must fail");
    let msg = err.to_string();
    assert!(msg.contains("created_at"), "{msg}");
    assert!(msg.contains("started_at"), "suggestion missing: {msg}");
}

/// `fields: ["*"]` is discovery mode: return the queryable field inventory
/// (with types) instead of documents.
#[tokio::test]
async fn wildcard_fields_returns_field_inventory() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let output = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentRequest".to_string(),
            filter: None,
            fields: vec!["*".to_string()],
            limit: None,
        },
    )
    .await
    .expect("discovery must succeed");

    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(parsed["collection"], "AgentRequest");
    assert_eq!(parsed["discovery"], true);
    let names: Vec<&str> = parsed["fields"]
        .as_array()
        .expect("fields array")
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"request_id"), "{names:?}");
    assert!(names.contains(&"status"), "{names:?}");
    assert!(!names.contains(&"AVG"), "aggregates hidden: {names:?}");
    assert!(!names.contains(&"_version"), "internals hidden: {names:?}");
}

/// Discovery must not advertise restricted secret fields.
#[tokio::test]
async fn discovery_excludes_restricted_fields() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let output = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "InferenceBackend".to_string(),
            filter: None,
            fields: vec!["*".to_string()],
            limit: None,
        },
    )
    .await
    .expect("discovery must succeed");

    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    let names: Vec<&str> = parsed["fields"]
        .as_array()
        .expect("fields array")
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"backend_id"), "{names:?}");
    assert!(!names.contains(&"api_key"), "secret leaked: {names:?}");
    assert!(
        !names.contains(&"api_key_env_var"),
        "secret leaked: {names:?}"
    );
}

/// Discovery still honors the collection scope.
#[tokio::test]
async fn discovery_respects_collection_scope() {
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
            fields: vec!["*".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("discovery outside scope must fail");
    assert!(
        err.to_string()
            .contains("not within the allowed query scope"),
        "{err}"
    );
}

/// Querying a collection that does not exist says so plainly.
#[tokio::test]
async fn unknown_collection_reports_does_not_exist() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "NoSuchCollection".to_string(),
            filter: None,
            fields: vec!["x".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("unknown collection must fail");
    let msg = err.to_string();
    assert!(msg.contains("NoSuchCollection"), "{msg}");
    assert!(msg.contains("does not exist"), "{msg}");
}

/// Mixing "*" with concrete fields is rejected with a pointer at discovery.
#[tokio::test]
async fn wildcard_mixed_with_fields_is_rejected_with_hint() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: "AgentRequest".to_string(),
            filter: None,
            fields: vec!["request_id".to_string(), "*".to_string()],
            limit: None,
        },
    )
    .await
    .expect_err("mixed wildcard must fail");
    assert!(err.to_string().contains("[\"*\"]"), "{err}");
}
