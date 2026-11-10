//! Pages and plugin sources held on the node: built through `config` alone by
//! an agent with no shell and no file tools.

use super::*;

/// A config tool whose agent has bash off, no file tools and no tool root.
async fn fileless_tool(
    label: &str,
    home: &std::path::Path,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let mut tool_config = config(&["tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    tool_config.enable_pack_install = true;
    tool_config.enable_pack_authority = true;
    assert_eq!(
        tool_config.process_ceiling.file_mode,
        crate::tools::surface::FileToolMode::Off
    );
    assert!(tool_config.process_ceiling.root.is_none());
    let tools = fixture.tools_with_plugins(
        &tool_config,
        Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.to_owned(),
        ))),
    );
    (fixture, tools)
}

async fn call(tools: &[Box<dyn ToolDyn>], args: Value) -> Result<Value, String> {
    let text = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
        .call(args.to_string())
        .await
        .map_err(|error| error.to_string())?;
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

async fn ok(tools: &[Box<dyn ToolDyn>], args: Value) -> Value {
    call(tools, args.clone())
        .await
        .unwrap_or_else(|error| panic!("{args}: {error}"))
}

const MODULE: &str = "import { NAV_AREA, PLUGIN_PAGE_AREA } from '@gents/ux-sdk'\n\
import { jsx } from 'react/jsx-runtime'\n\
export default { id: 'notes', name: 'Notes', register(ctx) {\n\
  ctx.registerMany([\n\
    { id: 'nav', area: NAV_AREA, data: { id: 'notes:nav', label: 'Notes', icon: 'puzzle', to: { name: 'plugin', pluginId: 'notes' }, placement: 'footer' } },\n\
    { id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx('p', { children: 'Notes' }) },\n\
  ]) } }\n";

#[tokio::test]
async fn a_fileless_agent_creates_edits_lints_disables_and_removes_a_page() {
    let home = tempfile::tempdir().unwrap();
    let (_fixture, tools) = fileless_tool("live-page", home.path()).await;
    let data = json!([{"surface": "notes-tools", "tools": ["list_notes"]}]);

    let created = ok(
        &tools,
        json!({"argv":["page","create"],"target_id":"notes","set":{
            "display_name":"Notes","module":MODULE,"css":".notes { gap: 8px }","data":data}}),
    )
    .await;
    assert!(created.to_string().contains("notes"), "{created}");

    let read = ok(&tools, json!({"argv":["page","get"],"target_id":"notes"})).await;
    assert_eq!(read["document"]["module"], MODULE, "{read}");
    assert_eq!(
        read["document"]["data"],
        json!({"surfaces": data}),
        "{read}"
    );
    let listed = ok(&tools, json!({"argv":["page","list"]})).await;
    assert!(listed.to_string().contains("\"notes\""), "{listed}");

    let lint = ok(&tools, json!({"argv":["page","lint"],"target_id":"notes"})).await;
    assert_eq!(lint["admitted"], true, "{lint}");
    assert_eq!(
        lint["next_call"]["argv"],
        json!(["ux", "preview", "page/notes"])
    );
    assert_eq!(
        ok(&tools, json!({"argv":["ux","lint","page/notes"]})).await["admitted"],
        true
    );

    let listing = ok(&tools, json!({"argv":["ux","list"]})).await;
    let page = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "page/notes")
        .unwrap_or_else(|| panic!("{listing}"));
    assert_eq!(page["door"], "document", "{page}");
    assert_eq!(page["defaultEnabled"], true, "{page}");
    assert_eq!(page["data"], data, "{page}");
    assert_eq!(
        page["contributes"]["areas"],
        json!(["nav", "plugin.pages"]),
        "{page}"
    );
    let digest = page["digest"].as_str().unwrap().to_owned();
    let module = ok(&tools, json!({"argv":["ux","module","page/notes"]})).await;
    assert_eq!(module["source"], MODULE, "{module}");
    assert_eq!(module["css"], ".notes { gap: 8px }", "{module}");

    let edited = MODULE.replace("children: 'Notes'", "children: 'All notes'");
    ok(
        &tools,
        json!({"argv":["page","update"],"target_id":"notes","set":{"module":edited,"enabled":false}}),
    )
    .await;
    let listing = ok(&tools, json!({"argv":["ux","list"]})).await;
    let page = &listing["items"][0];
    assert_eq!(page["defaultEnabled"], false, "{page}");
    assert_ne!(page["digest"], digest, "an edit changes the listing digest");

    let bad_import = ok(
        &tools,
        json!({"argv":["page","create"],"target_id":"leaky","set":{
            "module":"import fs from 'node:fs'\nexport default { id: 'leaky', register() {} }"}}),
    )
    .await;
    assert!(bad_import.to_string().contains("leaky"), "{bad_import}");
    let lint = ok(&tools, json!({"argv":["page","lint"],"target_id":"leaky"})).await;
    assert_eq!(lint["admitted"], false, "{lint}");

    for (set, expected) in [
        (
            json!({"module": "x".repeat(1024 * 1024 + 1)}),
            "at most 1024 KiB",
        ),
        (
            json!({"module": MODULE, "data": [{"surface":"s","tools":[]}]}),
            "with no tools",
        ),
        (json!({"css": "p{}"}), "module"),
    ] {
        let error = call(
            &tools,
            json!({"argv":["page","create"],"target_id":"refused","set":set}),
        )
        .await
        .unwrap_err();
        assert!(error.contains(expected), "{expected}: {error}");
    }
    let error = call(
        &tools,
        json!({"argv":["page","create"],"target_id":"Not Snake","set":{"module":MODULE}}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("snake_case"), "{error}");

    let preview = ok(
        &tools,
        json!({"argv":["page","preview","delete"],"target_id":"notes"}),
    )
    .await;
    ok(&tools, preview["apply_with"].clone()).await;
    let listing = ok(&tools, json!({"argv":["ux","list"]})).await;
    assert!(
        !listing.to_string().contains("page/notes"),
        "a removed page leaves the listing: {listing}"
    );
}

#[tokio::test]
async fn page_and_plugin_help_name_the_document_verbs() {
    let home = tempfile::tempdir().unwrap();
    let (_fixture, tools) = fileless_tool("live-help", home.path()).await;
    let page = call(&tools, json!({"argv":["help","page"]})).await.unwrap();
    let page = page.as_str().unwrap();
    for expected in [
        "create ID",
        "page lint ID",
        "ux preview page/ID",
        "module",
        "Topics:",
        "\"page\",\"create\"",
    ] {
        assert!(page.contains(expected), "{expected}: {page}");
    }
    let index = call(&tools, json!({"argv":["help"]})).await.unwrap();
    assert!(
        index.as_str().unwrap().contains("plugin-source:"),
        "{index}"
    );
    let plugin = call(&tools, json!({"argv":["help","plugin"]}))
        .await
        .unwrap();
    let plugin = plugin.as_str().unwrap();
    assert!(plugin.contains("plugin build NAME"), "{plugin}");
    assert!(plugin.contains("\"plugin\",\"build\""), "{plugin}");
    let source = call(&tools, json!({"argv":["help","plugin-source"]}))
        .await
        .unwrap();
    assert!(source.as_str().unwrap().contains("tests"), "{source}");
}

/// `plugin build` keeps the source as a document whether or not the build
/// succeeds, reports a failed compile and each failed case, and installs
/// nothing for either. The compiler is stubbed; the real one is the
/// `__plugin-build` process this stub stands in for.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn plugin_build_saves_the_source_and_reports_build_and_case_failures() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let builder = outside.path().join("builder.sh");
    let stub = |body: &str| {
        std::fs::write(&builder, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&builder, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let _stub = PLUGIN_BUILDER_STUB.lock().await;
    stub("echo 'main.js:1: unexpected token' >&2; exit 1");
    let _builder =
        crate::test_support::EnvVarGuard::set("GENTS_PLUGIN_BUILDER", builder.to_str().unwrap());
    let (_fixture, tools) = fileless_tool("live-plugin", home.path()).await;
    let set = json!({
        "language": "js", "description": "Doubles n.",
        "source": "module.exports = (input) => ({ n: input.n * 2 }",
        "input_schema": {"type":"object","properties":{"n":{"type":"number"}}},
        "tests": [{"name":"two","input":{"n":2},"expect":{"n":4}}],
    });
    let error = call(
        &tools,
        json!({"argv":["plugin","build"],"target_id":"double","set":set}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("did not build"), "{error}");
    assert!(error.contains("unexpected token"), "{error}");
    assert!(error.contains("source document is saved"), "{error}");

    let saved = ok(
        &tools,
        json!({"argv":["plugin-source","get"],"target_id":"double"}),
    )
    .await;
    assert_eq!(saved["document"]["language"], "js", "{saved}");
    assert_eq!(
        saved["document"]["tests"]["cases"][0]["expect"],
        json!({"n": 4}),
        "{saved}"
    );
    let dir = home.path().join("plugin-builds/double/double");
    assert_eq!(
        std::fs::read_to_string(dir.join("plugins/double/source/main.js")).unwrap(),
        set["source"].as_str().unwrap()
    );
    assert!(dir.join("plugins/double/tests/01_two.json").is_file());

    stub("printf '{\"cases\":[{\"plugin\":\"double\",\"passed\":0,\"failures\":[\"01_two.json: returned {\\\\\"n\\\\\":3} instead of {\\\\\"n\\\\\":4}\"]}]}'");
    let error = call(
        &tools,
        json!({"argv":["plugin","build"],"target_id":"double","set":{
            "source":"module.exports = (input) => ({ n: input.n + 1 });"}}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("1 of 1 test cases failed"), "{error}");
    assert!(error.contains("01_two.json"), "{error}");
    assert!(
        crate::plugin::store::read_record(home.path(), "node", "double").is_err(),
        "a failed case installs nothing"
    );
    let saved = ok(
        &tools,
        json!({"argv":["plugin-source","get"],"target_id":"double"}),
    )
    .await;
    assert!(
        saved["document"]["source"]
            .as_str()
            .unwrap()
            .contains("input.n + 1"),
        "the edit is saved: {saved}"
    );

    let refused = call(
        &tools,
        json!({"argv":["plugin","build"],"target_id":"cobol_tool","set":{"language":"cobol","source":"x"}}),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("supported languages"), "{refused}");
}

/// A plugin built from a source document has no pack: its `data.collections`
/// are application collections on the node, and the build carries their SDL
/// to the install's data check. Unknown and Gents-managed names are refused
/// before anything compiles, each by name. The compiler is stubbed with a
/// prebuilt artifact.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn plugin_build_accepts_data_collections_that_live_on_the_node() {
    let home = tempfile::tempdir().unwrap();
    // The command sandbox confines writes to the canonical build root.
    let home_path = std::fs::canonicalize(home.path()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let builder = outside.path().join("builder.sh");
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/packs/job_plugin_fixture/plugins/job_fixture.afb");
    std::fs::write(
        &builder,
        format!(
            "#!/bin/sh\ncat {:?} > plugins/probe.afb\nprintf '{{\"cases\":[{{\"plugin\":\"probe\",\"passed\":1,\"failures\":[]}}]}}'\n",
            artifact
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&builder, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _stub = PLUGIN_BUILDER_STUB.lock().await;
    let _builder =
        crate::test_support::EnvVarGuard::set("GENTS_PLUGIN_BUILDER", builder.to_str().unwrap());
    let (fixture, tools) = fileless_tool("live-plugin-data", &home_path).await;
    fixture.grant_agent_schema().await;
    fixture
        .add_collection("type DeskTicket { subject: String @index(unique: true) status: String }")
        .await;
    let build = |collections: Value| {
        json!({"argv":["plugin","build"],"target_id":"probe","set":{
            "language":"rust","source":"fn main() {}",
            "data":{"collections":collections,"ops":["query"]},
        }})
    };

    let refused = call(
        &tools,
        build(json!(["DeskTicket", "DeskTiket", "AgentRequest"])),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("Not on this node: DeskTiket"), "{refused}");
    assert!(refused.contains("cannot use: AgentRequest"), "{refused}");
    assert!(!refused.contains("schemas/*.graphql"), "{refused}");
    assert!(
        crate::plugin::store::read_record(&home_path, "node", "probe").is_err(),
        "a refused declaration installs nothing"
    );

    let built = ok(&tools, build(json!(["DeskTicket"]))).await;
    assert_eq!(built["installed"], true, "{built}");
    let record = crate::plugin::store::read_record(&home_path, "node", "probe").unwrap();
    let fields = &record.data_collection_fields["DeskTicket"];
    assert!(
        fields.contains(&"status".to_owned()) && fields.contains(&"subject".to_owned()),
        "the node's collection is the plugin's data declaration: {fields:?}"
    );
    assert_eq!(record.data_unique_keys["DeskTicket"], [["subject"]]);
}
