use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_loop::tool::Tool as _;
use gents_protocol::tool_names::SCHEMA_TOOL_NAME;
use gents_store::config_client::ConfigAccess;
use gents_tools::builtin::schema_help::*;
use gents_tools::surface::{AgentToolSurfaceConfig, ResolvedToolSelection, ToolCeiling};
use serde_json::{json, Value};

fn args(value: Value) -> SchemaParams {
    serde_json::from_value(value).unwrap()
}

async fn call(tool: &SchemaTool, value: Value) -> Value {
    serde_json::from_str(&tool.call(args(value)).await.unwrap()).unwrap()
}
async fn node() -> Arc<EmbeddedNode> {
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    node
}

#[tokio::test]
async fn schema_preview_and_publication_expose_ignored_native_directives() {
    use gents_store::config_client::NodePermission;

    let (db, _user, _user_access, binding) =
        crate::test_support::root::signed_test_node("schema-native-warnings").await;
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
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
