use gents_store::node_scope::ContextCalls as _;
use std::sync::Arc;

use gents_loop::tool::Tool;
use serde_json::{json, Value};

use super::*;

#[tokio::test]
async fn keyword_search_ranks_native_rows_and_preserves_query_boundaries() {
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    node.add_collection("type Reports { duty: String @fulltext rank: String }")
        .await
        .unwrap();
    for (duty, rank) in [
        ("Coordinate crew duty rotations", "Captain"),
        ("Inspect engine maintenance", "Commander"),
        ("Plan the cargo manifest", "Captain"),
    ] {
        gents_store::config_client::ConfigAccess::write_local(
            &node,
            "seed_keyword_query",
            &format!(
                "mutation {{ add_Reports(input: {{duty: \"{}\", rank: \"{}\"}}) {{_docID}} }}",
                gents_store::graphql::escape_graphql_string(duty),
                gents_store::graphql::escape_graphql_string(rank)
            ),
        )
        .await
        .unwrap();
    }
    let tool = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["Reports".into()]),
    );
    let base = json!({"argv":["search"],"collection":"Reports","options":{"fields":["duty"],"search_fields":["duty"],"text":"crew duty","limit":3,"order":[{"rank":"ASC"}]}});
    let result: Value = serde_json::from_str(
        &Tool::call(
            &tool,
            serde_json::from_value(base.clone()).unwrap(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["returned_count"], 3);
    assert_eq!(
        result["results"][0]["duty"],
        "Coordinate crew duty rotations"
    );
    assert!(result["results"][0]["_score"].as_f64().unwrap() > 0.0);
    assert_eq!(result["results"][1]["_score"], 0.0);
    assert_eq!(result["ranking"], "bm25");
    let mut filtered = base.clone();
    filtered["options"]["filter"] = json!({"rank":{"_eq":"Commander"}});
    let result: Value = serde_json::from_str(
        &Tool::call(
            &tool,
            serde_json::from_value(filtered).unwrap(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["returned_count"], 1);
    assert_eq!(result["results"][0]["duty"], "Inspect engine maintenance");
    for (key, value, expected) in [
        ("text", json!(""), "nonempty search terms"),
        ("text", json!(42), "nonempty search terms"),
        ("search_fields", json!([]), "at least one field"),
        (
            "search_fields",
            json!(["duty) { rank }"]),
            "invalid field name",
        ),
        (
            "search_fields",
            json!(["missing"]),
            "available String field",
        ),
        ("search_fields", json!(["BM25"]), "available String field"),
        ("limit", json!(0), "search limit"),
        ("limit", json!(1001), "search limit"),
        (
            "filter",
            json!({"missing":{"_eq":"x"}}),
            "unknown filter field",
        ),
        ("order", json!({"duty":"SIDEWAYS"}), "ASC or DESC"),
    ] {
        let mut args = base.clone();
        args["options"][key] = value;
        let error = Tool::call(
            &tool,
            serde_json::from_value(args).unwrap(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains(expected), "{key}: {error}");
        assert!(error.contains("search"), "{error}");
    }
    let mut escaped = base.clone();
    escaped["options"]["text"] = json!("crew\" ) { rank } #");
    escaped["options"]["filter"] = json!({"duty":{"_neq":") { order: ["}});
    let result: Value = serde_json::from_str(
        &Tool::call(
            &tool,
            serde_json::from_value(escaped).unwrap(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["returned_count"], 3);
    for collection in ["OtherReports", "OAuthCredential"] {
        let mut args = base.clone();
        args["collection"] = json!(collection);
        let error = Tool::call(
            &tool,
            serde_json::from_value(args).unwrap(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("scope") || error.contains("protected"),
            "{error}"
        );
    }
    node.shutdown().await.expect("shut down test node");
}

async fn seeded_node() -> Arc<defra_node::Node> {
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();

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
                    node_did: "did:key:z-test",
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

#[tokio::test]
async fn oversized_field_is_truncated_json_stays_valid() {
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();

    let big_content = "x".repeat(MAX_FIELD_STRING_BYTES + 5_000);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "req-big-content",
                purpose: "normal",
                node_did: "did:key:z-test",
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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect("query must succeed");

    let parsed: serde_json::Value =
        serde_json::from_str(&raw_output).expect("output must be parseable JSON after truncation");

    assert_eq!(parsed["collection"], "AgentRequest");
    assert_eq!(parsed["truncated"], true, "truncated flag must be true");
    assert!(
        parsed["total_bytes"].as_u64().unwrap_or(0) > 0,
        "total_bytes must be reported"
    );

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
        &gents_loop::tool::ToolCallEnvelope::default(),
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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect_err("querying outside the allowed scope must fail");

    assert!(
        err.to_string()
            .contains("not within the allowed query scope"),
        "{err}"
    );
}

#[tokio::test]
async fn protected_collections_are_refused_by_reads_and_discovery() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());
    for case in gents_lean_vocab::families::datastore_protection_refusal::cases() {
        let collection = case.input.collection.clone();
        let refused = |outcome: Option<String>| {
            outcome.is_some_and(|message| {
                message.contains("protected") && message.contains(collection.as_str())
            })
        };
        let read = Tool::call(
            &tool,
            DefraQueryParams {
                collection: collection.clone(),
                filter: None,
                fields: vec!["score_bp".to_string()],
                limit: None,
            }
            .into(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .err()
        .map(|error| format!("{error:#}"));
        let discovery = Tool::call(
            &tool,
            DefraQueryParams {
                collection: collection.clone(),
                filter: None,
                fields: vec!["*".to_string()],
                limit: None,
            }
            .into(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .err()
        .map(|error| format!("{error:#}"));
        case.check(
            gents_lean_vocab::families::datastore_protection_refusal::RefusalOutput {
                read_refused: refused(read),
                discovery_refused: refused(discovery),
            },
        );
    }
}

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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect_err("invalid field must fail");

    let msg = err.to_string();
    assert!(msg.contains("AgentToolCall"), "{msg}");
    assert!(msg.contains("created_at"), "{msg}");
    assert!(msg.contains("started_at"), "suggestion missing: {msg}");
    assert!(msg.contains("completed_at"), "suggestion missing: {msg}");
    assert!(msg.contains("tool_call_key"), "inventory missing: {msg}");
}

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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect_err("invalid field must fail");
    let msg = err.to_string();
    assert!(msg.contains("agent_name"), "{msg}");
    assert!(msg.contains("node_did"), "suggestion missing: {msg}");
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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect_err("invalid field must fail");
    let msg = err.to_string();
    assert!(msg.contains("updated_at"), "{msg}");
    assert!(msg.contains("created_at"), "suggestion missing: {msg}");
}

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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect_err("invalid filter key must fail");
    let msg = err.to_string();
    assert!(msg.contains("created_at"), "{msg}");
    assert!(msg.contains("started_at"), "suggestion missing: {msg}");
}

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
        &gents_loop::tool::ToolCallEnvelope::default(),
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

#[tokio::test]
async fn discovery_lists_exactly_the_fields_the_model_leaves_visible() {
    let node = seeded_node().await;
    let tool = DefraQueryTool::new(node, CollectionScope::all());
    for case in gents_lean_vocab::families::discovery_field_visibility_listing::cases() {
        let output = Tool::call(
            &tool,
            DefraQueryParams {
                collection: case.input.collection.clone(),
                filter: None,
                fields: vec!["*".to_string()],
                limit: None,
            }
            .into(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .expect("discovery must succeed");
        let parsed: Value = serde_json::from_str(&output).unwrap();
        let listed = parsed["fields"]
            .as_array()
            .expect("fields array")
            .iter()
            .any(|f| f["name"].as_str() == Some(case.input.field.as_str()));
        case.check(
            gents_lean_vocab::families::discovery_field_visibility_listing::FieldOutput { listed },
        );
    }
}

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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect_err("discovery outside scope must fail");
    assert!(
        err.to_string()
            .contains("not within the allowed query scope"),
        "{err}"
    );
}

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
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    .expect_err("unknown collection must fail");
    let msg = err.to_string();
    assert!(msg.contains("NoSuchCollection"), "{msg}");
    assert!(msg.contains("does not exist"), "{msg}");
    for collection in ["NoSuchCollection", "surface-name"] {
        let error = Tool::call(
            &tool,
            QueryParams {
                argv: vec!["fields".into()],
                collection: Some(collection.into()),
                ..Default::default()
            },
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .unwrap_err();
        let result: Value = serde_json::from_str(&error.to_string()).unwrap();
        assert_eq!(
            result["recovery"],
            json!({"tool":"schema","args":{"argv":["collection","list"]}})
        );
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("config datastore get"));
    }
}

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
        &gents_loop::tool::ToolCallEnvelope::default(),
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
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    node.add_collection("type Ticket { name: String priority: Int status: String }")
        .await
        .unwrap();
    for n in 0..73 {
        gents_store::config_client::ConfigAccess::write_local(&node,"seed_query_count",&format!("mutation {{ create_Ticket(input:{{name:\"T{n:03}\",priority:{n},status:\"open\"}}){{_docID}} }}")).await.unwrap();
    }
    let tool = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["Ticket".into()]),
    );
    let count:QueryParams=serde_json::from_value(json!({"argv":["count"],"collection":"Ticket","options":{"filter":{"status":{"_eq":"open"}}}})).unwrap();
    let result: serde_json::Value = serde_json::from_str(
        &Tool::call(&tool, count, &gents_loop::tool::ToolCallEnvelope::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["total_count"], 73);
    let find:QueryParams=serde_json::from_value(json!({"argv":["find"],"collection":"Ticket","options":{"fields":["name","priority"],"order":[{"priority":"ASC"}],"offset":5,"limit":5}})).unwrap();
    let result: serde_json::Value = serde_json::from_str(
        &Tool::call(&tool, find, &gents_loop::tool::ToolCallEnvelope::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["returned_count"], 5);
    assert_eq!(result["results"][0]["name"], "T005");
    assert_eq!(result["results"][4]["name"], "T009");
    assert!(result.get("count").is_none());
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn equal_collection_grants_read_only_the_bound_principals_acp_rows() {
    let keys = tempfile::tempdir().unwrap();
    let alice =
        gents_store::identity::KeyIdentity::load_or_create(keys.path().join("alice.key"), None)
            .unwrap();
    let alice_node_did = gents_store::identity::NodeIdentity::did(&alice).to_owned();
    let alice_agent = gents_store::identity::resolve_agent_identity(
        &alice,
        &gents_store::identity::provision_agent_identity(&alice).unwrap(),
    )
    .unwrap();
    let bob_agent = gents_store::identity::resolve_agent_identity(
        &alice,
        &gents_store::identity::provision_agent_identity(&alice).unwrap(),
    )
    .unwrap();
    let alice_did = gents_store::identity::NodeIdentity::did(&*alice_agent).to_owned();
    let bob_did = gents_store::identity::NodeIdentity::did(&*bob_agent).to_owned();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(alice_node_did.clone())
            .build()
            .await
            .unwrap(),
    );
    let policy = node
        .add_dac_policy_as(
            &defra_core::RequestContext::default().with_identity(Some(alice_node_did.clone())),
            "name: Private query rows\nresources:\n  - name: records\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
        )
        .await
        .unwrap();
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&format!(
        "type PrivateRecord @policy(id: \"{}\", resource: \"records\") {{ label: String @fulltext }}",
        gents_store::graphql::escape_graphql_string(&policy)
    )))
    .await
    .unwrap();
    for (agent, label) in [(&alice_agent, "Alice-only"), (&bob_agent, "Bob-only")] {
        let mutation = format!(
            "mutation {{ add_PrivateRecord(input: {{label: \"{}\"}}) {{_docID}} }}",
            gents_store::graphql::escape_graphql_string(label)
        );
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            ::identity::Did::new(gents_store::identity::NodeIdentity::did(&**agent)).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        access
            .transact_write("seed_private_query", |txn| {
                let mutation = &mutation;
                Box::pin(async move { txn.execute(mutation).await })
            })
            .await
            .unwrap();
    }
    let mut ids = rapidhash::RapidHashMap::default();
    for (did, label) in [(&alice_did, "Alice-only"), (&bob_did, "Bob-only")] {
        let rows = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            ::identity::Did::new(did.as_str()).unwrap(),
        )
        .transact_readonly("test.private_field_ids", |txn| {
            Box::pin(async move { txn.execute("{PrivateRecord {_docID label}}").await })
        })
        .await
        .unwrap();
        ids.insert(
            label,
            rows["data"]["PrivateRecord"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    for (did, label, hidden) in [
        (alice_did, "Alice-only", "Bob-only"),
        (bob_did, "Bob-only", "Alice-only"),
    ] {
        let tool = DefraQueryTool::new(
            node.clone(),
            CollectionScope::restricted(vec!["PrivateRecord".into()]),
        )
        .with_actor(::identity::Did::new(did.clone()).unwrap());
        let find: QueryParams = serde_json::from_value(
            json!({"argv":["find"],"collection":"PrivateRecord","options":{"fields":["label"]}}),
        )
        .unwrap();
        let result: Value = serde_json::from_str(
            &Tool::call(&tool, find, &gents_loop::tool::ToolCallEnvelope::default())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["returned_count"], 1);
        assert_eq!(result["results"][0]["label"], label);
        for (target, permitted) in [(label, true), (hidden, false)] {
            let request: QueryParams=serde_json::from_value(json!({"argv":["find"],"collection":"PrivateRecord","options":{"fields":["label"],"filter":{"_or":[{"label":{"_eq":label}},{"label":{"_eq":hidden}}]},"field_page":{"doc_id":ids[target],"field":"label"}}})).unwrap();
            let read = Tool::call(
                &tool,
                request,
                &gents_loop::tool::ToolCallEnvelope::default(),
            )
            .await;
            assert_eq!(read.is_ok(), permitted);
            if permitted {
                let value: Value = serde_json::from_str(&read.unwrap()).unwrap();
                assert_eq!(value["field_page"]["text"], label);
            }
        }
        let search: QueryParams = serde_json::from_value(json!({"argv":["search"],"collection":"PrivateRecord","options":{"fields":["label"],"search_fields":["label"],"text":label,"limit":10}})).unwrap();
        let result: Value = serde_json::from_str(
            &Tool::call(
                &tool,
                search,
                &gents_loop::tool::ToolCallEnvelope::default(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["returned_count"], 1);
        assert_eq!(result["results"][0]["label"], label);
        assert!(result["results"][0]["_score"].as_f64().unwrap() > 0.0);
        assert_eq!(result["ranking"], "bm25");

        let bounded = BoundedQueryTool::new(
            gents_store::config_client::ConfigAccess::local(node.clone()),
            gents_documents::QueryToolDecl {
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
                &gents_loop::tool::ToolCallEnvelope::default(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["results"].as_array().unwrap().len(), 1);
        assert_eq!(result["results"][0]["label"], label);

        let count: QueryParams = serde_json::from_value(json!({"argv":["count"],"collection":"PrivateRecord","options":{"filter":{"label":{"_eq":hidden}}}})).unwrap();
        let result: Value = serde_json::from_str(
            &Tool::call(&tool, count, &gents_loop::tool::ToolCallEnvelope::default())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total_count"], 0);
    }
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn explain_preserves_native_plan_and_requires_explicit_execution() {
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    node.add_collection("type PlanTicket { reference: String status: String @index }")
        .await
        .unwrap();
    gents_store::config_client::ConfigAccess::write_local(
        &node,
        "seed_plan",
        r#"mutation { add_PlanTicket(input:{reference:"P1",status:"open"}){_docID} }"#,
    )
    .await
    .unwrap();
    let access = gents_store::config_client::ConfigAccess::local(node.clone());
    let scope = CollectionScope::restricted(vec!["PlanTicket".into()]);
    let args: QueryParams = serde_json::from_value(json!({"argv":["explain"],"collection":"PlanTicket","options":{"fields":["reference"],"filter":{"status":{"_eq":"closed"}},"limit":5}})).unwrap();
    let result = execute_command(&access, &args, &scope).await.unwrap();
    let params = DefraQueryParams {
        collection: "PlanTicket".into(),
        fields: vec!["reference".into()],
        filter: args.options.get("filter").cloned(),
        limit: Some(5),
    };
    let rendered = build_paged_query(&params, &scope, None, 0).unwrap();
    let native = access
        .execute(&format!("query @explain(type: simple) {rendered}"))
        .await
        .unwrap();
    assert_eq!(result["plan"], native["data"]["explain"]);
    assert_eq!(result["mode"], "simple");
    assert!(result["plan"].get("executionSuccess").is_none());
    assert!(result["plan"].to_string().contains("indexName"));
    node.add_collection("type PlainTicket { reference: String status: String }")
        .await
        .unwrap();
    let plain_scope = CollectionScope::restricted(vec!["PlainTicket".into()]);
    let mut plain = args.clone();
    plain.collection = Some("PlainTicket".into());
    let full_scan = execute_command(&access, &plain, &plain_scope)
        .await
        .unwrap();
    assert!(full_scan["plan"].to_string().contains("scanNode"));
    assert!(!full_scan["plan"].to_string().contains("indexName"));
    assert!(full_scan["findings"].to_string().contains("no indexName"));
    let mut bad_field = args.clone();
    bad_field
        .options
        .insert("fields".into(), json!(["shipment_reference"]));
    let error = execute_command(&access, &bad_field, &scope)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("reference"));
    let mut execute = args.clone();
    execute.options.insert("mode".into(), json!("execute"));
    let executed = execute_command(&access, &execute, &scope).await.unwrap();
    assert_eq!(executed["plan"]["executionSuccess"], true);
    assert!(executed["plan"].get("planExecutions").is_some());
    let mut invalid = args.clone();
    invalid.options.insert("mode".into(), json!("debug"));
    assert!(execute_command(&access, &invalid, &scope)
        .await
        .unwrap_err()
        .to_string()
        .contains("simple or execute"));
    invalid.collection = Some("OAuthCredential".into());
    assert!(execute_command(&access, &invalid, &CollectionScope::all())
        .await
        .is_err());
    invalid.collection = Some("EvalVerdict".into());
    assert!(execute_command(&access, &invalid, &CollectionScope::all())
        .await
        .is_err());
    invalid.collection = Some("OtherTicket".into());
    assert!(execute_command(&access, &invalid, &scope).await.is_err());
    let persisted = access
        .execute("{ PlanTicket { reference status } }")
        .await
        .unwrap();
    assert_eq!(persisted["data"]["PlanTicket"][0]["status"], "open");
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn a_write_verb_is_sent_to_the_write_tool_and_its_grant() {
    let node = seeded_node().await;
    for (holds_tools, expected) in [
        (true, "Add it with next_call"),
        (false, "you do not hold the tools category"),
    ] {
        let tool = DefraQueryTool::new(Arc::clone(&node), CollectionScope::all())
            .with_tools_category(holds_tools);
        for verb in [
            "write", "create", "insert", "update", "delete", "mutate", "add",
        ] {
            let params: QueryParams = serde_json::from_value(
                json!({"argv":[verb],"collection":"Board","options":{"input":{"title":"x"}}}),
            )
            .unwrap();
            let error = Tool::call(
                &tool,
                params,
                &gents_loop::tool::ToolCallEnvelope::default(),
            )
            .await
            .expect_err("query has no write verb")
            .to_string();
            let error: serde_json::Value = serde_json::from_str(&error).expect(&error);
            assert!(
                error["error"]
                    .as_str()
                    .unwrap()
                    .contains("Writes go through the write tool"),
                "{verb}: {error}"
            );
            let recovery = &error["recovery"];
            assert!(
                recovery["how"].as_str().unwrap().contains(expected)
                    && recovery["how"].as_str().unwrap().contains("Board"),
                "{verb}: {error}"
            );
            assert_eq!(
                recovery.get("next_call").is_some(),
                holds_tools,
                "{verb}: {error}"
            );
        }
    }
}

#[tokio::test]
async fn find_signer_names_who_wrote_a_document_not_who_it_claims() {
    let keys = tempfile::tempdir().unwrap();
    let node_key =
        gents_store::identity::KeyIdentity::load_or_create(keys.path().join("node.key"), None)
            .unwrap();
    let agent = || {
        gents_store::identity::resolve_agent_identity(
            &node_key,
            &gents_store::identity::provision_agent_identity(&node_key).unwrap(),
        )
        .unwrap()
    };
    let (alice, bob) = (agent(), agent());
    let did = |a: &dyn gents_store::identity::NodeIdentity| {
        gents_store::identity::NodeIdentity::did(a).to_owned()
    };
    let (alice_did, bob_did) = (did(&*alice), did(&*bob));
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(gents_store::identity::NodeIdentity::did(&node_key).to_owned())
            .build()
            .await
            .unwrap(),
    );
    node.add_collection("type Order { login: String }")
        .await
        .unwrap();
    let write = |who: &std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
                 mutation: String| {
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            ::identity::Did::new(gents_store::identity::NodeIdentity::did(&**who)).unwrap(),
            who.as_ref(),
        )
        .unwrap();
        async move {
            access
                .transact_write("seed_signed_order", |txn| {
                    let mutation = &mutation;
                    Box::pin(async move { txn.execute(mutation).await })
                })
                .await
                .unwrap()
        }
    };
    // Alice writes an order that claims to be bob's.
    let forged = write(
        &alice,
        r#"mutation { add_Order(input: {login: "bob"}) {_docID} }"#.into(),
    )
    .await;
    let forged_id = forged["data"]["add_Order"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    // Bob writes his own, then edits alice's.
    let own = write(
        &bob,
        r#"mutation { add_Order(input: {login: "bob"}) {_docID} }"#.into(),
    )
    .await;
    let own_id = own["data"]["add_Order"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let update = |id: &str, login: &str| {
        format!(
            r#"mutation {{ update_Order(docID: "{id}", input: {{login: "{login}"}}) {{_docID}} }}"#
        )
    };
    write(&bob, update(&forged_id, "bob!")).await;
    // Alice edits bob's order in between, then bob edits it again: created and
    // latest both name bob, so only writers shows alice's edit.
    write(&alice, update(&own_id, "alice")).await;
    write(&bob, update(&own_id, "bob")).await;

    let tool = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["Order".into()]),
    );
    let find: QueryParams = serde_json::from_value(
        json!({"argv":["find"],"collection":"Order","options":{"fields":["login"],"signer":true}}),
    )
    .unwrap();
    let result: Value = serde_json::from_str(
        &Tool::call(&tool, find, &gents_loop::tool::ToolCallEnvelope::default())
            .await
            .unwrap(),
    )
    .unwrap();
    let mut signers = result["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            assert!(
                row.get("_docID").is_none(),
                "unselected _docID stays hidden"
            );
            assert_eq!(row["_signer"]["unsigned"], false);
            let writers: std::collections::BTreeSet<String> =
                serde_json::from_value(row["_signer"]["writers"].clone()).unwrap();
            (
                row["login"].as_str().unwrap().to_owned(),
                row["_signer"]["created"].as_str().unwrap().to_owned(),
                row["_signer"]["latest"].as_str().unwrap().to_owned(),
                writers,
            )
        })
        .collect::<Vec<_>>();
    signers.sort();
    let both = std::collections::BTreeSet::from([alice_did.clone(), bob_did.clone()]);
    assert_eq!(
        signers,
        vec![
            // Bob's own order, edited by alice in between.
            ("bob".into(), bob_did.clone(), bob_did.clone(), both.clone()),
            // Alice's forged order, edited by bob.
            ("bob!".into(), alice_did.clone(), bob_did.clone(), both),
        ]
    );

    let bad: QueryParams = serde_json::from_value(
        json!({"argv":["find"],"collection":"Order","options":{"fields":["login"],"signer":"yes"}}),
    )
    .unwrap();
    let error = Tool::call(&tool, bad, &gents_loop::tool::ToolCallEnvelope::default())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("signer must be true or false"));

    let paged: QueryParams = serde_json::from_value(json!({"argv":["find"],"collection":"Order","options":{"fields":["login"],"signer":true,"field_page":{"doc_id":own_id,"field":"login","offset_bytes":0}}}))
    .unwrap();
    let error = Tool::call(&tool, paged, &gents_loop::tool::ToolCallEnvelope::default())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("field_page cannot combine with signer"),
        "a field page never silently drops the requested _signer: {error:#}"
    );
}

async fn run_query(tool: &DefraQueryTool, args: Value) -> std::result::Result<Value, Value> {
    match Tool::call(
        tool,
        serde_json::from_value(args).unwrap(),
        &gents_loop::tool::ToolCallEnvelope::default(),
    )
    .await
    {
        Ok(text) => Ok(serde_json::from_str(&text).unwrap()),
        Err(error) => Err(serde_json::from_str(&error.to_string()).unwrap()),
    }
}

#[tokio::test]
async fn relationship_selections_follow_grants_and_acp() {
    let keys = tempfile::tempdir().unwrap();
    let owner =
        gents_store::identity::KeyIdentity::load_or_create(keys.path().join("owner.key"), None)
            .unwrap();
    let node_did = gents_store::identity::NodeIdentity::did(&owner).to_owned();
    let agents: Vec<_> = (0..2)
        .map(|_| {
            gents_store::identity::resolve_agent_identity(
                &owner,
                &gents_store::identity::provision_agent_identity(&owner).unwrap(),
            )
            .unwrap()
        })
        .collect();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(node_did.clone())
            .build()
            .await
            .unwrap(),
    );
    let policy = node
        .add_dac_policy_as(
            &defra_core::RequestContext::default().with_identity(Some(node_did.clone())),
            "name: Private related rows\nresources:\n  - name: records\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
        )
        .await
        .unwrap();
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&format!(
        "type LinkedRecord @policy(id: \"{}\", resource: \"records\") {{ label: String notes: [RecordNote] }} type RecordNote {{ text: String record: LinkedRecord }}",
        gents_store::graphql::escape_graphql_string(&policy)
    )))
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (agent, label) in agents.iter().zip(["Mine", "Theirs"]) {
        let did = ::identity::Did::new(gents_store::identity::NodeIdentity::did(&**agent)).unwrap();
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            did.clone(),
            agent.as_ref(),
        )
        .unwrap();
        let mutation =
            format!("mutation {{ add_LinkedRecord(input: {{label: \"{label}\"}}) {{_docID}} }}");
        let created = access
            .transact_write("seed_linked_record", |txn| {
                let mutation = &mutation;
                Box::pin(async move { txn.execute(mutation).await })
            })
            .await
            .unwrap();
        ids.push(
            created["data"]["add_LinkedRecord"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let mut notes = vec![
        ("about-mine".to_owned(), &ids[0]),
        ("about-theirs".to_owned(), &ids[1]),
    ];
    notes.extend((0..21).map(|n| (format!("more-{n:02}"), &ids[0])));
    for (text, record) in &notes {
        gents_store::config_client::ConfigAccess::write_local(
            &node,
            "seed_record_note",
            &format!("mutation {{ add_RecordNote(input: {{text: \"{text}\", record: \"{record}\"}}) {{_docID}} }}"),
        )
        .await
        .unwrap();
    }
    let reader =
        ::identity::Did::new(gents_store::identity::NodeIdentity::did(&*agents[0])).unwrap();
    let granted = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["RecordNote".into(), "LinkedRecord".into()]),
    )
    .with_actor(reader.clone());
    let about = json!({"text":{"_in":["about-mine","about-theirs"]}});
    let linked = run_query(&granted, json!({"argv":["find"],"collection":"RecordNote","options":{"fields":["text","record.label","record._docID"],"filter":about,"order":[{"text":"ASC"}]}})).await.unwrap();
    assert_eq!(
        linked["results"],
        json!([
            {"text":"about-mine","record":{"label":"Mine","_docID":ids[0]}},
            {"text":"about-theirs","record":null},
        ])
    );
    let many = run_query(&granted, json!({"argv":["find"],"collection":"LinkedRecord","options":{"fields":["label","notes.text"]}})).await.unwrap();
    assert_eq!(many["returned_count"], 1);
    assert_eq!(many["results"][0]["label"], "Mine");
    assert_eq!(many["results"][0]["notes"].as_array().unwrap().len(), 20);
    assert_eq!(
        many["truncated_relations"],
        json!([{"row":0,"relation":"notes","shown":20}])
    );
    let across = run_query(&granted, json!({"argv":["find"],"collection":"RecordNote","options":{"fields":["text"],"filter":{"record":{"label":{"_eq":"Mine"}}}}})).await.unwrap();
    assert_eq!(across["returned_count"], 22);
    let unselected = run_query(&granted, json!({"argv":["find"],"collection":"RecordNote","options":{"fields":["text"],"filter":{"record":{"label":{"_eq":"Theirs"}}}}})).await.unwrap();
    assert_eq!(unselected["results"], json!([{"text":"about-theirs"}]));
    for (fields, expected, next) in [
        (
            json!(["record"]),
            "select its fields as record.FIELD",
            json!({"argv":["fields"],"collection":"LinkedRecord"}),
        ),
        (
            json!(["text.size"]),
            "is a value, not a relationship",
            json!({"argv":["fields"],"collection":"RecordNote"}),
        ),
        (
            json!(["record.notes.text"]),
            "crosses more than one relationship",
            json!({"argv":["fields"],"collection":"RecordNote"}),
        ),
        (
            json!(["record.lable"]),
            "did you mean \"label\"",
            json!({"argv":["fields"],"collection":"LinkedRecord"}),
        ),
    ] {
        let refused = run_query(
            &granted,
            json!({"argv":["find"],"collection":"RecordNote","options":{"fields":fields}}),
        )
        .await
        .unwrap_err();
        assert!(
            refused["error"].as_str().unwrap().contains(expected),
            "{refused}"
        );
        assert_eq!(refused["recovery"]["args"], next, "{refused}");
    }
    let narrow = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["RecordNote".into()]),
    )
    .with_actor(reader);
    for options in [
        json!({"fields":["text","record.label"]}),
        json!({"fields":["text"],"filter":{"record":{"label":{"_eq":"Mine"}}}}),
    ] {
        let refused = run_query(
            &narrow,
            json!({"argv":["find"],"collection":"RecordNote","options":options}),
        )
        .await
        .unwrap_err();
        assert!(
            refused["error"]
                .as_str()
                .unwrap()
                .contains("not within the allowed query scope"),
            "{refused}"
        );
    }
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn vector_search_ranks_by_the_fields_index_metric() {
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    node.add_collection("type CatalogItem { sku: String coordinates: [Float32!] @index(vector: {dimensions: 3, flat: {metric: EUCLIDEAN}}) }")
        .await
        .unwrap();
    for (sku, coordinates) in [("a", "[0, 0, 0]"), ("b", "[1, 1, 1]"), ("c", "[5, 5, 5]")] {
        gents_store::config_client::ConfigAccess::write_local(
            &node,
            "seed_vector_search",
            &format!("mutation {{ add_CatalogItem(input: {{sku: \"{sku}\", coordinates: {coordinates}}}) {{_docID}} }}"),
        )
        .await
        .unwrap();
    }
    let tool = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["CatalogItem".into()]),
    );
    let base = json!({"argv":["search"],"collection":"CatalogItem","options":{"fields":["sku"],"vector_field":"coordinates","vector":[4, 4, 4],"limit":2}});
    let nearest = run_query(&tool, base.clone()).await.unwrap();
    assert_eq!(nearest["ranking"], "similarity");
    assert_eq!(nearest["results"][0]["sku"], "c");
    assert_eq!(nearest["results"][1]["sku"], "b");
    assert!(
        nearest["results"][0]["_score"].as_f64().unwrap()
            > nearest["results"][1]["_score"].as_f64().unwrap()
    );
    assert!(nearest["results"][0].get("_docID").is_none());
    let mut filtered = base.clone();
    filtered["options"]["filter"] = json!({"sku":{"_neq":"c"}});
    let filtered = run_query(&tool, filtered).await.unwrap();
    assert_eq!(filtered["results"][0]["sku"], "b");
    assert_eq!(filtered["results"][1]["sku"], "a");
    for (key, value, expected) in [
        ("text", json!("crate"), "give exactly one"),
        ("vector_field", json!("sku"), "numeric list field"),
        ("vector", json!([]), "nonempty array of numbers"),
        ("vector", json!([4, 4]), "same length"),
    ] {
        let mut args = base.clone();
        args["options"][key] = value;
        let refused = run_query(&tool, args).await.unwrap_err();
        assert!(
            refused["error"].as_str().unwrap().contains(expected),
            "{key}: {refused}"
        );
    }
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn definition_documents_verb_only_argv_and_option_properties() {
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    let tool = DefraQueryTool::new(node.clone(), CollectionScope::all());
    let def = Tool::definition(&tool, String::new()).await;

    assert_eq!(def.name, "query");
    assert!(
        def.description.contains("never in argv"),
        "{}",
        def.description
    );
    let props = &def.parameters["properties"];
    let argv_desc = props["argv"]["description"].as_str().unwrap();
    assert!(argv_desc.contains("collection"), "{argv_desc}");
    assert!(argv_desc.contains("Never put the collection name in argv"));

    let option_props = props["options"]["properties"].as_object().unwrap();
    for key in [
        "fields",
        "filter",
        "limit",
        "offset",
        "order",
        "text",
        "search_fields",
        "vector_field",
        "vector",
        "mode",
        "field_page",
        "signer",
    ] {
        let entry = option_props
            .get(key)
            .unwrap_or_else(|| panic!("missing options.{key} in definition"));
        assert!(
            entry["type"].is_string(),
            "options.{key} must declare a JSON Schema type: {entry}"
        );
        assert!(
            entry["description"].as_str().is_some_and(|s| !s.is_empty()),
            "options.{key} must have a description: {entry}"
        );
    }
    assert!(
        option_props["fields"]["description"]
            .as_str()
            .unwrap()
            .contains("Required for find"),
        "{}",
        option_props["fields"]
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn positional_collection_help_and_option_mistakes_return_actionable_recovery() {
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    node.add_collection("type Reports { duty: String @fulltext rank: String }")
        .await
        .unwrap();
    gents_store::config_client::ConfigAccess::write_local(
        &node,
        "seed_ux_query",
        r#"mutation { add_Reports(input: {duty: "Coordinate crew duty", rank: "Captain"}) {_docID} }"#,
    )
    .await
    .unwrap();
    let tool = DefraQueryTool::new(
        node.clone(),
        CollectionScope::restricted(vec!["Reports".into()]),
    );

    let err = run_query(&tool, json!({"argv": ["fields", "Reports"]}))
        .await
        .unwrap_err();
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("[\"fields\"]") && msg.contains("collection"),
        "{err}"
    );
    assert_eq!(
        err["recovery"],
        json!({"tool": "query", "args": {"argv": ["fields"], "collection": "Reports"}})
    );
    let recovered = run_query(&tool, err["recovery"]["args"].clone())
        .await
        .unwrap();
    assert_eq!(recovered["collection"], "Reports");

    let err = run_query(
        &tool,
        json!({"argv": ["find", "Reports"], "options": {"fields": ["duty"]}}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err["recovery"],
        json!({"tool": "query", "args": {"argv": ["find"], "collection": "Reports", "options": {"fields": ["duty"]}}})
    );
    let recovered = run_query(&tool, err["recovery"]["args"].clone())
        .await
        .unwrap();
    assert_eq!(recovered["returned_count"], 1);

    let err = run_query(&tool, json!({"argv": ["find", "Reports"]}))
        .await
        .unwrap_err();
    assert!(
        err["error"].as_str().unwrap().contains("options.fields"),
        "{err}"
    );
    assert_eq!(
        err["recovery"],
        json!({"tool": "query", "args": {"argv": ["find"], "collection": "Reports"}})
    );

    let err = run_query(&tool, json!({"argv": ["find"], "collection": "Reports"}))
        .await
        .unwrap_err();
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("options.fields is required"),
        "{err}"
    );
    assert_eq!(
        err["recovery"],
        json!({"tool": "query", "args": {"argv": ["fields"], "collection": "Reports"}})
    );
    assert_eq!(
        run_query(&tool, err["recovery"]["args"].clone())
            .await
            .unwrap()["collection"],
        "Reports"
    );

    for help_args in [
        json!({"argv": ["help"], "collection": "Reports"}),
        json!({"argv": ["help", "find"], "collection": "Reports"}),
        json!({"argv": ["find", "--help"], "collection": "Reports", "options": {"fields": ["duty"]}}),
    ] {
        let ok = run_query(&tool, help_args.clone()).await.unwrap();
        assert!(
            ok["help"].as_str().is_some_and(|s| !s.is_empty()),
            "{help_args}: {ok}"
        );
    }
    let err = run_query(
        &tool,
        json!({"argv": ["help"], "options": {"fields": ["duty"]}}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err["recovery"],
        json!({"tool": "query", "args": {"argv": ["help"]}})
    );

    let err = run_query(
        &tool,
        json!({"argv": ["search"], "collection": "Reports", "options": {"fields": ["duty"], "search_fields": ["duty"], "query": "crew duty"}}),
    )
    .await
    .unwrap_err();
    assert!(
        err["error"].as_str().unwrap().contains("options.text")
            && err["error"]
                .as_str()
                .unwrap()
                .contains("options.search_fields"),
        "{err}"
    );
    assert_eq!(err["recovery"]["args"]["options"]["text"], "crew duty");
    assert!(err["recovery"]["args"]["options"].get("query").is_none());
    let recovered = run_query(&tool, err["recovery"]["args"].clone())
        .await
        .unwrap();
    assert_eq!(recovered["ranking"], "bm25");

    let err = run_query(
        &tool,
        json!({"argv": ["search"], "collection": "Reports", "options": {"fields": ["duty"], "keywords": "crew"}}),
    )
    .await
    .unwrap_err();
    assert!(
        err["error"].as_str().unwrap().contains("options.text"),
        "{err}"
    );
    assert_eq!(
        err["recovery"],
        json!({"tool": "query", "args": {"argv": ["help", "search"]}})
    );

    node.shutdown().await.expect("shut down test node");
}
