use super::*;
use crate::tools::surface::{AgentToolSurfaceConfig, ResolvedToolSelection, ToolCeiling};
use gents_protocol::tool_names::SCHEMA_TOOL_NAME;

fn args(value: Value) -> SchemaParams {
    serde_json::from_value(value).unwrap()
}

#[test]
fn concept_help_examples_use_supported_native_sdl() {
    let paths = help::PATHS;
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
fn concept_discovery_resolves_inventory_topics_without_failed_calls() {
    for (requested, canonical) in [
        ("collections fields", "fields"),
        ("collection fields types", "fields types"),
        ("collections relationships", "relationships"),
        ("collections indexes fulltext", "indexes fulltext"),
        ("collections create", "collection create"),
        ("fulltext", "indexes fulltext"),
        ("unique", "indexes unique"),
        ("index", "indexes"),
        ("index vector hnsw", "indexes vector hnsw"),
        ("concepts SDL", ""),
    ] {
        let words = |text: &str| {
            text.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            help::page(&words(requested)).unwrap(),
            help::page(&words(canonical)).unwrap()
        );
    }
    for path in help::PATHS {
        let words: Vec<_> = path.split_whitespace().map(str::to_owned).collect();
        assert_eq!(help::resolve(&words).unwrap(), words, "{path}");
    }
}

#[test]
fn discovery_does_not_guess_ambiguous_topics_or_arbitrary_plurals() {
    for topic in ["create", "get", "indexess", "collectionss"] {
        let path = vec![topic.to_owned()];
        assert!(help::resolve(&path).is_none(), "{topic}");
        assert!(help::page(&path).is_err(), "{topic}");
        assert!(help::recovery_path(&path).is_empty());
    }
}

#[test]
fn argument_recovery_does_not_choose_between_conflicting_sdl_inputs() {
    let request = args(
        json!({"argv":["collection","preview","create"],"options":{"schema":"type A { name: String }","sdl":"type B { name: String }"}}),
    );
    assert!(argument_correction(&request).is_none());
    let read = args(
        json!({"argv":["version","preview","get","exact-version"],"options":{"digest":"old"}}),
    );
    let (_, next) = argument_correction(&read).unwrap();
    assert_eq!(next.argv, ["version", "get", "exact-version"]);
    assert!(next.options.is_empty());
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
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node
}

/// Schema tool acting as the node's runtime owner, the signed actor for the
/// node-owned collections these fixtures create and migrate.
fn runtime_owned_tool(node: &Arc<EmbeddedNode>) -> SchemaTool {
    let runtime = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
    SchemaTool {
        node_did: runtime.to_string(),
        access: ConfigAccess::local_as(node.clone(), runtime),
        home: None,
        caller: None,
    }
}

#[tokio::test]
async fn schema_preview_and_publication_expose_ignored_native_directives() {
    use crate::store::config_client::NodePermission;

    let (db, _user, _user_access, binding) =
        crate::test_support::signed_test_node("schema-native-warnings").await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let admin = ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let agent_did = identity::Did::new(agent.did()).unwrap();
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::CollectionPatch,
    ] {
        admin
            .grant_node_permission(&agent_did, permission)
            .await
            .unwrap();
    }
    let tool = SchemaTool::for_managed_agent(
        db.node.clone(),
        db.node_identity.did(),
        None,
        agent.as_ref(),
    )
    .unwrap();
    let preview = call(
        &tool,
        json!({"argv":["collection","preview","create"],"options":{
            "sdl":"type WarningProbe { code: String @unique }"
        }}),
    )
    .await;
    let warnings = preview["warnings"]
        .as_array()
        .expect("native warnings in preview");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0]
        .as_str()
        .unwrap()
        .contains("unknown directive @unique"));
    assert!(warnings[0].as_str().unwrap().contains("ignored"));
    let result = call(&tool, preview["next_call"]["args"].clone()).await;
    assert_eq!(result["committed"], true);
    assert_eq!(result["warnings"], preview["warnings"]);
    // The receipt says how the new collection becomes writable. This agent
    // does not hold the tools category, so it is told who grants it.
    let access = &result["result"]["write_access"];
    assert_eq!(access["writable"], false, "{result}");
    assert!(access.get("next_call").is_none(), "{result}");
    let how = access["how"].as_str().unwrap();
    assert!(
        how.contains("write tool")
            && how.contains("datastore.write_collections")
            && how.contains("WarningProbe")
            && how.contains("you do not hold the tools category"),
        "{result}"
    );
    // An agent that holds it gets the exact call.
    let holder = tool.clone().with_tools_category(true);
    let created = call(
        &holder,
        json!({"argv":["collection","preview","create"],"options":{
            "sdl":"type GrantProbe { code: String }"
        }}),
    )
    .await;
    let created = call(&holder, created["next_call"]["args"].clone()).await;
    let access = &created["result"]["write_access"];
    assert_eq!(access["next_call"]["tool"], "config", "{created}");
    assert_eq!(
        access["next_call"]["args"]["argv"],
        json!(["tools", "edit"]),
        "{created}"
    );
    assert_eq!(
        access["next_call"]["args"]["set"]["datastore"]["write_collections"],
        json!(["GrantProbe"]),
        "{created}"
    );

    let valid = call(
        &tool,
        json!({"argv":["collection","preview","create"],"options":{
            "sdl":"type IndexedProbe { code: String @index(ordered: {unique: true}) }"
        }}),
    )
    .await;
    assert!(valid.get("warnings").is_none(), "{valid}");
    db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema_publication_matches_executable_lean_contract() {
    for case in &gents_lean_vocab::lean_contract_snapshot().schema_publication_cases {
        if !case["native_allowed"].as_bool().unwrap() {
            continue;
        }
        let node = node().await;
        let tool = SchemaTool::new(node.clone());
        let access = ConfigAccess::local(node.clone());
        let grant = case["grant"].as_bool().unwrap();
        let compatible = case["compatible"].as_bool().unwrap();
        let artifact = case["artifact"].as_bool().unwrap();
        if !compatible {
            access
                .add_collection("type Probe { value: Int }")
                .await
                .unwrap();
        }
        let before = access.collection_version("Probe").await.unwrap();
        let surface = AgentToolSurfaceConfig::from_selection(
            "worker",
            ResolvedToolSelection {
                enable_schema_tool: grant,
                ..Default::default()
            },
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
        .unwrap()
        .resolve(
            &node,
            node.node_identity_did().unwrap(),
            &Default::default(),
        )
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
async fn schema_publication_uses_agent_native_authority() {
    use crate::store::config_client::NodePermission;
    use crate::store::identity::{KeyIdentity, NodeIdentity};

    for case in &gents_lean_vocab::lean_contract_snapshot().schema_publication_cases {
        if case["grant"] != true || case["artifact"] != true || case["compatible"] != true {
            continue;
        }
        let home = tempfile::tempdir().unwrap();
        let runtime = KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap();
        let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        let agent_did = identity::Did::new(agent.did()).unwrap();
        let did_only =
            crate::store::identity::RegisteredIdentity::from_registered_did(agent.did(), None)
                .unwrap();
        assert!(matches!(
            SchemaTool::for_managed_agent(
                node.clone(),
                node.node_identity_did().unwrap(),
                None,
                &did_only
            ),
            Err(SchemaIdentityError::ConfigClient(
                crate::store::config_client::ConfigClientError::ManagedAgentSignerRequired
            ))
        ));
        let foreign_root =
            KeyIdentity::load_or_create(home.path().join("foreign.key"), None).unwrap();
        let foreign_binding =
            crate::store::identity::provision_agent_identity(&foreign_root).unwrap();
        let foreign_agent =
            crate::store::identity::resolve_agent_identity(&foreign_root, &foreign_binding)
                .unwrap();
        assert!(matches!(
            SchemaTool::for_managed_agent(
                node.clone(),
                node.node_identity_did().unwrap(),
                None,
                foreign_agent.as_ref(),
            ),
            Err(SchemaIdentityError::ConfigClient(
                crate::store::config_client::ConfigClientError::TransactionActorMismatch
            ))
        ));
        let tool = SchemaTool::for_managed_agent(
            node.clone(),
            node.node_identity_did().unwrap(),
            None,
            agent.as_ref(),
        )
        .unwrap();
        assert!(
            tool.call(args(json!({"argv":["collection","list"]})))
                .await
                .is_err(),
            "collection discovery must honor the agent's native node permission"
        );
        let admin =
            ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
        admin
            .grant_node_permission(&agent_did, NodePermission::CollectionGet)
            .await
            .unwrap();
        if case["native_allowed"] == true {
            admin
                .grant_node_permission(&agent_did, NodePermission::CollectionPatch)
                .await
                .unwrap();
        }
        let preview = call(&tool, json!({"argv":["collection","preview","create"],"options":{"sdl":"type AgentSchemaProbe { value: String }"}})).await;
        let result = tool.call(args(preview["next_call"]["args"].clone())).await;
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{case}: {result:?}"
        );
        let installed = ConfigAccess::local(node.clone())
            .collection_version("AgentSchemaProbe")
            .await
            .unwrap()
            .is_some();
        assert_eq!(installed, case["accepted"].as_bool().unwrap(), "{case}");
        node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn schema_view_and_materialization_use_managed_agent_authority() {
    use crate::store::config_client::NodePermission;
    use crate::store::identity::{KeyIdentity, NodeIdentity};

    let home = tempfile::tempdir().unwrap();
    let runtime = KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    owner
        .add_collection("type AgentViewInput { name: String }")
        .await
        .unwrap();
    for scratch in ["AgentScratch", "AgentScratchKept"] {
        owner
            .add_collection(&format!("type {scratch} {{ name: String }}"))
            .await
            .unwrap();
        owner
            .write(
                "schema_test.seed",
                &format!(r#"mutation {{ create_{scratch}(input:{{name:"row"}}) {{ _docID }} }}"#),
            )
            .await
            .unwrap();
    }
    let actor_did = identity::Did::new(agent.did()).unwrap();
    let access =
        ConfigAccess::local_managed_agent(node.clone(), actor_did.clone(), agent.as_ref()).unwrap();
    assert!(access
        .add_schema_view(
            "AgentViewInput { name }",
            "type AgentViewDenied { name: String }"
        )
        .await
        .is_err());
    assert!(access
        .materialize_schema_collection("AgentViewInput")
        .await
        .is_err());
    assert!(access.truncate_collection("AgentScratch").await.is_err());
    assert!(access
        .remove_collections(&["AgentScratch".to_owned()])
        .await
        .is_err());

    let cases = gents_lean_vocab::lean_identity_household_cases();
    let group = cases
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        ._tool_grant_group_cases
        .iter()
        .find(|case| case["name"] == "schema-enable-all")
        .unwrap();
    let permissions: Vec<_> = group["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| NodePermission::parse(source["permission"].as_str().unwrap()).unwrap())
        .collect();
    for permission in &permissions {
        owner
            .grant_node_permission(&actor_did, *permission)
            .await
            .unwrap();
    }
    access
        .add_schema_view(
            "AgentViewInput { name }",
            "type AgentViewAllowed { name: String }",
        )
        .await
        .unwrap();
    access
        .materialize_schema_collection("AgentViewInput")
        .await
        .unwrap();
    assert!(permissions.contains(&NodePermission::CollectionTruncate));
    access.truncate_collection("AgentScratch").await.unwrap();
    access
        .remove_collections(&["AgentScratch".to_owned()])
        .await
        .unwrap();
    assert!(owner
        .collection_version("AgentScratch")
        .await
        .unwrap()
        .is_none());
    for permission in &permissions {
        owner
            .revoke_node_permission(&actor_did, *permission)
            .await
            .unwrap();
    }
    assert!(access
        .truncate_collection("AgentScratchKept")
        .await
        .is_err());
    assert_eq!(
        owner
            .execute("{ AgentScratchKept { name } }")
            .await
            .unwrap()["data"]["AgentScratchKept"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(access
        .add_schema_view(
            "AgentViewInput { name }",
            "type AgentViewRevoked { name: String }"
        )
        .await
        .is_err());
    assert!(access
        .materialize_schema_collection("AgentViewInput")
        .await
        .is_err());
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn additive_recovery_preserves_documents_and_rejects_stale_preview() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let access = ConfigAccess::local(node.clone());
    let installed = apply(
        &tool,
        json!({"argv":["collection","create"],"options":{"sdl":"type WorkItem { title: String }"}}),
    )
    .await;
    assert_eq!(
        installed,
        json!({"committed":true,"result":{"collections":["WorkItem"]}})
    );
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
    let surface = AgentToolSurfaceConfig::from_selection(
        "setup",
        selection,
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &Default::default(),
    )
    .await
    .unwrap();
    assert!(!surface.tool_names().contains(&SCHEMA_TOOL_NAME.to_owned()));
    let error=tool.call(args(json!({"argv":["collection","preview","update"],"target_id":"Tools","options":{"patch":[{"op":"add","path":"/Tools/Fields/-","value":{"Name":"bad","Kind":"String"}}]}}))).await.unwrap_err();
    assert!(error.to_string().contains("Gents-managed"));
}

#[tokio::test]
async fn inline_lens_migration_transforms_existing_rows_through_native_owner() {
    let wasm = crate::store::schema::migration::fixture_lens_wasm();
    assert!(
        wasm.len() > 16,
        "schema migration coverage requires the fixture lens build"
    );
    let node = node().await;
    let tool = runtime_owned_tool(&node);
    let access = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
    );
    apply(&tool,json!({"argv":["collection","create"],"options":{"sdl":"type FixtureDoc { name: String }"}})).await;
    access
        .write(
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
    assert!(patch["committed"].as_bool().unwrap_or(false), "{patch}");
    // The patch publishes an inactive version; collection_version names the
    // active one, so the destination is discovered through version list.
    let versions = call(
        &tool,
        json!({"argv":["version","list"],"target_id":"FixtureDoc"}),
    )
    .await;
    let dest = versions["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|version| version["IsActive"] == false && version["VersionID"] != source.as_str())
        .unwrap_or_else(|| panic!("patched inactive FixtureDoc version: {versions}"))["VersionID"]
        .as_str()
        .unwrap()
        .to_owned();
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
    let access = ConfigAccess::local(node);
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
    let before = ConfigAccess::local(node.clone())
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
        ConfigAccess::local(node)
            .collection_versions()
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn config_shaped_schema_calls_preview_then_apply_without_rewriting_the_receipt() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let access = ConfigAccess::local(node);
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
    assert!(ConfigAccess::local(node.clone())
        .collection_version("RecoveryShape")
        .await
        .unwrap()
        .is_none());
    call(&tool, preview["next_call"]["args"].clone()).await;
    assert!(ConfigAccess::local(node)
        .collection_version("RecoveryShape")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn schema_argument_recovery_preserves_intent_without_publishing() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let sdl = "type SyntaxProbe { title: String }";
    for request in [
        json!({"argv":["collection","create","SyntaxProbe"],"options":{"schema":sdl}}),
        json!({"argv":["collection","create"],"target_id":"SyntaxProbe","options":{"sdl":sdl,"digest":"old"}}),
    ] {
        let error = tool.call(args(request)).await.unwrap_err();
        let error: Value = serde_json::from_str(&error.to_string()).unwrap();
        let next = &error["recovery"]["args"];
        assert_eq!(next["argv"], json!(["collection", "preview", "create"]));
        assert!(next.get("target_id").is_none());
        assert_eq!(next["options"]["sdl"], sdl);
        assert!(next["options"].get("digest").is_none());
        assert!(next["options"].get("schema").is_none());
        call(&tool, next.clone()).await;
        assert!(ConfigAccess::local(node.clone())
            .collection_version("SyntaxProbe")
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn schema_help_with_command_context_does_not_mutate() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let help = tool.call(args(json!({"argv":["collection","update","--help"],"target_id":"Absent","options":{"patch":[{"op":"remove","path":"/Absent/Fields/0"}]}}))).await.unwrap();
    assert!(help.starts_with("collection preview update"));
    let error = tool
        .call(args(
            json!({"argv":["help","indexes","vector","imaginary"]}),
        ))
        .await
        .unwrap_err();
    let error: Value = serde_json::from_str(&error.to_string()).unwrap();
    assert_eq!(
        error["recovery"]["args"]["argv"],
        json!(["help", "indexes", "vector"])
    );
    tool.call(args(error["recovery"]["args"].clone()))
        .await
        .unwrap();
    assert!(ConfigAccess::local(node)
        .collection_version("Absent")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn invalid_sdl_points_to_its_concept_without_rewriting_it() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    for (sdl, expected) in [
        (
            "type BadScalar { active: Bool }",
            vec!["help", "fields", "types"],
        ),
        (
            "type BadText { body: String } @fulltext(fields: [body]) type BadText",
            vec!["help", "indexes", "fulltext"],
        ),
    ] {
        let error = tool
            .call(args(
                json!({"argv":["collection","preview","create"],"options":{"sdl":sdl}}),
            ))
            .await
            .unwrap_err();
        let error: Value = serde_json::from_str(&error.to_string()).unwrap();
        assert_eq!(error["recovery"]["args"]["argv"], json!(expected));
        let page = tool
            .call(args(error["recovery"]["args"].clone()))
            .await
            .unwrap();
        assert!(page.contains("```graphql"));
    }
    for name in ["BadScalar", "BadText"] {
        assert!(ConfigAccess::local(node.clone())
            .collection_version(name)
            .await
            .unwrap()
            .is_none());
    }
}

async fn refusal(tool: &SchemaTool, value: Value) -> Value {
    serde_json::from_str(&tool.call(args(value)).await.unwrap_err().0).unwrap()
}

async fn scratch_rows(node: &Arc<EmbeddedNode>) -> usize {
    ConfigAccess::local(node.clone())
        .execute("{ Scratch { title } }")
        .await
        .unwrap()["data"]["Scratch"]
        .as_array()
        .unwrap()
        .len()
}

async fn seed_scratch(node: &Arc<EmbeddedNode>, title: &str) {
    ConfigAccess::write_local(
        node,
        "schema_test.seed",
        &format!(r#"mutation {{ create_Scratch(input:{{title:"{title}"}}) {{ _docID }} }}"#),
    )
    .await
    .unwrap();
}

const SCRATCH_SDL: &str = "type Scratch { title: String @index }";

#[tokio::test]
async fn collection_cleanup_empties_removes_and_frees_the_name() {
    let node = node().await;
    let tool = SchemaTool::new(node.clone());
    let access = ConfigAccess::local(node.clone());
    apply(
        &tool,
        json!({"argv":["collection","create"],"options":{"sdl":SCRATCH_SDL}}),
    )
    .await;
    seed_scratch(&node, "one").await;
    seed_scratch(&node, "two").await;

    let full = refusal(
        &tool,
        json!({"argv":["collection","preview","remove","Scratch"]}),
    )
    .await;
    assert!(full["error"].as_str().unwrap().contains("2 document(s)"));
    assert_eq!(
        full["recovery"]["args"]["argv"],
        json!(["collection", "preview", "empty", "Scratch"])
    );

    let preview = call(&tool, full["recovery"]["args"].clone()).await;
    assert_eq!(preview["loss"]["documents"], 2);
    assert_eq!(preview["loss"]["indexes"].as_array().unwrap().len(), 1);
    assert_eq!(preview["loss"]["views_reading_it"], json!([]));
    assert_eq!(scratch_rows(&node).await, 2);

    seed_scratch(&node, "three").await;
    assert!(tool
        .call(args(preview["next_call"]["args"].clone()))
        .await
        .unwrap_err()
        .to_string()
        .contains("stale digest"));
    assert_eq!(scratch_rows(&node).await, 3);

    let emptied = apply(&tool, json!({"argv":["collection","empty","Scratch"]})).await;
    assert_eq!(
        emptied,
        json!({"committed":true,"result":{"collection":"Scratch","documents_removed":3}})
    );
    assert_eq!(scratch_rows(&node).await, 0);
    assert!(access
        .collection_version("Scratch")
        .await
        .unwrap()
        .is_some());

    let removal = call(
        &tool,
        json!({"argv":["collection","preview","remove","Scratch"]}),
    )
    .await;
    assert_eq!(removal["loss"]["documents"], 0);
    assert_eq!(removal["loss"]["versions"], 1);
    let removed = call(&tool, removal["next_call"]["args"].clone()).await;
    assert_eq!(
        removed,
        json!({"committed":true,"result":{"collection":"Scratch","removed":true}})
    );
    assert!(access
        .collection_version("Scratch")
        .await
        .unwrap()
        .is_none());
    assert!(!access.collection_names().await.unwrap().contains("Scratch"));
    assert!(access
        .collection_versions()
        .await
        .unwrap()
        .iter()
        .all(|version| version["Name"] != "Scratch"));

    apply(
        &tool,
        json!({"argv":["collection","create"],"options":{"sdl":SCRATCH_SDL}}),
    )
    .await;
    assert_eq!(scratch_rows(&node).await, 0);
    seed_scratch(&node, "one").await;
    let rows = access
        .execute(r#"{ Scratch(filter: {title: {_eq: "one"}}) { title } }"#)
        .await
        .unwrap();
    assert_eq!(rows["data"]["Scratch"].as_array().unwrap().len(), 1);
    assert_eq!(scratch_rows(&node).await, 1);
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn collection_cleanup_refuses_managed_and_referenced_collections() {
    use crate::pack::{HomePackInstall, PackKind, PackUxPlugin, UxContributions, UxDataSurface};

    let node = node().await;
    let node_did = node.node_identity_did().unwrap().to_owned();
    let home = tempfile::tempdir().unwrap();
    let tool = SchemaTool {
        access: ConfigAccess::local(node.clone()),
        node_did: node_did.clone(),
        home: Some(home.path().to_owned()),
        caller: None,
    };
    let access = ConfigAccess::local(node.clone());
    apply(
        &tool,
        json!({"argv":["collection","create"],"options":{"sdl":SCRATCH_SDL}}),
    )
    .await;
    seed_scratch(&node, "kept").await;

    for verb in ["empty", "remove"] {
        for managed in ["AgentSession", "DatastoreToolSurface", "PackInstallation"] {
            let refused =
                refusal(&tool, json!({"argv":["collection","preview",verb,managed]})).await;
            assert!(
                refused["error"].as_str().unwrap().contains("Gents-managed"),
                "{verb} {managed}: {refused}"
            );
        }
    }

    let users = |refused: &Value| -> Vec<String> {
        refused["recovery"]["release_first"]
            .as_array()
            .unwrap_or_else(|| panic!("{refused}"))
            .iter()
            .map(|user| {
                assert!(!user["release_with"].as_str().unwrap().is_empty());
                user["user"].as_str().unwrap().to_owned()
            })
            .collect()
    };
    let write = |mutation: String| {
        let node = node.clone();
        async move {
            let response = ConfigAccess::write_local(&node, "schema_test.reference", &mutation)
                .await
                .unwrap();
            let data = response.get("data").unwrap_or(&response);
            let (_, rows) = data.as_object().unwrap().iter().next().unwrap();
            rows[0]["_docID"].as_str().unwrap().to_owned()
        }
    };
    let delete = |collection: &'static str, id: String| {
        let node = node.clone();
        async move {
            ConfigAccess::write_local(
                &node,
                "schema_test.release",
                &format!(r#"mutation {{ delete_{collection}(docID: "{id}") {{ _docID }} }}"#),
            )
            .await
            .unwrap();
        }
    };
    let did = crate::store::graphql::escape_graphql_string(&node_did);
    let entries = gents_protocol::graphql::graphql_input_literal(&json!({"entries": [
        {"kind":"query","tool_name":"find_scratch","collection":"Scratch",
         "description":"Find scratch rows","fields":["title"]}
    ]}))
    .unwrap();
    let references = [
        (
            "DatastoreToolSurface",
            format!(
                r#"mutation {{ create_DatastoreToolSurface(input: {{node_did:"{did}",surface_id:"scratch-tools",enabled:true,entries:{entries}}}) {{_docID}} }}"#
            ),
            "datastore surface \"scratch-tools\"",
        ),
        (
            "EventSource",
            format!(
                r#"mutation {{ create_EventSource(input: {{node_did:"{did}",event_source_id:"scratch-events",source_collection:"Scratch",event_kind:"created"}}) {{_docID}} }}"#
            ),
            "event source \"scratch-events\"",
        ),
        (
            "PeerPairingDesired",
            format!(
                r#"mutation {{ create_PeerPairingDesired(input: {{peer_id:"peer-a",node_did:"{did}",actor_did:"{did}",source:"manual",template:"client",replicator_addresses:["addr"],collections:["Scratch"]}}) {{_docID}} }}"#
            ),
            "P2P pairing with peer \"peer-a\"",
        ),
    ];
    for (collection, mutation, expected) in references {
        let id = write(mutation).await;
        let refused = refusal(
            &tool,
            json!({"argv":["collection","preview","remove","Scratch"]}),
        )
        .await;
        assert!(refused["error"].as_str().unwrap().contains("still in use"));
        let users = users(&refused);
        assert_eq!(users.len(), 1, "{refused}");
        assert!(users[0].contains(expected), "{refused}");
        assert_eq!(
            refused["recovery"]["then"]["args"]["argv"],
            json!(["collection", "preview", "remove"])
        );
        let preview = call(
            &tool,
            json!({"argv":["collection","preview","empty","Scratch"]}),
        )
        .await;
        assert_eq!(preview["loss"]["documents"], 1);
        let listed = preview["loss"]["still_used_by"].as_array().unwrap();
        assert_eq!(listed.len(), 1, "{preview}");
        assert!(listed[0]["user"].as_str().unwrap().contains(expected));
        delete(collection, id).await;
    }

    let surface = write(format!(
        r#"mutation {{ create_DatastoreToolSurface(input: {{node_did:"{did}",surface_id:"scratch-board",enabled:true,entries:{entries}}}) {{_docID}} }}"#
    ))
    .await;
    let pack_home = home.path().to_owned();
    tokio::task::spawn_blocking(move || {
        let ux = vec![PackUxPlugin {
            name: "board".into(),
            description: "the board".into(),
            entry: Some("ux/board/plugin.js".into()),
            css: None,
            plugin: None,
            contributes: UxContributions {
                areas: vec!["plugin.pages".into()],
                directives: vec![],
            },
            default_enabled: None,
            data: vec![UxDataSurface {
                surface: "scratch-board".into(),
                tools: vec!["find_scratch".into()],
            }],
        }];
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("ux/board")).unwrap();
        std::fs::create_dir_all(source.path().join("schemas")).unwrap();
        std::fs::write(source.path().join("README.md"), "# Board").unwrap();
        std::fs::write(source.path().join("ux/board/plugin.js"), "export default {}").unwrap();
        std::fs::write(source.path().join("schemas/scratch.graphql"), SCRATCH_SDL).unwrap();
        std::fs::write(source.path().join("config.json"), "{}").unwrap();
        let manifest = json!({"manifest_version":1,"name":"board","namespace":"acme",
            "version":"1.0.0","description":"cleanup fixture","kind":"documents",
            "authors":["Fixture"],"assets":["README.md","ux/board/plugin.js","schemas/scratch.graphql","config.json"],
            "config":"config.json","schemas":["schemas/scratch.graphql"],"ux":&ux});
        std::fs::write(
            source.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let (bytes, header) = crate::pack::archive::pack_dir(source.path()).unwrap();
        crate::pack::store::PackStore::new(&pack_home)
            .import(bytes.as_slice(), Some(&header.digest))
            .unwrap();
        crate::pack::write_home_install(
            &pack_home,
            &HomePackInstall {
                coordinate: "acme/board".into(),
                version: "1.0.0".into(),
                digest: header.digest,
                kind: PackKind::Documents,
                assets: "packs/.materialized/acme/board/fixture".into(),
                plugins: vec![],
                dependency_assets: Vec::new(),
                installed_via: None,
                installed_at: "2026-01-01T00:00:00Z".into(),
                ux,
                source: None,
                source_history: Vec::new(),
                created_collections: Vec::new(),
            },
        )
        .unwrap();
    })
    .await
    .unwrap();
    let refused = refusal(
        &tool,
        json!({"argv":["collection","preview","remove","Scratch"]}),
    )
    .await;
    let found = users(&refused);
    assert_eq!(found.len(), 3, "{refused}");
    assert!(
        found[0].contains("page acme/board/board data grant"),
        "{refused}"
    );
    assert!(
        found[1].contains("installed pack acme/board declares it"),
        "{refused}"
    );
    assert!(found[2].contains("datastore surface"), "{refused}");

    let preview = call(
        &tool,
        json!({"argv":["collection","preview","empty","Scratch"]}),
    )
    .await;
    assert_eq!(
        preview["loss"]["still_used_by"].as_array().unwrap().len(),
        3
    );
    assert_eq!(scratch_rows(&node).await, 1);
    let emptied = call(&tool, preview["next_call"]["args"].clone()).await;
    assert_eq!(emptied["result"]["documents_removed"], 1);
    assert_eq!(scratch_rows(&node).await, 0);
    let refused = refusal(
        &tool,
        json!({"argv":["collection","preview","remove","Scratch"]}),
    )
    .await;
    assert_eq!(users(&refused).len(), 3, "{refused}");
    seed_scratch(&node, "again").await;

    let pack = |created: &[&str]| crate::collection_use::PackCollections {
        coordinate: "acme/board".into(),
        declared: ["Scratch", "AgentSession", "Absent"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        created: created.iter().map(|name| (*name).to_owned()).collect(),
    };
    let created = pack(&["Scratch", "AgentSession", "Absent"]);
    let kept = crate::collection_use::remove_unused_collections(
        &access,
        Some(home.path()),
        &node_did,
        &created,
    )
    .await
    .unwrap();
    assert_eq!(kept.len(), 1);
    assert!(!kept[0].removed);
    assert_eq!(kept[0].kept_for.len(), 1, "{kept:?}");
    assert!(kept[0].kept_for[0].user.contains("datastore surface"));
    assert_eq!(scratch_rows(&node).await, 1);

    delete("DatastoreToolSurface", surface).await;
    crate::pack::forget_home_install(home.path(), "acme/board").unwrap();

    // A collection the pack found on the node is not the pack's to remove.
    let adopted = crate::collection_use::remove_unused_collections(
        &access,
        Some(home.path()),
        &node_did,
        &pack(&[]),
    )
    .await
    .unwrap();
    assert!(!adopted[0].removed);
    assert!(
        adopted[0].kept_for[0].user.contains("did not create it"),
        "{adopted:?}"
    );
    assert_eq!(scratch_rows(&node).await, 1);

    // A collection that stays and relates to it refuses the removal before
    // anything is emptied.
    access
        .add_collection("type ScratchHolder { note: String scratch: Scratch }")
        .await
        .unwrap();
    let plan = crate::collection_use::collection_cleanup_plan(
        &access,
        Some(home.path()),
        &node_did,
        &created,
    )
    .await
    .unwrap();
    assert_eq!(plan.len(), 1);
    assert!(!plan[0].removable, "{plan:?}");
    assert_eq!(plan[0].documents, 1);
    assert!(
        plan[0].kept_for[0].user.contains("ScratchHolder"),
        "{plan:?}"
    );
    let held = crate::collection_use::remove_unused_collections(
        &access,
        Some(home.path()),
        &node_did,
        &created,
    )
    .await
    .unwrap();
    assert!(!held[0].removed);
    assert_eq!(
        scratch_rows(&node).await,
        1,
        "a refused removal keeps the data"
    );
    access
        .remove_collections(&["ScratchHolder".to_owned()])
        .await
        .unwrap();

    let cleaned = crate::collection_use::remove_unused_collections(
        &access,
        Some(home.path()),
        &node_did,
        &created,
    )
    .await
    .unwrap();
    assert_eq!(cleaned.len(), 1);
    assert!(cleaned[0].removed);
    assert_eq!(cleaned[0].documents_removed, Some(1));
    assert!(access
        .collection_version("Scratch")
        .await
        .unwrap()
        .is_none());
    assert!(access
        .collection_version("AgentSession")
        .await
        .unwrap()
        .is_some());
    node.shutdown().await.unwrap();
}
