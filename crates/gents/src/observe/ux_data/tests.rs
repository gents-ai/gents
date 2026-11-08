use std::path::PathBuf;
use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use serde_json::{json, Value};

use super::*;
use crate::documents::apply_desired_state_plan;
use crate::pack::{
    write_home_install, HomePackInstall, PackKind, PackUxPlugin, UxContributions, UxDataSurface,
};
use crate::store::config_client::{
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan, GraphqlEndpoint, NodePermission,
};
use crate::Collection;

const NODE: &str = "did:key:z";
const BOARD: &str = "acme/board/board";

#[test]
fn operator_ux_surface_checks_follow_model() {
    let model = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    assert_eq!(model.configuration_ux_datastore_cases.len(), 5);
    let initial: DatastoreToolSurfaceDocument =
        serde_json::from_value(board_surface(NODE, "board", true)).unwrap();
    let expected = initial
        .entries
        .as_ref()
        .unwrap()
        .iter()
        .find(|entry| entry.tool_name() == "list_tasks")
        .unwrap()
        .clone();
    for case in &model.configuration_ux_datastore_cases {
        let home = tempfile::tempdir().unwrap();
        let tools = if case["declared"] == true {
            vec!["list_tasks"]
        } else {
            vec!["add_task"]
        };
        install_board(home.path(), vec![declared("board", &tools)]);
        let listing = listing_with_data(home.path(), BOARD).unwrap();
        let allowed = manifest_allows_tool(&listing, "board", "list_tasks");
        let mut surface = initial.clone();
        surface.enabled = case["enabled"].as_bool().unwrap();
        if case["unchanged"] == false {
            surface
                .entries
                .as_mut()
                .unwrap()
                .retain(|entry| entry.tool_name() != "list_tasks");
        }
        let visible = case["visible"].as_bool().unwrap().then_some(&surface);
        let usable =
            allowed && crate::documents::confirm_visible_datastore_tool(visible, &expected).is_ok();
        assert_eq!(usable, case["expected"].as_bool().unwrap(), "{case}");
    }
}

struct Fixture {
    home: tempfile::TempDir,
    node: Arc<EmbeddedNode>,
    access: ConfigAccess,
    client_access: ConfigAccess,
    client_did: identity::Did,
    node_did: String,
    other_node_did: String,
    _identity: crate::store::identity::KeyIdentity,
}

impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let identity =
            crate::store::identity::KeyIdentity::load_or_create(home.path().join("node.key"), None)
                .unwrap();
        let other = crate::store::identity::KeyIdentity::load_or_create(
            home.path().join("other.key"),
            None,
        )
        .unwrap();
        let client = crate::store::identity::KeyIdentity::load_or_create(
            home.path().join("client.key"),
            None,
        )
        .unwrap();
        use crate::store::identity::NodeIdentity as _;
        let node_did = identity.did().to_owned();
        let other_node_did = other.did().to_owned();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(crate::store::identity::NodeIdentity::did(&identity))
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
                .build()
                .await
                .unwrap(),
        );
        crate::store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        node.add_collection("type BoardTask { title: String lane: String }")
            .await
            .unwrap();
        let access = ConfigAccess::local_as(node.clone(), identity::Did::new(&node_did).unwrap());
        let client_did = identity::Did::new(client.did()).unwrap();
        access
            .grant_node_permission(&client_did, NodePermission::DocumentRead)
            .await
            .unwrap();
        access
            .grant_node_permission(&client_did, NodePermission::DocumentUpdate)
            .await
            .unwrap();
        access
            .grant_node_permission(&client_did, NodePermission::CollectionGet)
            .await
            .unwrap();
        let client_access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), client.did())
                .with_client_signing_capability(client.client_signing_capability().unwrap())
                .unwrap(),
        );
        let nodes = [&node_did, &other_node_did]
            .into_iter()
            .map(|did| {
                let value = json!({"node_did":did, "enabled":true});
                DesiredStateApplyDocument {
                    collection: Collection::Node,
                    add: value.clone(),
                    update: value,
                }
            })
            .collect();
        let plan = DesiredStateApplyPlan::new(nodes).unwrap();
        access
            .transact_write("test.ux_data.nodes", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
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
            .join(crate::store::local::home::UX_PLUGINS_DIR_NAME)
            .join("loose");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join(crate::plugin::ux::DEV_ENTRY), "export default {}").unwrap();
        let fixture = Self {
            home,
            node,
            access,
            client_access,
            client_did,
            node_did,
            other_node_did,
            _identity: identity,
        };
        fixture
            .write_surface(board_surface(&fixture.node_did, "board", true))
            .await;
        let row = fixture
            .access
            .execute("{ DatastoreToolSurface(filter: {surface_id: {_eq: \"board\"}}) { _docID } }")
            .await
            .unwrap();
        let board_doc = row["data"]["DatastoreToolSurface"][0]["_docID"]
            .as_str()
            .unwrap();
        fixture
            .access
            .add_actor_relationship(
                "DatastoreToolSurface",
                board_doc,
                "reader",
                &fixture.client_did,
            )
            .await
            .unwrap();
        fixture
            .write_surface(board_surface(&fixture.node_did, "archive", false))
            .await;
        fixture
            .write_surface(board_surface(&fixture.other_node_did, "foreign", true))
            .await;
        fixture
            .write_surface(json!({
                "surface_id": "filled",
                "node_did": fixture.node_did,
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
        self.call_as(BOARD, &self.node_did, surface, tool, args)
            .await
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
            "{:#}",
            self.call(surface, tool, json!({})).await.unwrap_err()
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

async fn install_verified_board_schema(home: &Path, sdl: &str) {
    let home = home.to_owned();
    let sdl = sdl.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut record = crate::pack::read_home_install(&home, "acme/board")?.context("board record missing")?;
        let source = tempfile::tempdir()?;
        std::fs::create_dir_all(source.path().join("ux/board"))?;
        std::fs::create_dir_all(source.path().join("schemas"))?;
        std::fs::write(source.path().join("README.md"), "# Board")?;
        std::fs::write(source.path().join("ux/board/plugin.js"), "export default {}")?;
        std::fs::write(source.path().join("schemas/board.graphql"), sdl)?;
        std::fs::write(source.path().join("config.json"), "{}")?;
        let manifest = json!({"manifest_version":1, "name":"board", "namespace":"acme",
            "version":&record.version, "description":"trusted UX fixture", "kind":"documents",
            "authors":["Fixture"], "assets":["README.md", "ux/board/plugin.js", "schemas/board.graphql", "config.json"],
            "config":"config.json", "schemas":["schemas/board.graphql"], "ux":&record.ux});
        std::fs::write(source.path().join("manifest.json"), serde_json::to_vec(&manifest)?)?;
        let (bytes, header) = crate::pack::archive::pack_dir(source.path())?;
        crate::pack::store::PackStore::new(&home).import(bytes.as_slice(), Some(&header.digest))?;
        record.digest = header.digest;
        record.kind = PackKind::Documents;
        crate::pack::write_home_install(&home, &record)?;
        Ok(())
    }).await.unwrap().unwrap();
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
async fn watch_emits_declared_query_snapshots_for_writes_and_deletes() {
    let fixture = Fixture::new().await;
    let did = fixture.node.node_identity_did().unwrap().to_owned();
    fixture
        .write_surface(board_surface(&did, "board", true))
        .await;
    let request = UxDataCall {
        ux: BOARD.into(),
        node_did: did.clone(),
        surface: "board".into(),
        tool: "list_tasks".into(),
        args: json!({}),
    };
    let mut watch = watch_local(fixture.home.path(), fixture.node.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(watch.initial()["output"]["count"], 0);
    fixture
        .access
        .write(
            "test.ux_watch.create",
            r#"mutation { create_BoardTask(input: {title:"first", lane:"todo"}) { _docID } }"#,
        )
        .await
        .unwrap();
    let updated = tokio::time::timeout(std::time::Duration::from_secs(5), watch.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated["output"]["count"], 1);
    fixture
        .access
        .write(
            "test.ux_watch.delete",
            r#"mutation { delete_BoardTask(filter: {title:{_eq:"first"}}) { _docID } }"#,
        )
        .await
        .unwrap();
    let deleted = tokio::time::timeout(std::time::Duration::from_secs(5), watch.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted["output"]["count"], 0);
    let reopened = watch_local(fixture.home.path(), fixture.node.clone(), request)
        .await
        .unwrap();
    assert_eq!(reopened.initial()["output"]["count"], 0);
}

#[tokio::test]
async fn watch_refuses_foreign_nodes_undeclared_and_mutating_tools() {
    let fixture = Fixture::new().await;
    let did = fixture.node.node_identity_did().unwrap().to_owned();
    fixture
        .write_surface(board_surface(&did, "board", true))
        .await;
    let request = UxDataCall {
        ux: BOARD.into(),
        node_did: did,
        surface: "board".into(),
        tool: "list_tasks".into(),
        args: json!({}),
    };
    let mut other = request.clone();
    other.node_did = fixture.other_node_did.clone();
    assert!(
        watch_local(fixture.home.path(), fixture.node.clone(), other)
            .await
            .is_err()
    );
    let mut other = request.clone();
    other.tool = "add_task".into();
    assert!(
        watch_local(fixture.home.path(), fixture.node.clone(), other)
            .await
            .is_err()
    );
    let mut other = request;
    other.tool = "undeclared".into();
    assert!(
        watch_local(fixture.home.path(), fixture.node.clone(), other)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn watch_stops_when_the_declared_surface_is_disabled() {
    let fixture = Fixture::new().await;
    let did = fixture.node.node_identity_did().unwrap().to_owned();
    fixture
        .write_surface(board_surface(&did, "board", true))
        .await;
    let request = UxDataCall {
        ux: BOARD.into(),
        node_did: did.clone(),
        surface: "board".into(),
        tool: "list_tasks".into(),
        args: json!({}),
    };
    let mut watch = watch_local(fixture.home.path(), fixture.node.clone(), request)
        .await
        .unwrap();
    fixture
        .write_surface(board_surface(&did, "board", false))
        .await;
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), watch.next())
        .await
        .unwrap()
        .unwrap_err();
    assert!(format!("{error:#}").contains("disabled"));
    fixture
        .write_surface(board_surface(&did, "board", true))
        .await;
    assert!(watch
        .next()
        .await
        .unwrap_err()
        .to_string()
        .contains("closed"));
}

#[tokio::test]
async fn watch_stops_when_the_query_projection_changes() {
    let fixture = Fixture::new().await;
    let did = fixture.node.node_identity_did().unwrap().to_owned();
    fixture
        .write_surface(board_surface(&did, "board", true))
        .await;
    let request = UxDataCall {
        ux: BOARD.into(),
        node_did: did.clone(),
        surface: "board".into(),
        tool: "list_tasks".into(),
        args: json!({}),
    };
    let mut watch = watch_local(fixture.home.path(), fixture.node.clone(), request)
        .await
        .unwrap();
    let mut surface = board_surface(&did, "board", true);
    surface["entries"]["entries"][0]["fields"] = json!(["title"]);
    fixture.write_surface(surface).await;
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), watch.next())
        .await
        .unwrap()
        .unwrap_err();
    assert!(format!("{error:#}").contains("declaration changed"));
}

#[tokio::test]
async fn a_page_creates_through_the_declared_create_tool_and_reads_it_back() {
    let fixture = Fixture::new().await;
    let model = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let embedded = model
        .configuration_ux_create_cases
        .iter()
        .find(|case| case["name"] == "embedded-held")
        .unwrap();
    let created = fixture
        .call(
            "board",
            "add_task",
            json!({"title": "Write the tests", "lane": "doing"}),
        )
        .await
        .unwrap();
    assert!(embedded["expected"].as_bool().unwrap());
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
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn cached_operator_tools_recheck_current_source_and_roll_back() {
    let fixture = Fixture::new().await;
    let surface = installed_surface(&fixture.access, &fixture.node_did, "board")
        .await
        .unwrap();
    let entries = surface.entries.as_ref().unwrap();
    let query = entries
        .iter()
        .find_map(|entry| match entry {
            SurfaceToolDecl::Query(decl) if decl.tool_name == "list_tasks" => Some(decl.clone()),
            _ => None,
        })
        .unwrap();
    let write = entries
        .iter()
        .find_map(|entry| match entry {
            SurfaceToolDecl::Create(decl) if decl.tool_name == "add_task" => Some(decl.clone()),
            _ => None,
        })
        .unwrap();
    let query = BoundedQueryTool::new(fixture.access.clone(), query)
        .declared_by(Some("board".into()))
        .with_visible_surface_owner(fixture.node_did.clone());
    let write = BoundedWriteTool::new(fixture.access.clone(), write)
        .declared_by(Some("board".into()))
        .with_visible_surface_owner(fixture.node_did.clone());
    let initial = query.call(BoundedQueryParams(Map::new())).await.unwrap();
    assert!(initial.contains("BoardTask"));
    let successful = write
        .call(BoundedWriteParams(
            serde_json::from_value(json!({"title":"kept", "lane":"doing"})).unwrap(),
        ))
        .await
        .unwrap();
    assert!(successful.contains("kept"));
    let before = fixture
        .access
        .execute("{ BoardTask { _docID title lane } }")
        .await
        .unwrap();
    let (failed, mutations) =
        crate::store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            write.call(BoundedWriteParams(
                serde_json::from_value(json!({"title":"rolled-back", "lane":"doing"})).unwrap(),
            )),
        )
        .await;
    assert!(failed.is_err());
    assert_eq!(mutations, 1);
    assert_eq!(
        fixture
            .access
            .execute("{ BoardTask { _docID title lane } }")
            .await
            .unwrap(),
        before
    );
    fixture
        .write_surface(board_surface(&fixture.node_did, "board", false))
        .await;
    assert!(query.call(BoundedQueryParams(Map::new())).await.is_err());
    assert!(write
        .call(BoundedWriteParams(
            serde_json::from_value(json!({"title":"denied", "lane":"doing"})).unwrap()
        ))
        .await
        .is_err());
    assert_eq!(
        fixture
            .access
            .execute("{ BoardTask { _docID title lane } }")
            .await
            .unwrap(),
        before
    );
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn cached_http_operator_query_refuses_after_native_reader_revocation() {
    let fixture = Fixture::new().await;
    let request = || UxDataCall {
        ux: BOARD.into(),
        node_did: fixture.node_did.clone(),
        surface: "board".into(),
        tool: "list_tasks".into(),
        args: json!({}),
    };
    let initial = call(fixture.home.path(), &fixture.client_access, request())
        .await
        .unwrap();
    assert_eq!(initial["kind"], "query");
    let before = fixture
        .access
        .execute("{ BoardTask { _docID title lane } }")
        .await
        .unwrap();
    let write = UxDataCall {
        ux: BOARD.into(),
        node_did: fixture.node_did.clone(),
        surface: "board".into(),
        tool: "add_task".into(),
        args: json!({"title":"must-not-publish", "lane":"doing"}),
    };
    let error = call(fixture.home.path(), &fixture.client_access, write)
        .await
        .unwrap_err();
    let model = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let remote = model
        .configuration_ux_create_cases
        .iter()
        .find(|case| case["name"] == "remote-unsigned")
        .unwrap();
    assert!(!remote["expected"].as_bool().unwrap());
    assert!(
        gents_protocol::error_chain::find_map_source(error.as_ref(), |cause| {
            matches!(cause.downcast_ref::<crate::store::config_client::ConfigClientError>(),
            Some(crate::store::config_client::ConfigClientError::ClientUnsignedMutationUnsupported))
            .then_some(())
        })
        .is_some()
    );
    assert_eq!(
        fixture
            .access
            .execute("{ BoardTask { _docID title lane } }")
            .await
            .unwrap(),
        before
    );
    let surface = installed_surface(&fixture.client_access, &fixture.node_did, "board")
        .await
        .unwrap();
    let declaration = surface
        .entries
        .as_ref()
        .unwrap()
        .iter()
        .find_map(|entry| match entry {
            SurfaceToolDecl::Query(decl) if decl.tool_name == "list_tasks" => Some(decl.clone()),
            _ => None,
        })
        .unwrap();
    let cached = BoundedQueryTool::new(fixture.client_access.clone(), declaration)
        .declared_by(Some("board".into()))
        .with_visible_surface_owner(fixture.node_did.clone());
    assert!(cached.call(BoundedQueryParams(Map::new())).await.is_ok());
    let row = fixture
        .access
        .execute("{ DatastoreToolSurface(filter: {surface_id: {_eq: \"board\"}}) { _docID } }")
        .await
        .unwrap();
    let doc = row["data"]["DatastoreToolSurface"][0]["_docID"]
        .as_str()
        .unwrap();
    fixture
        .access
        .delete_actor_relationship("DatastoreToolSurface", doc, "reader", &fixture.client_did)
        .await
        .unwrap();
    let denied = cached
        .call(BoundedQueryParams(Map::new()))
        .await
        .unwrap_err();
    assert!(
        denied.to_string().contains("surface is unavailable"),
        "{denied}"
    );
    assert!(call(fixture.home.path(), &fixture.client_access, request())
        .await
        .is_err());
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn http_operator_signs_ux_create_from_the_verified_installed_pack() {
    let fixture = Fixture::new().await;
    install_verified_board_schema(
        fixture.home.path(),
        "type BoardTask { title: String lane: String }",
    )
    .await;
    let model = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let approved = model
        .configuration_ux_create_cases
        .iter()
        .find(|case| case["name"] == "remote-approved-graph")
        .unwrap();
    let request = UxDataCall {
        ux: BOARD.into(),
        node_did: fixture.node_did.clone(),
        surface: "board".into(),
        tool: "add_task".into(),
        args: json!({"title":"client-authored", "lane":"doing"}),
    };
    let outcome = call(fixture.home.path(), &fixture.client_access, request).await;
    assert_eq!(
        outcome.is_ok(),
        approved["expected"].as_bool().unwrap(),
        "{outcome:?}"
    );
    let outcome = outcome.unwrap();
    let doc_id = outcome["output"]["document_id"].as_str().unwrap();
    let history = fixture
        .client_access
        .execute(&format!(
        "{{ _commits(docID: \"{}\") {{ cid height fieldName signature {{ identity type }} }} }}",
        crate::store::graphql::escape_graphql_string(doc_id),
    ))
        .await
        .unwrap();
    let commits =
        crate::store::graphql::composite_commits_at(history["data"].get("_commits"), "_commits")
            .unwrap();
    let genesis = commits.iter().min_by_key(|commit| commit.height).unwrap();
    let signer =
        crate::store::identity::commit_signer_identity_for_did(fixture.client_did.as_str())
            .unwrap();
    assert_eq!(
        genesis
            .signature
            .as_ref()
            .map(|signature| signature.identity.as_str()),
        Some(signer.as_str())
    );
    assert_ne!(fixture.client_did.as_str(), fixture.node_did);
    let before = fixture
        .client_access
        .execute("{ BoardTask { _docID title lane } }")
        .await
        .unwrap();
    let rollback_request = UxDataCall {
        ux: BOARD.into(),
        node_did: fixture.node_did.clone(),
        surface: "board".into(),
        tool: "add_task".into(),
        args: json!({"title":"rolled-back-client", "lane":"doing"}),
    };
    let (failed, mutations) =
        crate::store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            call(
                fixture.home.path(),
                &fixture.client_access,
                rollback_request,
            ),
        )
        .await;
    assert!(failed.is_err());
    assert_eq!(mutations, 1);
    assert_eq!(
        fixture
            .client_access
            .execute("{ BoardTask { _docID title lane } }")
            .await
            .unwrap(),
        before
    );
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn http_operator_refuses_ux_manifest_tampering_before_signing() {
    let fixture = Fixture::new().await;
    install_verified_board_schema(
        fixture.home.path(),
        "type BoardTask { title: String lane: String }",
    )
    .await;
    let mut record = crate::pack::read_home_install(fixture.home.path(), "acme/board")
        .unwrap()
        .unwrap();
    record.ux[0].data.push(declared("forged", &["list_tasks"]));
    crate::pack::write_home_install(fixture.home.path(), &record).unwrap();
    let before = fixture
        .access
        .execute("{ BoardTask { _docID title lane } }")
        .await
        .unwrap();
    let request = UxDataCall {
        ux: BOARD.into(),
        node_did: fixture.node_did.clone(),
        surface: "board".into(),
        tool: "add_task".into(),
        args: json!({"title":"forged", "lane":"doing"}),
    };
    let error = call(fixture.home.path(), &fixture.client_access, request)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match its trusted pack"),
        "{error:#}"
    );
    assert_eq!(
        fixture
            .access
            .execute("{ BoardTask { _docID title lane } }")
            .await
            .unwrap(),
        before
    );
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn http_operator_never_signs_a_server_graph_outside_the_verified_schema() {
    for schema in [
        "type BoardTask { title: String lane: Int }",
        "type BoardTask { title: String lane: String } type Other { value: String }",
    ] {
        let fixture = Fixture::new().await;
        install_verified_board_schema(fixture.home.path(), schema).await;
        let before = fixture
            .access
            .execute("{ BoardTask { _docID title lane } }")
            .await
            .unwrap();
        let request = UxDataCall {
            ux: BOARD.into(),
            node_did: fixture.node_did.clone(),
            surface: "board".into(),
            tool: "add_task".into(),
            args: json!({"title":"must-not-sign", "lane":"doing"}),
        };
        assert!(call(fixture.home.path(), &fixture.client_access, request)
            .await
            .is_err());
        assert_eq!(
            fixture
                .access
                .execute("{ BoardTask { _docID title lane } }")
                .await
                .unwrap(),
            before
        );
        fixture.node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn the_tools_own_refusals_reach_the_page() {
    let fixture = Fixture::new().await;
    let error = fixture
        .call("board", "add_task", json!({"title": "no lane"}))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("lane"),
        "the bounded write tool's own refusal: {error:#}"
    );
    let error = fixture
        .call("board", "list_tasks", json!({"fields": ["_docID"]}))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("allowlist"), "{error:#}");
    let error = fixture
        .call("board", "list_tasks", json!(["title"]))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("args must be a JSON object"),
        "{error:#}"
    );
    fixture.node.shutdown().await.expect("node shutdown");
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
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_disabled_missing_or_foreign_surface_is_refused() {
    let fixture = Fixture::new().await;
    let error = fixture.refusal("archive", "list_tasks").await;
    assert_eq!(error, "surface \"archive\" is disabled");
    let error = fixture.refusal("nowhere", "list_tasks").await;
    assert_eq!(
        error,
        format!(
            "surface \"nowhere\" is not installed on node {}",
            fixture.node_did
        )
    );
    let error = fixture.refusal("foreign", "list_tasks").await;
    assert_eq!(
        error,
        format!(
            "surface \"foreign\" is not installed on node {}",
            fixture.node_did
        )
    );
    let error = format!(
        "{:#}",
        fixture
            .call_as(
                BOARD,
                &fixture.other_node_did,
                "board",
                "list_tasks",
                json!({})
            )
            .await
            .unwrap_err()
    );
    assert_eq!(
        error,
        format!(
            "surface \"board\" is not installed on node {}",
            fixture.other_node_did
        )
    );
    fixture.node.shutdown().await.expect("node shutdown");
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
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_dev_door_listing_declares_nothing_and_is_refused() {
    let fixture = Fixture::new().await;
    let error = format!(
        "{:#}",
        fixture
            .call_as("loose", &fixture.node_did, "board", "list_tasks", json!({}))
            .await
            .unwrap_err()
    );
    assert_eq!(error, "ux plugin \"loose\" declares no data surfaces");
    let error = format!(
        "{:#}",
        surfaces(
            fixture.home.path(),
            &fixture.access,
            "loose",
            &fixture.node_did
        )
        .await
        .unwrap_err()
    );
    assert_eq!(error, "ux plugin \"loose\" declares no data surfaces");
    let error = format!(
        "{:#}",
        fixture
            .call_as(
                "acme/nope/board",
                &fixture.node_did,
                "board",
                "list_tasks",
                json!({})
            )
            .await
            .unwrap_err()
    );
    assert!(
        error.contains("no ux plugin \"acme/nope/board\""),
        "{error}"
    );
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_tool_with_a_runtime_filled_field_is_not_callable_from_a_page() {
    let fixture = Fixture::new().await;
    let error = fixture.refusal("filled", "add_note").await;
    assert_eq!(
        error,
        "tool \"add_note\" has runtime-filled fields and is not callable from a page"
    );
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn surfaces_lists_each_declared_surface_with_its_callable_tools() {
    let fixture = Fixture::new().await;
    let listed = surfaces(
        fixture.home.path(),
        &fixture.access,
        BOARD,
        &fixture.node_did,
    )
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
    fixture.node.shutdown().await.expect("node shutdown");
}
