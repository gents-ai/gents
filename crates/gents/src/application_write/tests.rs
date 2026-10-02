use super::*;
use std::sync::Arc;

#[test]
fn admission_matches_executable_lean_owner() {
    let cases = crate::lean_vocab_test::lean_application_write_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let observation: Observation = serde_json::from_value(case.clone()).unwrap();
        assert_eq!(
            observation.admitted(),
            case["admitted"].as_bool().unwrap(),
            "{case}"
        );
        assert_eq!(
            observation.may_apply(),
            case["may_apply"].as_bool().unwrap(),
            "{case}"
        );
    }
}

fn call(operation: &str, input: Value, filter: Option<Value>) -> WriteParams {
    let mut options = Map::new();
    if operation != "delete" {
        options.insert("input".into(), input);
    }
    if let Some(filter) = filter {
        options.insert("filter".into(), filter);
        options.insert("max_targets".into(), json!(2));
    }
    WriteParams {
        argv: vec!["preview".into(), operation.into()],
        collection: Some("Parcel".into()),
        options,
    }
}
async fn fixture() -> (Arc<crate::defra_node::EmbeddedNode>, WriteTool) {
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .unwrap(),
    );
    node.add_schema(
        "type Parcel { reference: String status: String tags: [String] payload: JSON }",
    )
    .await
    .unwrap();
    let tool = WriteTool::new(
        ConfigAccess::Local(node.clone()),
        BTreeSet::from(["Parcel".into()]),
        None,
    );
    (node, tool)
}
async fn apply(tool: &WriteTool, preview: WriteParams) -> Result<Value> {
    let receipt = tool.execute(&preview).await?;
    let next = serde_json::from_value(receipt["next_call"]["args"].clone())?;
    tool.execute(&next).await
}

#[tokio::test]
async fn preview_create_update_delete_and_fresh_reads_preserve_json() {
    let (node, tool) = fixture().await;
    let create = call(
        "create",
        json!({"reference":"a\" } bad { x","status":"queued","tags":[],"payload":{"empty":[],"a-b":[null]}}),
        None,
    );
    let preview = tool.execute(&create).await.unwrap();
    assert_eq!(preview["effect"]["target_count"], 1);
    let access = ConfigAccess::Local(node.clone());
    let before = access.execute("{Parcel{_docID}}").await.unwrap();
    assert_eq!(before["data"]["Parcel"], json!([]));
    let next: WriteParams = serde_json::from_value(preview["next_call"]["args"].clone()).unwrap();
    tool.execute(&next).await.unwrap();
    assert!(tool
        .execute(&next)
        .await
        .unwrap_err()
        .to_string()
        .contains("stale"));
    let rows = access
        .execute("{Parcel{reference status tags payload}}")
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["Parcel"][0]["payload"],
        json!({"empty":[],"a-b":[null]})
    );
    assert!(rows["data"]["Parcel"][0]["tags"].is_null());
    let filter = json!({"reference":{"_eq":"a\" } bad { x"}});
    apply(
        &tool,
        call(
            "update",
            json!({"status":"delivered"}),
            Some(filter.clone()),
        ),
    )
    .await
    .unwrap();
    let rows = access.execute("{Parcel{status}}").await.unwrap();
    assert_eq!(rows["data"]["Parcel"][0]["status"], "delivered");
    apply(&tool, call("delete", Value::Null, Some(filter)))
        .await
        .unwrap();
    assert_eq!(
        access.execute("{Parcel{_docID}}").await.unwrap()["data"]["Parcel"],
        json!([])
    );
    node.shutdown().await;
}

#[tokio::test]
async fn changed_targets_and_overbroad_selection_never_apply() {
    let (node, tool) = fixture().await;
    for reference in ["one", "two", "three"] {
        apply(
            &tool,
            call(
                "create",
                json!({"reference":reference,"status":"queued"}),
                None,
            ),
        )
        .await
        .unwrap();
    }
    let mut update = call(
        "update",
        json!({"status":"sent"}),
        Some(json!({"reference":{"_eq":"one"}})),
    );
    let preview = tool.execute(&update).await.unwrap();
    let next: WriteParams = serde_json::from_value(preview["next_call"]["args"].clone()).unwrap();
    apply(
        &tool,
        call(
            "update",
            json!({"status":"changed"}),
            Some(json!({"reference":{"_eq":"one"}})),
        ),
    )
    .await
    .unwrap();
    assert!(tool
        .execute(&next)
        .await
        .unwrap_err()
        .to_string()
        .contains("stale"));
    update
        .options
        .insert("filter".into(), json!({"status":{"_ne":"missing"}}));
    assert!(tool
        .execute(&update)
        .await
        .unwrap_err()
        .to_string()
        .contains("3 targets"));
    update
        .options
        .insert("filter".into(), json!({"reference":{"_eq":"none"}}));
    assert!(tool
        .execute(&update)
        .await
        .unwrap_err()
        .to_string()
        .contains("0 targets"));
    node.shutdown().await;
}

#[tokio::test]
async fn managed_credentials_scope_and_injection_are_refused() {
    let (node, tool) = fixture().await;
    for (collection, expected) in [
        ("Tools", "Gents-managed"),
        ("OAuthCredential", "credentials"),
        ("EvalVerdict", "protected"),
        ("Other", "outside"),
        ("Parcel) {bad", "identifier"),
    ] {
        let mut args = call("create", json!({"reference":"x"}), None);
        args.collection = Some(collection.into());
        let error = tool.execute(&args).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    let error = tool
        .execute(&call("create", json!({"status) {bad":"x"}), None))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("identifier"), "{error}");
    node.shutdown().await;
}

#[tokio::test]
async fn native_relationship_input_uses_the_target_document_id() {
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .unwrap(),
    );
    node.add_schema(
        "type Writer { name: String books: [Volume] } type Volume { title: String author: Writer }",
    )
    .await
    .unwrap();
    let tool = WriteTool::new(
        ConfigAccess::Local(node.clone()),
        BTreeSet::from(["Writer".into(), "Volume".into()]),
        None,
    );
    let mut writer = call("create", json!({"name":"Nora"}), None);
    writer.collection = Some("Writer".into());
    let receipt = apply(&tool, writer).await.unwrap();
    let id = receipt["effect"]["documents"][0]["_docID"]
        .as_str()
        .unwrap();
    let mut volume = call("create", json!({"title":"First","author":id}), None);
    volume.collection = Some("Volume".into());
    apply(&tool, volume).await.unwrap();
    let rows = ConfigAccess::Local(node.clone())
        .execute("{Volume {title author {name}}}")
        .await
        .unwrap();
    assert_eq!(rows["data"]["Volume"][0]["author"]["name"], "Nora");
    node.shutdown().await;
}

#[tokio::test]
async fn a_collection_grant_cannot_bypass_native_document_acp() {
    const OWNER: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    const READER: &str = "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR";
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .unwrap(),
    );
    let policy = node.add_dac_policy(OWNER, "name: Application write ACP\nresources:\n  - name: parcels\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n").await.unwrap();
    node.add_schema(&format!("type Parcel @policy(id: \"{}\", resource: \"parcels\") {{ reference: String status: String }}",crate::graphql::escape_graphql_string(&policy))).await.unwrap();
    let owner = WriteTool::new(
        ConfigAccess::Local(node.clone()),
        BTreeSet::from(["Parcel".into()]),
        Some(identity::Did::new(OWNER).unwrap()),
    );
    let created = apply(
        &owner,
        call(
            "create",
            json!({"reference":"PRIVATE","status":"queued"}),
            None,
        ),
    )
    .await
    .unwrap();
    let doc_id = created["effect"]["documents"][0]["_docID"]
        .as_str()
        .unwrap();
    node.add_dac_actor_relationship(OWNER, "Parcel", doc_id, "reader", READER)
        .await
        .unwrap();
    let reader = WriteTool::new(
        ConfigAccess::Local(node.clone()),
        BTreeSet::from(["Parcel".into()]),
        Some(identity::Did::new(READER).unwrap()),
    );
    let filter = json!({"reference":{"_eq":"PRIVATE"}});
    assert!(apply(
        &reader,
        call("update", json!({"status":"changed"}), Some(filter.clone()))
    )
    .await
    .is_err());
    assert!(apply(&reader, call("delete", Value::Null, Some(filter)))
        .await
        .is_err());
    let rows = ConfigAccess::transact_local(
        &node,
        Some(identity::Did::new(OWNER).unwrap()),
        "verify_application_acp",
        |txn| Box::pin(async move { txn.execute("{Parcel{reference status}}").await }),
    )
    .await
    .unwrap();
    assert_eq!(rows["data"]["Parcel"][0]["status"], "queued");
    node.shutdown().await;
}
