use super::*;

/// A config tool for an agent with no shell and read-write file tools scoped
/// to `root`: the surface the pack loop has to work on alone.
async fn authoring_tool(
    label: &str,
    home: &std::path::Path,
    root: &std::path::Path,
    categories: &[&str],
) -> (Arc<EmbeddedNode>, String, Vec<Box<dyn ToolDyn>>) {
    let node = build_agent_node().await;
    let identity = node_identity(label);
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    let mut tool_config = config(categories);
    tool_config.agent_id = "setup".to_string();
    tool_config.enable_pack_install = true;
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
        authoring_tool("pack-loop", home.path(), root.path(), &["tools"]).await;

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

/// The pack with a page goes as far as its author's authority does: it lints
/// and builds, and the install names the operator's step instead.
#[tokio::test]
async fn a_pack_with_a_page_builds_but_its_install_stays_with_the_operator() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_node, _did, tools) =
        authoring_tool("pack-page", home.path(), root.path(), &["tools"]).await;

    config_json(&tools, &["pack", "scaffold", "board", "--page", "true"]).await;
    let lint = config_json(&tools, &["ux", "lint", "board/ux/board/plugin.js"]).await;
    assert_eq!(lint["admitted"], true, "{lint}");

    let built = config_json(&tools, &["pack", "build", "board"]).await;
    assert!(
        built["operator_action"]
            .as_str()
            .unwrap()
            .contains("gents pack install"),
        "{built}"
    );
    assert_eq!(
        built["next_call"]["argv"],
        json!(["help", "page"]),
        "{built}"
    );

    let error = config_call(
        &tools,
        &[
            "pack",
            "preview",
            "install",
            "./board",
            "--inference-slot",
            "worker=setup:inference",
        ],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("operator install"), "{error}");
}

/// A sealed plugins pack needs no compiler, so a page added to one reaches the
/// plugins installer; the operator's step has to hold there as well.
#[tokio::test]
async fn a_page_on_a_plugins_pack_stays_with_the_operator_too() {
    let (workspace, _artifact, _) = crate::pack::plugin_lifecycle::tests::fixture("1.0.0", false);
    let home = tempfile::tempdir().unwrap();
    let (_node, _did, tools) = authoring_tool(
        "pack-plugin-page",
        home.path(),
        workspace.path(),
        &["tools"],
    )
    .await;

    config_json(&tools, &["pack", "add", "page", "board", "--dir", "source"]).await;
    let built = config_json(&tools, &["pack", "build", "source"]).await;
    assert_eq!(
        built["next_call"]["argv"],
        json!(["help", "page"]),
        "{built}"
    );

    let preview = config_json(&tools, &["pack", "preview", "install", "./source"]).await;
    assert_eq!(preview["ready"], false, "{preview}");
    assert_eq!(preview["apply_with"], Value::Null, "{preview}");
    assert!(
        preview["installation_blockers"]
            .to_string()
            .contains("cannot authorize UI code"),
        "{preview}"
    );
    let digest = built["digest"].as_str().unwrap();
    config_call(&tools, &["pack", "install", "./source", "--digest", digest])
        .await
        .unwrap_err();
    assert!(
        crate::plugin::ux::list(home.path()).unwrap().is_empty(),
        "a refused install records no page"
    );
}

#[tokio::test]
async fn plugin_source_is_refused_with_the_operator_step() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_node, _did, tools) =
        authoring_tool("pack-compile", home.path(), root.path(), &["tools"]).await;
    config_json(
        &tools,
        &["pack", "scaffold", "tool_a", "--template", "plugin-tool"],
    )
    .await;
    for argv in [
        vec!["pack", "build", "tool_a"],
        vec!["pack", "preview", "install", "./tool_a"],
    ] {
        let error = config_call(&tools, &argv).await.unwrap_err().to_string();
        assert!(error.contains("needs a compiler"), "{argv:?}: {error}");
        assert!(error.contains("gents pack build"), "{argv:?}: {error}");
    }
    assert!(
        !root.path().join("tool_a/target").exists(),
        "nothing is mirrored for a compiler that will not run"
    );
}

#[tokio::test]
async fn authoring_paths_stay_inside_the_tool_root() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let (_node, _did, tools) =
        authoring_tool("pack-root", home.path(), root.path(), &["tools"]).await;
    config_json(
        &tools,
        &["pack", "scaffold", "inside", "--template", "assets"],
    )
    .await;
    let outside = config_call(
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
            "inside",
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
        authoring_tool("ux-read", home.path(), root.path(), &["tools"]).await;

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
    assert!(
        build.contains("refused when a plugin has source"),
        "{build}"
    );
    let run = help(vec!["task", "run", "--help"]).await;
    assert!(run.contains("options: args"), "{run}");
}
