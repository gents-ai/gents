//! Pages held on the node: built through `config` alone by an agent with no
//! shell and no file tools.

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
    assert_eq!(read["document"]["data"], json!({"surfaces": data}), "{read}");
    let listed = ok(&tools, json!({"argv":["page","list"]})).await;
    assert!(listed.to_string().contains("\"notes\""), "{listed}");

    let lint = ok(&tools, json!({"argv":["page","lint"],"target_id":"notes"})).await;
    assert_eq!(lint["admitted"], true, "{lint}");
    assert_eq!(lint["next_call"]["argv"], json!(["ux","preview","page/notes"]));
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
        (json!({"module": "x".repeat(1024 * 1024 + 1)}), "at most 1024 KiB"),
        (json!({"module": MODULE, "data": [{"surface":"s","tools":[]}]}), "with no tools"),
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
async fn page_help_names_the_document_verbs() {
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
}
