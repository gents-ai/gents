use gents_protocol::error_chain::ErrorChain;
use std::path::PathBuf;
use std::sync::Arc;

use defra_node::EmbeddedNode;
use serde_json::{json, Value};

use super::*;
use crate::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::pack::{
    write_home_install, HomePackInstall, PackKind, PackUxPlugin, UxContributions, UxDataSurface,
};
use crate::Collection;

const NODE: &str = "did:key:z";
const OTHER_NODE: &str = "did:key:other";
const BOARD: &str = "acme/board/board";

struct Fixture {
    home: tempfile::TempDir,
    node: Arc<EmbeddedNode>,
    access: ConfigAccess,
}

impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let identity =
            crate::identity::KeyIdentity::load_or_create(home.path().join("node.key"), None)
                .unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(crate::identity::NodeIdentity::did(&identity))
                .build()
                .await
                .unwrap(),
        );
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        node.add_schema("type BoardTask { title: String lane: String }")
            .await
            .unwrap();
        let access = ConfigAccess::local(node.clone());
        install_board(
            home.path(),
            vec![
                declared("board", &["list_tasks", "add_task"]),
                declared("archive", &["list_tasks"]),
                declared("foreign", &["list_tasks"]),
                declared("filled", &["add_note"]),
                declared("nowhere", &["list_tasks"]),
            ],
        );
        let dev = home
            .path()
            .join(crate::home::UX_PLUGINS_DIR_NAME)
            .join("loose");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join(crate::ux_plugin::DEV_ENTRY), "export default {}").unwrap();
        let fixture = Self { home, node, access };
        fixture
            .write_surface(board_surface(NODE, "board", true))
            .await;
        fixture
            .write_surface(board_surface(NODE, "archive", false))
            .await;
        fixture
            .write_surface(board_surface(OTHER_NODE, "foreign", true))
            .await;
        fixture
            .write_surface(json!({
                "surface_id": "filled",
                "node_did": NODE,
                "entries": {"entries": [{
                    "tool_name": "add_note",
                    "collection": "BoardTask",
                    "description": "Add one note under the current run.",
                    "fields": [
                        {"name": "title", "required": true},
                        {"name": "lane", "required": false, "fill": "correlation"},
                    ],
                }]},
            }))
            .await;
        fixture
    }

    async fn write_surface(&self, surface: Value) {
        let document: DatastoreToolSurfaceDocument = serde_json::from_value(surface).unwrap();
        let value = serde_json::to_value(document).unwrap();
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::DatastoreToolSurface,
            add: value.clone(),
            update: value,
        }])
        .unwrap();
        self.access
            .transact_write("test.ux_data.surface", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
    }

    async fn call(&self, surface: &str, tool: &str, args: Value) -> Result<Value> {
        self.call_as(BOARD, NODE, surface, tool, args).await
    }

    async fn call_as(
        &self,
        ux: &str,
        node_did: &str,
        surface: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value> {
        call(
            self.home.path(),
            &self.access,
            UxDataCall {
                ux: ux.into(),
                node_did: node_did.into(),
                surface: surface.into(),
                tool: tool.into(),
                args,
            },
        )
        .await
    }

    async fn refusal(&self, surface: &str, tool: &str) -> String {
        format!(
            "{}",
            ErrorChain(
                self.call(surface, tool, json!({}))
                    .await
                    .unwrap_err()
                    .as_ref()
            )
        )
    }
}

fn declared(surface: &str, tools: &[&str]) -> UxDataSurface {
    UxDataSurface {
        surface: surface.into(),
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
    }
}

fn install_board(home: &std::path::Path, data: Vec<UxDataSurface>) {
    let assets = PathBuf::from("packs/.materialized/acme/board/deadbeef");
    std::fs::create_dir_all(home.join(&assets).join("ux/board")).unwrap();
    std::fs::write(
        home.join(&assets).join("ux/board/plugin.js"),
        "export default {}",
    )
    .unwrap();
    write_home_install(
        home,
        &HomePackInstall {
            coordinate: "acme/board".into(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
            kind: PackKind::Plugins,
            assets: assets.to_string_lossy().into_owned(),
            plugins: vec![],
            dependency_assets: Vec::new(),
            installed_via: None,
            installed_at: "2026-01-01T00:00:00Z".into(),
            ux: vec![PackUxPlugin {
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
                data,
            }],
        },
    )
    .unwrap();
}

fn board_surface(node_did: &str, surface_id: &str, enabled: bool) -> Value {
    json!({
        "surface_id": surface_id,
        "node_did": node_did,
        "enabled": enabled,
        "entries": {"entries": [
            {
                "kind": "query",
                "tool_name": "list_tasks",
                "collection": "BoardTask",
                "description": "Every task on the board.",
                "fields": ["title", "lane"],
            },
            {
                "tool_name": "add_task",
                "collection": "BoardTask",
                "description": "Add one task to the board.",
                "fields": [
                    {"name": "title", "required": true},
                    {"name": "lane", "required": true},
                ],
            },
        ]},
    })
}

#[tokio::test]
async fn a_page_creates_through_the_declared_create_tool_and_reads_it_back() {
    let fixture = Fixture::new().await;
    let created = fixture
        .call(
            "board",
            "add_task",
            json!({"title": "Write the tests", "lane": "doing"}),
        )
        .await
        .unwrap();
    assert_eq!(created["kind"], "create");
    assert_eq!(created["output"]["collection"], "BoardTask");
    let doc_id = created["output"]["document_id"].as_str().unwrap();
    assert!(!doc_id.is_empty());
    assert_eq!(created["output"]["document"]["title"], "Write the tests");
    assert_eq!(created["output"]["document"]["lane"], "doing");
    assert_eq!(created["output"]["document"]["_docID"], doc_id);

    let listed = fixture
        .call("board", "list_tasks", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["kind"], "query");
    assert_eq!(listed["output"]["collection"], "BoardTask");
    assert_eq!(listed["output"]["count"], 1);
    assert_eq!(listed["output"]["returned_count"], 1);
    assert_eq!(listed["output"]["truncated"], false);
    assert_eq!(
        listed["output"]["results"],
        json!([{"title": "Write the tests", "lane": "doing"}])
    );

    let absent = fixture
        .call("board", "list_tasks", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        absent["output"]["count"], 1,
        "absent args mean no arguments"
    );
    let projected = fixture
        .call("board", "list_tasks", json!({"fields": ["title"]}))
        .await
        .unwrap();
    assert_eq!(
        projected["output"]["results"],
        json!([{"title": "Write the tests"}])
    );
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn the_tools_own_refusals_reach_the_page() {
    let fixture = Fixture::new().await;
    let error = fixture
        .call("board", "add_task", json!({"title": "no lane"}))
        .await
        .unwrap_err();
    assert!(
        ErrorChain(error.as_ref()).to_string().contains("lane"),
        "the bounded write tool's own refusal: {}",
        ErrorChain(error.as_ref())
    );
    let error = fixture
        .call("board", "list_tasks", json!({"fields": ["_docID"]}))
        .await
        .unwrap_err();
    assert!(
        ErrorChain(error.as_ref()).to_string().contains("allowlist"),
        "{}",
        ErrorChain(error.as_ref())
    );
    let error = fixture
        .call("board", "list_tasks", json!(["title"]))
        .await
        .unwrap_err();
    assert!(
        ErrorChain(error.as_ref())
            .to_string()
            .contains("args must be a JSON object"),
        "{}",
        ErrorChain(error.as_ref())
    );
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn an_undeclared_tool_or_surface_is_refused_before_the_node_is_asked() {
    let fixture = Fixture::new().await;
    let error = fixture.refusal("board", "delete_task").await;
    assert!(error.contains("not declared"), "{error}");
    assert!(
        error.contains("may not call tool \"delete_task\" of surface \"board\""),
        "{error}"
    );
    let error = fixture.refusal("notes", "list_tasks").await;
    assert!(error.contains("not declared"), "{error}");
    assert!(error.contains("surface \"notes\""), "{error}");
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn a_disabled_missing_or_foreign_surface_is_refused() {
    let fixture = Fixture::new().await;
    let error = fixture.refusal("archive", "list_tasks").await;
    assert_eq!(error, "surface \"archive\" is disabled");
    let error = fixture.refusal("nowhere", "list_tasks").await;
    assert_eq!(
        error,
        format!("surface \"nowhere\" is not installed on node {NODE}")
    );
    let error = fixture.refusal("foreign", "list_tasks").await;
    assert_eq!(
        error,
        format!("surface \"foreign\" is not installed on node {NODE}")
    );
    let error = format!(
        "{}",
        ErrorChain(
            fixture
                .call_as(BOARD, OTHER_NODE, "board", "list_tasks", json!({}))
                .await
                .unwrap_err()
                .as_ref()
        )
    );
    assert_eq!(
        error,
        format!("surface \"board\" is not installed on node {OTHER_NODE}")
    );
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn a_surface_without_the_tool_is_refused() {
    let fixture = Fixture::new().await;
    install_board(
        fixture.home.path(),
        vec![declared(
            "board",
            &["list_tasks", "add_task", "rename_task"],
        )],
    );
    let error = fixture.refusal("board", "rename_task").await;
    assert_eq!(error, "surface \"board\" has no tool \"rename_task\"");
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn a_dev_door_listing_declares_nothing_and_is_refused() {
    let fixture = Fixture::new().await;
    let error = format!(
        "{}",
        ErrorChain(
            fixture
                .call_as("loose", NODE, "board", "list_tasks", json!({}))
                .await
                .unwrap_err()
                .as_ref()
        )
    );
    assert_eq!(error, "ux plugin \"loose\" declares no data surfaces");
    let error = format!(
        "{}",
        ErrorChain(
            surfaces(fixture.home.path(), &fixture.access, "loose", NODE)
                .await
                .unwrap_err()
                .as_ref()
        )
    );
    assert_eq!(error, "ux plugin \"loose\" declares no data surfaces");
    let error = format!(
        "{}",
        ErrorChain(
            fixture
                .call_as("acme/nope/board", NODE, "board", "list_tasks", json!({}))
                .await
                .unwrap_err()
                .as_ref()
        )
    );
    assert!(
        error.contains("no ux plugin \"acme/nope/board\""),
        "{error}"
    );
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn a_tool_with_a_runtime_filled_field_is_not_callable_from_a_page() {
    let fixture = Fixture::new().await;
    let error = fixture.refusal("filled", "add_note").await;
    assert_eq!(
        error,
        "tool \"add_note\" has runtime-filled fields and is not callable from a page"
    );
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn surfaces_lists_each_declared_surface_with_its_callable_tools() {
    let fixture = Fixture::new().await;
    let listed = surfaces(fixture.home.path(), &fixture.access, BOARD, NODE)
        .await
        .unwrap();
    let listed = listed["surfaces"].as_array().unwrap();
    assert_eq!(listed.len(), 5);

    let board = &listed[0];
    assert_eq!(board["surface"], "board");
    assert_eq!(board["enabled"], true);
    assert!(board.get("missing").is_none());
    let tools = board["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| (
                tool["tool_name"].as_str().unwrap(),
                tool["kind"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![("list_tasks", "query"), ("add_task", "create")]
    );
    assert_eq!(tools[0]["fields"], json!(["title", "lane"]));
    assert_eq!(tools[0]["collection"], "BoardTask");
    assert_eq!(
        tools[1]["fields"],
        json!([{"name": "title", "required": true}, {"name": "lane", "required": true}])
    );

    let archive = &listed[1];
    assert_eq!(archive["surface"], "archive");
    assert_eq!(archive["enabled"], false);
    assert_eq!(
        archive["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["tool_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["list_tasks"],
        "only the tools the manifest names are listed"
    );

    let foreign = &listed[2];
    assert_eq!(foreign["surface"], "foreign");
    assert_eq!(foreign["missing"], true);
    assert_eq!(foreign["enabled"], false);
    assert_eq!(foreign["tools"], json!([]));

    assert_eq!(listed[3]["surface"], "filled");
    assert_eq!(listed[3]["tools"][0]["kind"], "create");
    assert_eq!(
        listed[3]["tools"][0]["fields"][1],
        json!({"name": "lane", "required": false, "fill": "correlation"}),
        "the page sees the fill and can expect the refusal"
    );

    assert_eq!(
        listed[4],
        json!({"surface": "nowhere", "enabled": false, "missing": true, "tools": []})
    );
    fixture.node.shutdown().await;
}
