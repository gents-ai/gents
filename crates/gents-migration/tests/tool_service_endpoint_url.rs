use gents_protocol::schemas;

mod common;

/// A store created before `endpoint_url` keeps its registry lineage, field ids and
/// documents when the field is appended, and a second pass applies nothing.
#[tokio::test]
async fn endpoint_url_extends_a_registry_created_before_it() {
    let previous = schemas::TOOL_SERVICE_REGISTRY
        .lines()
        .filter(|line| !line.trim_start().starts_with("endpoint_url:"))
        .collect::<Vec<_>>()
        .join("\n");
    let node = common::fresh_node().await;
    node.add_collection(&previous).await.unwrap();
    let before = node.get_collection("ToolServiceRegistry").unwrap().unwrap();
    let created = node
        .execute(
            r#"mutation { add_ToolServiceRegistry(input: {
        service_id: "retained", node_did: "did:key:owner", hostname: "host", mcp_port: 9000
    }) { _docID } }"#,
        )
        .await;
    assert!(!created.has_errors(), "{created:?}");
    let document = created.data.unwrap()["add_ToolServiceRegistry"][0]["_docID"].clone();
    gents_migration::ensure_migrations(&node).await.unwrap();
    let after = node.get_collection("ToolServiceRegistry").unwrap().unwrap();
    assert_eq!(before.collection_id, after.collection_id);
    for field in before.fields {
        let retained = after
            .fields
            .iter()
            .find(|retained| retained.name == field.name)
            .unwrap();
        assert_eq!(retained.id, field.id, "{} moved", field.name);
        assert_eq!(retained.kind, field.kind, "{} changed kind", field.name);
    }
    let repeat = gents_migration::ensure_migrations(&node).await.unwrap();
    assert_eq!(repeat.steps_applied, 0);
    let retained = node
        .execute("{ ToolServiceRegistry { _docID service_id mcp_port endpoint_url } }")
        .await;
    assert!(!retained.has_errors(), "{retained:?}");
    let retained = retained.data.unwrap();
    assert_eq!(retained["ToolServiceRegistry"][0]["_docID"], document);
    assert_eq!(retained["ToolServiceRegistry"][0]["mcp_port"], 9000);
    assert!(retained["ToolServiceRegistry"][0]["endpoint_url"].is_null());
    node.shutdown().await;
}

#[tokio::test]
async fn a_fresh_store_registers_the_current_registry_baseline() {
    let expected = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == "ToolServiceRegistry")
        .unwrap();
    let fresh = common::fresh_node().await;
    fresh.add_collection(expected.sdl).await.unwrap();
    let version = fresh
        .get_collection("ToolServiceRegistry")
        .unwrap()
        .unwrap();
    assert_eq!(Some(version.version_id.as_str()), expected.expected_version);
    fresh.shutdown().await;
}
