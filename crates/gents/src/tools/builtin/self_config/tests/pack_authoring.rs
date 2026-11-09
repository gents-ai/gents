use super::*;

/// A config tool for an agent with no shell and read-write file tools scoped
/// to `root`: the surface the pack loop has to work on alone. `authority` is
/// the Engineer preset's `enable_pack_authority`.
async fn authoring_tool(
    label: &str,
    home: &std::path::Path,
    root: &std::path::Path,
    categories: &[&str],
    authority: bool,
) -> (Arc<EmbeddedNode>, String, Vec<Box<dyn ToolDyn>>) {
    let node = build_agent_node().await;
    let identity = node_identity(label);
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    let mut tool_config = config(categories);
    tool_config.agent_id = "setup".to_string();
    tool_config.enable_pack_install = true;
    tool_config.enable_pack_authority = authority;
    tool_config.process_ceiling.file_mode = crate::tools::surface::FileToolMode::ReadWrite;
    tool_config.process_ceiling.root = Some(root.to_owned());
    let tools = build_self_config_tools(
        std::sync::Arc::default(),
        node.clone(),
        node_did.clone(),
        Some(identity),
        &tool_config,
        Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.to_owned(),
        ))),
    );
    tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap()
        .call(
            json!({"argv":["tools","update"],"set":{"host":{"root":root,"files":{"mode":"ReadWrite"},"bash":{"mode":"Off"}}}})
                .to_string(),
        )
        .await
        .unwrap();
    (node, node_did, tools)
}

async fn config_json(tools: &[Box<dyn ToolDyn>], argv: &[&str]) -> Value {
    let text = config_call(tools, argv)
        .await
        .unwrap_or_else(|error| panic!("{argv:?}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{argv:?}: {error}: {text}"))
}

#[tokio::test]
async fn a_shell_less_agent_takes_a_data_pack_from_scaffold_to_installed() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (node, _did, tools) =
        authoring_tool("pack-loop", home.path(), root.path(), &["tools"], false).await;

    config_json(
        &tools,
        &["pack", "scaffold", "notes_app", "--template", "assets"],
    )
    .await;
    let added = config_json(
        &tools,
        &["pack", "add", "schema", "Note", "--dir", "notes_app"],
    )
    .await;
    assert_eq!(added["next_call"]["argv"][1], "check", "{added}");
    let check = config_json(&tools, &["pack", "check", "notes_app"]).await;
    assert_eq!(check["problems"], json!([]), "{check}");

    let built = config_json(&tools, &["pack", "build", "notes_app"]).await;
    let digest = built["digest"].as_str().unwrap().to_owned();
    assert_eq!(built["next_call"]["argv"][1], "preview", "{built}");
    assert_eq!(built["operator_action"], Value::Null, "{built}");
    assert!(built["out"].as_str().unwrap().ends_with(".pack"), "{built}");

    let preview = config_json(&tools, &["pack", "preview", "install", "./notes_app"]).await;
    assert_eq!(preview["ready"], true, "{preview}");
    assert_eq!(preview["artifact_digest"], digest.as_str(), "{preview}");

    let installed = config_json(
        &tools,
        &["pack", "install", "./notes_app", "--digest", &digest],
    )
    .await;
    assert_eq!(installed["artifact_digest"], digest.as_str(), "{installed}");
    let access = crate::store::config_client::ConfigAccess::local(node.clone());
    let response = access.execute("query { Note { run_id } }").await.unwrap();
    assert!(response.get("errors").is_none(), "{response}");
}

/// The Engineer installs a pack it wrote, page included, and the page reads
/// exactly as an operator install's does.
#[tokio::test]
async fn a_shell_less_agent_installs_a_pack_with_a_page() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_node, _did, tools) =
        authoring_tool("pack-page", home.path(), root.path(), &["tools"], true).await;

    config_json(
        &tools,
        &[
            "pack",
            "scaffold",
            "board",
            "--template",
            "assets",
            "--page",
            "true",
        ],
    )
    .await;
    let lint = config_json(&tools, &["ux", "lint", "board/ux/board/plugin.js"]).await;
    assert_eq!(lint["admitted"], true, "{lint}");
    config_json(&tools, &["pack", "add", "schema", "Note", "--dir", "board"]).await;

    let built = config_json(&tools, &["pack", "build", "board"]).await;
    assert_eq!(built["next_call"]["argv"][1], "preview", "{built}");
    let digest = built["digest"].as_str().unwrap().to_owned();
    let preview = config_json(&tools, &["pack", "preview", "install", "./board"]).await;
    assert_eq!(preview["ready"], true, "{preview}");
    config_json(&tools, &["pack", "install", "./board", "--digest", &digest]).await;

    let listings = crate::plugin::ux::list(home.path()).unwrap();
    let page = listings
        .iter()
        .find(|listing| listing.id == "gents/board/board")
        .unwrap_or_else(|| panic!("{listings:?}"));
    assert_eq!(page.default_enabled, None, "follows the manifest: {page:?}");
    let module = config_json(&tools, &["ux", "module", "gents/board/board"]).await;
    assert!(
        module["source"].as_str().unwrap().contains("register"),
        "{module}"
    );
}

/// With the grant, an install gives a plugin the authority it declares and
/// the receipt says what was granted; without it the blockers stand.
#[tokio::test]
async fn pack_authority_grants_what_a_plugins_pack_asks_for() {
    for authority in [false, true] {
        let (workspace, _artifact, _) =
            crate::pack::plugin_lifecycle::tests::fixture("1.0.0", true);
        let home = tempfile::tempdir().unwrap();
        let (_node, _did, tools) = authoring_tool(
            &format!("pack-authority-{authority}"),
            home.path(),
            workspace.path(),
            &["tools"],
            authority,
        )
        .await;

        config_json(&tools, &["pack", "add", "page", "board", "--dir", "source"]).await;
        let built = config_json(&tools, &["pack", "build", "source"]).await;
        let digest = built["digest"].as_str().unwrap();
        let preview = config_json(&tools, &["pack", "preview", "install", "./source"]).await;
        assert_eq!(preview["ready"], authority, "{preview}");
        assert!(
            preview["would_grant"]
                .to_string()
                .contains("HTTP to https://example.invalid"),
            "{preview}"
        );
        let result =
            config_call(&tools, &["pack", "install", "./source", "--digest", digest]).await;
        if !authority {
            result.unwrap_err();
            continue;
        }
        let installed: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(
            installed["granted_authority"][0]["plugin"], "fixture/native_echo",
            "{installed}"
        );
        let record =
            crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
        assert!(record.granted.is_some(), "the store keeps the grant");
        assert!(
        let ran = tools
            .iter()
            .find(|tool| tool.name() == CONFIG_TOOL_NAME)
            .unwrap()
            .call(
                json!({"argv":["plugin","run","fixture/native_echo"],"options":{"input":{}}})
                    .to_string(),
            )
            .await
            .unwrap();
        let ran: Value = serde_json::from_str(&ran).unwrap();
        assert_eq!(ran["output"]["native_pack_echo"], "ok", "{ran}");
            crate::plugin::ux::list(home.path())
                .unwrap()
                .iter()
                .any(|listing| listing.id == "fixture/native_echo_pack/board"),
            "the page lands with the plugin"
        );
    }
}

#[tokio::test]
async fn compiling_plugin_source_needs_the_authority_grant() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_node, _did, tools) =
        authoring_tool("pack-compile", home.path(), root.path(), &["tools"], false).await;
    config_json(
        &tools,
        &["pack", "scaffold", "tool_a", "--template", "plugin-tool"],
    )
    .await;
    for verb in ["build", "test"] {
        let error = config_call(&tools, &["pack", verb, "tool_a"])
/// An assets pack carries no documents: its page installs through the home
/// record alone.
#[tokio::test]
async fn a_shell_less_agent_installs_an_assets_pack_with_a_page() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_fixture, tools) =
        authoring_tool("pack-assets", home.path(), root.path(), &["tools"], false).await;
    config_json(
        &tools,
        &[
            "pack",
            "scaffold",
            "board",
            "--template",
            "assets",
            "--page",
            "true",
        ],
    )
    .await;
    let built = config_json(&tools, &["pack", "build", "board"]).await;
    let digest = built["digest"].as_str().unwrap().to_owned();
    let preview = config_json(&tools, &["pack", "preview", "install", "./board"]).await;
    assert_eq!(preview["ready"], true, "{preview}");
    assert_eq!(preview["manifest"]["kind"], "assets", "{preview}");
    let installed = config_json(&tools, &["pack", "install", "./board", "--digest", &digest]).await;
    assert_eq!(installed["committed"], true, "{installed}");
    assert!(
        crate::plugin::ux::list(home.path())
            .unwrap()
            .iter()
            .any(|listing| listing.id == "gents/board/board"),
        "the assets pack's page is listed"
    );
    let got = config_json(&tools, &["pack", "get", "./board"]).await;
    assert_eq!(got["installed"]["digest"], digest.as_str(), "{got}");
}

            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("enable_pack_authority"), "{verb}: {error}");
    }
    assert!(
        !root.path().join("tool_a/target").exists(),
        "nothing is mirrored for a compile that will not run"
    );
    let directory = config_call(&tools, &["pack", "preview", "install", "./tool_a"])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        directory.contains(r#"[\"pack\",\"build\",DIR]"#),
        "{directory}"
    );
}

/// The build runs through the command sandbox: a builder that tries to write
/// outside the tool root is refused, and its report still reaches the caller.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_plugin_build_runs_confined_to_the_tool_root() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let escaped = std::fs::canonicalize(outside.path())
        .unwrap()
        .join("escaped");
    let builder = outside.path().join("builder.sh");
    std::fs::write(
        &builder,
        format!(
            "#!/bin/sh\nprintf stub > \"$2/plugins/tool_a.afb\"\nprintf x > {} 2>/dev/null\nprintf '{{\"cases\":[{{\"plugin\":\"tool_a\",\"passed\":2,\"failures\":[\"one\"]}}]}}'\n",
            escaped.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&builder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _builder =
        crate::test_support::EnvVarGuard::set("GENTS_PLUGIN_BUILDER", builder.to_str().unwrap());
    let (_node, _did, tools) =
        authoring_tool("pack-sandbox", home.path(), root.path(), &["tools"], true).await;
    config_json(
        &tools,
        &["pack", "scaffold", "tool_a", "--template", "plugin-tool"],
    )
    .await;

    let built = config_json(&tools, &["pack", "build", "tool_a"]).await;
    assert_eq!(built["sandbox"], "macos_seatbelt", "{built}");
    assert!(built["out"].as_str().unwrap().ends_with(".pack"), "{built}");
    assert!(
        !escaped.exists(),
        "the sandbox confines the builder's writes"
    );

    let tested = config_json(&tools, &["pack", "test", "tool_a"]).await;
    assert_eq!(tested["passed"], 2, "{tested}");
    assert_eq!(tested["failed"], 1, "{tested}");
}

#[tokio::test]
async fn authoring_paths_stay_inside_the_tool_root() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let (_node, _did, tools) =
        authoring_tool("pack-root", home.path(), root.path(), &["tools"], false).await;
    let scaffolded = config_json(
        &tools,
        &["pack", "scaffold", "in-side", "--template", "assets"],
    )
    .await;
    let outside = config_call(
    assert_eq!(scaffolded["pack"], "in_side", "{scaffolded}");
    assert_eq!(scaffolded["dir"], "in_side", "{scaffolded}");
    assert!(
        scaffolded["renamed"].as_str().unwrap().contains("in-side"),
        "{scaffolded}"
    );
        &tools,
        &[
            "pack",
            "add",
            "page",
            "board",
            "--dir",
            elsewhere.path().to_str().unwrap(),
        ],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        outside.contains("outside the allowed tool root"),
        "{outside}"
    );
    let from = config_call(
        &tools,
        &[
            "pack",
            "add",
            "skill",
            "triage",
            "--dir",
            "in_side",
            "--from",
            "/etc/hosts",
        ],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(from.contains("outside the allowed tool root"), "{from}");
    let at_root = config_call(&tools, &["pack", "build"])
        .await
        .unwrap_err()
        .to_string();
    assert!(at_root.contains("outside the tool root"), "{at_root}");
}

#[tokio::test]
async fn ux_reads_what_the_desktop_would_load() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dev = home
        .path()
        .join(crate::store::local::home::UX_PLUGINS_DIR_NAME)
        .join("notes");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(
        dev.join(crate::plugin::ux::DEV_ENTRY),
        "export default { id: 'notes', register() {} }\n",
    )
    .unwrap();
    std::fs::write(root.path().join("bad.js"), "eval('1')\n").unwrap();
    let (_node, _did, tools) =
        authoring_tool("ux-read", home.path(), root.path(), &["tools"], false).await;

    let list = config_json(&tools, &["ux", "list"]).await;
    assert_eq!(list["items"][0]["id"], "notes", "{list}");
    let module = config_json(&tools, &["ux", "module", "notes"]).await;
    assert_eq!(module["truncated"], false, "{module}");
    assert!(
        module["source"].as_str().unwrap().contains("register"),
        "{module}"
    );
    let lint = config_json(&tools, &["ux", "lint", "bad.js"]).await;
    assert_eq!(lint["admitted"], false, "{lint}");
    let outside = config_call(&tools, &["ux", "lint", "/etc/hosts"])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        outside.contains("outside the allowed tool root"),
        "{outside}"
    );
}

#[tokio::test]
async fn task_run_fires_a_task_now_and_reuses_target_id() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (node, _did, tools) = authoring_tool(
        "task-run",
        home.path(),
        root.path(),
        &["tools", "automation"],
        false,
    )
    .await;
    config_call(
        &tools,
        &[
            "task",
            "create",
            "review",
            "--set",
            r#"prompt_template="Review {{ args.what }}""#,
        ],
    )
    .await
    .unwrap();

    let text = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap()
        .call(
            json!({"argv":["task","run"],"target_id":"review","options":{"args":{"what":"the bill"}}})
                .to_string(),
        )
        .await
        .unwrap();
    let fired: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(fired["status"], "pending", "{fired}");
    assert_eq!(fired["duplicate"], false, "{fired}");
    let request_id = fired["request_id"].as_str().unwrap();
    let access = crate::store::config_client::ConfigAccess::local(node.clone());
    let query = format!(
        r#"query {{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{ content }} }}"#,
        crate::store::graphql::escape_graphql_string(request_id)
    );
    let response = access.execute(&query).await.unwrap();
    assert_eq!(
        response["data"]["AgentRequest"][0]["content"], "Review the bill",
        "{response}"
    );

    let missing = config_call(&tools, &["task", "run", "absent"])
        .await
        .unwrap_err()
        .to_string();
    assert!(missing.contains("task not found"), "{missing}");
}

#[tokio::test]
async fn page_help_discloses_detail_per_command_and_topic() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_node, _did, tools) = authoring_tool(
        "page-help",
        home.path(),
        root.path(),
        &["tools", "automation"],
        false,
    )
    .await;
    let help = |argv: Vec<&'static str>| {
        let tools = &tools;
        async move { config_call(tools, &argv).await.unwrap() }
    };

    let index = help(vec!["help"]).await;
    assert!(index.contains("page: author a desktop page"), "{index}");

    let page = help(vec!["help", "page"]).await;
    assert!(page.starts_with("page: "), "{page}");
    assert!(page.len() <= 2_600, "page help is {} chars", page.len());
    assert!(page.contains("Topics: "), "{page}");
    assert!(page.contains("\nNext: "), "{page}");

    let topic = help(vec!["help", "page", "data"]).await;
    assert!(topic.contains("ux[].data"), "{topic}");

    let lint = help(vec!["ux", "lint", "--help"]).await;
    assert!(lint.starts_with("ux lint FILE"), "{lint}");
    assert!(!lint.contains("Topics: "), "{lint}");

    let add = help(vec!["help", "pack", "add"]).await;
    assert!(add.starts_with("pack add PART ID"), "{add}");
    let build = help(vec!["pack", "build", "--help"]).await;
    assert!(build.contains("command sandbox"), "{build}");
    let run = help(vec!["task", "run", "--help"]).await;
    assert!(run.contains("options: args"), "{run}");
}
