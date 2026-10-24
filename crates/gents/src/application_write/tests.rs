use super::*;
use gents_protocol::error_chain::ErrorChain;
use std::sync::Arc;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

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
        ConfigAccess::local(node.clone()),
        BTreeSet::from(["Parcel".into()]),
        None,
    );
    (node, tool)
}
async fn apply(tool: &WriteTool, preview: WriteParams) -> TestResult<Value> {
    let receipt = tool.execute(&preview).await?;
    let next = serde_json::from_value(receipt["next_call"]["args"].clone())?;
    Ok(tool.execute(&next).await?)
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
    let access = ConfigAccess::local(node.clone());
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
    for operation in ["update", "delete"] {
        let malformed = call(
            operation,
            json!({"status":"sent"}),
            Some(json!({"and":[{"eq":{"reference":"one"}}]})),
        );
        let error = tool.execute(&malformed).await.unwrap_err();
        assert!(ErrorChain(error.as_ref())
            .to_string()
            .contains("unknown filter field/operator"));
    }
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
        ConfigAccess::local(node.clone()),
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
    let rows = ConfigAccess::local(node.clone())
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
        ConfigAccess::local(node.clone()),
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
        ConfigAccess::local(node.clone()),
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
    let rows = ConfigAccess::transact_local_write(
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

mod plugin {
    use super::*;
    use crate::plugin::data::{OnConflict, PluginData, Stamp, StampContext};
    use gents_protocol::job::{DataOp, DataWrite, PluginDataDeclaration};
    use std::collections::BTreeMap;

    const NODE: &str = "did:key:z6Mknode";

    async fn node() -> Arc<crate::defra_node::EmbeddedNode> {
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        node.add_schema(
            "type Note { owner: String body: String tags: [String] }
             type Keyed { owner: String key: String @index(unique: true) body: String created_at: String }
             type Twin { owner: String key: String body: String created_at: String }",
        )
        .await
        .unwrap();
        node
    }

    fn grant(collections: &[&str]) -> PluginData {
        PluginData {
            declaration: PluginDataDeclaration {
                collections: collections.iter().map(|c| (*c).into()).collect(),
                ops: vec!["insert".into(), "update".into(), "delete".into()],
                stamped: BTreeMap::from([("owner".to_owned(), Stamp::NodeDid)]),
            },
            collection_fields: collections
                .iter()
                .map(|c| ((*c).to_owned(), vec!["owner".to_owned(), "body".to_owned()]))
                .collect(),
            unique_keys: BTreeMap::new(),
        }
    }

    fn keyed_grant(collection: &str, keys: &[&[&str]]) -> PluginData {
        let mut grant = grant(&[collection]);
        grant.collection_fields.insert(
            collection.into(),
            ["owner", "key", "body", "created_at"]
                .map(String::from)
                .to_vec(),
        );
        grant.unique_keys.insert(
            collection.into(),
            keys.iter()
                .map(|key| key.iter().map(|f| (*f).to_owned()).collect())
                .collect(),
        );
        grant
    }

    fn insert_with(collection: &str, fields: Value, on_conflict: Option<OnConflict>) -> DataWrite {
        DataWrite {
            on_conflict,
            ..write(DataOp::Insert, collection, None, fields)
        }
    }

    async fn rows(node: &Arc<crate::defra_node::EmbeddedNode>, collection: &str) -> Value {
        ConfigAccess::local(node.clone())
            .execute(&format!("{{{collection}{{_docID body created_at}}}}"))
            .await
            .unwrap()["data"][collection]
            .clone()
    }

    fn stamps() -> StampContext {
        StampContext::Callback {
            node_did: NODE.into(),
        }
    }

    fn write(op: DataOp, collection: &str, doc_id: Option<&str>, fields: Value) -> DataWrite {
        DataWrite {
            op,
            collection: collection.into(),
            doc_id: doc_id.map(Into::into),
            fields: fields.as_object().cloned().unwrap(),
            on_conflict: None,
        }
    }

    async fn apply_all(
        node: &Arc<crate::defra_node::EmbeddedNode>,
        grant: &PluginData,
        writes: Vec<DataWrite>,
    ) -> TestResult<Vec<Value>> {
        Ok(
            ConfigAccess::transact_local_write(node, None, "plugin_writes_test", |txn| {
                let writes = writes.clone();
                Box::pin(
                    async move { apply_plugin_writes_in_txn(txn, grant, &stamps(), writes).await },
                )
            })
            .await?,
        )
    }

    async fn notes(node: &Arc<crate::defra_node::EmbeddedNode>) -> Value {
        ConfigAccess::local(node.clone())
            .execute("{Note{_docID owner body tags}}")
            .await
            .unwrap()["data"]["Note"]
            .clone()
    }

    #[tokio::test]
    async fn plugin_insert_update_and_delete_commit() {
        let node = node().await;
        let grant = grant(&["Note"]);
        let receipts = apply_all(
            &node,
            &grant,
            vec![write(
                DataOp::Insert,
                "Note",
                None,
                json!({"body": "a \" } b", "tags": []}),
            )],
        )
        .await
        .unwrap();
        let doc_id = receipts[0]["_docID"].as_str().unwrap().to_owned();
        assert_eq!(notes(&node).await[0]["body"], "a \" } b");
        assert!(notes(&node).await[0]["tags"].is_null());
        apply_all(
            &node,
            &grant,
            vec![write(
                DataOp::Update,
                "Note",
                Some(&doc_id),
                json!({"body": "changed"}),
            )],
        )
        .await
        .unwrap();
        assert_eq!(notes(&node).await[0]["body"], "changed");
        apply_all(
            &node,
            &grant,
            vec![write(DataOp::Delete, "Note", Some(&doc_id), json!({}))],
        )
        .await
        .unwrap();
        assert_eq!(notes(&node).await, json!([]));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn plugin_write_to_a_protected_collection_is_refused() {
        let node = node().await;
        for collection in [gents_protocol::schemas::EVAL_RUN_NAME, "AgentRequest"] {
            let grant = grant(&[collection]);
            let error = apply_all(
                &node,
                &grant,
                vec![write(
                    DataOp::Insert,
                    collection,
                    None,
                    json!({"body": "x"}),
                )],
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains("declared data grant"),
                "{}",
                ErrorChain(error.as_ref())
            );
        }
        node.shutdown().await;
    }

    #[tokio::test]
    async fn plugin_write_outside_the_grant_is_refused() {
        let node = node().await;
        let error = apply_all(
            &node,
            &grant(&["Other"]),
            vec![write(DataOp::Insert, "Note", None, json!({"body": "x"}))],
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("declared data grant"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert_eq!(notes(&node).await, json!([]));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn one_failing_plugin_write_commits_nothing() {
        let node = node().await;
        let error = apply_all(
            &node,
            &grant(&["Note"]),
            vec![
                write(DataOp::Insert, "Note", None, json!({"body": "kept?"})),
                write(DataOp::Insert, "Note", None, json!({"missing_field": "x"})),
            ],
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("missing_field"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert_eq!(notes(&node).await, json!([]));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn stamped_values_come_from_host_facts() {
        let node = node().await;
        let grant = grant(&["Note"]);
        apply_all(
            &node,
            &grant,
            vec![write(DataOp::Insert, "Note", None, json!({"body": "x"}))],
        )
        .await
        .unwrap();
        assert_eq!(notes(&node).await[0]["owner"], NODE);
        let forged = apply_all(
            &node,
            &grant,
            vec![write(
                DataOp::Insert,
                "Note",
                None,
                json!({"body": "y", "owner": "did:key:guest"}),
            )],
        )
        .await;
        assert!(forged.is_err());
        assert_eq!(notes(&node).await.as_array().unwrap().len(), 1);
        node.shutdown().await;
    }
    #[tokio::test]
    async fn on_conflict_ignore_with_an_equal_row_is_a_no_op() {
        let node = node().await;
        let grant = keyed_grant("Keyed", &[&["key"]]);
        let write = insert_with(
            "Keyed",
            json!({"key": "a", "body": "x"}),
            Some(OnConflict::Ignore),
        );
        let first = apply_all(&node, &grant, vec![write.clone()]).await.unwrap();
        let second = apply_all(&node, &grant, vec![write]).await.unwrap();
        assert_eq!(second[0]["_docID"], first[0]["_docID"]);
        assert!(second[0].get("duplicate").is_none());
        assert_eq!(rows(&node, "Keyed").await.as_array().unwrap().len(), 1);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn on_conflict_ignore_with_a_different_row_reports_duplicate() {
        let node = node().await;
        let grant = keyed_grant("Keyed", &[&["key"]]);
        let write = |body| {
            insert_with(
                "Keyed",
                json!({"key": "a", "body": body}),
                Some(OnConflict::Ignore),
            )
        };
        apply_all(&node, &grant, vec![write("x")]).await.unwrap();
        let receipts = apply_all(&node, &grant, vec![write("y")]).await.unwrap();
        assert_eq!(receipts[0], json!({"duplicate": true}));
        let stored = rows(&node, "Keyed").await;
        assert_eq!(stored.as_array().unwrap().len(), 1);
        assert_eq!(stored[0]["body"], "x");
        node.shutdown().await;
    }

    #[tokio::test]
    async fn on_conflict_fail_refuses_a_different_existing_row_and_keeps_an_equal_one() {
        let node = node().await;
        let grant = keyed_grant("Keyed", &[&["key"]]);
        let write = |body| {
            insert_with(
                "Keyed",
                json!({"key": "a", "body": body}),
                Some(OnConflict::Fail),
            )
        };
        apply_all(&node, &grant, vec![write("x")]).await.unwrap();
        let equal = apply_all(&node, &grant, vec![write("x")]).await.unwrap();
        assert!(equal[0].get("duplicate").is_none());
        let error = apply_all(&node, &grant, vec![write("y")])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("duplicate Keyed a"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert_eq!(rows(&node, "Keyed").await.as_array().unwrap().len(), 1);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn on_conflict_needs_exactly_one_unique_key() {
        let node = node().await;
        let write = insert_with(
            "Keyed",
            json!({"key": "a", "body": "x"}),
            Some(OnConflict::Ignore),
        );
        for keys in [&[][..], &[&["key"][..], &["body"][..]][..]] {
            let error = apply_all(&node, &keyed_grant("Keyed", keys), vec![write.clone()])
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("exactly one unique key"),
                "{}",
                ErrorChain(error.as_ref())
            );
        }
        let missing = insert_with("Keyed", json!({"body": "x"}), Some(OnConflict::Ignore));
        let error = apply_all(&node, &keyed_grant("Keyed", &[&["key"]]), vec![missing])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("key"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert_eq!(rows(&node, "Keyed").await, json!([]));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn p2p_twin_readers_pick_the_earliest_created_then_lowest_doc_id() {
        let node = node().await;
        let grant = keyed_grant("Twin", &[&["key"]]);
        let seed = |key, body, created_at| {
            insert_with(
                "Twin",
                json!({"key": key, "body": body, "created_at": created_at}),
                None,
            )
        };
        apply_all(
            &node,
            &grant,
            vec![
                seed("k1", "late", "2026-02"),
                seed("k1", "early", "2026-01"),
                seed("k2", "a", "2026-03"),
                seed("k2", "b", "2026-03"),
            ],
        )
        .await
        .unwrap();
        let conflicting = |key, body| {
            insert_with(
                "Twin",
                json!({"key": key, "body": body}),
                Some(OnConflict::Ignore),
            )
        };
        let early = apply_all(&node, &grant, vec![conflicting("k1", "early")])
            .await
            .unwrap();
        assert!(early[0].get("duplicate").is_none(), "{early:?}");
        let late = apply_all(&node, &grant, vec![conflicting("k1", "late")])
            .await
            .unwrap();
        assert_eq!(late[0], json!({"duplicate": true}));
        let ties = rows(&node, "Twin").await;
        let winner = ties
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["created_at"] == "2026-03")
            .min_by_key(|row| row["_docID"].as_str().unwrap().to_owned())
            .unwrap()["body"]
            .as_str()
            .unwrap()
            .to_owned();
        let loser = if winner == "a" { "b" } else { "a" };
        let same = apply_all(&node, &grant, vec![conflicting("k2", &winner)])
            .await
            .unwrap();
        assert!(same[0].get("duplicate").is_none(), "{same:?}");
        let other = apply_all(&node, &grant, vec![conflicting("k2", loser)])
            .await
            .unwrap();
        assert_eq!(other[0], json!({"duplicate": true}));
        node.shutdown().await;
    }
}
