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
async fn fixture() -> (Arc<crate::defra_node::Node>, WriteTool) {
    let node = Arc::new(crate::defra_node::Node::builder().build().await.unwrap());
    node.add_collection(
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
    node.shutdown().await.expect("node shutdown");
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
        assert!(format!("{error:#}").contains("unknown filter field/operator"));
    }
    node.shutdown().await.expect("node shutdown");
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn native_relationship_input_uses_the_target_document_id() {
    let node = Arc::new(crate::defra_node::Node::builder().build().await.unwrap());
    node.add_collection(
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_collection_grant_cannot_bypass_native_document_acp() {
    use crate::store::identity::NodeIdentity;
    let home = tempfile::tempdir().unwrap();
    let runtime_path = home.path().join("runtime.key");
    let runtime = crate::KeyIdentity::load_or_create(&runtime_path, None).unwrap();
    let user = crate::KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let reader = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let key = crate::store::identity::load_file_identity(&runtime_path)
        .unwrap()
        .private_key_bytes();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        crate::defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(crate::defra_node::SigningKey::Ed25519(key))
            .with_node_acp_enabled()
            .with_http(crate::defra_node::HttpConfig::with_listener(listener).unwrap())
            .build()
            .await
            .unwrap(),
    );
    let administrator =
        ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    for did in [user.did(), reader.did()] {
        for permission in [
            acp::NodePermission::CollectionGet,
            acp::NodePermission::DocumentRead,
            acp::NodePermission::DocumentUpdate,
            acp::NodePermission::DocumentDelete,
            acp::NodePermission::DacRelationAdd,
        ] {
            administrator
                .grant_node_permission(&identity::Did::new(did).unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let policy = node
        .add_dac_policy_as(
            &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
            "name: Application write ACP\nresources:\n  - name: parcels\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
        )
        .await
        .unwrap();
    let schema = format!("type Parcel @policy(id: \"{}\", resource: \"parcels\") {{ reference: String status: String }}",crate::store::graphql::escape_graphql_string(&policy));
    node.add_collection(&schema).await.unwrap();
    let owner = ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(endpoint, user.did())
            .with_client_signing_capability(user.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
    );
    let created = owner
        .transact_write("test.application_acp.create", |txn| {
            let schema = &schema;
            Box::pin(async move {
                txn.execute_client_signed_create(
            "mutation($input: ParcelMutationInputArg!) { create_Parcel(input:$input) {_docID} }",
            &json!({"input":{"reference":"PRIVATE","status":"queued"}}), schema,
        ).await
            })
        })
        .await
        .unwrap();
    let doc_id = crate::store::graphql::created_doc_id(&created, "Parcel").unwrap();
    let owners = owner
        .transact_readonly("test.application_acp.owner", |txn| {
            let doc_id = &doc_id;
            Box::pin(async move {
                txn.document_owners("Parcel", std::slice::from_ref(doc_id))
                    .await
            })
        })
        .await
        .unwrap();
    assert_eq!(
        owners[0].owner.as_ref().map(|did| did.as_str()),
        Some(user.did())
    );
    let reader_did = identity::Did::new(reader.did()).unwrap();
    owner
        .add_actor_relationship("Parcel", &doc_id, "reader", &reader_did)
        .await
        .unwrap();
    let reader_actor = crate::defra_node::AcpIdentity::Authenticated(reader_did.clone());
    for (permission, expected) in [
        (crate::defra_node::DocumentPermission::Read, true),
        (crate::defra_node::DocumentPermission::Update, false),
        (crate::defra_node::DocumentPermission::Delete, false),
    ] {
        assert_eq!(
            node.document_acp()
                .check_doc_access(&reader_actor, permission, &policy, "parcels", &doc_id)
                .await
                .unwrap(),
            expected
        );
    }
    let reader_access =
        ConfigAccess::local_managed_agent(node.clone(), reader_did.clone(), reader.as_ref())
            .unwrap();
    let reader = WriteTool::new(
        reader_access,
        BTreeSet::from(["Parcel".into()]),
        Some(reader_did),
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
    let rows = owner.execute("{Parcel{reference status}}").await.unwrap();
    assert_eq!(rows["data"]["Parcel"][0]["status"], "queued");
    node.shutdown().await.expect("node shutdown");
}

mod plugin {
    use super::*;
    use crate::plugin::data::{OnConflict, PluginData, Stamp, StampContext};
    use gents_protocol::job::{DataOp, DataWrite, PluginDataDeclaration};
    use std::collections::BTreeMap;

    struct Fixture {
        node: Arc<crate::defra_node::Node>,
        runtime: identity::Did,
        agent: Arc<dyn crate::store::identity::NodeIdentity>,
        _home: tempfile::TempDir,
    }

    impl std::ops::Deref for Fixture {
        type Target = Arc<crate::defra_node::Node>;
        fn deref(&self) -> &Self::Target {
            &self.node
        }
    }

    async fn node() -> Fixture {
        use crate::store::identity::NodeIdentity;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("runtime.key");
        let runtime = crate::KeyIdentity::load_or_create(&path, None).unwrap();
        let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
        let key = crate::store::identity::load_file_identity(&path)
            .unwrap()
            .private_key_bytes();
        let node = Arc::new(
            crate::defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(crate::defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        let runtime_did = identity::Did::new(runtime.did()).unwrap();
        let owner = ConfigAccess::local_as(node.clone(), runtime_did.clone());
        for permission in [
            acp::NodePermission::CollectionGet,
            acp::NodePermission::DocumentRead,
            acp::NodePermission::DocumentUpdate,
            acp::NodePermission::DocumentDelete,
        ] {
            owner
                .grant_node_permission(&identity::Did::new(agent.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        node.add_collection(
            "type Note { owner: String body: String tags: [String] }
             type Keyed { owner: String key: String @index(unique: true) body: String created_at: String }
             type Twin { owner: String key: String body: String created_at: String }",
        )
        .await
        .unwrap();
        Fixture {
            node,
            runtime: runtime_did,
            agent,
            _home: home,
        }
    }

    fn grant(collections: &[&str]) -> PluginData {
        PluginData {
            declaration: PluginDataDeclaration {
                collections: collections.iter().map(|c| (*c).into()).collect(),
                ops: vec!["insert".into(), "update".into(), "delete".into()],
                stamped: BTreeMap::from([("owner".to_owned(), Stamp::CallerDid)]),
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

    async fn rows(node: &Fixture, collection: &str) -> Value {
        ConfigAccess::local_as(node.node.clone(), node.runtime.clone())
            .execute(&format!("{{{collection}{{_docID body created_at}}}}"))
            .await
            .unwrap()["data"][collection]
            .clone()
    }

    fn stamps(node: &Fixture) -> StampContext {
        StampContext::Callback {
            caller_did: node.agent.did().into(),
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
        node: &Fixture,
        grant: &PluginData,
        writes: Vec<DataWrite>,
    ) -> Result<Vec<Value>> {
        let actor = identity::Did::new(node.agent.did())?;
        let access = ConfigAccess::local_managed_agent(
            node.node.clone(),
            actor.clone(),
            node.agent.as_ref(),
        )?;
        let stamps = stamps(node);
        access
            .transact_write("plugin_writes_test", |txn| {
                let writes = writes.clone();
                let actor = actor.clone();
                let stamps = stamps.clone();
                Box::pin(async move {
                    apply_plugin_writes_in_txn(txn, &actor, grant, &stamps, writes).await
                })
            })
            .await
    }

    async fn notes(node: &Fixture) -> Value {
        ConfigAccess::local_as(node.node.clone(), node.runtime.clone())
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
        node.shutdown().await.expect("node shutdown");
    }

    #[tokio::test]
    async fn plugin_write_to_a_protected_collection_is_refused() {
        let node = node().await;
        let publication = &crate::lean_vocab_test::lean_contract_snapshot()
            .client_publication_protected_collection;
        for collection in [
            gents_protocol::schemas::EVAL_RUN_NAME,
            "AgentRequest",
            publication.as_str(),
        ] {
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
                "{error:#}"
            );
        }
        node.shutdown().await.expect("node shutdown");
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
            "{error:#}"
        );
        assert_eq!(notes(&node).await, json!([]));
        node.shutdown().await.expect("node shutdown");
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
        assert!(error.to_string().contains("missing_field"), "{error:#}");
        assert_eq!(notes(&node).await, json!([]));
        node.shutdown().await.expect("node shutdown");
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
        assert_eq!(notes(&node).await[0]["owner"], node.agent.did());
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
        node.shutdown().await.expect("node shutdown");
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
        node.shutdown().await.expect("node shutdown");
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
        node.shutdown().await.expect("node shutdown");
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
        assert!(error.to_string().contains("duplicate Keyed a"), "{error:#}");
        assert_eq!(rows(&node, "Keyed").await.as_array().unwrap().len(), 1);
        node.shutdown().await.expect("node shutdown");
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
                "{error:#}"
            );
        }
        let missing = insert_with("Keyed", json!({"body": "x"}), Some(OnConflict::Ignore));
        let error = apply_all(&node, &keyed_grant("Keyed", &[&["key"]]), vec![missing])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("key"), "{error:#}");
        assert_eq!(rows(&node, "Keyed").await, json!([]));
        node.shutdown().await.expect("node shutdown");
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
        node.shutdown().await.expect("node shutdown");
    }
}
