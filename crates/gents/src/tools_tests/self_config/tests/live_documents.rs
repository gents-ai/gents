//! Pages and plugin sources held on the node: built through `config` alone by
//! an agent with no shell and no file tools.

use super::*;

/// A config tool whose agent has bash off, no file tools and no tool root.
async fn fileless_tool(
    label: &str,
    home: &std::path::Path,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    fileless_tool_guarded(label, home, false).await
}

/// `no_lockout` is the guard an agent editing its own configuration runs
/// under, which refuses a Tools edit that drops a setting.
async fn fileless_tool_guarded(
    label: &str,
    home: &std::path::Path,
    no_lockout: bool,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    fileless_tool_under(label, home, no_lockout, None).await
}

/// `ceiling_plugins` is the process ceiling's plugin allowlist; `None` admits all.
async fn fileless_tool_under(
    label: &str,
    home: &std::path::Path,
    no_lockout: bool,
    ceiling_plugins: Option<BTreeSet<String>>,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let mut tool_config = config(&["tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    tool_config.no_lockout = no_lockout;
    tool_config.enable_pack_install = true;
    tool_config.enable_pack_authority = true;
    tool_config.ceiling_plugins = ceiling_plugins;
    assert_eq!(
        tool_config.process_ceiling.file_mode,
        gents_documents::FileToolMode::Off
    );
    assert!(tool_config.process_ceiling.root.is_none());
    let tools = fixture.tools_with_plugins(
        &tool_config,
        Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(Some(
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
    let _builder = gents_store::test_support::EnvVarGuard::set(
        "GENTS_PLUGIN_BUILDER",
        builder.to_str().unwrap(),
    );
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
        gents_pack::plugin::store::read_record(home.path(), "node", "double").is_err(),
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
    let _builder = gents_store::test_support::EnvVarGuard::set(
        "GENTS_PLUGIN_BUILDER",
        builder.to_str().unwrap(),
    );
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
        gents_pack::plugin::store::read_record(&home_path, "node", "probe").is_err(),
        "a refused declaration installs nothing"
    );

    let built = ok(&tools, build(json!(["DeskTicket"]))).await;
    assert_eq!(built["installed"], true, "{built}");
    let record = gents_pack::plugin::store::read_record(&home_path, "node", "probe").unwrap();
    let fields = &record.data_collection_fields["DeskTicket"];
    assert!(
        fields.contains(&"status".to_owned()) && fields.contains(&"subject".to_owned()),
        "the node's collection is the plugin's data declaration: {fields:?}"
    );
    assert_eq!(record.data_unique_keys["DeskTicket"], [["subject"]]);
}

/// Rebuilding `node/NAME` moves the pins of the Tools that select it, since
/// the builder is who selected it, and the receipt lists them.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_rebuild_moves_the_pins_of_the_tools_that_select_the_plugin() {
    let home = tempfile::tempdir().unwrap();
    let home_path = std::fs::canonicalize(home.path()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let artifact = outside.path().join("probe.afb");
    let write_artifact = |output: &[u8]| {
        std::fs::write(
            &artifact,
            gents_pack::plugin::test_fixtures::build_plugin_afb(
                &gents_pack::plugin::test_fixtures::constant_output_wat(output),
            ),
        )
        .unwrap();
    };
    let builder = outside.path().join("builder.sh");
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
    let _builder = gents_store::test_support::EnvVarGuard::set(
        "GENTS_PLUGIN_BUILDER",
        builder.to_str().unwrap(),
    );
    let (_fixture, tools) = fileless_tool_guarded("live-plugin-pins", &home_path, true).await;
    let build = json!({"argv":["plugin","build"],"target_id":"probe","set":{"language":"rust","source":"fn main() {}"}});

    write_artifact(br#"{"build":1}"#);
    let first = ok(&tools, build.clone()).await;
    assert_eq!(first["selections_moved"], Value::Null, "{first}");
    let tools_document = ok(&tools, json!({"argv":["tools","get"]})).await;
    let mut integrations = tools_document["document"]["integrations"].clone();
    if !integrations.is_object() {
        integrations = json!({});
    }
    integrations["plugins"] = json!([first["selection"]]);
    // Selecting a plugin nothing installed would leave this agent without a
    // tool surface on its next request, so the edit is refused whole, with
    // the install as the next call (#1796).
    let mut absent = integrations.clone();
    absent["plugins"] = json!([
        first["selection"],
        {"plugin": "acme/absent"}
    ]);
    let lockout = call(
        &tools,
        json!({"argv":["tools","update"],"set":{"integrations":absent}}),
    )
    .await
    .expect_err("selecting a plugin nothing installed is refused");
    assert!(
        lockout.contains("nothing was changed")
            && lockout.contains("would select plugin acme/absent, which is not installed")
            && lockout.contains("preview")
            && lockout.contains("install"),
        "{lockout}"
    );
    assert_eq!(
        ok(&tools, json!({"argv":["tools","get"]})).await["document"]["integrations"],
        tools_document["document"]["integrations"],
        "the refused edit wrote nothing"
    );
    ok(
        &tools,
        json!({"argv":["tools","update"],"set":{"integrations":integrations}}),
    )
    .await;

    write_artifact(br#"{"build":2}"#);
    // The pin move is the builder's own Tools edit, made under the guard
    // that refuses dropped settings: a moved pin replaces, it drops nothing.
    let second = ok(&tools, build).await;
    assert_ne!(second["digest"], first["digest"], "{second}");
    assert_eq!(
        second["selections_moved"][0]["from"], first["digest"],
        "{second}"
    );
    assert_eq!(
        second["selections_moved"][0]["to"], second["digest"],
        "{second}"
    );
    let after = ok(&tools, json!({"argv":["tools","get"]})).await;
    assert_eq!(
        after["document"]["integrations"]["plugins"][0]["digest"], second["digest"],
        "{after}"
    );

    // The saved source rebuilds with no fields, and an unchanged artifact
    // leaves the pin where it is.
    let again = ok(&tools, json!({"argv":["plugin","build","probe"]})).await;
    assert_eq!(again["digest"], second["digest"], "{again}");
    assert_eq!(again["selections_moved"], Value::Null, "{again}");
    // Deselecting the plugin is still a drop.
    let dropped = call(
        &tools,
        json!({"argv":["tools","update"],"set":{"integrations":{"plugins":[]}}}),
    )
    .await
    .unwrap_err();
    assert!(
        dropped.contains("drop existing settings from your own Tools: integrations.plugins item"),
        "{dropped}"
    );

    // Retiring a plugin is refused while a Tools still selects it: the
    // refusal names the Tools and recovers to the pack wire --remove that
    // drops that selection. Once nothing selects it, the removal retires it.
    let failure = |error: String| -> Value {
        serde_json::from_str(&error[error.find('{').unwrap()..]).unwrap()
    };
    let remove = json!({"argv":["pack","remove","node/probe"]});
    let selected = failure(call(&tools, remove.clone()).await.unwrap_err());
    let message = selected["error"].as_str().unwrap();
    assert!(message.contains("node/probe was not removed"), "{selected}");
    assert!(message.contains("selected by Tools"), "{selected}");
    assert!(
        gents_pack::plugin::store::read_record(&home_path, "node", "probe").is_ok(),
        "a selected plugin stays installed"
    );
    let deselect = &selected["recovery"]["next_call"];
    assert_eq!(
        deselect["argv"],
        json!(["pack", "wire", "node/probe", "--remove"]),
        "{selected}"
    );
    ok(&tools, deselect.clone()).await;

    let retired = ok(&tools, remove).await;
    assert_eq!(
        retired["removed"]["plugins"][0]["name"], "probe",
        "{retired}"
    );
    assert_eq!(
        retired["removed"]["plugin_bytes"],
        json!([second["digest"]]),
        "{retired}"
    );
    assert_eq!(
        retired["next_call"],
        json!({"argv":["plugin-source","preview","delete"],"target_id":"probe"}),
        "{retired}"
    );
    assert!(
        gents_pack::plugin::store::read_record(&home_path, "node", "probe").is_err(),
        "the retired plugin is no longer installed"
    );
    assert!(
        gents_pack::pack::read_home_install(&home_path, "node/probe")
            .unwrap()
            .is_none()
    );
    let listed = ok(&tools, json!({"argv":["plugin","list"]})).await;
    assert_eq!(listed["items"], json!([]), "{listed}");
    let uncallable = call(&tools, json!({"argv":["plugin","run","node/probe"]}))
        .await
        .unwrap_err();
    assert!(uncallable.contains("is not installed"), "{uncallable}");
}

/// `plugin run` is one tool call of the plugin: a `data_calls` round is
/// answered under the calling agent's access and the plugin is re-invoked
/// with the rows, and a request no round serves is an error naming the
/// service and the next call, never the plugin's result.
#[tokio::test]
async fn plugin_run_drives_the_rounds_a_tool_call_does() {
    use gents_toolcalls::plugin::tests::executor::{asking_plugin_wat, installed_plugin};
    let canned = json!({"data_calls": {"requests": [{"id": "notes", "query": {
        "collection": "Note", "filter": {}, "limit": 5, "fields": ["body"]}}]}});
    let (home, mut record) = installed_plugin(
        &asking_plugin_wat("data_calls", "data_results", &canned, false),
        None,
    );
    record.declaration.data = Some(gents_protocol::job::PluginDataDeclaration {
        collections: vec!["Note".into()],
        ops: vec!["query".into()],
        stamped: BTreeMap::new(),
    });
    gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
    let (fixture, tools) = fileless_tool("plugin-run-rounds", home.path()).await;
    fixture
        .db
        .node
        .add_collection("type Note { body: String }")
        .await
        .unwrap();
    gents_store::config_client::ConfigAccess::transact_local_write(
        &fixture.db.node,
        None,
        "plugin_run_test",
        |txn| {
            Box::pin(async move {
                txn.execute("mutation { add_Note(input: { body: \"kept\" }) { _docID } }")
                    .await
            })
        },
    )
    .await
    .unwrap();
    let coordinate = format!("team/{}", record.name);

    let ran = ok(
        &tools,
        json!({"argv":["plugin","run",coordinate],"options":{"input":{}}}),
    )
    .await;
    assert_eq!(
        ran["output"]["data_results"]["notes"]["result"]["rows"],
        json!([{"body": "kept"}]),
        "{ran}"
    );
    assert!(
        ran["output"]
            .get("data_calls")
            .is_none_or(|asked| asked == true),
        "{ran}"
    );

    // A result that carries a request-shaped field beside its own fields is
    // the plugin's result; the field is pointed out, not turned into a failure.
    let job_request = json!({"requests": [{"id": "a", "child_output": {"slots": ["s"], "offset": 0, "limit": 1}}]});
    let carried = ok(
        &tools,
        json!({"argv":["plugin","run",coordinate],"options":{"input":{"job_calls": job_request}}}),
    )
    .await;
    assert_eq!(carried["output"]["job_calls"], job_request, "{carried}");
    assert!(carried["output"]["data_results"].is_object(), "{carried}");
    let note = carried["unserved"].as_str().unwrap();
    assert!(
        note.contains("output is the plugin's result") && note.contains("job_calls"),
        "{carried}"
    );

    // A plugin the process ceiling does not admit is not run, selected or not.
    let (_ceiling_fixture, under_ceiling) = fileless_tool_under(
        "plugin-run-ceiling",
        home.path(),
        false,
        Some(BTreeSet::from(["other".to_owned()])),
    )
    .await;
    let refused = call(
        &under_ceiling,
        json!({"argv":["plugin","run",coordinate],"options":{"input":{}}}),
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("was not run")
            && refused.contains("process tool ceiling")
            && refused.contains("it admits other"),
        "{refused}"
    );
}
