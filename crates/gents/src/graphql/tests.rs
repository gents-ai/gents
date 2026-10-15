use super::*;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
enum StringInput {
    Borrowed,
    Owned,
}

impl<'de> Deserialize<'de> for StringInput {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StringInputVisitor;

        impl<'de> serde::de::Visitor<'de> for StringInputVisitor {
            type Value = StringInput;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_borrowed_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StringInput::Borrowed)
            }

            fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StringInput::Owned)
            }

            fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StringInput::Owned)
            }
        }

        deserializer.deserialize_str(StringInputVisitor)
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct TypedRow {
    name: String,
    count: u32,
}

#[test]
fn rows_deserializes_borrowed_data_and_preserves_shape_errors() {
    let response = QueryResponse::success(serde_json::json!({
        "Large": ["x".repeat(1024 * 1024)],
        "Typed": [{"name": "row", "count": 3}],
        "Null": null,
        "Object": {"name": "row", "count": 3}
    }));

    assert_eq!(
        rows::<StringInput>(&response, "Large").unwrap(),
        [StringInput::Borrowed]
    );
    assert_eq!(
        rows::<TypedRow>(&response, "Typed").unwrap(),
        [TypedRow {
            name: "row".into(),
            count: 3,
        }]
    );
    assert!(rows::<TypedRow>(&response, "Missing").unwrap().is_empty());
    assert!(rows::<TypedRow>(&response, "Null").is_err());
    assert!(rows::<TypedRow>(&response, "Object").is_err());
}

#[test]
fn native_data_envelope_preserves_payload_allocations_and_null_data() {
    let payload = "x".repeat(1024 * 1024);
    let pointer = payload.as_ptr();
    let data = Value::Array(vec![Value::String(payload)]);
    let envelope = into_data_envelope(Some(data));
    assert_eq!(envelope["data"][0].as_str().unwrap().as_ptr(), pointer);
    assert_eq!(into_data_envelope(None), serde_json::json!({"data": null}));
}

#[test]
#[ignore = "manual native GraphQL envelope benchmark"]
fn native_data_envelope_payload_size_benchmark() {
    let data = serde_json::json!({"AgentOutputSegment": (0..128).map(|index| {
        serde_json::json!({"_docID": format!("segment-{index}"), "payload": "x".repeat(64 * 1024)})
    }).collect::<Vec<_>>()});
    let mut copied = Vec::new();
    let mut moved = Vec::new();
    for iteration in 0..12 {
        let measure = |copy| {
            let native = Some(data.clone());
            let start = std::time::Instant::now();
            let envelope = if copy {
                serde_json::json!({"data": native.unwrap_or(Value::Null)})
            } else {
                into_data_envelope(native)
            };
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            std::hint::black_box(envelope);
            ms
        };
        let (copy_ms, move_ms) = if iteration % 2 == 0 {
            (measure(true), measure(false))
        } else {
            let move_ms = measure(false);
            (measure(true), move_ms)
        };
        if iteration > 0 {
            copied.push(copy_ms);
            moved.push(move_ms);
        }
    }
    copied.sort_by(f64::total_cmp);
    moved.sort_by(f64::total_cmp);
    let result = serde_json::json!({
        "payload_bytes": 128 * 64 * 1024, "baseline_median_ms": copied[5], "updated_median_ms": moved[5],
        "includes_database_reads": false, "includes_fixture_clone": false,
    });
    if let Some(path) = std::env::var_os("GENTS_ENVELOPE_BENCHMARK_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    tracing::info!(%result, "native GraphQL envelope benchmark");
}

#[test]
fn validate_graphql_name_accepts_conforming_names() {
    for name in [
        "AgentResponse",
        "_docID",
        "_eq",
        "snake_case_2",
        "a",
        "_",
        "WebhookEvent",
    ] {
        assert!(
            validate_graphql_name(name).is_ok(),
            "{name:?} conforms to the GraphQL Name grammar and must pass"
        );
    }
}

#[test]
fn validate_graphql_name_rejects_nonconforming_names() {
    for name in [
        "",
        "1abc",
        "Msg (limit: 1) { _docID }",
        "a b",
        "a-b",
        "a.b",
        "Msg{",
        "a\"b",
        "a\nb",
        "naïve",
        "名前",
        "a\u{200b}b",
        " AgentResponse",
        "AgentResponse ",
    ] {
        assert!(
            validate_graphql_name(name).is_err(),
            "{name:?} violates the GraphQL Name grammar and must be rejected"
        );
    }
}

#[test]
fn validate_collection_identifier_rejects_introspection_reserved_names() {
    for name in ["__Type", "__schema", "__typename", "__"] {
        assert!(
            validate_collection_identifier(name).is_err(),
            "{name:?} is GraphQL-reserved (__ prefix) and must be rejected as a collection"
        );
    }
    assert!(validate_collection_identifier("AgentResponse").is_ok());
    assert!(
        validate_collection_identifier("_Private").is_ok(),
        "a single leading underscore is a legal Name and not introspection-reserved"
    );
    assert!(validate_collection_identifier("Msg) { x } (").is_err());
}

#[test]
fn test_escape_graphql_string() {
    assert_eq!(escape_graphql_string("hello"), "hello");
    assert_eq!(escape_graphql_string("he\"llo"), "he\\\"llo");
    assert_eq!(escape_graphql_string("line1\nline2"), "line1\\nline2");
    assert_eq!(escape_graphql_string("cr\r"), "cr\\r");
    assert_eq!(escape_graphql_string("tab\there"), "tab\\there");
    assert_eq!(escape_graphql_string("back\\slash"), "back\\\\slash");
    assert_eq!(escape_graphql_string("mixed\"\n\\"), "mixed\\\"\\n\\\\");
}

#[test]
fn single_mutation_document_normalizes_create_to_add_response_key() {
    let response = QueryResponse::success(serde_json::json!({
        "add_AgentRequest": [{ "_docID": "doc-1" }]
    }));

    let document = single_mutation_document(&response, "create_AgentRequest")
        .unwrap()
        .expect("normalized mutation document");
    assert_eq!(document["_docID"], "doc-1");
}

#[test]
fn created_doc_id_uses_the_shared_single_document_contract() {
    for field in ["create_AgentOutputSegment", "add_AgentOutputSegment"] {
        for value in [
            serde_json::json!({"_docID": "physical"}),
            serde_json::json!([{"_docID": "physical"}]),
        ] {
            let response = serde_json::json!({"data": {field: value}});
            assert_eq!(
                created_doc_id(&response, "AgentOutputSegment").unwrap(),
                "physical"
            );
        }
    }
    for response in [
        serde_json::json!({"data": {}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": []}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": [{"_docID": "one"}, {"_docID": "two"}]}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": {"_docID": " "}}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": {"_docID": "one"}, "create_AgentOutputSegment": {"_docID": "one"}}}),
        serde_json::json!({"errors": ["failed"], "data": {"add_AgentOutputSegment": {"_docID": "one"}}}),
    ] {
        assert!(
            created_doc_id(&response, "AgentOutputSegment").is_err(),
            "{response}"
        );
    }
}

#[test]
fn mutation_write_ledger_counts_returned_documents_not_just_calls() {
    let response = QueryResponse::success(serde_json::json!({
        "upsert_One": { "_docID": "doc-1" },
        "delete_Many": [
            { "_docID": "doc-2" },
            { "_docID": "doc-3" }
        ],
        "delete_None": []
    }));
    assert_eq!(mutation_affected_documents(&response), 3);

    let no_match = QueryResponse::success(serde_json::json!({ "delete_None": [] }));
    assert_eq!(mutation_affected_documents(&no_match), 0);
}

#[test]
fn single_mutation_document_rejects_duplicate_normalized_keys() {
    let response = QueryResponse::success(serde_json::json!({
        "create_AgentRequest": { "_docID": "doc-create" },
        "add_AgentRequest": [{ "_docID": "doc-add" }]
    }));

    let error = single_mutation_document(&response, "create_AgentRequest").unwrap_err();
    assert!(error.to_string().contains("returned both"));
}

#[test]
fn mutation_composite_version_rejects_concurrent_newest_heads() {
    let response = QueryResponse::success(serde_json::json!({
        "update_AgentRequest": [{
            "_version": [
                { "cid": "bafy-head-a", "height": 7, "fieldName": "_C" },
                { "cid": "bafy-head-b", "height": 7, "fieldName": "_C" },
                { "cid": "bafy-parent", "height": 6, "fieldName": "_C" }
            ]
        }]
    }));

    let error = mutation_composite_version(&response, "update_AgentRequest").unwrap_err();
    assert!(error.to_string().contains("document version is ambiguous"));
}

#[test]
fn document_composite_version_sorts_and_never_falls_back_to_field_commits() {
    let document = serde_json::json!({
        "_version": [
            { "cid": "bafy-old", "height": 2, "fieldName": "_C" },
            { "cid": "bafy-field", "height": 9, "fieldName": "content" },
            { "cid": "bafy-new", "height": 4, "fieldName": "_C" }
        ]
    });
    let commit = document_composite_version(&document, "AgentMessage boundary")
        .unwrap()
        .expect("composite commit");
    assert_eq!(commit.cid, "bafy-new");

    let field_only = serde_json::json!({
        "_version": [{ "cid": "bafy-field", "height": 9, "fieldName": "content" }]
    });
    assert!(
        document_composite_version(&field_only, "AgentMessage boundary")
            .unwrap()
            .is_none()
    );
}

#[test]
fn mutation_composite_version_accepts_unique_newest_composite() {
    let response = QueryResponse::success(serde_json::json!({
        "add_AgentRequest": [{
            "_version": [
                { "cid": "bafy-field", "height": 8, "fieldName": "status" },
                { "cid": "bafy-new", "height": 8, "fieldName": "_C" },
                { "cid": "bafy-old", "height": 7, "fieldName": "_C" }
            ]
        }]
    }));

    let commit = mutation_composite_version(&response, "create_AgentRequest")
        .unwrap()
        .expect("composite commit");
    assert_eq!(commit.cid, "bafy-new");
}

#[tokio::test]
async fn concurrent_mutations_share_the_node_write_path() {
    const WRITES: usize = 32;

    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema("type GateWrite { write_id: String }")
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(WRITES));
    let mut tasks = tokio::task::JoinSet::new();

    for index in 0..WRITES {
        let node = Arc::clone(&node);
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            barrier.wait().await;
            let mutation = format!(
                r#"mutation {{ create_GateWrite(input: {{ write_id: "write-{index}" }}) {{ _docID }} }}"#
            );
            crate::config_client::ConfigAccess::write_local(
                &node,
                "test.concurrent_write",
                &mutation,
            )
            .await
        });
    }

    while let Some(result) = tasks.join_next().await {
        result
            .expect("concurrent mutation task panicked")
            .expect("concurrent mutation exhausted the shared write path");
    }

    let response = graphql_with_transaction_retry(&node, "{ GateWrite { write_id } }", "count")
        .await
        .unwrap();
    assert_eq!(rows::<Value>(&response, "GateWrite").unwrap().len(), WRITES);
    node.shutdown().await;
}

#[tokio::test]
async fn public_graphql_read_helpers_reject_mutations_before_execution() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    node.add_schema("type ReadBoundaryFact { value: String }")
        .await
        .unwrap();
    let mutation = r#"mutation { create_ReadBoundaryFact(input: {value: "bypass"}) { _docID } }"#;

    assert!(
        graphql_with_transaction_retry(&node, mutation, "read boundary")
            .await
            .is_err()
    );
    assert!(
        graphql_response_with_transaction_retry(&node, mutation, "raw read boundary")
            .await
            .is_err()
    );

    let response = node.execute("{ ReadBoundaryFact { value } }").await;
    assert_eq!(
        response.data.unwrap()["ReadBoundaryFact"],
        serde_json::json!([])
    );
    node.shutdown().await;
}
