use std::sync::Arc;

use crate::llm::tool::Tool;
use serde_json::{json, Value};

use super::*;

async fn seeded_node() -> Arc<defra_node::EmbeddedNode> {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    for (request_id, lifecycle_state) in [
        ("req-pending-1", "pending"),
        ("req-pending-2", "pending"),
        ("req-completed-1", "completed"),
    ] {
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    purpose: "normal",
                    agent_did: "did:key:z-test",
                    lifecycle_state: "{lifecycle_state}",
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
                purpose: "normal",
                agent_did: "did:key:z-test",
                lifecycle_state: "pending",
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
        }
        .into(),
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
            filter: Some(json!({ "lifecycle_state": { "_eq": "pending" } })),
            fields: vec!["request_id".to_string(), "lifecycle_state".to_string()],
            limit: None,
        }
        .into(),
    )
    .await
    .expect("filtered query should succeed");

    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(parsed["collection"], "AgentRequest");
    assert_eq!(parsed["returned_count"], 2);

    let results = parsed["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for row in results {
        assert_eq!(row["lifecycle_state"], "pending");
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
        }
        .into(),
    )
    .await
    .expect_err("querying outside the allowed scope must fail");

    assert!(
        err.to_string()
            .contains("not within the allowed query scope"),
        "{err}"
    );
}

/// The unrestricted scope is the default for the MCP endpoint and for
/// `gents query`, so it is the path that would otherwise hand an agent in the
/// launching home the eval material. Both the row read and the discovery field
/// inventory must refuse before they touch the datastore.
#[tokio::test]
async fn refuses_a_protected_collection_even_when_unrestricted() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: gents_protocol::schemas::EVAL_VERDICT_NAME.to_string(),
            filter: None,
            fields: vec!["score_bp".to_string()],
            limit: None,
        }
        .into(),
    )
    .await
    .expect_err("a protected collection must never be readable");

    let message = format!("{err:#}");
    assert!(message.contains("protected"), "{message}");
    assert!(
        message.contains(gents_protocol::schemas::EVAL_VERDICT_NAME),
        "{message}"
    );
}

#[tokio::test]
async fn refuses_discovery_of_a_protected_collection() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());

    let err = Tool::call(
        &tool,
        DefraQueryParams {
            collection: gents_protocol::schemas::EVAL_VERDICT_NAME.to_string(),
            filter: None,
            fields: vec!["*".to_string()],
            limit: None,
        }
        .into(),
    )
    .await
    .expect_err("a protected collection's field inventory must never be listed");

    let message = format!("{err:#}");
    assert!(message.contains("protected"), "{message}");
    assert!(
        message.contains(gents_protocol::schemas::EVAL_VERDICT_NAME),
        "{message}"
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
        }
        .into(),
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

/// `AgentRequest.agent_name` (a retired field)
/// and `AgentRequest.updated_at` (only `created_at` exists) are common
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
        }
        .into(),
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
        }
        .into(),
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
        }
        .into(),
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
        }
        .into(),
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
    assert!(names.contains(&"lifecycle_state"), "{names:?}");
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
        }
        .into(),
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
    assert!(names.contains(&"endpoint"), "{names:?}");
    assert!(!names.contains(&"auth"), "secret leaked: {names:?}");
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
        }
        .into(),
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
        }
        .into(),
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
        }
        .into(),
    )
    .await
    .expect_err("mixed wildcard must fail");
    assert!(
        serde_json::from_str::<serde_json::Value>(&err.to_string()).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("[\"*\"]"),
        "{err}"
    );
}

#[tokio::test]
async fn count_is_filtered_total_and_find_is_a_stable_page() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type Ticket { name: String priority: Int status: String }")
        .await
        .unwrap();
    for n in 0..73 {
        crate::config_client::ConfigAccess::write_local(&node,"seed_query_count",&format!("mutation {{ create_Ticket(input:{{name:\"T{n:03}\",priority:{n},status:\"open\"}}){{_docID}} }}")).await.unwrap();
    }
    let tool = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["Ticket".into()]),
    );
    let count:QueryParams=serde_json::from_value(json!({"argv":["count"],"collection":"Ticket","options":{"filter":{"status":{"_eq":"open"}}}})).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&Tool::call(&tool, count).await.unwrap()).unwrap();
    assert_eq!(result["total_count"], 73);
    let find:QueryParams=serde_json::from_value(json!({"argv":["find"],"collection":"Ticket","options":{"fields":["name","priority"],"order":[{"priority":"ASC"}],"offset":5,"limit":5}})).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&Tool::call(&tool, find).await.unwrap()).unwrap();
    assert_eq!(result["returned_count"], 5);
    assert_eq!(result["results"][0]["name"], "T005");
    assert_eq!(result["results"][4]["name"], "T009");
    assert!(result.get("count").is_none());
    node.shutdown().await;
}

#[tokio::test]
async fn equal_collection_grants_read_only_the_bound_principals_acp_rows() {
    const ALICE: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    const BOB: &str = "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR";
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .with_node_identity_did(ALICE.to_owned())
            .build()
            .await
            .unwrap(),
    );
    let policy = node.add_dac_policy(ALICE, "name: Private query rows\nresources:\n  - name: records\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n").await.unwrap();
    node.add_schema(&format!(
        "type PrivateRecord @policy(id: \"{}\", resource: \"records\") {{ label: String }}",
        crate::graphql::escape_graphql_string(&policy)
    ))
    .await
    .unwrap();
    for (did, label) in [(ALICE, "Alice-only"), (BOB, "Bob-only")] {
        let mutation = format!(
            "mutation {{ add_PrivateRecord(input: {{label: \"{}\"}}) {{_docID}} }}",
            crate::graphql::escape_graphql_string(label)
        );
        crate::config_client::ConfigAccess::transact_local(
            &node,
            Some(::identity::Did::new(did).unwrap()),
            "seed_private_query",
            |txn| Box::pin(async { txn.execute(&mutation).await }),
        )
        .await
        .unwrap();
    }
    for (did, label, hidden) in [
        (ALICE, "Alice-only", "Bob-only"),
        (BOB, "Bob-only", "Alice-only"),
    ] {
        let tool = DefraQueryTool::new(
            node.clone(),
            CollectionScope::restricted(vec!["PrivateRecord".into()]),
        )
        .with_actor(::identity::Did::new(did).unwrap());
        let find: QueryParams = serde_json::from_value(
            json!({"argv":["find"],"collection":"PrivateRecord","options":{"fields":["label"]}}),
        )
        .unwrap();
        let result: Value = serde_json::from_str(&Tool::call(&tool, find).await.unwrap()).unwrap();
        assert_eq!(result["returned_count"], 1);
        assert_eq!(result["results"][0]["label"], label);
        let bounded = BoundedQueryTool::new(
            node.clone(),
            crate::document_config::QueryToolDecl {
                tool_name: "private_records".into(),
                collection: "PrivateRecord".into(),
                description: String::new(),
                fields: vec!["label".into()],
                filter_fields: vec![],
            },
        )
        .with_actor(::identity::Did::new(did).unwrap());
        let result: Value = serde_json::from_str(
            &Tool::call(
                &bounded,
                super::bounded::BoundedQueryParams(serde_json::Map::new()),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["results"].as_array().unwrap().len(), 1);
        assert_eq!(result["results"][0]["label"], label);

        let count: QueryParams = serde_json::from_value(json!({"argv":["count"],"collection":"PrivateRecord","options":{"filter":{"label":{"_eq":hidden}}}})).unwrap();
        let result: Value = serde_json::from_str(&Tool::call(&tool, count).await.unwrap()).unwrap();
        assert_eq!(result["total_count"], 0);
    }
    node.shutdown().await;
}
