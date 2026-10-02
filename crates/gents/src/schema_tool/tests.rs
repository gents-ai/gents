use super::*;
use crate::tool_surface::{BehaviorToolConfig, ResolvedToolSelection, ToolCeiling};

fn args(value: Value) -> SchemaParams {
    serde_json::from_value(value).unwrap()
}

#[test]
fn concept_help_examples_use_supported_native_sdl() {
    let paths = [
        "collections",
        "collections policy",
        "collections branchable",
        "collections governed",
        "collections downsample",
        "views",
        "views virtual",
        "views materialized",
        "views refresh",
        "fields",
        "fields types",
        "fields nullability",
        "fields arrays",
        "fields defaults",
        "fields crdt",
        "fields immutable",
        "fields constraints",
        "relationships",
        "relationships one-to-many",
        "relationships one-to-one",
        "relationships named",
        "relationships self",
        "indexes",
        "indexes ordered",
        "indexes unique",
        "indexes composite",
        "indexes vector",
        "indexes vector dimensions",
        "indexes vector metrics",
        "indexes vector hnsw",
        "indexes vector flat",
        "indexes vector ivfpq",
        "indexes vector ivfflat",
        "indexes vector ssg",
        "indexes fulltext",
        "indexes encrypted",
        "embeddings",
        "embeddings generation",
        "embeddings indexing",
        "evolution",
        "evolution additive",
        "evolution versions",
        "evolution migrations",
        "migration workflow",
        "migration authoring",
        "migration authoring rust",
        "migration build",
        "migration contract",
        "migration contract memory",
        "migration contract documents",
        "migration arguments",
        "migration inverse",
        "migration versions",
        "migration verify",
        "migration verify host",
        "migration recovery",
    ];
    for path in paths {
        let words: Vec<_> = path.split_whitespace().map(str::to_owned).collect();
        let page = help::page(&words).unwrap_or_else(|error| panic!("{path}: {error}"));
        assert!(
            page.len() <= 2_600,
            "{path}: {} bytes; split into subtopics",
            page.len()
        );
        for fenced in page.split("```graphql\n").skip(1) {
            let (sdl, _) = fenced.split_once("```").expect("closed SDL example");
            let parsed = query::sdl_parse::parse_sdl_with_warnings(sdl)
                .unwrap_or_else(|error| panic!("{path}: {error}\n{sdl}"));
            assert!(parsed.warnings.is_empty(), "{path}: {:?}", parsed.warnings);
            schema::definition_validation::validate_new_collections(&parsed.collections)
                .unwrap_or_else(|error| panic!("{path}: {error}\n{sdl}"));
            schema::definition_validation::validate_new_collections_with_existing(
                &parsed.collections,
                &[],
            )
            .unwrap_or_else(|error| panic!("{path}: {error}\n{sdl}"));
        }
    }
    assert!(help::page(&[]).unwrap().len() < 2_000);
}

#[test]
fn unknown_concept_help_returns_nearest_available_parent() {
    let path = ["indexes", "vector", "unknown"].map(str::to_owned);
    let error = help::page(&path).unwrap_err().to_string();
    let argv: Vec<String> = serde_json::from_str(error.split_once("argv:").unwrap().1).unwrap();
    assert_eq!(argv, ["help", "indexes", "vector"]);
    assert!(help::page(&argv[1..]).is_ok());
}
async fn call(tool: &SchemaTool, value: Value) -> Value {
    serde_json::from_str(&tool.call(args(value)).await.unwrap()).unwrap()
}
async fn apply(tool: &SchemaTool, mut value: Value) -> Value {
    value["argv"]
        .as_array_mut()
        .unwrap()
        .insert(1, json!("preview"));
    let preview = call(tool, value).await;
    call(tool, preview["next_call"]["args"].clone()).await
}
async fn node() -> Arc<EmbeddedNode> {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node
}

#[tokio::test]
async fn schema_publication_matches_executable_lean_contract() {
    for case in &crate::lean_vocab_test::lean_contract_snapshot().schema_publication_cases {
        let node = node().await;
        let tool = SchemaTool::new(node.clone());
        let access = ConfigAccess::Local(node.clone());
        let grant = case["grant"].as_bool().unwrap();
        let compatible = case["compatible"].as_bool().unwrap();
        let artifact = case["artifact"].as_bool().unwrap();
        if !compatible {
            access
                .add_schema("type Probe { value: Int }")
                .await
                .unwrap();
        }
        let before = access.collection_version("Probe").await.unwrap();
        let surface = BehaviorToolConfig::from_selection(
            "worker",
            ResolvedToolSelection {
                enable_schema_tool: grant,
                ..Default::default()
            },
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
        .unwrap()
        .resolve(&node, "did:key:test")
        .await
        .unwrap();
        let mut intent = json!({"argv":["collection","preview","create"],"options":{"sdl":"type Probe { value: String }"}});
        let preview = tool.call(args(intent.clone())).await;
        intent["argv"].as_array_mut().unwrap().remove(1);
        intent["options"]["digest"] = if artifact {
            preview
                .as_ref()
                .ok()
                .and_then(|v| serde_json::from_str::<Value>(v).ok())
                .map(|v| v["next_call"]["args"]["options"]["digest"].clone())
                .unwrap_or(json!("unavailable"))
        } else {
            json!("wrong")
        };
        let accepted = if surface.tool_names().contains(&SCHEMA_TOOL_NAME.to_owned()) {
            tool.call(args(intent)).await.is_ok()
        } else {
            false
        };
        assert_eq!(accepted, case["accepted"].as_bool().unwrap(), "{case}");
        if !accepted {
            assert_eq!(
                before,
                access.collection_version("Probe").await.unwrap(),
                "{case}"
            );
        }
        assert!(!surface.tool_names().contains(&"config".to_owned()));
    }
}

#[tokio::test]
async fn additive_recovery_preserves_documents_and_rejects_stale_preview() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let access = ConfigAccess::Local(node.clone());
    apply(
        &tool,
        json!({"argv":["collection","create"],"options":{"sdl":"type WorkItem { title: String }"}}),
    )
    .await;
    ConfigAccess::write_local(
        &node,
        "schema_test.seed",
        r#"mutation { create_WorkItem(input:{title:"keep me"}) { _docID } }"#,
    )
    .await
    .unwrap();
    let original = access
        .collection_version("WorkItem")
        .await
        .unwrap()
        .unwrap();
    let intent = json!({"argv":["collection","preview","update"],"target_id":"WorkItem","options":{"patch":[{"op":"add","path":"/WorkItem/Fields/-","value":{"Name":"handoff_id","Kind":"String"}}]}});
    let preview = call(&tool, intent.clone()).await;
    assert_eq!(
        original,
        access
            .collection_version("WorkItem")
            .await
            .unwrap()
            .unwrap()
    );
    let applied = call(&tool, preview["next_call"]["args"].clone()).await;
    assert_eq!(applied["committed"], true);
    assert!(tool
        .call(args(preview["next_call"]["args"].clone()))
        .await
        .unwrap_err()
        .to_string()
        .contains("stale digest"));
    let rows = access
        .execute("{ WorkItem { title handoff_id } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["WorkItem"][0]["title"], "keep me");
    assert!(rows["data"]["WorkItem"][0]["handoff_id"].is_null());
    let versions = call(
        &tool,
        json!({"argv":["version","list"],"target_id":"WorkItem"}),
    )
    .await;
    assert_eq!(versions["versions"].as_array().unwrap().len(), 2);
    let current = access
        .collection_version("WorkItem")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(original["VersionID"], current["VersionID"]);
    apply(
        &tool,
        json!({"argv":["collection","materialize"],"target_id":"WorkItem"}),
    )
    .await;
    apply(
        &tool,
        json!({"argv":["version","activate"],"target_id":original["VersionID"]}),
    )
    .await;
    assert_eq!(
        access
            .collection_version("WorkItem")
            .await
            .unwrap()
            .unwrap()["VersionID"],
        original["VersionID"]
    );
}

#[tokio::test]
async fn schema_recovery_help_and_grant_are_independent_of_config() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    for path in [
        vec!["help"],
        vec!["help", "collection"],
        vec!["help", "version"],
        vec!["help", "migration"],
        vec!["help", "view"],
        vec!["help", "indexes", "vector", "hnsw"],
    ] {
        assert!(!tool
            .call(args(json!({"argv":path})))
            .await
            .unwrap()
            .is_empty());
    }
    for intent in [
        json!({"argv":["collection","preview","create"],"options":{"sdl":"type Bad { value: Bool }"}}),
        json!({"argv":["collection","get"],"target_id":"Missing"}),
    ] {
        let error = tool.call(args(intent)).await.unwrap_err();
        let body: Value = serde_json::from_str(&error.to_string()).unwrap();
        assert!(tool
            .call(args(body["recovery"]["args"].clone()))
            .await
            .is_ok());
    }
    let selection = ResolvedToolSelection {
        enable_self_config: true,
        self_config_categories: Some(vec!["automation".into()]),
        ..Default::default()
    };
    let surface = BehaviorToolConfig::from_selection(
        "setup",
        selection,
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node, "did:key:test")
    .await
    .unwrap();
    assert!(!surface.tool_names().contains(&SCHEMA_TOOL_NAME.to_owned()));
    let error=tool.call(args(json!({"argv":["collection","preview","update"],"target_id":"Tools","options":{"patch":[{"op":"add","path":"/Tools/Fields/-","value":{"Name":"bad","Kind":"String"}}]}}))).await.unwrap_err();
    assert!(error.to_string().contains("Gents-managed"));
}

#[tokio::test]
async fn inline_lens_migration_transforms_existing_rows_through_native_owner() {
    let wasm = crate::migration::fixture_lens_wasm();
    assert!(
        wasm.len() > 16,
        "schema migration coverage requires the fixture lens build"
    );
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let access = ConfigAccess::Local(node.clone());
    apply(&tool,json!({"argv":["collection","create"],"options":{"sdl":"type FixtureDoc { name: String }"}})).await;
    ConfigAccess::write_local(
        &node,
        "schema_test.lens_seed",
        r#"mutation { create_FixtureDoc(input:{name:"alice"}) { _docID } }"#,
    )
    .await
    .unwrap();
    let source = access
        .collection_version("FixtureDoc")
        .await
        .unwrap()
        .unwrap()["VersionID"]
        .as_str()
        .unwrap()
        .to_owned();
    let patch=apply(&tool,json!({"argv":["collection","update"],"target_id":"FixtureDoc","options":{"patch":[{"op":"add","path":"/FixtureDoc/Fields/-","value":{"Name":"label","Kind":"String"}},{"op":"replace","path":"/FixtureDoc/IsActive","value":false}]}})).await;
    let dest = patch["result"]["VersionID"].as_str().unwrap().to_owned();
    let lens = crate::defra_node::LensConfig::new(
        &source,
        &dest,
        crate::defra_node::LensModule::from_bytes(wasm.to_vec()),
    );
    let registered = apply(
        &tool,
        json!({"argv":["migration","set"],"options":{"config":lens}}),
    )
    .await;
    assert!(registered["result"]["lensId"].is_string());
    apply(
        &tool,
        json!({"argv":["version","activate"],"target_id":dest}),
    )
    .await;
    apply(
        &tool,
        json!({"argv":["collection","materialize"],"target_id":"FixtureDoc"}),
    )
    .await;
    let rows = access
        .execute("{ FixtureDoc { name label } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["FixtureDoc"][0]["label"], "ALICE");
    let path_lens = crate::defra_node::LensConfig::new(
        &source,
        &dest,
        crate::defra_node::LensModule::from_path("/tmp/forbidden.wasm"),
    );
    assert!(tool
        .call(args(
            json!({"argv":["migration","preview","set"],"options":{"config":path_lens}})
        ))
        .await
        .unwrap_err()
        .to_string()
        .contains("file path"));
}

#[tokio::test]
async fn view_creation_is_previewed_and_discoverable() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    apply(&tool,json!({"argv":["collection","create"],"options":{"sdl":"type ViewInput { title: String }"}})).await;
    apply(&tool,json!({"argv":["view","create"],"options":{"query":"ViewInput { title }","sdl":"type Titles { title: String }"}})).await;
    let found = call(
        &tool,
        json!({"argv":["collection","get"],"target_id":"Titles"}),
    )
    .await;
    assert_eq!(found["collection"]["Name"], "Titles");
}

#[tokio::test]
async fn batch_retains_prior_commit_and_does_not_attempt_later_calls() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let access = ConfigAccess::Local(node);
    let first=call(&tool,json!({"argv":["collection","preview","create"],"options":{"sdl":"type BatchFirst { title: String }"}})).await;
    let last=call(&tool,json!({"argv":["collection","preview","create"],"options":{"sdl":"type BatchLast { title: String }"}})).await;
    let result=tool.call(args(json!({"argv":["batch"],"options":{"operations":[first["next_call"]["args"],{"argv":["collection","get"],"target_id":"Missing"},last["next_call"]["args"]]}}))).await.unwrap_err();
    let failure: Value = serde_json::from_str(&result.to_string()).unwrap();
    assert_eq!(failure["unattempted"], 1);
    assert_eq!(failure["results"][0]["ok"], true);
    assert!(access
        .collection_version("BatchFirst")
        .await
        .unwrap()
        .is_some());
    assert!(access
        .collection_version("BatchLast")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn malformed_schema_previews_return_executable_help_without_publication() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    apply(
        &tool,
        json!({"argv":["collection","create"],"options":{"sdl":"type Shape { title: String }"}}),
    )
    .await;
    let before = ConfigAccess::Local(node.clone())
        .collection_versions()
        .await
        .unwrap();
    for value in [
        json!({"argv":["view","preview","create"],"options":{"sdl":"type Titles { title: String }"}}),
        json!({"argv":["collection","preview","update"],"target_id":"Shape","options":{"patch":[{"op":"add","path":"/Shape/Fields/-"}]}}),
        json!({"argv":["unknown","create"]}),
    ] {
        let error = tool.call(args(value)).await.unwrap_err();
        let error: Value = serde_json::from_str(&error.0).unwrap();
        tool.call(args(error["recovery"]["args"].clone()))
            .await
            .unwrap();
    }
    assert_eq!(
        before,
        ConfigAccess::Local(node)
            .collection_versions()
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn config_shaped_schema_calls_preview_then_apply_without_rewriting_the_receipt() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let access = ConfigAccess::Local(node);
    let help = tool
        .call(args(json!({"argv":["collection","create","--help"]})))
        .await
        .unwrap();
    assert_eq!(
        help,
        tool.call(args(json!({"argv":["help","collection","create"]})))
            .await
            .unwrap()
    );
    let request = json!({"argv":["collection","create"],"options":{"sdl":"type CallShape { value: String }"}});
    let error: Value =
        serde_json::from_str(&tool.call(args(request)).await.unwrap_err().0).unwrap();
    assert_eq!(
        error["recovery"]["args"]["argv"],
        json!(["collection", "preview", "create"])
    );
    let text = tool
        .call(args(error["recovery"]["args"].clone()))
        .await
        .unwrap();
    assert!(text.find("\"effect\"").unwrap() < text.find("\"next_call\"").unwrap());
    assert!(text.find("\"next_call\"").unwrap() < text.find("\"metadata\"").unwrap());
    let preview: Value = serde_json::from_str(&text).unwrap();
    assert!(access
        .collection_version("CallShape")
        .await
        .unwrap()
        .is_none());
    assert!(preview["next_call"]["args"]["options"]["digest"].is_string());
    call(&tool, preview["next_call"]["args"].clone()).await;
    let positional = call(&tool, json!({"argv":["collection","get","CallShape"]})).await;
    let named = call(
        &tool,
        json!({"argv":["collection","get"],"target_id":"CallShape"}),
    )
    .await;
    assert_eq!(positional, named);
    assert!(tool
        .call(args(
            json!({"argv":["collection","get","CallShape"],"target_id":"Other"})
        ))
        .await
        .unwrap_err()
        .0
        .contains("conflicts"));
}

#[tokio::test]
async fn misplaced_preview_returns_the_corrected_call_without_publishing() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let request = json!({"argv":["collection","create"],"options":{"preview":true,"sdl":"type RecoveryShape { value: String }"}});
    let error: Value =
        serde_json::from_str(&tool.call(args(request)).await.unwrap_err().0).unwrap();
    assert_eq!(
        error["recovery"]["args"]["argv"],
        json!(["collection", "preview", "create"])
    );
    assert!(error["recovery"]["args"]["options"]
        .get("preview")
        .is_none());
    let preview = call(&tool, error["recovery"]["args"].clone()).await;
    assert!(ConfigAccess::Local(node.clone())
        .collection_version("RecoveryShape")
        .await
        .unwrap()
        .is_none());
    call(&tool, preview["next_call"]["args"].clone()).await;
    assert!(ConfigAccess::Local(node)
        .collection_version("RecoveryShape")
        .await
        .unwrap()
        .is_some());
}

#[test]
fn schema_command_normalizes_config_conventions_without_changing_the_intent() {
    for (resource, verb) in [
        ("collection", "get"),
        ("collection", "update"),
        ("collection", "materialize"),
        ("version", "list"),
        ("version", "get"),
        ("version", "activate"),
    ] {
        let positional = SchemaCommand::parse(args(json!({"argv":[resource,verb,"id"]}))).unwrap();
        let named =
            SchemaCommand::parse(args(json!({"argv":[resource,verb],"target_id":"id"}))).unwrap();
        assert_eq!(
            serde_json::to_value(&positional.args).unwrap(),
            serde_json::to_value(&named.args).unwrap()
        );
        assert!(SchemaCommand::parse(args(
            json!({"argv":[resource,verb,"id"],"target_id":"different"})
        ))
        .is_err());
    }
    let preview = SchemaCommand::parse(args(json!({"argv":["collection","preview","update","WorkItem"],"options":{"patch":[{"op":"add","path":"/WorkItem/Fields/-","value":{"Name":"title","Kind":"String"}}]}}))).unwrap();
    assert!(preview.preview);
    assert!(preview.digest.is_none());
    let apply = SchemaCommand::parse(apply_call(&preview.args, "sha256:receipt")).unwrap();
    assert!(!apply.preview);
    assert_eq!(apply.digest.as_deref(), Some("sha256:receipt"));
    assert_eq!(
        serde_json::to_value(preview.args).unwrap(),
        serde_json::to_value(apply.args).unwrap()
    );
    assert!(SchemaCommand::parse(args(
        json!({"argv":["collection","create"],"options":{"digest":false}})
    ))
    .is_err());
}
