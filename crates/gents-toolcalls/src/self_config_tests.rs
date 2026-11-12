use crate::test_support::self_config::{
    call_config_tool, config, config_call, node_identity, take_agent_tool, test_plugins,
    SelfConfigFixture,
};
use anyhow::Result;
use gents_documents::patch::SelfConfigTarget;
use gents_loop::tool::ToolDyn;
use gents_protocol::tool_names::CONFIG_TOOL_NAME;
use gents_tools::builtin::self_config::agent_management::agent_mutate;
use gents_tools::builtin::self_config::command::agents::agent_params;
use gents_tools::builtin::self_config::test_support::{
    agent_request, anchored_request, node_default_agent, tools_request,
};
use gents_tools::builtin::self_config::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[tokio::test]
async fn selected_configuration_requires_and_observes_user_grant() {
    let fixture = SelfConfigFixture::new("self-config-selected-grant").await;
    let tools = fixture.tools(&["tools"]);
    let update =
        |name| json!({"argv": ["tools", "update"], "set": {"display_name": name}}).to_string();
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    assert!(tool.call(update("Denied before grant")).await.is_err());

    fixture.grant_selected_configuration().await;
    tool.call(update("Allowed"))
        .await
        .expect("User grant permits the selected Agent update");

    let admin = gents_store::config_client::ConfigAccess::local_as(
        fixture.db.node.clone(),
        identity::Did::new(fixture.db.node_identity.did()).unwrap(),
    );
    let user = identity::Did::new(fixture.user_access.actor_did().unwrap()).unwrap();
    admin
        .grant_node_permission(
            &user,
            gents_store::config_client::NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    let response = fixture
        .user_access
        .execute("{ Agent { _docID } }")
        .await
        .unwrap();
    let doc_id = response["data"]["Agent"][0]["_docID"].as_str().unwrap();
    fixture
        .user_access
        .delete_actor_relationship(
            "Agent",
            doc_id,
            "config_writer",
            &identity::Did::new(&fixture.agent_did).unwrap(),
        )
        .await
        .unwrap();
    assert!(tool.call(update("Denied after revocation")).await.is_err());
}

async fn signed_pack_tool(
    label: &str,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    fixture.grant_agent_schema().await;
    let mut tool_config = config(&[]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    let tools = fixture.tools_with_plugins(&tool_config, plugins);
    (fixture, tools)
}

#[tokio::test]
async fn a_digest_the_store_does_not_hold_points_at_the_built_pack_file() {
    let (home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (_fixture, tools) = signed_pack_tool("pack-absent-digest", plugins).await;
    let absent = format!("sha256:{}", "ab".repeat(32));
    for verb in ["update", "install"] {
        let error = config_call(&tools, &["pack", verb, &absent, "--digest", &absent])
            .await
            .unwrap_err()
            .to_string();
        let failure: Value = serde_json::from_str(&error[error.find('{').unwrap()..]).unwrap();
        let message = failure["error"].as_str().unwrap();
        assert!(
            message.contains("not in this node's pack store"),
            "{failure}"
        );
        assert!(message.contains("./FILE.pack"), "{failure}");
        assert!(!message.contains("os error"), "{failure}");
        assert!(
            !message.contains(home.path().to_str().unwrap()),
            "{failure}"
        );
        assert_eq!(
            failure["recovery"]["next_call"]["argv"],
            json!(["pack", "list"]),
            "{failure}"
        );
    }
}

#[tokio::test]
async fn pack_list_pages_the_packs_the_home_store_holds() {
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (_fixture, tools) = signed_pack_tool("pack-list", plugins).await;
    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["source"], "store_and_installations");
    assert_eq!(page["page"]["total"], 1);
    assert_eq!(page["page"]["truncated"], false);
    assert_eq!(page["items"][0]["name"], "fixture/documents_fixture");
    assert_eq!(page["items"][0]["version"], Value::Null);
    assert_eq!(page["items"][0]["available_version"], "1.0.0");
    assert_eq!(
        page["items"][0]["other_stored_versions"][0]["version"],
        "1.0.0"
    );
    assert_eq!(page["items"][0]["installable"], true);
    assert_eq!(page["items"][0]["installed"], Value::Null);

    let after: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "list", "--cursor", "fixture/documents_fixture"],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(after["items"].as_array().unwrap().is_empty());

    let error = config_call(&tools, &["pack", "list", "--limit", "51"])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("between 1 and 50"), "{error}");

    let (_fixture, homeless) = signed_pack_tool("pack-list-homeless", test_plugins()).await;
    let page: Value =
        serde_json::from_str(&config_call(&homeless, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["page"]["total"], 0);
    assert!(page["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn pack_list_reports_a_damaged_archive_as_an_error_row() {
    let (home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (_fixture, tools) = signed_pack_tool("pack-list-damaged", plugins).await;
    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    let digest = page["items"][0]["artifact_digest"].as_str().unwrap();
    let path = gents_pack::pack::store::PackStore::new(home.path())
        .path(digest)
        .unwrap();
    std::fs::write(path, b"not a pack archive").unwrap();
    std::fs::remove_dir_all(home.path().join("packs").join("unpacked")).unwrap();

    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["page"]["returned"], 1);
    assert_eq!(page["items"][0]["name"], "fixture/documents_fixture");
    assert_eq!(page["items"][0]["installable"], false);
    assert!(page["items"][0]["error"]
        .as_str()
        .unwrap()
        .contains("could not be opened"));
}

static PACK_REGISTRY_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn pack_update_without_a_version_asks_the_registry_and_fails_loudly_offline() {
    let _registry_lock = PACK_REGISTRY_ENV.lock().await;
    let _registry =
        gents_store::test_support::EnvVarGuard::set("GENTS_REGISTRY", "http://127.0.0.1:9");
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (_fixture, tools) = signed_pack_tool("pack-update-offline", plugins).await;
    let error = config_call(
        &tools,
        &["pack", "preview", "update", "fixture/documents_fixture"],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("the newest version of fixture/documents_fixture could not be looked up"),
        "{error}"
    );
    let error = config_call(
        &tools,
        &[
            "pack",
            "preview",
            "update",
            "fixture/documents_fixture@1.0.0",
        ],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("is not installed"), "{error}");
}

#[tokio::test]
async fn pack_install_refuses_a_pack_shipped_job_block() {
    let (_guard, dir) =
        gents_pack::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let config_path = dir.join("pack_config.json");
    let mut pack_config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    pack_config["tools"][0]["integrations"] =
        json!({"plugins": [{"plugin": "acme/runner", "job": {"agent_targets": ["wf-run"]}}]});
    pack_config["agent_targets"] = json!([{"target_id": "wf-run", "target_node_did": "did:key:foreign", "agent_did": "fixture-worker", "name": "wf-run"}]);
    std::fs::write(&config_path, serde_json::to_vec(&pack_config).unwrap()).unwrap();
    let (_home, plugins) = crate::test_support::home_with_pack_dir(&dir);
    let (fixture, tools) = signed_pack_tool("pack-job-block", plugins.clone()).await;
    let slot = format!("worker={}:inference", fixture.agent_did());
    let argv = [
        "pack",
        "preview",
        "install",
        "fixture/documents_fixture",
        "--inference-slot",
        &slot,
    ];
    let error = format!("{:#}", config_call(&tools, &argv).await.unwrap_err());
    assert!(error.contains("operator install"), "{error}");

    // Under pack authority the block is judged as an operator install's: the
    // refusal that remains is the plugin this fixture never installs.
    let mut tool_config = config(&[]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    tool_config.enable_pack_authority = true;
    let tools = fixture.tools_with_plugins(&tool_config, plugins);
    let error = format!("{:#}", config_call(&tools, &argv).await.unwrap_err());
    assert!(!error.contains("operator install"), "{error}");
    assert!(error.contains("acme/runner"), "{error}");
}

#[tokio::test]
async fn pack_install_refuses_tools_naming_an_unresolved_plugin() {
    let (_guard, dir) =
        gents_pack::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let config_path = dir.join("pack_config.json");
    let mut pack_config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    pack_config["tools"][0]["integrations"] = json!({"plugins": [{"plugin": "acme/missing"}]});
    std::fs::write(&config_path, serde_json::to_vec(&pack_config).unwrap()).unwrap();
    let (_home, plugins) = crate::test_support::home_with_pack_dir(&dir);
    let (fixture, tools) = signed_pack_tool("pack-unresolved-plugin", plugins).await;
    let slot = format!("worker={}:inference", fixture.agent_did());
    let error = format!(
        "{:#}",
        config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                "fixture/documents_fixture",
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap_err()
    );
    assert!(
        error.contains("acme/missing") && error.contains("fixture-worker-tools"),
        "{error}"
    );
    let written = fixture
        .user_access
        .execute(r#"{ Tools(filter: {tools_id: {_eq: "fixture-worker-tools"}}) { tools_id } }"#)
        .await
        .unwrap();
    assert!(written["data"]["Tools"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn config_tools_cannot_self_grant_pack_install() {
    let fixture = SelfConfigFixture::new("pack-self-grant").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["tools"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let error = tool
        .call(
            serde_json::json!({
                "argv": ["tools", "edit", "--set", format!("self_config={}", json!({
                        "enable_self_config": true,
                        "enable_pack_install": true
                    }))]
            })
            .to_string(),
        )
        .await
        .expect_err("pack install cannot be self-granted");
    assert!(
        error.to_string().contains("cannot be self-granted"),
        "{error:#}"
    );
}

#[tokio::test]
async fn config_tools_unrelated_edit_on_granted_tools_is_accepted() {
    let fixture = SelfConfigFixture::new("granted-tools-edit").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let tools = fixture.tools(&["tools"]);

    let edited = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--set".into(),
            r#"agents={"enabled":true}"#.into(),
        ],
    )
    .await
    .expect("an edit that raises no grant is accepted without holding it");
    assert_eq!(
        serde_json::from_str::<Value>(&edited).unwrap()["committed"],
        true
    );
    call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--set".into(),
            format!(
                "self_config={}",
                json!({"enable_self_config": true, "enable_pack_install": true,
                       "self_config_preview": true})
            ),
        ],
    )
    .await
    .expect("keeping a stored grant while rewriting its group is not a raise");
}

#[tokio::test]
async fn config_tools_holder_may_grant_pack_install_to_a_sibling() {
    let fixture = SelfConfigFixture::new("pack-holder-sibling").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let sibling = fixture.install_sibling("sibling").await;
    fixture.grant_node_reader().await;
    fixture.grant_sibling_configuration(&sibling).await;
    let mut tool_config = config(&["node", "tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    let tools = fixture.tools_with_config(&tool_config);
    let applied = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            sibling.clone(),
            "--set".into(),
            r#"self_config={"enable_self_config":true,"enable_pack_install":true}"#.into(),
        ],
    )
    .await
    .expect("a holder may grant pack installation to a sibling");
    assert_eq!(
        serde_json::from_str::<Value>(&applied).unwrap()["committed"],
        true
    );
}

fn argv_of(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn config_error_message(error: gents_loop::tool::ToolError) -> String {
    let gents_loop::tool::ToolError::ToolCallError(error) = error else {
        panic!("missing typed config error: {error}");
    };
    let envelope: Value =
        serde_json::from_str(&error.to_string()).expect("a config refusal is a JSON envelope");
    envelope["error"]
        .as_str()
        .expect("the envelope carries an error message")
        .to_owned()
}

async fn config_call_message(
    tools: &[Box<dyn gents_loop::tool::ToolDyn>],
    argv: Vec<String>,
) -> Result<String, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    tool.call(json!({ "argv": argv }).to_string())
        .await
        .map_err(config_error_message)
}

struct ReselectionFixture {
    _fixture: SelfConfigFixture,
    sibling: String,
    granted: String,
    granted2: String,
    tools: Vec<Box<dyn gents_loop::tool::ToolDyn>>,
}

impl std::ops::Deref for ReselectionFixture {
    type Target = [Box<dyn gents_loop::tool::ToolDyn>];
    fn deref(&self) -> &Self::Target {
        &self.tools
    }
}

async fn reselection_tools(label: &str, hold_pack_install: bool) -> ReselectionFixture {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    // Re-pointing re-keys the selected Agents' tool sources, which native NAC
    // admits only for an actor holding their permissions, as the Engineer does.
    fixture.grant_engineer_tool_sources().await;
    fixture.grant_node_reader().await;
    let sibling = fixture.install_sibling("sibling").await;
    let granted = fixture.install_sibling("granted").await;
    let granted2 = fixture.install_sibling("granted-2").await;
    for target in [&sibling, &granted, &granted2] {
        fixture.grant_sibling_configuration(target).await;
    }
    fixture.grant_pack_install_to(&granted).await;
    fixture.grant_pack_install_to(&granted2).await;
    if hold_pack_install {
        fixture.grant_pack_install().await;
    }
    let mut tool_config = config(&["node", "agent", "tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = hold_pack_install;
    let tools = fixture.tools_with_config(&tool_config);
    ReselectionFixture {
        _fixture: fixture,
        sibling,
        granted,
        granted2,
        tools,
    }
}

#[tokio::test]
async fn context_reselect_cannot_acquire_grants() {
    let tools = reselection_tools("context-reselect", false).await;
    let granted_tools = format!("{}:tools", tools.granted);
    let selection = format!("tools_id=\"{granted_tools}\"");
    for target in [None, Some(tools.sibling.as_str())] {
        let mut argv = argv_of(&["agent", "context", "edit"]);
        if let Some(target) = target {
            argv.extend(argv_of(&["--agent", target]));
        }
        argv.extend(argv_of(&["--set", &selection]));
        let refused = config_call_message(&tools, argv)
            .await
            .expect_err("re-pointing a Context at granted Tools must not acquire the grant");
        assert!(refused.contains(&format!("tools_id {granted_tools:?} selects Tools carrying an operator grant this agent does not hold")), "{target:?}: {refused}");
        assert!(
            refused.contains("cannot be self-granted"),
            "{target:?}: {refused}"
        );
    }
    let missing = call_config_tool(
        &tools,
        argv_of(&["agent", "context", "edit", "--set", r#"tools_id="missing""#]),
    )
    .await
    .expect_err("a missing Tools reference is still refused");
    assert!(
        !missing.contains("operator grant"),
        "the reference validator refuses a missing document: {missing}"
    );
    let holder = reselection_tools("context-reselect-holder", true).await;
    let selection = format!("tools_id=\"{}:tools\"", holder.granted);
    call_config_tool(
        &holder,
        argv_of(&["agent", "context", "edit", "--set", &selection]),
    )
    .await
    .expect("an agent holding the grant may select Tools that carry it");
}

#[tokio::test]
async fn agent_reselect_cannot_acquire_grants() {
    let tools = reselection_tools("agent-reselect", false).await;
    let granted_context = format!("{}:context", tools.granted);
    let selection = format!("context_id=\"{granted_context}\"");
    for target in [tools._fixture.agent_did(), tools.sibling.as_str()] {
        let refused = config_call_message(
            &tools,
            argv_of(&["agent", "edit", target, "--set", &selection]),
        )
        .await
        .expect_err("re-pointing an Agent at a granted chain must not acquire the grant");
        assert!(
            refused.contains("enable_pack_install cannot be self-granted"),
            "{target}: {refused}"
        );
    }
    let holder = reselection_tools("agent-reselect-holder", true).await;
    let selection = format!("context_id=\"{}:context\"", holder.granted);
    call_config_tool(
        &holder,
        argv_of(&["agent", "edit", &holder.sibling, "--set", &selection]),
    )
    .await
    .expect("an agent holding the grant may select a chain that carries it");
}

#[tokio::test]
async fn context_reselect_between_granted_tools_is_accepted() {
    let tools = reselection_tools("context-between-granted", false).await;
    let selection = format!("tools_id=\"{}:tools\"", tools.granted2);
    call_config_tool(
        &tools,
        argv_of(&[
            "agent",
            "context",
            "edit",
            "--agent",
            &tools.granted,
            "--set",
            &selection,
        ]),
    )
    .await
    .expect("re-pointing between granted Tools raises nothing");
}

#[tokio::test]
async fn context_create_selecting_granted_tools_requires_the_grant() {
    let tools = reselection_tools("context-create-granted", false).await;
    let selection = format!("tools_id=\"{}:tools\"", tools.granted);
    let create = argv_of(&["context", "create", "fresh:context", "--set", &selection]);
    let refused = config_call_message(&tools, create)
        .await
        .expect_err("a new Context must not select Tools whose grant the agent does not hold");
    assert!(
        refused.contains("selects Tools carrying an operator grant this agent does not hold"),
        "{refused}"
    );
    let holder = reselection_tools("context-create-holder", true).await;
    let selection = format!("tools_id=\"{}:tools\"", holder.granted);
    call_config_tool(
        &holder,
        argv_of(&["context", "create", "fresh:context", "--set", &selection]),
    )
    .await
    .expect("an agent holding the grant may create a Context that selects it");
}

#[tokio::test]
async fn automation_rejects_invalid_template_before_publication_and_can_recover() {
    let fixture = SelfConfigFixture::new("template-config").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["automation"]);
    for verb in ["preview", "edit"] {
        let error = call_config_tool(
            &tools,
            vec![
                "automation".into(),
                verb.into(),
                "task".into(),
                "template-check".into(),
                "--set".into(),
                "prompt_template=\"{{.message}}\"".into(),
            ],
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("MiniJinja"), "{error:#}");
        let rows = fixture
            .user_access
            .execute("{ Task { task_id } }")
            .await
            .unwrap();
        assert!(rows["data"]["Task"].as_array().unwrap().is_empty());
    }
    call_config_tool(
        &tools,
        vec![
            "automation".into(),
            "edit".into(),
            "task".into(),
            "template-check".into(),
            "--set".into(),
            "prompt_template=\"Process {{ doc.message }}\"".into(),
        ],
    )
    .await
    .unwrap();
    let rows = fixture
        .user_access
        .execute("{ Task { task_id prompt_template } }")
        .await
        .unwrap();
    let data = &rows["data"];
    assert_eq!(data["Task"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["Task"][0]["prompt_template"],
        "Process {{ doc.message }}"
    );
}

#[tokio::test]
async fn automation_rejects_a_count_field_the_runtime_cannot_read_and_can_recover() {
    let fixture = SelfConfigFixture::new("event-source-config").await;
    fixture.grant_selected_configuration().await;
    fixture
        .add_collection("type SelfConfigProbe { batch: String flag: Boolean total: Int }")
        .await;
    let tools = fixture.tools(&["automation"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let set = |count_field: &str| {
        json!({
            "argv": ["automation", "edit", "event-source"],
            "target_id": "probe",
            "set": {
                "source_collection": "SelfConfigProbe",
                "correlation_field": "batch",
                "group": {"expected_count": {"source_field": count_field}}
            }
        })
        .to_string()
    };
    for verb in ["preview", "edit"] {
        let argv = json!({
            "argv": ["automation", verb, "event-source"],
            "target_id": "probe",
            "set": {
                "source_collection": "SelfConfigProbe",
                "correlation_field": "batch",
                "group": {"expected_count": {"source_field": "flag"}}
            }
        })
        .to_string();
        let error = tool.call(argv).await.unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("cannot carry the count"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("Boolean"), "{diagnostic}");
        let rows = fixture
            .user_access
            .execute("{ EventSource { event_source_id } }")
            .await
            .unwrap();
        assert!(
            rows["data"]["EventSource"].as_array().unwrap().is_empty(),
            "a refused event source publishes no row"
        );
    }
    tool.call(set("total")).await.unwrap();
    let rows = fixture
        .user_access
        .execute("{ EventSource { event_source_id group } }")
        .await
        .unwrap();
    let rows = rows["data"]["EventSource"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["group"]["expected_count"]["source_field"], "total");
}

#[tokio::test]
async fn skill_import_previews_without_writes_and_requires_file_authority() {
    let fixture = SelfConfigFixture::new("skill-import").await;
    fixture.grant_selected_configuration().await;
    let root = tempfile::tempdir().unwrap();
    fixture.publish_workspace_root(root.path()).await;
    let file = root.path().join("SKILL.md");
    std::fs::write(
        &file,
        "---\nname: Review\ndescription: Review code\n---\nCheck the diff carefully.",
    )
    .unwrap();
    let mut grants = config(&["tools", "agent"]);
    grants.agent_did = fixture.agent_did().to_owned();
    grants.preview = true;
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let denied_tools = fixture.tools_with_config(&grants);
    let denied = call_config_tool(
        &denied_tools,
        command(&["skill", "import", "review", file.to_str().unwrap()]),
    )
    .await
    .unwrap_err();
    assert!(denied.contains("file read permission"), "{denied}");
    grants.process_ceiling = gents_tools::surface::SelfConfigProcessCeiling {
        file_mode: gents_documents::FileToolMode::ReadOnly,
        bash_mode: gents_documents::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(), "files": {"mode": "ReadOnly"}
            })),
        )],
    ))
    .await
    .unwrap();
    let tools = fixture.tools_with_config(&grants);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("SKILL.md"),
        "Outside the effective root.",
    )
    .unwrap();
    let rejected = call_config_tool(
        &tools,
        command(&[
            "skill",
            "import",
            "outside",
            outside.path().to_str().unwrap(),
        ]),
    )
    .await
    .unwrap_err();
    assert!(
        rejected.contains("outside the allowed tool root"),
        "{rejected}"
    );
    assert!(
        call_config_tool(&tools, command(&["skill", "get", "outside"]))
            .await
            .is_err()
    );
    let preview = call_config_tool(
        &tools,
        command(&[
            "skill",
            "preview",
            "import",
            "review",
            file.to_str().unwrap(),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&preview).unwrap()["committed"],
        false
    );
    assert!(
        call_config_tool(&tools, command(&["skill", "get", "review"]))
            .await
            .is_err()
    );
    call_config_tool(
        &tools,
        command(&["skill", "import", "review", root.path().to_str().unwrap()]),
    )
    .await
    .unwrap();
    assert!(call_config_tool(
        &tools,
        command(&["skill", "import", "review", file.to_str().unwrap()])
    )
    .await
    .is_err());
    let read = call_config_tool(&tools, command(&["skill", "get", "review"]))
        .await
        .unwrap();
    assert!(read.contains("Check the diff carefully."));
    assert!(read.contains("source_directory"));
    assert!(read.contains(root.path().canonicalize().unwrap().to_str().unwrap()));
    call_config_tool(
        &tools,
        command(&[
            "agent",
            "context",
            "edit",
            "--set",
            "skill_ids=[\"review\"]",
        ]),
    )
    .await
    .unwrap();
    let effective = core
        .with_process_ceiling(grants.process_ceiling)
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    assert_eq!(effective["context"]["skill_ids"], json!(["review"]));
    assert_eq!(
        effective["runtime_effective"]["effective"]["file_mode"],
        "ReadOnly"
    );
    assert_eq!(
        effective["runtime_effective"]["effective"]["bash_mode"],
        "Off"
    );
}

#[tokio::test]
async fn configuration_discovery_is_read_only_root_bounded_and_sanitized() {
    let fixture = SelfConfigFixture::new("configuration-discovery").await;
    fixture.grant_selected_configuration().await;
    let root = tempfile::tempdir().unwrap();
    fixture.publish_workspace_root(root.path()).await;
    let source = root.path().join("synthetic-codex");
    std::fs::create_dir_all(&source).unwrap();
    let marker = root.path().join("SHOULD_NEVER_RUN");
    let source_config =
        include_str!("../../gents/tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &marker.to_string_lossy());
    std::fs::write(source.join("config.toml"), source_config).unwrap();

    let mut grants = config(&["tools"]);
    grants.agent_did = fixture.agent_did().to_owned();
    let denied_tools = fixture.tools_with_config(&grants);
    let command = |args: &[&str]| args.iter().map(|value| (*value).to_owned()).collect();
    let help = call_config_tool(&denied_tools, command(&["help", "discovery"]))
        .await
        .unwrap();
    assert!(help.starts_with("discovery: "), "{help}");
    let legacy = call_config_tool(&denied_tools, command(&["discover", "scan"]))
        .await
        .unwrap_err();
    assert!(
        legacy.contains("unknown config resource or command"),
        "{legacy}"
    );
    let denied = call_config_tool(
        &denied_tools,
        command(&[
            "discovery",
            "scan",
            "--source",
            "codex-user",
            "codex",
            "user",
            source.to_str().unwrap(),
        ]),
    )
    .await
    .unwrap_err();
    assert!(denied.contains("file read permission"), "{denied}");
    let unsupported = call_config_tool(
        &denied_tools,
        command(&[
            "discovery",
            "scan",
            "--source",
            "s",
            "grok",
            "user",
            source.to_str().unwrap(),
        ]),
    )
    .await
    .unwrap_err();
    assert!(
        unsupported.contains("expected claude or codex"),
        "{unsupported}"
    );

    grants.process_ceiling = gents_tools::surface::SelfConfigProcessCeiling {
        file_mode: gents_documents::FileToolMode::ReadOnly,
        bash_mode: gents_documents::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(), "files": {"mode": "ReadOnly"}
            })),
        )],
    ))
    .await
    .unwrap();
    let before = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    let tools = fixture.tools_with_config(&grants);
    let outside = tempfile::tempdir().unwrap();
    let outside_error = call_config_tool(
        &tools,
        command(&[
            "discovery",
            "scan",
            "--source",
            "outside",
            "codex",
            "user",
            outside.path().to_str().unwrap(),
        ]),
    )
    .await
    .unwrap_err();
    assert!(
        outside_error.contains("outside the allowed tool root"),
        "{outside_error}"
    );
    let output = call_config_tool(
        &tools,
        command(&[
            "discovery",
            "scan",
            "--source",
            "codex-user",
            "codex",
            "user",
            source.to_str().unwrap(),
        ]),
    )
    .await
    .unwrap();
    let after = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();

    assert_eq!(before, after, "discovery must not mutate configuration");
    assert!(!marker.exists(), "discovery must not execute MCP commands");
    assert!(!output.contains("FAKE_DISCOVERY_SECRET_123"));
    assert!(!output.contains("SHOULD_NEVER_RUN"));
    let inventory: gents_tools::builtin::self_config::configuration_discovery::ConfigurationDiscoveryInventory =
        serde_json::from_str(&output).unwrap();
    assert_eq!(inventory.schema_version, 1);
    assert!(inventory.items.iter().any(|item| {
        item.category
            == gents_tools::builtin::self_config::configuration_discovery::DiscoveryCategory::RemoteTool
            && item.display_label == "shared"
    }));
}

#[tokio::test]
async fn setup_discovery_clarification_apply_and_verification_preserve_disabled_settings() {
    use gents_tools::builtin::self_config::configuration_discovery::{
        ConfigurationDiscoveryInventory, DiscoveryCategory, DiscoveryItemState,
        DiscoverySourceOutcome, MappingSupportLevel,
    };

    let fixture = SelfConfigFixture::new("discovery-apply").await;
    fixture.grant_selected_configuration().await;
    let node = fixture.db.node.clone();
    let owner = fixture.node_did().to_owned();
    let identity = fixture.agent.clone();
    let agent_did = fixture.agent_did().to_owned();
    let runtime_access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(&owner).unwrap(),
    );
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(&agent_did).unwrap(),
        identity.as_ref(),
    )
    .unwrap();

    let root = tempfile::tempdir().unwrap();
    fixture.publish_workspace_root(root.path()).await;
    let user_root = root.path().join("synthetic-user/.codex");
    let project_root = root.path().join("synthetic-project");
    std::fs::create_dir_all(&user_root).unwrap();
    std::fs::create_dir_all(project_root.join(".codex")).unwrap();
    let marker = root.path().join("SHOULD_NEVER_RUN");
    std::fs::write(
        user_root.join("config.toml"),
        include_str!("../../gents/tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &marker.to_string_lossy()),
    )
    .unwrap();
    std::fs::write(
        user_root.join("AGENTS.md"),
        include_str!("../../gents/tests/fixtures/configuration_discovery/codex-user/AGENTS.md"),
    )
    .unwrap();
    std::fs::write(
        project_root.join(".codex/config.toml"),
        include_str!(
            "../../gents/tests/fixtures/configuration_discovery/project/.codex/config.toml"
        ),
    )
    .unwrap();
    std::fs::write(
        project_root.join("AGENTS.md"),
        include_str!("../../gents/tests/fixtures/configuration_discovery/project/AGENTS.md"),
    )
    .unwrap();

    let mut grants = config(&["tools", "agent", "backend"]);
    grants.preview = true;
    grants.agent_did = agent_did.clone();
    grants.process_ceiling = gents_tools::surface::SelfConfigProcessCeiling {
        file_mode: gents_documents::FileToolMode::ReadOnly,
        bash_mode: gents_documents::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(),
                "files": {"mode": "ReadOnly"}
            })),
        )],
    ))
    .await
    .unwrap();
    let tools = fixture.tools_with_config(&grants);
    let scan = vec![
        "discovery".into(),
        "scan".into(),
        "--source".into(),
        "fixture-user-codex".into(),
        "codex".into(),
        "user".into(),
        user_root.to_string_lossy().into_owned(),
        "--source".into(),
        "fixture-project-codex".into(),
        "codex".into(),
        "project".into(),
        project_root.to_string_lossy().into_owned(),
    ];

    let discovered: ConfigurationDiscoveryInventory =
        serde_json::from_str(&call_config_tool(&tools, scan.clone()).await.unwrap()).unwrap();
    assert!(discovered
        .sources
        .iter()
        .all(|source| source.outcome == DiscoverySourceOutcome::Complete));
    assert!(!discovered.conflicts.is_empty());
    assert!(
        !marker.exists(),
        "discovery must not execute configured commands"
    );
    let serialized = serde_json::to_string(&discovered).unwrap();
    assert!(!serialized.contains("FAKE_DISCOVERY_SECRET_123"));
    assert!(!serialized.contains("SHOULD_NEVER_RUN"));

    let approved = discovered
        .items
        .iter()
        .find(|item| {
            item.source_id == "fixture-project-codex"
                && item.category == DiscoveryCategory::Instruction
        })
        .expect("project instruction candidate");
    let disabled = discovered
        .items
        .iter()
        .find(|item| {
            item.source_id == "fixture-project-codex"
                && item.category == DiscoveryCategory::RemoteTool
        })
        .expect("disabled project remote tool");
    assert_eq!(disabled.state, DiscoveryItemState::Disabled);
    assert_eq!(disabled.mapping.level, MappingSupportLevel::Partial);
    assert!(!disabled.mapping.reasons.is_empty());
    assert!(
        discovered
            .items
            .iter()
            .filter(|item| { item.category == DiscoveryCategory::ModelPreference })
            .count()
            >= 2
    );

    let command = |args: &[&str]| args.iter().map(|value| (*value).to_owned()).collect();
    let context_before = call_config_tool(
        &tools,
        command(&["agent", "context", "get", "--agent", &agent_did]),
    )
    .await
    .unwrap();
    async fn runtime_backend(
        access: &gents_store::config_client::ConfigAccess,
        owner: &str,
        id: &str,
    ) -> Value {
        access
            .transact_readonly("test.self_config.backend", |txn| {
                Box::pin(async move {
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::InferenceBackend,
                        owner,
                        id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("runtime backend")
            .1
    }
    let backend_id = format!("{agent_did}:backend");
    let backend_before = runtime_backend(&runtime_access, &owner, &backend_id).await;
    assert!(!backend_before
        .to_string()
        .contains("FAKE_DISCOVERY_SECRET_123"));
    let services_before =
        gents_tools::mcp::registry::configured_mcp_services_on(&agent_access, &owner)
            .await
            .unwrap();

    let approved_prompt = format!(
        "Review repository changes and report evidence. Source: {}.",
        approved.relative_source_file
    );
    let prompt_patch = format!(
        "system_prompt={}",
        serde_json::to_string(&approved_prompt).unwrap()
    );
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "context".into(),
                "preview".into(),
                "--agent".into(),
                agent_did.clone(),
                "--set".into(),
                prompt_patch.clone(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(
        call_config_tool(
            &tools,
            command(&["agent", "context", "get", "--agent", &agent_did]),
        )
        .await
        .unwrap(),
        context_before,
        "preview must not apply the proposal"
    );

    call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
            agent_did.clone(),
            "--set".into(),
            prompt_patch,
        ],
    )
    .await
    .unwrap();

    let verified: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            command(&["agent", "context", "get", "--agent", &agent_did]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["document"]["system_prompt"], approved_prompt);
    assert_eq!(
        runtime_backend(&runtime_access, &owner, &backend_id).await,
        backend_before,
        "discovery-backed setup must not modify operator-owned inference credentials"
    );
    assert_eq!(
        gents_tools::mcp::registry::configured_mcp_services_on(&agent_access, &owner)
            .await
            .unwrap()
            .len(),
        services_before.len(),
        "a disabled discovered remote tool must not be configured or enabled"
    );

    let verified_discovery: ConfigurationDiscoveryInventory =
        serde_json::from_str(&call_config_tool(&tools, scan).await.unwrap()).unwrap();
    let verified_disabled = verified_discovery
        .items
        .iter()
        .find(|item| item.item_id == disabled.item_id)
        .expect("disabled item survives verification scan");
    assert_eq!(verified_disabled.state, DiscoveryItemState::Disabled);
    assert_eq!(verified_disabled.mapping, disabled.mapping);
}

#[tokio::test]
async fn datastore_preview_create_and_sparse_edit_use_owned_patch_path() {
    let fixture = SelfConfigFixture::new("datastore-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_engineer_tool_sources().await;
    let tools = fixture.tools(&["tools"]);
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let expected_help = call_config_tool(&tools, command(&["help", "datastore"]))
        .await
        .unwrap();
    assert_eq!(
        call_config_tool(&tools, command(&["datastore", "--help"]))
            .await
            .unwrap(),
        expected_help
    );
    let create_help = call_config_tool(&tools, command(&["datastore", "preview", "create", "-h"]))
        .await
        .unwrap();
    assert!(
        create_help.starts_with("datastore create: "),
        "{create_help}"
    );
    assert!(
        create_help.contains("DatastoreToolSurface fields"),
        "{create_help}"
    );
    assert!(!create_help.contains("Recipe:"), "{create_help}");
    assert_eq!(
        call_config_tool(&tools, command(&["datastore", "help", "create"]))
            .await
            .unwrap(),
        create_help
    );
    for args in [
        vec!["datastore", "create", "--set", "display_name=\"Test\""],
        vec![
            "datastore",
            "preview",
            "create",
            "--set",
            "display_name=\"Test\"",
        ],
    ] {
        let error = call_config_tool(&tools, command(&args)).await.unwrap_err();
        assert!(error.contains("missing SURFACE_ID"), "{error}");
    }
    let preview = call_config_tool(
        &tools,
        command(&[
            "datastore",
            "preview",
            "create",
            "jobs",
            "--set",
            "display_name=\"Jobs\"",
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&preview).unwrap()["committed"],
        false
    );
    let config_tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let named_preview = config_tool
        .call(
            json!({
                "argv":["datastore","preview","create"],
                "target_id":"jobs",
                "set":{"display_name":"Jobs"}
            })
            .to_string(),
        )
        .await
        .unwrap();
    let mut mailbox_args = json!({
        "argv":["datastore","preview","create"],
        "target_id":"host-attention",
        "options":{"mailbox":{"identity":{"mode":"condition","key":"host-health"},"kind":"flag","action":"ack"}},
        "set":{"enabled":true}
    });
    let mailbox_preview: Value =
        serde_json::from_str(&config_tool.call(mailbox_args.to_string()).await.unwrap()).unwrap();
    assert_eq!(mailbox_preview["committed"], false);
    assert!(
        call_config_tool(&tools, command(&["datastore", "get", "host-attention"]))
            .await
            .is_err()
    );
    mailbox_args["argv"] = json!(["datastore", "create"]);
    config_tool.call(mailbox_args.to_string()).await.unwrap();
    let mailbox_created: Value = serde_json::from_str(
        &call_config_tool(&tools, command(&["datastore", "get", "host-attention"]))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mailbox_created["document"]["entries"]["entries"][0]["tool_name"],
        "file_mailbox_item"
    );
    assert_eq!(
        mailbox_created["document"]["entries"]["entries"][0]["notification"]["identity"]["key"],
        "host-health"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&named_preview).unwrap(),
        serde_json::from_str::<Value>(&preview).unwrap()
    );
    let error = config_tool
        .call(json!({"argv":["datastore","create"],"set":{"display_name":"Test"}}).to_string())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing SURFACE_ID"));
    assert!(
        call_config_tool(&tools, command(&["datastore", "get", "jobs"]))
            .await
            .is_err()
    );
    call_config_tool(
        &tools,
        command(&[
            "datastore",
            "create",
            "jobs",
            "--set",
            "display_name=\"Jobs\"",
        ]),
    )
    .await
    .unwrap();
    assert!(call_config_tool(
        &tools,
        command(&[
            "datastore",
            "create",
            "jobs",
            "--set",
            "display_name=\"Duplicate\"",
        ])
    )
    .await
    .is_err());
    assert!(call_config_tool(
        &tools,
        command(&["datastore", "edit", "jobs", "--set", "node_did=\"foreign\"",])
    )
    .await
    .is_err());
    call_config_tool(
        &tools,
        command(&["datastore", "edit", "jobs", "--set", "tags=[\"user:keep\"]"]),
    )
    .await
    .unwrap();
    let actual = call_config_tool(&tools, command(&["datastore", "get", "jobs"]))
        .await
        .unwrap();
    assert!(actual.contains("Jobs"));
    assert!(actual.contains("user:keep"));
    assert!(!actual.contains("Duplicate"));
    fixture
        .add_collection("type ConfiguredWork { message: String }")
        .await;
    let entries = r#"entries=[{"tool_name":"submit_work","collection":"ConfiguredWork","fields":[{"name":"message","required":true}]}]"#;
    call_config_tool(
        &tools,
        command(&["datastore", "edit", "jobs", "--set", entries]),
    )
    .await
    .unwrap();
    let entry_read = call_config_tool(&tools, command(&["datastore", "get", "jobs"]))
        .await
        .unwrap();
    let entry_read: Value = serde_json::from_str(&entry_read).unwrap();
    assert_eq!(
        entry_read["document"]["entries"]["entries"][0]["tool_name"],
        "submit_work"
    );
    assert!(call_config_tool(
        &tools,
        command(&[
            "datastore",
            "preview",
            "edit",
            "jobs",
            "--set",
            r#"entries=[{"tool_name":"submit_work","collection":"ConfiguredWork","fields":[{"name":"bad-name"}]}]"#
        ])
    )
    .await
    .is_err());
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "datastore".into(),
            Some(json!({
                "datastore_tool_surface_ids": ["jobs"]
            })),
        )],
    ))
    .await
    .unwrap();
    core.apply(agent_request(
        &core,
        vec![(
            "tags".into(),
            Some(json!([
                gents_tools::builtin::self_config::ENGINEER_AGENT_TAG
            ])),
        )],
    ))
    .await
    .unwrap();
    for verb in [
        &["datastore", "preview", "edit"][..],
        &["datastore", "edit"],
    ] {
        call_config_tool(
            &tools,
            command(&[verb, &["jobs", "--set", "display_name=\"Engineer jobs\""]].concat()),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn structured_config_preview_and_apply_round_trip_literal_prompt() {
    let fixture = SelfConfigFixture::new("structured-config").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["agent"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let read = json!({"argv":["agent","context","get"]}).to_string();
    let before = tool.call(read.clone()).await.unwrap();
    let prompt = "Quoted \"text\"\nActual newline; literal \\n; Unicode λ; {{ doc.message }}";
    let mut request = json!({"argv":["agent","context","preview"],"set":{"system_prompt":prompt}});
    let preview: Value =
        serde_json::from_str(&tool.call(request.to_string()).await.unwrap()).unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["config_execution"]["mutation_entered"], false);
    assert_eq!(tool.call(read.clone()).await.unwrap(), before);
    request["argv"][2] = json!("edit");
    let applied: Value =
        serde_json::from_str(&tool.call(request.to_string()).await.unwrap()).unwrap();
    assert_eq!(applied["config_execution"]["mutation_entered"], true);
    let after: Value = serde_json::from_str(&tool.call(read.clone()).await.unwrap()).unwrap();
    assert_eq!(after["document"]["system_prompt"], prompt);
    request["set"] = json!({"node_did":"foreign"});
    assert!(tool.call(request.to_string()).await.is_err());
    assert_eq!(
        serde_json::from_str::<Value>(&tool.call(read).await.unwrap()).unwrap(),
        after
    );
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

#[tokio::test]
async fn outcome_schema_error_returns_an_executable_schema_recovery() {
    use gents_loop::tool::Tool;
    let fixture = SelfConfigFixture::new("schema-recovery").await;
    fixture.grant_selected_configuration().await;
    fixture
        .add_collection("type Delivery { body: String }")
        .await;
    let admin = gents_store::config_client::ConfigAccess::local_as(
        fixture.db.node.clone(),
        identity::Did::new(fixture.db.node_identity.did()).unwrap(),
    );
    let actor = identity::Did::new(fixture.agent_did()).unwrap();
    for permission in [
        gents_store::config_client::NodePermission::CollectionGet,
        gents_store::config_client::NodePermission::CollectionPatch,
    ] {
        admin
            .grant_node_permission(&actor, permission)
            .await
            .unwrap();
    }
    let tools = fixture.tools(&["automation"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    for call in [
        json!({"argv":["event-source","create","deliveries"],"set":{"source_collection":"Delivery","event_kind":"created"}}),
        json!({"argv":["task","create","handle"],"set":{"prompt_template":"Handle {{ doc.body }}","emit_outcome":true}}),
    ] {
        tool.call(call.to_string()).await.unwrap();
    }
    let trigger = json!({"argv":["trigger","create","on-delivery"],"set":{"task_id":"handle","source":{"kind":"event","event_source_id":"deliveries"}}});
    let error = tool.call(trigger.to_string()).await.unwrap_err();
    let gents_loop::tool::ToolError::ToolCallError(error) = error else {
        panic!("{error}")
    };
    let failure: Value = serde_json::from_str(&error.to_string()).unwrap();
    assert_eq!(failure["recovery"]["tool"], "schema");
    assert_eq!(
        failure["recovery"]["next_call"]["argv"],
        json!(["collection", "preview", "update", "Delivery"])
    );
    let schema = gents_tools::builtin::schema_help::SchemaTool::for_managed_agent(
        fixture.db.node.clone(),
        fixture.db.node_identity.did(),
        None,
        fixture.agent.as_ref(),
    )
    .unwrap();
    let preview: Value = serde_json::from_str(
        &Tool::call(
            &schema,
            serde_json::from_value(failure["recovery"]["next_call"].clone()).unwrap(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    Tool::call(
        &schema,
        serde_json::from_value(preview["next_call"]["args"].clone()).unwrap(),
    )
    .await
    .unwrap();
    tool.call(trigger.to_string()).await.unwrap();
    let rows = fixture
        .user_access
        .execute("{ Trigger { trigger_id } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["Trigger"].as_array().unwrap().len(), 1);
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

#[tokio::test]
async fn config_errors_name_the_next_call() {
    let fixture = SelfConfigFixture::new("next-call").await;
    fixture.grant_selected_configuration().await;
    let builder = fixture.install_sibling("Builder").await;
    fixture.grant_node_reader().await;
    let tools = fixture.tools(&["node", "tools", "agent"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let failure = |args: Value| async move {
        let error = tool.call(args.to_string()).await.unwrap_err();
        let gents_loop::tool::ToolError::ToolCallError(error) = error else {
            panic!("missing typed config error: {error}");
        };
        serde_json::from_str::<Value>(&error.to_string()).unwrap()
    };

    let skill = failure(json!({"argv":["agent","context","preview"],"options":{"agent":builder},"set":{"skill_ids":["eval-coding-check"]}})).await;
    let message = skill["error"].as_str().unwrap();
    assert!(
        message.starts_with("Skill \"eval-coding-check\" does not exist yet"),
        "{message}"
    );
    assert!(
        message.contains("create it with skill create or import it with skill import"),
        "{message}"
    );
    assert_eq!(
        skill["recovery"]["next_call"],
        json!({"argv":["skill","list"]})
    );
    tool.call(skill["recovery"]["next_call"].to_string())
        .await
        .unwrap();
    assert_eq!(skill["config_execution"]["mutation_entered"], false);

    let tools_ref = failure(json!({"argv":["agent","context","preview"],"options":{"agent":builder},"set":{"tools_id":"proposed-tools"}})).await;
    let message = tools_ref["error"].as_str().unwrap();
    assert!(
        message.contains("create it with its own resource command first"),
        "{message}"
    );
    assert!(!message.contains("plan"), "{message}");
    assert_eq!(
        tools_ref["recovery"]["next_call"],
        json!({"argv":["tools","list"]})
    );
    tool.call(tools_ref["recovery"]["next_call"].to_string())
        .await
        .unwrap();

    let aliased = tool
        .call(json!({"argv":["agent","context","preview","edit"],"options":{"agent":builder},"set":{"skill_ids":[]}}).to_string())
        .await
        .unwrap();
    assert!(aliased.contains("\"committed\": false"), "{aliased}");
    let stray =
        failure(json!({"argv":["agent","context","edit","builder"],"set":{"skill_ids":[]}})).await;
    let message = stray["error"].as_str().unwrap();
    assert!(
        message.contains(r#"agent context edit takes no positional argument "builder""#)
            && message.contains(r#"{"argv":["agent","context","edit"],"options":{"agent":"AGENT_DID"},"set":{"FIELD":VALUE}}"#),
        "{message}"
    );
    assert_eq!(stray["config_execution"]["mutation_entered"], false);

    let named = failure(json!({"argv":["agent","get","Builder"]})).await;
    assert_eq!(
        named["error"],
        format!("unknown agent_did \"Builder\"; did you mean {builder:?}?")
    );
    let core = fixture.core();
    core.apply(agent_request(
        &core,
        vec![("display_name".into(), Some(json!("Night Shift")))],
    ))
    .await
    .unwrap();
    let display = failure(json!({"argv":["agent","get","night shift"]})).await;
    assert_eq!(
        display["error"],
        format!(
            "unknown agent_did \"night shift\"; did you mean {:?}?",
            fixture.agent_did()
        )
    );
    let unknown = failure(json!({"argv":["agent","get","nobody"]})).await;
    assert!(
        unknown["error"]
            .as_str()
            .unwrap()
            .contains(r#"copy an exact DID from ["agent","list"]"#),
        "{unknown}"
    );
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

#[tokio::test]
async fn config_execution_receipts_separate_rejected_syntax_from_write_dispatch() {
    let fixture = SelfConfigFixture::new("config-execution").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&[
        "node",
        "tools",
        "automation",
        "profile",
        "backend",
        "mcp_service",
    ]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    for (args, mutation) in [
        (
            json!({"argv":["agent","create","preview"],"set":{"system_prompt":"literal"}}),
            false,
        ),
        (json!({"argv":["unknown","operation"]}), false),
        (
            json!({"argv":["datastore","create"],"set":{"display_name":"Missing ID"}}),
            false,
        ),
        (
            json!({"argv":["datastore","get"],"target_id":"missing"}),
            false,
        ),
        (
            json!({"argv":["datastore","preview","create"],"target_id":"notifications","set":{"display_name":"Notifications"}}),
            false,
        ),
        (
            json!({"argv":["datastore","create"],"target_id":"notifications","set":{"display_name":"Notifications"}}),
            true,
        ),
        (
            json!({"argv":["datastore","create"],"target_id":"notifications","set":{"display_name":"Duplicate"}}),
            true,
        ),
        (
            json!({"argv":["schema","install"],"options":{"sdl":"type ReceiptProbe { value: String }","digest":"wrong"}}),
            false,
        ),
        (json!({"argv":["schema","install"]}), false),
        (
            json!({"argv":["automation","edit","task"],"target_id":"missing","set":{"display_name":"Missing agent"},"options":{"agent":"missing"}}),
            false,
        ),
    ] {
        let text = match tool.call(args.to_string()).await {
            Ok(text) => text,
            Err(gents_loop::tool::ToolError::ToolCallError(error)) => error.to_string(),
            Err(error) => panic!("missing typed result for {args}: {error}"),
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["config_execution"]["mutation_entered"], mutation,
            "{args}: {value}"
        );
    }
    let read: Value = serde_json::from_str(
        &tool
            .call(json!({"argv":["datastore","get"],"target_id":"notifications"}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read["config_execution"]["mutation_entered"], false);
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

#[tokio::test]
async fn agent_category_gates_the_tool() {
    let fixture = SelfConfigFixture::new("agent-gate").await;
    let without_agent = fixture.tools(&["tools"]);
    let error = call_config_tool(&without_agent, vec!["agent".into(), "list".into()])
        .await
        .expect_err("catalog read requires agent grant");
    assert!(error.contains("not granted"), "{error}");

    let with_agent = fixture.tools(&["node"]);
    assert!(with_agent
        .iter()
        .any(|tool| tool.name() == CONFIG_TOOL_NAME));
}

#[tokio::test]
async fn agent_only_grant_cannot_acquire_node_authority_and_writes_require_exact_signer() {
    let fixture = SelfConfigFixture::new("agent-only-owner").await;
    fixture.grant_selected_configuration().await;
    let node = fixture.db.node.clone();
    let owner = fixture.db.node_identity.did().to_owned();
    let current = fixture.agent_did();
    let profile = format!("{current}:inference");
    let tools = fixture.tools(&["agent"]);
    // The agent category exposes the Agent command (NodeProvisioning.agentCommandExposed);
    // catalog operations are refused before any write because the Agent does
    // not hold manage_agents (SelfConfig.catalog_refused_without_grants).
    call_config_tool(&tools, vec!["agent".into(), "list".into()])
        .await
        .expect("agent authority can read the agent catalog");
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            ["agent", "preview", "default", current]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        )
        .await
        .expect("a refused preview is an answer"),
    )
    .unwrap();
    assert_eq!(preview["refusal"], "catalog_not_held", "{preview}");
    for argv in [
        vec!["agent", "default", current],
        vec![
            "agent",
            "create",
            "--display-name",
            "Other",
            "--system-prompt",
            "Other instructions",
            "--profile",
            &profile,
            "--default",
        ],
    ] {
        let error = call_config_tool(&tools, argv.into_iter().map(str::to_owned).collect())
            .await
            .expect_err("agent-only authority cannot manage the node catalog");
        assert!(error.contains("manage_agents"), "{error}");
    }
    let edited = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "update".into(),
            current.into(),
            "--set".into(),
            r#"display_name="Renamed""#.into(),
        ],
    )
    .await
    .expect("agent authority can edit its own document");
    let edited: Value = serde_json::from_str(&edited).unwrap();
    assert_eq!(edited["committed"], true);
    let foreign_identity = tokio::task::spawn_blocking(|| node_identity("foreign-signer"))
        .await
        .unwrap();
    let params = agent_params(
        "edit",
        None,
        &[
            "--id".into(),
            current.into(),
            "--display-name".into(),
            "Renamed".into(),
        ],
    )
    .unwrap();
    let rejected = agent_mutate(
        &node,
        &owner,
        foreign_identity.as_ref(),
        &params,
        &Default::default(),
    )
    .await
    .expect_err("foreign signer must fail before publishing configuration");
    assert!(rejected.to_string().contains("exact local node identity"));
}

#[tokio::test]
async fn config_lists_are_bounded_paginated_and_inference_inventory_is_read_only() {
    let fixture = SelfConfigFixture::new("config-inventory").await;
    fixture.install_sibling("beta").await;
    fixture.install_sibling("gamma").await;
    fixture.grant_node_reader().await;
    // `agent get` of a listed row may name this Agent, whose own read needs the
    // agent category; the catalog alone reaches only other Agents.
    let tools = fixture.tools(&["node", "agent", "profile", "backend"]);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();

    let first: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["agent", "list", "--limit", "1"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["page"]["returned"], 1);
    assert_eq!(first["page"]["truncated"], true);
    let cursor = first["page"]["next_cursor"].as_str().unwrap();
    let second: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["agent", "list", "--limit", "1", "--cursor", cursor]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_ne!(first["agents"][0], second["agents"][0]);

    let agent_did = first["agents"][0]["agent_did"].as_str().unwrap();
    let inspected: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["agent", "get", agent_did]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inspected["agent_did"], agent_did);

    for resource in ["profile", "backend"] {
        let inventory: Value = serde_json::from_str(
            &config
                .call(json!({"argv":[resource, "list", "--limit", "2"]}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(inventory["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        assert!(inventory["note"].as_str().unwrap().contains("read-only"));
        assert!(inventory.to_string().find("\"auth\"").is_none());
    }
}

#[tokio::test]
async fn config_creates_and_discovers_an_unauthenticated_local_backend() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(request.starts_with("GET /v1/models "), "{request}");
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        let body = r#"{"data":[{"id":"fixture-local-model","max_model_len":32768}]}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });

    let fixture = SelfConfigFixture::new("local-backend-create").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let tools = fixture.tools(&["backend", "profile"]);
    let endpoint = format!("http://{address}/v1");
    let profiles_before: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "profile".into(),
                "list".into(),
                "--limit".into(),
                "50".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let create = vec![
        "backend".into(),
        "preview".into(),
        "create".into(),
        "fixture-local".into(),
        "--endpoint".into(),
        endpoint.clone(),
        "--name".into(),
        "Fixture local".into(),
    ];
    let preview: Value =
        serde_json::from_str(&call_config_tool(&tools, create.clone()).await.unwrap()).unwrap();
    assert_eq!(preview["committed"], false);
    assert!(call_config_tool(
        &tools,
        vec!["backend".into(), "get".into(), "fixture-local".into()]
    )
    .await
    .is_err());
    assert!(call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "preview".into(),
            "create".into(),
            "credential-backend".into(),
            "--endpoint".into(),
            endpoint.clone(),
            "--auth".into(),
            "secret".into(),
        ],
    )
    .await
    .unwrap_err()
    .contains("unknown backend create option --auth"));

    let mut apply = create;
    apply.remove(1);
    let created: Value =
        serde_json::from_str(&call_config_tool(&tools, apply.clone()).await.unwrap()).unwrap();
    assert_eq!(created["committed"], true);
    assert!(call_config_tool(&tools, apply)
        .await
        .unwrap_err()
        .contains("already exists"));

    let discovered: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["backend".into(), "discover".into(), "fixture-local".into()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(discovered["endpoint"], endpoint);
    assert_eq!(discovered["observation"]["probe_status"], "healthy");
    assert_eq!(
        discovered["observation"]["catalog"]["models"][0]["model_name"],
        "fixture-local-model"
    );
    assert!(discovered["note"]
        .as_str()
        .unwrap()
        .contains("did not create a profile"));
    let profiles_after: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "profile".into(),
                "list".into(),
                "--limit".into(),
                "50".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(profiles_after["items"], profiles_before["items"]);
    server.await.unwrap();
}

#[tokio::test]
async fn config_targets_owned_working_agent_for_all_bound_documents() {
    let fixture = SelfConfigFixture::new("targeted-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_engineer_tool_sources().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    fixture.grant_agent_relation_delete().await;
    let working_did = fixture.install_sibling("working").await;
    fixture.grant_sibling_configuration(&working_did).await;
    let setup = fixture.core();
    setup
        .apply(agent_request(
            &setup,
            vec![(
                "tags".into(),
                Some(json!([
                    gents_tools::builtin::self_config::ENGINEER_AGENT_TAG
                ])),
            )],
        ))
        .await
        .unwrap();
    setup
        .apply(tools_request(
            &setup,
            vec![(
                "self_config".into(),
                Some(json!({"enable_self_config": true})),
            )],
        ))
        .await
        .unwrap();

    let mut tool_config = config(&["node", "agent", "tools", "profile", "backend"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    tool_config.no_lockout = true;
    let tools = fixture.tools_with_config(&tool_config);

    call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "edit".into(),
            working_did.clone(),
            "--set".into(),
            "tags=[\"ui:review\"]".into(),
        ],
    )
    .await
    .unwrap();
    call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "system_prompt=\"Review carefully.\"".into(),
            "--set".into(),
            "skill_ids=[]".into(),
        ],
    )
    .await
    .unwrap();
    call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            format!("--agent={working_did}"),
            "--set".into(),
            "agents={\"target_ids\":[],\"enabled\":false}".into(),
            "--set".into(),
            "remote={\"services\":[]}".into(),
        ],
    )
    .await
    .unwrap();
    call_config_tool(
        &tools,
        vec![
            "profile".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "display_name=\"Working profile\"".into(),
        ],
    )
    .await
    .unwrap();

    let working: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["get".into(), "--agent".into(), working_did.clone()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(working["agent"]["tags"], json!(["ui:review"]));
    assert_eq!(working["context"]["system_prompt"], "Review carefully.");
    assert_eq!(working["documents"]["Tools"]["agents"]["enabled"], false);
    assert!(working["documents"]["Tools"]["remote"]["services"].is_null());
    assert!(
        working["documents"]["Tools"]["self_config"].is_null(),
        "targeting a sibling must protect the invoking Engineer chain without granting config to the sibling"
    );
    let working_core = SelfConfigCore::new(
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        working_did.clone(),
    )
    .unwrap()
    .with_lockout_agent_did(fixture.agent_did().to_owned())
    .with_agent_signer(fixture.agent.managed_agent_signer())
    .with_runtime_identity(Some(fixture.db.node_identity.clone()))
    .with_node_binding(Some(fixture.node_binding.clone()));
    working_core
        .apply(agent_request(
            &working_core,
            vec![(
                "inference_profile_id".into(),
                Some(json!(format!("{}:inference", fixture.agent_did()))),
            )],
        ))
        .await
        .unwrap();
    let shared_backend = call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "enabled=false".into(),
        ],
    )
    .await
    .expect_err("a sibling edit must not disable the invoking Engineer backend");
    assert!(
        shared_backend.contains("backend disabled"),
        "{shared_backend}"
    );
    working_core
        .apply(agent_request(
            &working_core,
            vec![(
                "inference_profile_id".into(),
                Some(json!(format!("{working_did}:inference"))),
            )],
        ))
        .await
        .unwrap();
    assert_eq!(
        working["inference_profile"]["display_name"],
        "Working profile"
    );

    let help = call_config_tool(&tools, vec!["help".into(), "tools".into()])
        .await
        .unwrap();
    assert!(
        help.contains("Fields of Tools:") && help.contains("agents"),
        "{help}"
    );
    let shapes = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--help".into(),
            "agents".into(),
        ],
    )
    .await
    .unwrap();
    assert!(shapes.contains("Tools.agents: {\"enabled\""), "{shapes}");
    assert!(shapes.contains("target_ids"), "{shapes}");

    let mailbox_help = call_config_tool(
        &tools,
        vec!["datastore".into(), "create".into(), "--help".into()],
    )
    .await
    .unwrap();
    assert!(
        mailbox_help.contains("{\"key\":\"monitor-summary\",\"mode\":\"condition\"}"),
        "{mailbox_help}"
    );

    let create_profile_args = vec![
        "profile".into(),
        "preview".into(),
        "create".into(),
        "review-profile".into(),
        "--set".into(),
        format!("backend_id=\"{working_did}:backend\""),
        "--set".into(),
        "model_name=\"review-model\"".into(),
        "--set".into(),
        "display_name=\"Review profile\"".into(),
    ];
    let preview: Value = serde_json::from_str(
        &call_config_tool(&tools, create_profile_args.clone())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert!(call_config_tool(
        &tools,
        vec!["profile".into(), "get".into(), "review-profile".into()]
    )
    .await
    .is_err());
    let mut create_profile_args = create_profile_args;
    create_profile_args.remove(1);
    let created: Value = serde_json::from_str(
        &call_config_tool(&tools, create_profile_args.clone())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(created["committed"], true);
    assert!(call_config_tool(&tools, create_profile_args)
        .await
        .unwrap_err()
        .contains("already exists"));
    let profile: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["profile".into(), "get".into(), "review-profile".into()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(profile["document"]["model_name"], "review-model");

    let default_preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "preview".into(),
                "default".into(),
                working_did.clone(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(default_preview["committed"], false);
    assert_eq!(default_preview["admitted"], true);
    assert_eq!(default_preview["make_default"], true);

    call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            fixture.agent_did().to_owned(),
            "--set".into(),
            "tags=[\"changed\"]".into(),
        ],
    )
    .await
    .unwrap();
    let lockout = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--set".into(),
            "self_config={\"enable_self_config\":false}".into(),
        ],
    )
    .await
    .unwrap_err();
    assert!(
        lockout.contains("self-config must remain enabled"),
        "{lockout}"
    );

    working_core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![(
                "tools_id".into(),
                Some(json!(format!("{}:tools", fixture.agent_did()))),
            )],
        ))
        .await
        .unwrap();
    let shared_tools = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "display_name=\"must not land\"".into(),
        ],
    )
    .await
    .expect_err("Tools shared with Engineer must be protected transactionally");
    assert!(
        shared_tools.contains("unshared Context and Tools"),
        "{shared_tools}"
    );
    working_core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![(
                "tools_id".into(),
                Some(json!(format!("{working_did}:tools"))),
            )],
        ))
        .await
        .unwrap();
    working_core
        .apply(agent_request(
            &working_core,
            vec![(
                "context_id".into(),
                Some(json!(format!("{}:context", fixture.agent_did()))),
            )],
        ))
        .await
        .unwrap();
    let shared_context = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "display_name=\"must not land\"".into(),
        ],
    )
    .await
    .expect_err("Context shared with Engineer must be protected transactionally");
    assert!(
        shared_context.contains("unshared Context and Tools"),
        "{shared_context}"
    );
}

#[tokio::test]
async fn cleanup_previews_and_removes_exact_unreferenced_cycles_atomically() {
    let fixture = SelfConfigFixture::new("config-cleanup").await;
    fixture.grant_selected_configuration().await;
    let root = fixture.admitted_workspace_root().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    fixture.grant_agent_delete().await;
    fixture.grant_agent_relation_delete().await;
    let mut tool_config = config(&["node", "tools", "profile", "backend"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    let tools = fixture.tools_with_config(&tool_config);
    call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "create".into(),
            "orphan-backend".into(),
            "--set".into(),
            "endpoint=\"http://127.0.0.1:1/v1\"".into(),
        ],
    )
    .await
    .expect("Agent creates the removable backend");
    call_config_tool(
        &tools,
        vec![
            "profile".into(),
            "create".into(),
            "orphan-profile".into(),
            "--set".into(),
            "backend_id=\"orphan-backend\"".into(),
            "--set".into(),
            "model_name=\"test-model\"".into(),
        ],
    )
    .await
    .expect("Agent creates the removable profile");
    let created: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "create".into(),
                "--display-name".into(),
                "Orphan".into(),
                "--system-prompt".into(),
                "Orphan fixture".into(),
                "--preset".into(),
                "readonly".into(),
                "--root".into(),
                root.clone(),
                "--profile".into(),
                "orphan-profile".into(),
            ],
        )
        .await
        .expect("Agent creates the removable agent and configuration"),
    )
    .unwrap();
    let orphan = created["agent_did"].as_str().unwrap().to_owned();

    let referenced: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "cleanup".into(),
                "preview".into(),
                "--target".into(),
                "backend=orphan-backend".into(),
            ],
        )
        .await
        .expect("a refused preview is an answer"),
    )
    .unwrap();
    assert_eq!(referenced["refusal"], "still_referenced", "{referenced}");
    assert_eq!(referenced["committed"], false);
    assert!(
        referenced["message"]
            .as_str()
            .unwrap()
            .contains("references missing InferenceBackend"),
        "{referenced}"
    );

    let targets = [
        format!("agent={orphan}"),
        format!("context={orphan}:context"),
        format!("tools={orphan}:tools"),
        "profile=orphan-profile".to_owned(),
        "backend=orphan-backend".to_owned(),
    ];
    let argv = |verb: &str| {
        let mut argv = vec!["cleanup".to_owned(), verb.to_owned()];
        for target in &targets {
            argv.push("--target".to_owned());
            argv.push(target.clone());
        }
        argv
    };
    let preview: Value = serde_json::from_str(
        &call_config_tool(&tools, argv("preview"))
            .await
            .expect("complete unreferenced cycle previews"),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["targets"].as_array().unwrap().len(), targets.len());
    call_config_tool(&tools, vec!["agent".into(), "get".into(), orphan.clone()])
        .await
        .expect("preview performs no writes");

    call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "edit".into(),
            "--agent".into(),
            orphan.clone(),
            "--set".into(),
            "name=\"Changed after preview\"".into(),
        ],
    )
    .await
    .expect("change one target after preview");
    let mut stale_argv = argv("remove");
    stale_argv.push("--digest".into());
    stale_argv.push(preview["plan_digest"].as_str().unwrap().to_owned());
    let stale = call_config_tool(&tools, stale_argv)
        .await
        .expect_err("cleanup refuses content that changed after preview");
    assert!(stale.contains("preview again"), "{stale}");

    let refreshed: Value = serde_json::from_str(
        &call_config_tool(&tools, argv("preview"))
            .await
            .expect("changed target set can be previewed again"),
    )
    .unwrap();
    let mut remove_argv = argv("remove");
    remove_argv.push("--digest".into());
    remove_argv.push(refreshed["plan_digest"].as_str().unwrap().to_owned());
    let removed: Value = serde_json::from_str(
        &call_config_tool(&tools, remove_argv)
            .await
            .expect("complete unreferenced cycle removes atomically"),
    )
    .unwrap();
    assert_eq!(removed["committed"], true);
    let missing = call_config_tool(&tools, vec!["agent".into(), "get".into(), orphan.clone()])
        .await
        .expect_err("removed agent is no longer inspectable");
    assert!(
        missing.contains("missing")
            || missing.contains("no owned")
            || missing.contains("unknown agent_did"),
        "{missing}"
    );

    call_config_tool(
        &tools,
        vec![
            "get".into(),
            "--agent".into(),
            fixture.agent_did().to_owned(),
        ],
    )
    .await
    .expect("cleanup preserves unrelated configuration");
}

/// Lean `SelfConfig.catalog_refused_without_grants`: choosing the default needs
/// `manage_agents` on the Node, which an Agent holding only its own grants does
/// not hold, whatever its Tools categories; the Node's owner chooses it through
/// the selection document.
#[tokio::test]
async fn user_owned_node_default_rejects_agent_update_and_accepts_user() {
    let fixture = SelfConfigFixture::new("agent-default-authority").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let working = fixture.install_sibling("working").await;
    let seed = fixture.agent_did();
    let core = fixture.core();
    let tools = fixture.tools(&["node"]);
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "preview".into(),
                "default".into(),
                working.clone(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["admitted"], false, "{preview}");
    assert_eq!(preview["refusal"], "catalog_not_held", "{preview}");
    assert_eq!(
        preview["next_call"],
        json!({"argv":["agent","get"]}),
        "{preview}"
    );
    assert_eq!(
        node_default_agent(&core).await.unwrap().as_deref(),
        Some(seed)
    );
    let agent_update = call_config_tool(
        &tools,
        vec!["agent".into(), "default".into(), working.clone()],
    )
    .await
    .expect_err("an Agent without manage_agents cannot choose the default");
    assert!(agent_update.contains("manage_agents"), "{agent_update}");
    assert!(
        agent_update.contains(r#""next_call":{"argv":["agent","get"]}"#),
        "{agent_update}"
    );
    assert_eq!(
        node_default_agent(&core).await.unwrap().as_deref(),
        Some(seed)
    );
    fixture
        .update_signed_configuration(
            gents_store::collection::Collection::DefaultAgentSelection,
            fixture.node_did(),
            json!({"default_agent_did": working}),
        )
        .await;
    assert_eq!(
        node_default_agent(&core).await.unwrap().as_deref(),
        Some(working.as_str())
    );
}

#[tokio::test]
async fn agent_clone_accepts_sibling_agent_did() {
    let fixture = SelfConfigFixture::new("agent-clone").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    let node = &fixture.db.node;
    let node_did = fixture.node_did();
    let qualified_sibling_id = fixture.install_sibling("sibling-agent").await;
    // Reusing the sibling's profile selects it, which native document ACP
    // admits only for a manager of that selection.
    fixture
        .grant_sibling_configuration(&qualified_sibling_id)
        .await;
    let profile_id = format!("{qualified_sibling_id}:inference");
    let tools = fixture.tools(&["node"]);
    let tool = take_agent_tool(tools);
    let args = json!({"argv": [
        "agent", "clone",
        "--display-name", "Cloned Agent",
        "--from", qualified_sibling_id,
        "--profile", profile_id
    ]})
    .to_string();
    let output: Value =
        serde_json::from_str(&tool.call(args).await.expect("clone commits")).unwrap();
    assert_eq!(output["committed"], true);
    let source = gents_documents::list_agents(node, node_did)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.agent_did == qualified_sibling_id)
        .unwrap();
    let cloned = gents_documents::list_agents(node, node_did)
        .await
        .unwrap()
        .into_iter()
        .find(|a| Some(a.agent_did.as_str()) == output["agent_did"].as_str())
        .unwrap();
    assert_eq!(source.inference_profile_id, cloned.inference_profile_id);
    assert_ne!(source.context_id, cloned.context_id);
}

/// Lean `SelfConfig.clone_inherits_no_self_config`: a clone never carries its
/// source's self-config group, so a source's operator grant (pack
/// installation) is never copied, whether or not the invoker holds it.

#[tokio::test]
async fn agent_clone_never_copies_the_source_self_config_grants() {
    let fixture = SelfConfigFixture::new("agent-clone-grants").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    let granted = fixture.install_sibling("granted").await;
    fixture.grant_pack_install_to(&granted).await;
    // Reusing the sibling's profile selects it, which native document ACP
    // admits only for a manager of that selection.
    fixture.grant_sibling_configuration(&granted).await;
    let profile = format!("{granted}:inference");
    let mut tool_config = config(&["node"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    let tool = take_agent_tool(fixture.tools_with_config(&tool_config));
    let argv = |preview: bool| {
        let mut argv = vec!["agent"];
        if preview {
            argv.push("preview");
        }
        argv.extend([
            "clone",
            "--display-name",
            "Granted Copy",
            "--from",
            &granted,
        ]);
        argv.extend(["--profile", &profile]);
        json!({ "argv": argv }).to_string()
    };
    let preview: Value = serde_json::from_str(&tool.call(argv(true)).await.unwrap()).unwrap();
    assert_eq!(preview["admitted"], true, "{preview}");
    let output: Value =
        serde_json::from_str(&tool.call(argv(false)).await.expect("the clone commits")).unwrap();
    assert_eq!(output["committed"], true);
    let tools_id = format!("{}:tools", output["agent_did"].as_str().unwrap());
    let node_did = fixture.node_did().to_owned();
    let cloned = fixture
        .user_access
        .transact_readonly("test.self_config.read_cloned_tools", |txn| {
            let tools_id = &tools_id;
            let node_did = &node_did;
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Tools,
                    node_did,
                    tools_id,
                )
                .await
            })
        })
        .await
        .unwrap()
        .expect("the clone publishes its Tools")
        .1;
    assert!(cloned["self_config"].is_null(), "{cloned}");
}

#[tokio::test]
async fn agent_clone_by_holder_copies_granted_source() {
    let fixture = SelfConfigFixture::new("agent-clone-holder").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    let node = &fixture.db.node;
    let node_did = fixture.node_did();
    let granted = fixture.install_sibling("granted").await;
    fixture.grant_pack_install_to(&granted).await;
    // Reusing the sibling's profile selects it, which native document ACP
    // admits only for a manager of that selection.
    fixture.grant_sibling_configuration(&granted).await;
    let profile = format!("{granted}:inference");
    fixture.grant_pack_install().await;
    let mut tool_config = config(&["node"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    tool_config.enable_pack_install = true;
    let tools = fixture.tools_with_config(&tool_config);
    let tool = take_agent_tool(tools);
    tool.call(
        json!({"argv": ["agent", "preview", "clone", "--display-name", "Granted Copy",
                        "--from", granted, "--profile", profile]})
        .to_string(),
    )
    .await
    .expect("a holder may preview copying a granted source");
    let args = json!({"argv": ["agent", "clone", "--display-name", "Granted Copy",
                               "--from", granted, "--profile", profile]})
    .to_string();
    let output: Value =
        serde_json::from_str(&tool.call(args).await.expect("a holder's clone commits")).unwrap();
    assert_eq!(output["committed"], true);
    assert!(
        gents_documents::list_agents(node, node_did)
            .await
            .unwrap()
            .iter()
            .any(|agent| Some(agent.agent_did.as_str()) == output["agent_did"].as_str()),
        "a holder's clone publishes its Agent"
    );
}

#[tokio::test]
async fn foreign_agent_configuration_needs_the_catalog_grant() {
    let fixture = SelfConfigFixture::new("foreign-agent-catalog").await;
    fixture.grant_selected_configuration().await;
    let sibling = fixture.install_sibling("sibling-agent").await;
    let agent_only = fixture.tools(&["agent"]);
    for argv in [vec![
        "agent".into(),
        "context".into(),
        "get".into(),
        "--agent".into(),
        sibling.clone(),
    ]] {
        let error = call_config_tool(&agent_only, argv.clone())
            .await
            .expect_err("an agent-category grant does not reach another Agent");
        assert!(error.contains("agent catalog grant"), "{argv:?}: {error}");
    }
}

#[tokio::test]
async fn clone_does_not_inherit_self_config_from_its_source() {
    let fixture = SelfConfigFixture::new("clone-self-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    let node_did = fixture.node_did().to_owned();
    let source = fixture.install_sibling("self-config-source").await;
    fixture.grant_pack_install_to(&source).await;
    // Reusing the sibling's profile selects it, which native document ACP
    // admits only for a manager of that selection.
    fixture.grant_sibling_configuration(&source).await;
    let profile = format!("{source}:inference");
    let mut tool_config = config(&["node"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    let tool = take_agent_tool(fixture.tools_with_config(&tool_config));
    let output: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": ["agent", "clone", "--display-name", "Plain Copy",
                                "--from", source, "--profile", profile]})
                .to_string(),
            )
            .await
            .expect("a catalog holder clones a source that carries self-config"),
    )
    .unwrap();
    assert_eq!(output["committed"], true);
    let clone_tools_id = format!("{}:tools", output["agent_did"].as_str().unwrap());
    let stored = fixture
        .user_access()
        .transact_readonly("test.self_config.clone_tools", |txn| {
            let owner = node_did.clone();
            let tools_id = clone_tools_id.clone();
            Box::pin(async move {
                let (_, tools) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Tools,
                    &owner,
                    &tools_id,
                )
                .await?
                .expect("the clone's Tools exists");
                Ok::<_, anyhow::Error>(tools)
            })
        })
        .await
        .unwrap();
    assert!(
        stored.get("self_config").is_none_or(Value::is_null),
        "a clone must not inherit self-config: {stored}"
    );
}

#[tokio::test]
async fn explicit_tools_grant_preserves_lsp_settings_guard_for_preview_and_apply() {
    let fixture = SelfConfigFixture::new("self-config-lsp-guard").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["tools"]);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    // LSP is a host tool; on a signed Node it needs an admitted Tools.host.root.
    let root = fixture.admitted_workspace_root().await;
    config
        .call(json!({"argv":["tools","edit"],"set":{"host":{"root":root,"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}}).to_string())
        .await
        .unwrap();
    let safe = json!({"integrations":{"lsp":{"config":json!({"servers":{"rust-analyzer":{"disabled":true,"priority":2}}}).to_string()}}});
    config
        .call(json!({"argv":["tools", "preview", "--set", format!("integrations={}", safe["integrations"])]}).to_string())
        .await
        .unwrap();
    config
        .call(json!({"argv":["tools", "edit", "--set", format!("integrations={}", safe["integrations"])]}).to_string())
        .await
        .unwrap();
    let baseline: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["get"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        baseline["documents"]["Tools"]["integrations"],
        safe["integrations"]
    );
    for field in ["settings", "init_options", "initOptions"] {
        let server = json!({field:{"check":{"overrideCommand":["custom-check"]}}});
        let raw = json!({"servers":{"rust-analyzer":server}}).to_string();
        gents_documents::LspConfigDocument::parse_operator(Some(&raw)).unwrap();
        let patch = json!({"integrations":{"lsp":{"config":raw}}});
        let preview = config
            .call(json!({"argv":["tools", "preview", "--set", format!("integrations={}", patch["integrations"])]}).to_string())
            .await
            .unwrap_err();
        assert!(preview.to_string().contains(field), "{preview}");
        let apply = config
            .call(json!({"argv":["tools", "edit", "--set", format!("integrations={}", patch["integrations"])]}).to_string())
            .await
            .unwrap_err();
        assert!(apply.to_string().contains(field), "{apply}");
        let after: Value = serde_json::from_str(
            &config
                .call(json!({"argv":["get"]}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after["documents"]["Tools"], baseline["documents"]["Tools"]);
    }
}

#[tokio::test]
async fn profile_edit_rejects_a_context_window_above_the_advertised_maximum() {
    let fixture = SelfConfigFixture::new("profile-context-window").await;
    fixture.grant_selected_configuration().await;
    let owner = fixture.node_did();
    let agent = fixture.agent_did();
    let node = &fixture.db.node;
    let backend_id = format!("{agent}:backend");
    let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
        "node_did": owner, "backend_id": backend_id, "name": "Test inference",
        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind": "unauthenticated"},
    }))
    .unwrap();
    gents_inference::backends::registry::record_model_catalog(
        node,
        &backend,
        serde_json::from_value(json!({
            "node_did": null,
            "observed_at": "2026-09-25T00:00:00Z",
            "models": [{"model_name":"test-model","context_window":272000,"max_context_window":872000}],
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let tools = fixture.tools(&["profile"]);
    let edit = |verb: &str, window: u64| -> Vec<String> {
        vec![
            "profile".into(),
            verb.into(),
            "--agent".into(),
            agent.into(),
            "--set".into(),
            format!("context_window={window}"),
        ]
    };

    for verb in ["preview", "edit"] {
        let error = call_config_tool(&tools, edit(verb, 900_000))
            .await
            .expect_err("a window above the advertised maximum must be refused");
        assert!(
            error.contains("advertised maximum 872000"),
            "{verb}: {error}"
        );
    }
    call_config_tool(&tools, edit("edit", 500_000))
        .await
        .expect("a window within the advertised maximum is published");
    let stored = call_config_tool(
        &tools,
        vec![
            "profile".into(),
            "get".into(),
            "--agent".into(),
            agent.into(),
        ],
    )
    .await
    .unwrap();
    assert!(stored.contains("500000"), "{stored}");
}

#[tokio::test]
async fn engineer_configures_targets_executions_and_itself_but_cannot_lock_out() {
    let fixture = SelfConfigFixture::new("engineer-god-mode").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_delete().await;
    fixture.grant_agent_relation_delete().await;
    let lead = fixture.install_sibling("lead").await;
    let caller = fixture.install_sibling("caller").await;
    fixture.grant_sibling_configuration(&lead).await;
    fixture.grant_sibling_configuration(&caller).await;
    // Lean `AgentTargetAccess.authorized`: selecting the `gatekeeper` target
    // declares `session_writer` on `lead`, so the Engineer must natively
    // manage that relation (`engineer_with_authority_applies_the_same_relation`).
    fixture.grant_agent_administration(&lead).await;
    let owner = fixture.node_did().to_owned();
    let setup_did = fixture.agent_did().to_owned();
    let setup = fixture.core();
    setup
        .apply(agent_request(
            &setup,
            vec![(
                "tags".into(),
                Some(json!([
                    gents_tools::builtin::self_config::ENGINEER_AGENT_TAG
                ])),
            )],
        ))
        .await
        .unwrap();
    setup
        .apply(tools_request(
            &setup,
            vec![
                (
                    "self_config".into(),
                    Some(json!({"enable_self_config": true, "self_config_no_lockout": true})),
                ),
                ("agents".into(), Some(json!({"enabled": true}))),
            ],
        ))
        .await
        .unwrap();
    let mut grants = config(&["node", "agent", "tools", "profile", "automation"]);
    grants.agent_did = setup_did.clone();
    grants.preview = true;
    grants.no_lockout = true;
    let tools = fixture.tools_with_config(&grants);
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let ok = |result: Result<String, String>| -> Value {
        serde_json::from_str(&result.unwrap_or_else(|error| panic!("{error}"))).unwrap()
    };

    let target_patch = [
        "--set",
        "name=\"gatekeeper\"",
        "--set",
        &format!("target_node_did={}", json!(owner)),
        "--set",
        &format!("agent_did={}", json!(lead)),
    ];
    let preview = ok(call_config_tool(
        &tools,
        command(
            &[
                &["agent-target", "preview", "create", "gatekeeper"][..],
                &target_patch,
            ]
            .concat(),
        ),
    )
    .await);
    assert_eq!(preview["committed"], false);
    let missing = call_config_tool(
        &tools,
        command(&[
            "agent-target",
            "preview",
            "create",
            "dangling",
            "--set",
            "name=\"dangling\"",
            "--set",
            &format!("target_node_did={}", json!(owner)),
            "--set",
            "agent_did=\"missing\"",
        ]),
    )
    .await
    .unwrap_err();
    assert!(missing.contains("missing"), "{missing}");
    ok(call_config_tool(
        &tools,
        command(&[&["agent-target", "create", "gatekeeper"][..], &target_patch].concat()),
    )
    .await);
    let listed = ok(call_config_tool(&tools, command(&["agent-target", "list"])).await);
    assert_eq!(listed["items"][0]["target_id"], "gatekeeper");
    ok(call_config_tool(
        &tools,
        command(&[
            "agent-target",
            "edit",
            "gatekeeper",
            "--set",
            "description=\"Reviews lead work\"",
        ]),
    )
    .await);

    for selection in [
        r#"agents={"target_ids":["gatekeeper"]}"#,
        r#"agents={"enabled":false,"target_ids":["gatekeeper"]}"#,
    ] {
        let receipt = ok(call_config_tool(
            &tools,
            command(&["tools", "edit", "--agent", &caller, "--set", selection]),
        )
        .await);
        assert_eq!(receipt["committed"], true);
        assert!(receipt["effect"]
            .as_str()
            .unwrap()
            .contains("Selected targets are inactive"));
        let stored =
            ok(call_config_tool(&tools, command(&["tools", "get", "--agent", &caller])).await);
        assert_ne!(stored["document"]["agents"]["enabled"], true);
        assert_eq!(
            stored["document"]["agents"]["target_ids"],
            json!(["gatekeeper"])
        );
    }

    let recovery = ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--agent",
            &caller,
            "--set",
            r#"agents={"enabled":true,"target_ids":["gatekeeper"]}"#,
        ]),
    )
    .await);
    assert!(!recovery["effect"]
        .as_str()
        .unwrap()
        .contains("Selected targets are inactive"));
    let recovered =
        ok(call_config_tool(&tools, command(&["tools", "get", "--agent", &caller])).await);
    assert_eq!(recovered["document"]["agents"]["enabled"], true);
    assert_eq!(
        recovered["document"]["agents"]["target_ids"],
        json!(["gatekeeper"])
    );

    ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--agent",
            &caller,
            "--set",
            r#"agents={"enabled":true}"#,
            "--allow-drop",
            "agents",
        ]),
    )
    .await);

    let enabled = ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--set",
            r#"agents={"enabled":true,"target_ids":["gatekeeper"]}"#,
            "--set",
            r#"built_ins={"enable_session_history_tool":true}"#,
            "--set",
            r#"datastore={"enable_defra_query":true}"#,
        ]),
    )
    .await);
    assert!(!enabled["effect"]
        .as_str()
        .unwrap()
        .contains("Selected targets are inactive"));
    let stored = ok(call_config_tool(&tools, command(&["tools", "get"])).await);
    assert_eq!(stored["document"]["agents"]["enabled"], true);
    for (patch, refusal) in [
        (
            r#"agents={"enabled":false,"target_ids":["gatekeeper"]}"#,
            "agents tools must remain enabled",
        ),
        (
            r#"self_config={"enable_self_config":false}"#,
            "self-config must remain enabled",
        ),
        (
            r#"self_config={"enable_self_config":true,"self_config_no_lockout":false}"#,
            "self_config_no_lockout must remain enabled",
        ),
        (
            r#"self_config={"enable_self_config":true,"self_config_no_lockout":true,"self_config_categories":["profile"]}"#,
            "must keep the tools category",
        ),
    ] {
        for verb in ["preview", "edit"] {
            let error = call_config_tool(&tools, command(&["tools", verb, "--set", patch]))
                .await
                .unwrap_err();
            assert!(error.contains(refusal), "{verb} {patch}: {error}");
        }
    }
    let disable = call_config_tool(
        &tools,
        command(&["agent", "edit", &setup_did, "--set", "enabled=false"]),
    )
    .await
    .unwrap_err();
    assert!(disable.contains("must stay enabled"), "{disable}");
    // Lean `SelfConfig.keepsReach`: the Engineer tag confers nothing, so
    // dropping it is an ordinary edit.
    ok(call_config_tool(
        &tools,
        command(&[
            "agent",
            "edit",
            &setup_did,
            "--set",
            "tags=[\"ui:engineer\"]",
        ]),
    )
    .await);
    ok(call_config_tool(
        &tools,
        command(&[
            "agent",
            "edit",
            &setup_did,
            "--set",
            &format!(
                "tags={}",
                json!([
                    gents_tools::builtin::self_config::ENGINEER_AGENT_TAG,
                    "ui:engineer"
                ])
            ),
        ]),
    )
    .await);
    let agent_disable =
        call_config_tool(&tools, command(&["agent", "disable", "--id", &setup_did]))
            .await
            .unwrap_err();
    assert!(
        agent_disable.contains("must stay enabled"),
        "{agent_disable}"
    );
    for name in ["\"\"", "\"   \""] {
        let blank = call_config_tool(
            &tools,
            command(&[
                "agent-target",
                "edit",
                "gatekeeper",
                "--set",
                &format!("name={name}"),
            ]),
        )
        .await
        .unwrap_err();
        assert!(blank.contains("invalid AgentTarget"), "{blank}");
    }
    ok(call_config_tool(
        &tools,
        command(
            &[
                &["agent-target", "create", "gatekeeper-twin"][..],
                &target_patch,
            ]
            .concat(),
        ),
    )
    .await);
    let duplicate = call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--set",
            r#"agents={"enabled":true,"target_ids":["gatekeeper","gatekeeper-twin"]}"#,
        ]),
    )
    .await
    .unwrap_err();
    assert!(
        duplicate.contains("duplicate agent target name"),
        "{duplicate}"
    );

    for verb in [
        &["execution", "preview", "create"][..],
        &["execution", "create"],
    ] {
        ok(call_config_tool(&tools, command(&[verb, &["default-execution"]].concat())).await);
    }
    let defaults =
        ok(call_config_tool(&tools, command(&["execution", "get", "default-execution"])).await);
    assert_eq!(defaults["document"]["execution_id"], "default-execution");
    assert_eq!(
        defaults["effective"]["max_turns"],
        gents_inference::resolved::DEFAULT_MAX_TURNS
    );
    assert_eq!(
        defaults["effective"]["provider_idle_timeout_secs"],
        gents_inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS
    );
    assert_eq!(
        defaults["effective"]["deadline_duration_secs"],
        gents_documents::inference_defaults::DEFAULT_DEADLINE_DURATION_SECS
    );
    assert!(defaults["effective"]["max_total_tokens"].is_null());
    assert!(defaults["document"]["max_turns"].is_null());
    let inventory = ok(call_config_tool(&tools, command(&["execution", "list"])).await);
    let listed = inventory["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["execution_id"] == "default-execution")
        .unwrap();
    assert_eq!(listed["effective"], defaults["effective"]);
    assert!(
        call_config_tool(&tools, command(&["execution", "edit", "default-execution"]))
            .await
            .unwrap_err()
            .contains("empty patch")
    );
    ok(call_config_tool(
        &tools,
        command(&[
            "execution",
            "preview",
            "create",
            "lead-execution",
            "--set",
            "max_turns=40",
        ]),
    )
    .await);
    ok(call_config_tool(
        &tools,
        command(&[
            "execution",
            "create",
            "lead-execution",
            "--set",
            "max_turns=40",
        ]),
    )
    .await);
    ok(call_config_tool(
        &tools,
        command(&[
            "profile",
            "edit",
            "--agent",
            &lead,
            "--set",
            "execution_id=\"lead-execution\"",
        ]),
    )
    .await);
    ok(call_config_tool(
        &tools,
        command(&[
            "profile",
            "edit",
            "execution",
            "--agent",
            &lead,
            "--set",
            "max_turns=12",
        ]),
    )
    .await);
    let execution =
        ok(call_config_tool(&tools, command(&["execution", "get", "lead-execution"])).await);
    assert_eq!(execution["document"]["max_turns"], 12);
    assert_eq!(execution["effective"]["max_turns"], 12);
    let bound = ok(call_config_tool(
        &tools,
        command(&["profile", "get", "execution", "--agent", &lead]),
    )
    .await);
    assert_eq!(bound["effective"], execution["effective"]);

    ok(call_config_tool(
        &tools,
        command(&[
            "automation",
            "edit",
            "task",
            "engineer-inbox",
            "--agent",
            &setup_did,
            "--set",
            "prompt_template=\"Review {{ doc.message }}\"",
        ]),
    )
    .await);

    assert_eq!(
        ok(call_config_tool(
            &tools,
            command(&["cleanup", "preview", "--target", "agent-target=gatekeeper"]),
        )
        .await)["refusal"],
        "still_referenced"
    );
    let refused = call_config_tool(
        &tools,
        command(&["tools", "edit", "--set", r#"agents={"enabled":true}"#]),
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("drop existing settings from these Tools: agents.target_ids")
            && refused.contains("allow-drop"),
        "{refused}"
    );
    ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--allow-drop",
            "agents",
            "--set",
            r#"agents={"enabled":true}"#,
        ]),
    )
    .await);
    let cleanup = ok(call_config_tool(
        &tools,
        command(&["cleanup", "preview", "--target", "agent-target=gatekeeper"]),
    )
    .await);
    let digest = cleanup["plan_digest"].as_str().unwrap().to_owned();
    ok(call_config_tool(
        &tools,
        command(&[
            "cleanup",
            "remove",
            "--digest",
            &digest,
            "--target",
            "agent-target=gatekeeper",
        ]),
    )
    .await);
}

#[tokio::test]
async fn backend_reads_expose_operator_catalogs_without_credentials_or_provider_calls() {
    const SECRET: &str = "operator-api-key-never-exposed";
    let fixture = SelfConfigFixture::new("backend-catalog-read").await;
    fixture.grant_node_reader().await;
    fixture.grant_selected_configuration().await;
    let owner = fixture.node_did();
    let access = fixture.user_access();
    let runtime_access = gents_store::config_client::ConfigAccess::local_as(
        fixture.db.node.clone(),
        identity::Did::new(owner).unwrap(),
    );
    let backend = |id: &str, kind: &str, auth: Value| -> gents_documents::InferenceBackend {
        serde_json::from_value(json!({
            "node_did": owner, "backend_id": id, "name": id, "provider_kind": kind,
            "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap()
    };
    let claude = backend(
        "claude",
        "ClaudeCliSubscription",
        json!({"kind": "node_oauth"}),
    );
    let keyed = backend(
        "keyed",
        "OpenAiCompatible",
        json!({"kind": "api_key", "key": SECRET}),
    );
    let unobserved = backend(
        "unobserved",
        "OpenAiCompatible",
        json!({"kind": "unauthenticated"}),
    );
    let models: Vec<gents_documents::AdvertisedModel> = serde_json::from_value(json!([{
        "model_name": "claude-opus-5-5", "display_name": "Claude Opus 5.5",
        "context_window": 1_000_000, "max_context_window": null, "max_output_tokens": 128_000,
        "reasoning_efforts": ["low", "high"],
    }]))
    .unwrap();
    for document in [&claude, &keyed, &unobserved] {
        gents_documents::write_inference_backend_document(
            access,
            document,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }
    let stored_keyed = access
        .transact_readonly("test.backend_catalog.metadata", |txn| {
            Box::pin(async move {
                gents_documents::load_inference_backend_in_txn(txn, owner, "keyed").await
            })
        })
        .await
        .unwrap()
        .expect("persisted keyed backend");
    assert!(matches!(
        &stored_keyed.auth,
        gents_documents::BackendAuth::Credential { .. }
    ));
    assert_eq!(
        gents_inference::backends::registry::record_connection_catalog_on(
            &runtime_access,
            owner,
            claude.provider_kind,
            &claude.endpoint,
            &claude.auth,
            models.clone(),
        )
        .await
        .unwrap(),
        1
    );
    gents_inference::backends::registry::record_discovered_catalog_on(
        &runtime_access,
        &stored_keyed,
        models,
    )
    .await
    .unwrap();

    let tools = fixture.tools(&["backend"]);
    let call = |argv: &[&str]| {
        let argv = std::iter::once("backend")
            .chain(argv.iter().copied())
            .map(String::from)
            .collect::<Vec<_>>();
        let tools = &tools;
        async move {
            let output = call_config_tool(tools, argv).await.unwrap();
            assert!(!output.contains(SECRET), "credential exposed: {output}");
            serde_json::from_str::<Value>(&output).unwrap()
        }
    };

    let list = call(&["list"]).await;
    let items = list["items"].as_array().unwrap();
    let page = call(&["list", "--limit", "1"]).await;
    assert_eq!(page["items"][0], items[0]);
    assert_eq!(page["page"]["total"], items.len());
    assert_eq!(page["page"]["truncated"], true);
    for id in ["claude", "keyed"] {
        let item = items.iter().find(|item| item["backend_id"] == id).unwrap();
        assert_eq!(
            item["catalog"]["models"][0]["model_name"],
            "claude-opus-5-5"
        );
        assert!(item.get("catalogs").is_none() && item.get("auth").is_none());
    }
    for id in ["claude", "keyed"] {
        let get = call(&["get", id]).await;
        let listed = items.iter().find(|item| item["backend_id"] == id).unwrap();
        for field in ["catalog", "probe_status", "last_probe"] {
            assert_eq!(listed[field], get["observation"][field], "{id}: {field}");
        }
        let model = &get["observation"]["catalog"]["models"][0];
        assert_eq!(model["context_window"], 1_000_000);
        assert_eq!(model["reasoning_efforts"], json!(["low", "high"]));
        let discover = call(&["discover", id]).await;
        assert_eq!(discover["refreshed"], false);
        assert_eq!(
            discover["observation"]["catalog"]["models"][0]["model_name"],
            "claude-opus-5-5"
        );
    }
    let item = items
        .iter()
        .find(|item| item["backend_id"] == "unobserved")
        .unwrap();
    assert!(item["catalog"].is_null());
    assert!(item["probe_status"].is_null());
    assert!(item["last_probe"].is_null());
    let bound = call(&["get"]).await;
    assert!(bound["observation"].get("catalog").is_some(), "{bound}");
}

#[tokio::test]
async fn modeled_resource_reads_use_resource_category_and_native_access() {
    let fixture = SelfConfigFixture::new("resource-read-exposure").await;
    fixture.grant_node_reader().await;
    fixture.grant_selected_configuration().await;
    let backend_id = format!("{}:backend", fixture.agent_did());
    let task = json!({"node_did": fixture.node_did(), "task_id": "read-exposure-task",
        "agent_did": fixture.agent_did(), "prompt_template": "Do the work"});
    let task_plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Task,
            add: task.clone(),
            update: task,
        },
    ])
    .unwrap();
    fixture
        .user_access()
        .transact_write("test.read_exposure.task", |txn| {
            let task_plan = &task_plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    task_plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let cases =
        &gents_lean_vocab::lean_contract_snapshot().self_config_resource_read_exposure_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let categories = case
            .categories
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let expected = case.exposed;
        let tools = fixture.tools(&categories);
        let commands = match case.resource.as_str() {
            "backend" => vec![
                vec!["backend".to_owned(), "list".to_owned()],
                vec!["backend".to_owned(), "get".to_owned(), backend_id.clone()],
                vec!["backend".to_owned(), "get".to_owned()],
            ],
            "tools" => vec![vec!["tools".into(), "get".into()]],
            "profile" => vec![
                vec!["profile".into(), "list".into()],
                vec!["profile".into(), "get".into()],
            ],
            "automation" => vec![
                vec!["automation".into(), "list".into(), "task".into()],
                vec![
                    "automation".into(),
                    "get".into(),
                    "task".into(),
                    "read-exposure-task".into(),
                ],
            ],
            "effective" => vec![
                vec!["get".into()],
                vec![
                    "get".into(),
                    "--agent".into(),
                    fixture.agent_did().to_owned(),
                ],
            ],
            other => panic!("unexpected modeled read resource {other}"),
        };
        for command in commands {
            let result = call_config_tool(&tools, command.clone()).await;
            assert_eq!(
                result.is_ok(),
                expected,
                "{case:?}: {command:?}: {result:?}"
            );
            if expected && case.resource == "effective" {
                let value: Value = serde_json::from_str(result.as_ref().unwrap()).unwrap();
                assert_eq!(value["agent_did"], fixture.agent_did());
            }
            if expected
                && command.first().is_some_and(|part| part == "automation")
                && command.get(1).is_some_and(|part| part == "get")
            {
                let value: Value = serde_json::from_str(result.as_ref().unwrap()).unwrap();
                assert_eq!(value["document"]["agent_did"], fixture.agent_did());
                assert_eq!(value["document"]["task_id"], "read-exposure-task");
            }
        }
    }
}

#[tokio::test]
async fn automation_read_preserves_native_selected_agent_visibility() {
    let fixture = SelfConfigFixture::new("automation-read-native-scope").await;
    fixture.grant_selected_configuration().await;
    let sibling = fixture.install_sibling("Other Agent").await;
    let task = json!({"node_did": fixture.node_did(), "task_id": "other-agent-task",
        "agent_did": sibling, "prompt_template": "Do the work"});
    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Task,
            add: task.clone(),
            update: task,
        },
    ])
    .unwrap();
    fixture
        .user_access()
        .transact_write("test.automation_read.other_task", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let expected = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .configuration_task_target_cases
        .iter()
        .find(|case| case["name"] == "task-unrelated")
        .unwrap()["expected"]
        .as_bool()
        .unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        fixture.db.node.clone(),
        identity::Did::new(fixture.agent_did()).unwrap(),
        fixture.agent.as_ref(),
    )
    .unwrap();
    let response = access
        .execute("{ Task(filter: {task_id: {_eq: \"other-agent-task\"}}) {_docID} }")
        .await
        .unwrap();
    let visible = !response["data"]["Task"].as_array().unwrap().is_empty();
    assert_eq!(visible, expected, "native selected Task access");
    let tools = fixture.tools(&["automation"]);
    let result = call_config_tool(
        &tools,
        vec![
            "automation".into(),
            "get".into(),
            "task".into(),
            "other-agent-task".into(),
            "--agent".into(),
            sibling,
        ],
    )
    .await;
    assert_eq!(
        result.is_ok(),
        expected,
        "tool must retain invoking Agent access: {result:?}"
    );
}

#[tokio::test]
async fn pack_search_uses_registry_pagination_without_installing() {
    let _registry_lock = PACK_REGISTRY_ENV.lock().await;
    let app = axum::Router::new().route("/api/v1/packs", axum::routing::get(
        |axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>| async move {
            assert_eq!(query.get("q").map(String::as_str), Some("code review"));
            let page = &query["page"];
            axum::Json(json!({"packs":[{"namespace":"fixture","name":"documents_fixture","latest":"1.0.0","description":"Code review"}],"has_more":page == "1"}))
        }
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let _registry =
        gents_store::test_support::EnvVarGuard::set("GENTS_REGISTRY", format!("http://{address}"));
    let (fixture, tools) = signed_pack_tool("registry-search", test_plugins()).await;
    let tool = tools.iter().find(|t| t.name() == CONFIG_TOOL_NAME).unwrap();
    let first: Value = serde_json::from_str(
        &tool
            .call(json!({"argv":["pack","search"],"options":{"query":"code review"}}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        first["items"][0]["inspect"],
        json!({"argv":["pack","get","fixture/documents_fixture"]})
    );
    let next: Value =
        serde_json::from_str(&tool.call(first["next_call"].to_string()).await.unwrap()).unwrap();
    assert_eq!(next["page"], 2);
    assert!(next["next_call"].is_null());
    for options in [
        json!({"page":0}),
        json!({"page":"abc"}),
        json!({"registry":"http://untrusted"}),
    ] {
        assert!(tool
            .call(json!({"argv":["pack","search"],"options":options}).to_string())
            .await
            .is_err());
    }
    assert!(gents_pack::pack::read_installed_pack(
        &fixture.user_access,
        fixture.db.node_identity.did(),
        "fixture/documents_fixture",
    )
    .await
    .unwrap()
    .is_none());
    server.abort();
}

#[tokio::test]
async fn pack_install_blocks_a_dependency_plugin_without_operator_authority() {
    let edit = |dir: &std::path::Path, change: &dyn Fn(&mut Value)| {
        let path = dir.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        change(&mut manifest);
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    };
    let (_dependee, dependee_dir) =
        gents_pack::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    std::fs::create_dir_all(dependee_dir.join("plugins")).unwrap();
    let wat = gents_pack::plugin::test_fixtures::constant_output_wat(b"{}");
    std::fs::write(
        dependee_dir.join("plugins/echo.afb"),
        gents_pack::plugin::test_fixtures::build_plugin_afb(&wat),
    )
    .unwrap();
    edit(&dependee_dir, &|manifest| {
        manifest["assets"]
            .as_array_mut()
            .unwrap()
            .push(json!("plugins/echo.afb"));
        manifest["plugins"] = json!([{
            "name": "echo",
            "description": "Echoes nothing.",
            "artifact": "plugins/echo.afb",
            "language": "rust",
            "input_schema": {"type": "object"},
            "manifold": {
                "fs": "None",
                "net": {"OutboundHttp": ["api.example.com"]},
                "env": "None",
                "crypto": false,
                "child_process": false,
            },
        }]);
    });
    let (_dependent, dependent_dir) =
        gents_pack::test_support::fixture_pack_copy("dependent_fixture", &json!({}));
    let (home, plugins) = crate::test_support::home_with_pack_dir(&dependent_dir);
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&dependee_dir).unwrap();
    gents_pack::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap();
    let (fixture, tools) = signed_pack_tool("dependency-plugin-authority", plugins).await;
    let slot = format!("worker={}:inference", fixture.agent_did());
    let slots = ["--inference-slot", slot.as_str()];
    let mut preview_argv = vec!["pack", "preview", "install", "fixture/dependent_fixture"];
    preview_argv.extend(slots);
    let preview: Value =
        serde_json::from_str(&config_call(&tools, &preview_argv).await.unwrap()).unwrap();
    assert_eq!(preview["ready"], false, "{preview}");
    assert!(preview["installation_blockers"]
        .to_string()
        .contains("api.example.com"));
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut install_argv = vec![
        "pack",
        "install",
        "fixture/dependent_fixture",
        "--digest",
        digest,
    ];
    install_argv.extend(slots);
    let error = format!(
        "{:#}",
        config_call(&tools, &install_argv).await.unwrap_err()
    );
    assert!(error.contains("api.example.com"), "{error}");
    assert!(
        gents_pack::plugin::store::read_record(home.path(), "fixture", "echo").is_err(),
        "a refused plugin leaves no record"
    );
}

/// Editing an installed document live and installing again is the agent's
/// loop: the preview lists the edited documents and what each choice does,
/// the refusal names the same call with the choice, and each choice does
/// what the operator CLI's `--keep` and `--overwrite` do.

#[tokio::test]
async fn an_agent_decides_what_a_pack_does_with_documents_edited_live() {
    let (_fixture, directory) =
        gents_pack::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let manifest_path = directory.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (_home, plugins) = crate::test_support::home_with_pack_dir(&directory);
    let (fixture, tools) = signed_pack_tool("pack-edited-documents", plugins).await;
    let slot = format!("worker={}:inference", fixture.agent_did());
    let pack = "fixture/documents_fixture";
    let pinned = "fixture/documents_fixture@1.0.0";
    async fn slotted(
        tools: &[Box<dyn ToolDyn>],
        slot: &str,
        words: &[&str],
    ) -> Result<String, gents_loop::tool::ToolError> {
        let mut argv = words.to_vec();
        argv.extend(["--inference-slot", slot]);
        config_call(tools, &argv).await
    }
    let json_of = |text: String| serde_json::from_str::<Value>(&text).unwrap();
    let preview = json_of(
        slotted(&tools, &slot, &["pack", "preview", "install", pack])
            .await
            .unwrap(),
    );
    let digest = preview["artifact_digest"].as_str().unwrap().to_owned();
    assert_eq!(preview["edited_documents"], Value::Null, "{preview}");
    slotted(
        &tools,
        &slot,
        &["pack", "install", pack, "--digest", &digest],
    )
    .await
    .unwrap();

    let task_name = || async {
        fixture
            .user_access
            .execute(r#"{ Task(filter: {task_id: {_eq: "fixture-worker-task"}}) { _docID display_name } }"#)
            .await
            .unwrap()["data"]["Task"][0]
            .clone()
    };
    let doc = task_name().await["_docID"].as_str().unwrap().to_owned();
    fixture
        .user_access
        .transact_write("test.pack.edit_task", |txn| {
            let doc = &doc;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(r#"mutation($input: TaskMutationInputArg!) {{ update_Task(docID: "{doc}", input: $input) {{ _docID }} }}"#),
                    &json!({"input": {"display_name": "edited live"}}),
                    gents_protocol::schemas::TASK,
                )
                .await
            })
        })
        .await
        .unwrap();

    let preview = json_of(
        slotted(&tools, &slot, &["pack", "preview", "update", pinned])
            .await
            .unwrap(),
    );
    let edited = &preview["edited_documents"];
    assert_eq!(
        edited["documents"],
        json!(["Task/fixture-worker-task"]),
        "{preview}"
    );
    assert!(
        edited["keep"].as_str().unwrap().contains("as edited"),
        "{preview}"
    );
    assert!(
        edited["overwrite"]
            .as_str()
            .unwrap()
            .contains("pack's versions"),
        "{preview}"
    );

    let refused = slotted(
        &tools,
        &slot,
        &["pack", "update", pinned, "--digest", &digest],
    )
    .await
    .unwrap_err()
    .to_string();
    let refused = refused.strip_prefix("ToolCallError: ").unwrap_or(&refused);
    let failure: Value = serde_json::from_str(&refused[refused.find('{').unwrap()..]).unwrap();
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .contains("Task/fixture-worker-task of fixture/documents_fixture was edited"),
        "{failure}"
    );
    assert!(!refused.contains("operator CLI"), "{refused}");
    let next = &failure["recovery"]["next_call"];
    assert_eq!(next["options"]["edited"], "keep", "{failure}");
    assert_eq!(next["argv"][1], "update", "{failure}");
    assert_eq!(task_name().await["display_name"], "edited live");

    let recover = |decision: &'static str| {
        let tools = &tools;
        let mut next = next.clone();
        next["options"]["edited"] = json!(decision);
        async move {
            tools
                .iter()
                .find(|tool| tool.name() == CONFIG_TOOL_NAME)
                .unwrap()
                .call(next.to_string())
                .await
                .unwrap_or_else(|error| panic!("{decision}: {error}"))
        }
    };
    let kept = json_of(recover("keep").await);
    assert_eq!(
        kept["install"]["kept"],
        json!(["Task/fixture-worker-task"]),
        "{kept}"
    );
    assert_eq!(task_name().await["display_name"], "edited live");
    recover("overwrite").await;
    assert_eq!(task_name().await["display_name"], "fixture-worker-task");

    let bad = config_call(&tools, &["pack", "remove", pack, "--edited", "maybe"])
        .await
        .unwrap_err()
        .to_string();
    assert!(bad.contains("overwrite") && bad.contains("keep"), "{bad}");
    let removal = json_of(
        config_call(&tools, &["pack", "preview", "remove", pack])
            .await
            .unwrap(),
    );
    assert_eq!(removal["committed"], false, "{removal}");
    assert_eq!(removal["edited_documents"], Value::Null, "{removal}");
    assert_eq!(
        removal["apply_with"]["argv"],
        json!(["pack", "remove", pack]),
        "{removal}"
    );
}

async fn account_state_rows(fixture: &SelfConfigFixture) -> Value {
    let access = gents_inference::auth::credential::runtime_credential_access(
        &fixture.db.node,
        fixture.node_did(),
    )
    .unwrap();
    let credentials = access
        .execute("{ OAuthCredential { _docID credential_id enabled label access_token } }")
        .await
        .unwrap();
    let backend = fixture.user_access.execute("{ InferenceBackendMetadata { _docID backend_id enabled auth_kind } BackendCredential { _docID credential_id secret } }").await.unwrap();
    json!({"credentials": credentials["data"], "backend": backend["data"]})
}

async fn seed_account_view(
    cache: &gents_inference::usage_observation::UsageCache,
    fixture: &SelfConfigFixture,
) {
    use gents_inference::auth::credential::{
        oauth_credential_id, upsert_oauth_credential, OAuthCredential,
    };
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let provider = gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER;
    let account = |account_ref: Option<&str>, label: &str, enabled: bool| OAuthCredential {
        doc_id: None,
        credential_id: match account_ref {
            Some(account_ref) => format!("{}:{account_ref}", oauth_credential_id(owner, provider)),
            None => oauth_credential_id(owner, provider),
        },
        node_did: owner.to_string(),
        provider: provider.to_string(),
        access_token: "access-SECRET".into(),
        refresh_token: "refresh-SECRET".into(),
        id_token: None,
        account_id: Some("identity-SECRET".into()),
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled,
        account_ref: account_ref.map(str::to_string),
        connected_at: account_ref.map(|_| chrono::Utc::now()),
        provider_account_key: None,
        label: Some(label.to_string()),
    };
    upsert_oauth_credential(node, &account(None, "Agentl", true))
        .await
        .unwrap();
    upsert_oauth_credential(node, &account(Some("acct-l2"), "Work", false))
        .await
        .unwrap();
    let access = fixture.user_access();
    for (backend_id, kind, auth, endpoint) in [
        (
            "backend-usage-claude",
            "ClaudeCliSubscription",
            json!({ "kind": "node_oauth" }),
            "claude-cli://subscription",
        ),
        (
            "backend-usage-work",
            "ClaudeCliSubscription",
            json!({ "kind": "node_oauth", "account_ref": "acct-l2" }),
            "claude-cli://subscription",
        ),
        (
            "backend-usage-key",
            "OpenRouter",
            json!({ "kind": "api_key", "key": "key-SECRET" }),
            "http://127.0.0.1:9/api/v1",
        ),
    ] {
        let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner,
            "backend_id": backend_id,
            "name": backend_id,
            "provider_kind": kind,
            "endpoint": endpoint,
            "auth": auth,
        }))
        .unwrap();
        gents_documents::write_inference_backend_document(
            access,
            &backend,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }
    let profile = serde_json::from_value(json!({
        "node_did": owner,
        "profile_id": "profile-usage-a",
        "backend_id": "backend-usage-claude",
        "model_name": "model-x",
    }))
    .unwrap();
    gents_documents::write_inference_profile_document(
        access,
        &profile,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    gents_inference::usage_observation::record_usage(
        cache,
        node,
        &gents_inference::usage_observation::UsageAccount::Credential {
            doc_id: None,
            node_did: owner.to_string(),
            provider: provider.to_string(),
            account_ref: None,
        },
        gents_inference::usage_observation::account_usage::UsageReport {
            windows: vec![
                gents_inference::usage_observation::account_usage::UsageWindow {
                    label: "5h".into(),
                    window_minutes: Some(300),
                    used_pct: 42.0,
                    resets_at: None,
                    source: gents_inference::usage_observation::account_usage::UsageSource::Header,
                    observed_at: chrono::Utc::now(),
                },
            ],
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn config_backend_accounts_lists_accounts_and_usage_read_only() {
    let fixture = SelfConfigFixture::new("config-accounts").await;
    fixture.grant_node_reader().await;
    let cache = std::sync::Arc::new(
        gents_inference::usage_observation::UsageCache::with_endpoints(
            gents_inference::usage_observation::UsageEndpoints {
                claude_usage: "http://127.0.0.1:9/usage".into(),
                grok_billing: "http://127.0.0.1:9/billing".into(),
            },
        ),
    );
    seed_account_view(&cache, &fixture).await;
    let mut tool_config = config(&["node", "profile", "backend"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    let tools = build_self_config_tools_with_runtime(
        cache,
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        Some(fixture.agent.clone()),
        Some(fixture.db.node_identity.clone()),
        &tool_config,
        test_plugins(),
        Arc::new(gents_tools::task_run::NoTaskRunService),
        Some(fixture.node_binding.clone()),
    );
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let before = account_state_rows(&fixture).await;

    let text = config
        .call(json!({"argv":["backend", "accounts"]}).to_string())
        .await
        .unwrap();

    let view: Value = serde_json::from_str(&text).unwrap();
    let items = view["items"].as_array().expect("items");
    let item = |label: &str| {
        items
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("no {label}: {text}"))
    };
    let agentl = item("Agentl");
    assert_eq!(agentl["provider"], "claude-subscription");
    assert_eq!(agentl["state"], "enabled");
    assert_eq!(agentl["profiles"], json!(["profile-usage-a"]));
    assert_eq!(agentl["usage"]["windows"][0]["used_pct"], 42.0);
    let work = item("Work");
    assert_eq!(work["state"], "disabled");
    assert_eq!(work["usage"], Value::Null);
    let key = item("backend-usage-key");
    assert_eq!(key["provider"], "OpenRouter");
    assert_eq!(key["state"], "enabled");
    assert_eq!(key["usage"]["note"], "unknown");
    assert!(
        view["note"].as_str().unwrap().contains("Read-only"),
        "{text}"
    );
    for hidden in [
        "SECRET",
        "identity-",
        "credential_id",
        "account_ref",
        "acct-l2",
    ] {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    assert_eq!(account_state_rows(&fixture).await, before);

    let help = config
        .call(json!({"argv":["help", "backend"]}).to_string())
        .await
        .unwrap();
    assert!(help.contains("backend accounts"), "{help}");
    assert!(config
        .call(json!({"argv":["backend", "accounts", "--set", "x"]}).to_string())
        .await
        .is_err());
}

#[tokio::test]
async fn config_backend_accounts_needs_the_backend_grant() {
    let fixture = SelfConfigFixture::new("config-accounts-grant").await;
    let tools = fixture.tools(&["node", "profile"]);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let error = config
        .call(json!({"argv":["backend", "accounts"]}).to_string())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("backend configuration is not granted"), "{error}");
    assert!(config
        .call(json!({"argv":["backend", "list"]}).to_string())
        .await
        .is_err());
}

#[tokio::test]
async fn no_lockout_rejects_current_agent_disable_in_preview_and_commit() {
    let fixture = SelfConfigFixture::new("disable-preview-parity").await;
    fixture.grant_selected_configuration().await;
    let default = fixture.install_sibling("default").await;
    fixture.grant_node_reader().await;
    fixture
        .update_signed_configuration(
            gents_store::collection::Collection::DefaultAgentSelection,
            fixture.node_did(),
            json!({"default_agent_did": default}),
        )
        .await;
    let current = fixture.agent_did();
    let mut grants = config(&["node", "agent"]);
    grants.agent_did = current.to_owned();
    grants.no_lockout = true;
    grants.preview = true;
    let tools = fixture.tools_with_config(&grants);
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            ["agent", "preview", "disable", "--id", current]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["admitted"], false, "{preview}");
    assert_eq!(preview["refusal"], "self_lockout", "{preview}");
    let error = call_config_tool(
        &tools,
        ["agent", "disable", "--id", current]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .await
    .unwrap_err();
    assert!(error.contains("an actor cannot disable itself"), "{error}");
    assert!(
        error.contains(&preview["message"].as_str().unwrap().replace('"', "\\\"")),
        "{error}"
    );
    let current = gents_documents::list_agents(&fixture.db.node, fixture.node_did())
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == current)
        .unwrap();
    assert!(current.enabled);
}

#[tokio::test]
async fn agent_only_edits_preserve_an_optional_absent_context() {
    let fixture = SelfConfigFixture::new("contextless-agent-edit").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_agent_relation_delete().await;
    let other = fixture.install_sibling("other").await;
    fixture.grant_node_reader().await;
    fixture.grant_sibling_configuration(&other).await;
    let current = fixture.agent_did();
    let other_profile = format!("{other}:inference");
    let core = fixture.core();
    core.apply(agent_request(&core, vec![("context_id".into(), None)]))
        .await
        .unwrap();
    let tools = fixture.tools(&["agent"]);
    for (prefix, committed) in [
        (vec!["agent", "preview", "edit", current], false),
        (vec!["agent", "edit", current], true),
    ] {
        let mut argv: Vec<String> = prefix.into_iter().map(str::to_owned).collect();
        argv.extend(
            [
                "--set",
                r#"display_name="Contextless""#,
                "--set",
                &format!(
                    "inference_profile_id={}",
                    serde_json::to_string(&other_profile).unwrap()
                ),
            ]
            .into_iter()
            .map(str::to_owned),
        );
        let result: Value =
            serde_json::from_str(&call_config_tool(&tools, argv).await.unwrap()).unwrap();
        assert_eq!(result["committed"], committed);
    }
    let current = gents_documents::list_agents(&fixture.db.node, fixture.node_did())
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == current)
        .unwrap();
    assert_eq!(current.display_name.as_deref(), Some("Contextless"));
    assert_eq!(current.inference_profile_id, other_profile);
    assert_eq!(current.context_id, None);
}

#[tokio::test]
async fn native_pack_local_install_inspection_selection_and_digest_fence() {
    let (workspace, artifact, archive) =
        gents_pack::pack::plugin_lifecycle::tests::fixture("1.0.0", false);
    let home = tempfile::tempdir().unwrap();
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let fixture = SelfConfigFixture::new("native-pack").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    fixture.publish_workspace_root(workspace.path()).await;
    let node = fixture.db.node.clone();
    let did = fixture.db.node_identity.did().to_owned();
    let mut cfg = config(&["tools", "agent"]);
    cfg.agent_did = fixture.agent_did().to_owned();
    cfg.enable_pack_install = true;
    cfg.process_ceiling.file_mode = gents_documents::FileToolMode::ReadOnly;
    cfg.process_ceiling.root = Some(workspace.path().to_owned());
    let tools = fixture.tools_with_plugins(&cfg, plugins.clone());
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    tool.call(json!({"argv":["tools","update"],"set":{"host":{"root":workspace.path(),"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}}).to_string()).await.unwrap();
    tool.call(
        json!({"argv":["tools","update"],"set":{"integrations":{"lsp":{"timeout_secs":31}}}})
            .to_string(),
    )
    .await
    .unwrap();
    let path = artifact.to_str().unwrap();
    let preview: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "preview", "install", path])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], true);
    assert_eq!(preview["artifact_digest"], archive.digest());
    assert_eq!(
        preview["apply_with"]["argv"],
        json!(["pack", "install", archive.digest()])
    );
    assert!(gents_pack::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    let wrong = format!("sha256:{}", "0".repeat(64));
    let error = config_call(&tools, &["pack", "install", path, "--digest", &wrong])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&wrong) && error.contains("nothing was written"),
        "{error}"
    );
    assert!(gents_pack::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "install", path, "--digest", archive.digest()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["committed"], true);
    let inspect: Value = serde_json::from_str(
        &config_call(&tools, &["plugin", "get", "fixture/native_echo"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inspect["selection"]["plugin"], "fixture/native_echo");
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let before: Value =
        serde_json::from_str(&config_call(&tools, &["tools", "get"]).await.unwrap()).unwrap();
    assert!(
        !before.to_string().contains("native_echo"),
        "install must not select a tool"
    );
    let mut integrations = before["document"]["integrations"].clone();
    integrations
        .as_object_mut()
        .unwrap()
        .insert("plugins".into(), json!([inspect["selection"].clone()]));
    tool.call(json!({"argv":["tools","update"],"set":{"integrations":integrations}}).to_string())
        .await
        .unwrap();
    let readback = config_call(&tools, &["tools", "get"]).await.unwrap();
    assert!(readback.contains("fixture/native_echo"));
    let persisted: gents_documents::Tools = fixture
        .user_access
        .transact_readonly("test.self_config.read_native_pack_tools", |txn| {
            let did = did.clone();
            let tools_id = format!("{}:tools", fixture.agent_did());
            Box::pin(async move {
                let value = gents_documents::read_desired_state_document_in_txn(
                    txn,
                    gents_store::collection::Collection::Tools,
                    &did,
                    &tools_id,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("persisted Tools document disappeared"))?;
                Ok::<_, anyhow::Error>(serde_json::from_value(value)?)
            })
        })
        .await
        .unwrap();
    assert_eq!(
        persisted
            .integrations
            .as_ref()
            .and_then(|integrations| integrations.lsp.as_ref())
            .and_then(|lsp| lsp.timeout_secs),
        Some(31),
        "selecting the plugin must preserve another configured integration"
    );
    let record =
        gents_pack::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    assert!(record.granted.is_none());
    let surface = gents_tools::surface::AgentToolSurfaceConfig::from_tools_document(
        fixture.agent_did(),
        &persisted,
        &gents_tools::surface::ToolCeiling::readonly_at(workspace.path()),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node, &did, &*plugins)
    .await
    .unwrap();
    let runtime =
        gents_tools::surface::ToolRuntimeContext::oneshot_with_node_did(node.clone(), &did)
            .with_agent_for_test(fixture.db.node_identity.clone(), fixture.agent.clone())
            .with_plugins(plugins.clone());
    let fresh_tools = surface.build_tools(&runtime).await.unwrap();
    let fresh_tool = fresh_tools
        .iter()
        .find(|tool| tool.name() == "native_echo")
        .expect("a fresh ToolSurface built from persisted Tools should expose the selected plugin");
    let output: Value = serde_json::from_str(&fresh_tool.call("{}".into()).await.unwrap()).unwrap();
    assert_eq!(output, json!({"native_pack_echo":"ok"}));
    let (_next_dir, next_path, next) =
        gents_pack::pack::plugin_lifecycle::tests::fixture("2.0.0", false);
    std::fs::copy(next_path, &artifact).unwrap();
    let error = config_call(
        &tools,
        &["pack", "update", path, "--digest", archive.digest()],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains(archive.digest()) && error.contains("nothing was written"));
    config_call(&tools, &["pack", "update", path, "--digest", next.digest()])
        .await
        .unwrap();
    let status: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", "fixture/native_echo_pack"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status["installed"]["version"], "2.0.0");

    // A rebuilt artifact meets the pin: the refusal is the Tools edit that
    // moves it, and after that edit the same update goes through.
    let (_rebuilt_dir, rebuilt_path, rebuilt) =
        gents_pack::pack::plugin_lifecycle::tests::fixture_with_output(
            "3.0.0",
            false,
            br#"{"native_pack_echo":"rebuilt"}"#,
        );
    std::fs::copy(rebuilt_path, &artifact).unwrap();
    let pinned = config_call(
        &tools,
        &["pack", "update", path, "--digest", rebuilt.digest()],
    )
    .await
    .unwrap_err()
    .to_string();
    let failure: Value = serde_json::from_str(&pinned[pinned.find('{').unwrap()..]).unwrap();
    let message = failure["error"].as_str().unwrap();
    assert!(
        message.contains("nothing was installed") && message.contains("fixture/native_echo"),
        "{failure}"
    );
    let move_pin = &failure["recovery"]["next_call"];
    assert_eq!(move_pin["argv"], json!(["tools", "update"]), "{failure}");
    assert_eq!(
        move_pin["set"]["integrations"]["lsp"]["timeout_secs"], 31,
        "the pin move keeps the other integrations: {failure}"
    );
    assert_eq!(
        gents_pack::pack::read_home_install(home.path(), "fixture/native_echo_pack")
            .unwrap()
            .unwrap()
            .version,
        "2.0.0"
    );
    tool.call(move_pin.to_string()).await.unwrap();
    config_call(
        &tools,
        &["pack", "update", path, "--digest", rebuilt.digest()],
    )
    .await
    .unwrap();
    let rebuilt_record =
        gents_pack::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    assert_eq!(
        move_pin["set"]["integrations"]["plugins"][0]["digest"],
        rebuilt_record.digest.as_str()
    );
    let removal = config_call(&tools, &["pack", "remove", "fixture/native_echo_pack"])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        removal.contains(
            "fixture/native_echo_pack was not removed (fixture/native_echo): selected by"
        ) && removal.contains(r#""argv":["pack","wire","fixture/native_echo_pack","--remove"]"#),
        "{removal}"
    );
    assert_eq!(
        gents_pack::plugin::store::read_record(home.path(), "fixture", "native_echo")
            .unwrap()
            .version,
        "3.0.0"
    );
}

#[tokio::test]
async fn native_pack_local_paths_and_approval_fail_closed() {
    let (workspace, artifact, _) =
        gents_pack::pack::plugin_lifecycle::tests::fixture("1.0.0", true);
    let home = tempfile::tempdir().unwrap();
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let fixture = SelfConfigFixture::new("native-pack-refusals").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    fixture.publish_workspace_root(workspace.path()).await;
    let mut cfg = config(&["tools"]);
    cfg.agent_did = fixture.agent_did().to_owned();
    cfg.enable_pack_install = true;
    cfg.process_ceiling.root = Some(workspace.path().to_owned());
    let tools = fixture.tools_with_plugins(&cfg, plugins.clone());
    let path = artifact.to_str().unwrap();
    assert!(config_call(&tools, &["pack", "get", path])
        .await
        .unwrap_err()
        .to_string()
        .contains("effective file read permission"));
    cfg.process_ceiling.file_mode = gents_documents::FileToolMode::ReadOnly;
    let tools = fixture.tools_with_plugins(&cfg, plugins);
    assert!(config_call(&tools, &["pack", "get", path])
        .await
        .unwrap_err()
        .to_string()
        .contains("effective file read permission"));
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    tool.call(json!({"argv":["tools","update"],"set":{"host":{"root":workspace.path(),"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}}).to_string()).await.unwrap();
    let preview: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "preview", "install", path])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], false);
    assert_eq!(preview["apply_with"], Value::Null);
    let digest = preview["artifact_digest"].as_str().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    assert!(tool
        .call(
            json!({"argv":["pack","install",path],"options":{"digest":digest,"approved":true}})
                .to_string()
        )
        .await
        .is_err());
    assert!(
        config_call(&tools, &["pack", "install", path, "--digest", digest])
            .await
            .is_err()
    );
    assert!(gents_pack::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("outside.pack");
    std::fs::copy(&artifact, &outside_path).unwrap();
    let error = config_call(&tools, &["pack", "get", outside_path.to_str().unwrap()])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("outside the allowed tool root"));
    #[cfg(unix)]
    {
        let link = workspace.path().join("escape.pack");
        std::os::unix::fs::symlink(&outside_path, &link).unwrap();
        assert!(
            config_call(&tools, &["pack", "get", link.to_str().unwrap()])
                .await
                .unwrap_err()
                .to_string()
                .contains("outside the allowed tool root")
        );
    }
    let corrupt = workspace.path().join("corrupt.pack");
    std::fs::write(&corrupt, b"not a pack").unwrap();
    assert!(
        config_call(&tools, &["pack", "get", corrupt.to_str().unwrap()])
            .await
            .is_err()
    );
    assert!(
        config_call(&tools, &["pack", "get", workspace.path().to_str().unwrap()])
            .await
            .is_err()
    );
}

/// The local built lifecycle accepts the same pack kinds the registry
/// lifecycle accepts. A documents pack (documents + plugins + inference
/// slots) reached by explicit path must preview and install through the
/// canonical document installer, with plugin authority still fenced behind
/// operator blockers: the pack's plugin is refused at commit until the
/// grant exists in the plugin home, so the exact downloaded-style artifact
/// bytes flow through the same digest-pinned receipt.

#[tokio::test]
async fn native_pack_local_path_installs_mixed_documents_pack_with_fenced_plugin_authority() {
    let (fixture, directory) =
        gents_pack::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let manifest_path = directory.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    // Make this a genuinely mixed pack: the documents pack ships a plugin
    // that asks for network authority, so the operator fence is exercised.
    // The fixture compiler replaces plugin artifacts, so the plugin is
    // written by hand after the copy.
    let plugins = json!([{
        "name": "fixture_notifier",
        "description": "Deterministic native pack fixture plugin asking for outbound HTTP",
        "artifact": "plugins/fixture_notifier.afb",
        "language": "rust",
        "input_schema": {"type": "object", "properties": {}, "additionalProperties": false},
        "manifold": {"fs": "None", "net": {"OutboundHttp": ["https://example.invalid"]}, "env": "None", "crypto": false, "child_process": false}
    }]);
    manifest["plugins"] = plugins.clone();
    manifest["assets"] = json!(manifest["assets"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .chain(std::iter::once(Value::String(
            "plugins/fixture_notifier.afb".into(),
        )))
        .collect::<Vec<_>>());
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let plugins_dir = directory.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    let wat =
        gents_pack::plugin::test_fixtures::constant_output_wat(br#"{"native_mixed_echo":"ok"}"#);
    std::fs::write(
        plugins_dir.join("fixture_notifier.afb"),
        gents_pack::plugin::test_fixtures::build_plugin_afb(&wat),
    )
    .unwrap();

    // The artifact on disk is a built .pack, reached by explicit path.
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&directory).unwrap();
    let artifact = fixture.path().join("local_mixed.pack");
    std::fs::write(&artifact, &bytes).unwrap();

    let (home, plugins) = crate::test_support::home_with_pack_dir(&directory);
    let (agent_fixture, tools) =
        pack_tool_with_local_read("local-mixed-documents", plugins, fixture.path()).await;
    let node = &agent_fixture.db.node;
    let did = agent_fixture.db.node_identity.did();
    let slot = format!("worker={}:inference", agent_fixture.agent_did());

    // get: reports kind, digest and the operator blocker for the plugin.
    let get: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", artifact.to_str().unwrap()])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(get["manifest"]["kind"], "documents");
    assert_eq!(get["installable"], true);
    assert_eq!(get["source"], "local");
    let digest = get["artifact_digest"].as_str().unwrap().to_owned();
    assert_eq!(
        get["installation_blockers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|blocker| blocker["plugin"] == "fixture_notifier")
            .count(),
        1,
        "{get}"
    );

    // preview: no slot bound yet -> missing_inference_slots, not committed.
    let missing: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "preview", "install", artifact.to_str().unwrap()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(missing["ready"], false);
    assert_eq!(missing["missing_inference_slots"], json!(["worker"]));
    assert_eq!(missing["committed"], false);

    // preview with the slot bound: ready, digest-pinned apply_with, and the
    // plugin blocker still reported for the operator.
    let preview: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                artifact.to_str().unwrap(),
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], false, "{preview}");
    assert_eq!(preview["apply_with"], Value::Null);
    assert_eq!(preview["missing_inference_slots"], Value::Null);
    assert_eq!(
        preview["installation_blockers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|blocker| blocker["plugin"] == "fixture_notifier")
            .count(),
        1,
        "{preview}"
    );
    assert_eq!(preview["artifact_digest"], Value::String(digest.clone()));

    // Commit without the operator grant is refused; nothing installs.
    let refused = config_call(
        &tools,
        &[
            "pack",
            "install",
            artifact.to_str().unwrap(),
            "--digest",
            &digest,
            "--inference-slot",
            &slot,
        ],
    )
    .await
    .unwrap_err();
    assert!(
        refused.to_string().contains("operator authorization"),
        "{refused}"
    );
    let access = &agent_fixture.user_access;
    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );

    // The operator authorizes this exact artifact through the plugin grant
    // owner: the home's plugin store records a grant covering the declared
    // manifold. Model input cannot create it; the commit fence reads it.
    let plugin_declaration: gents_pack::pack::PackPlugin =
        serde_json::from_value(manifest["plugins"][0].clone()).unwrap();
    let granted = gents_pack::plugin::authority::declared_manifold(&plugin_declaration).unwrap();
    gents_pack::plugin::store::write_record(
        home.path(),
        &gents_pack::plugin::store::InstalledPlugin {
            namespace: "fixture".into(),
            name: "fixture_notifier".into(),
            version: "1.0.0".into(),
            digest: "sha256:operator-grant".into(),
            language: "rust".into(),
            declaration: plugin_declaration,
            granted: Some(granted),
            instructions: None,
            owner_pack_coordinate: None,
            owner_pack_digest: None,
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
            data_collection_schemas: Default::default(),
        },
    )
    .unwrap();
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "install",
                artifact.to_str().unwrap(),
                "--digest",
                &digest,
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        installed["effective"]["digest"],
        Value::String(digest.clone())
    );
    assert!(node.get_collection("FixtureJob").unwrap().is_some());
    let record =
        gents_pack::plugin::store::read_record(home.path(), "fixture", "fixture_notifier").unwrap();
    assert_eq!(
        record.owner_pack_coordinate.as_deref(),
        Some("fixture/documents_fixture")
    );
    assert!(
        record.granted.is_some(),
        "the operator grant the fence required is on the installed record"
    );
}

/// A plugins-only pack keeps the pre-existing local path behavior: preview
/// reports blockers under an untrusted namespace and commit stays blocked
/// until the operator grant exists in the plugin home.

#[tokio::test]
async fn native_pack_local_path_plugins_only_still_requires_operator_grant() {
    let (workspace, artifact, _) =
        gents_pack::pack::plugin_lifecycle::tests::fixture("1.0.0", true);
    let home = tempfile::tempdir().unwrap();
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let workspace = &workspace;
    let (_agent_fixture, tools) =
        pack_tool_with_local_read("local-plugins-only", plugins, workspace.path()).await;
    let path = artifact.to_str().unwrap().to_owned();

    let preview: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "preview", "install", &path])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], false, "{preview}");
    assert_eq!(preview["apply_with"], Value::Null);
    assert!(!preview["installation_blockers"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        gents_pack::plugin::store::list_records(home.path())
            .unwrap()
            .is_empty(),
        "preview must not publish plugins"
    );
}

/// A pack tool whose invoking agent holds effective file read authority over
/// `root` (process ceiling ReadOnly plus an effective tools grant), the
/// authority the local built-artifact path requires before it opens a .pack.

async fn pack_tool_with_local_read(
    label: &str,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
    root: &std::path::Path,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    fixture.grant_engineer_tool_sources().await;
    fixture.grant_agent_schema().await;
    fixture.publish_workspace_root(root).await;
    let mut tool_config = config(&["tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    tool_config.process_ceiling.file_mode = gents_documents::FileToolMode::ReadOnly;
    tool_config.process_ceiling.root = Some(root.to_owned());
    let tools = fixture.tools_with_plugins(&tool_config, plugins);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    tool.call(json!({"argv":["tools","update"],"set":{"host":{"root":root,"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}}).to_string()).await.unwrap();
    (fixture, tools)
}

/// Installs two ChatGptCodex OAuth backends for `owner` — the provider's
/// default account and another account of the same provider — with one
/// inference profile each, plus the enabled credentials the default-account
/// resolver needs to keep the account-free reference as the provider
/// default. Binding to `p-default` is allowed; `p-other` selects another
/// account and must be refused.

async fn install_chat_account_fences(fixture: &SelfConfigFixture) {
    let owner = fixture.db.node_identity.did();
    let access = &fixture.user_access;
    let node = &fixture.db.node;
    for (id, auth) in [
        ("chat-original", json!({"kind":"node_oauth"})),
        (
            "chat-b",
            json!({"kind":"node_oauth","account_ref":"acct-b"}),
        ),
    ] {
        let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": id, "name": id,
            "provider_kind": "ChatGptCodex", "endpoint": "http://127.0.0.1:1/v1",
            "auth": auth,
        }))
        .unwrap();
        gents_documents::write_inference_backend_document(
            access,
            &backend,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }
    for (id, backend) in [("p-default", "chat-original"), ("p-other", "chat-b")] {
        let profile: gents_documents::InferenceProfile = serde_json::from_value(json!({
            "node_did": owner, "profile_id": id, "backend_id": backend,
            "model_name": "test-model",
        }))
        .unwrap();
        gents_documents::write_inference_profile_document(
            access,
            &profile,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }
    let provider = gents_inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
    let base_id = gents_inference::auth::credential::oauth_credential_id(owner, provider);
    for (account_ref, connected) in [(None, false), (Some("acct-b"), true)] {
        let credential = gents_inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: match account_ref {
                Some(account_ref) => format!("{base_id}:{account_ref}"),
                None => base_id.clone(),
            },
            node_did: owner.to_string(),
            provider: provider.to_string(),
            access_token: "access-TEST".into(),
            refresh_token: "refresh-TEST".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
            account_ref: account_ref.map(str::to_string),
            connected_at: connected.then(chrono::Utc::now),
            provider_account_key: None,
            label: None,
        };
        gents_inference::auth::credential::upsert_oauth_credential(node, &credential)
            .await
            .unwrap();
    }
}

/// A documents pack enforces inference account authority: a slot bound to a
/// profile whose backend selects another
/// account of one provider is refused at preview and at commit — before any
/// schema or installation side effect — while the provider's default account
/// and an account-free backend install.

#[tokio::test]
async fn pack_documents_install_refuses_non_default_account_inference_bindings() {
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (fixture, tools) = signed_pack_tool("documents-account-fence", plugins).await;
    install_chat_account_fences(&fixture).await;
    // The User publishes the fenced profiles under the Node; the Agent reads
    // them through its Node read grant and manages their selection.
    fixture.grant_node_reader().await;
    fixture
        .grant_account_fixture_selections(&[
            (
                gents_store::collection::Collection::InferenceProfile,
                "p-default".into(),
                false,
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                "p-other".into(),
                false,
            ),
        ])
        .await;
    let node = &fixture.db.node;
    let did = fixture.db.node_identity.did();
    let access = &fixture.user_access;
    let get: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", "fixture/documents_fixture"])
            .await
            .unwrap(),
    )
    .unwrap();
    let digest = get["artifact_digest"].as_str().unwrap().to_owned();

    // Preview refuses the non-default account binding.
    let refused = config_call(
        &tools,
        &[
            "pack",
            "preview",
            "install",
            "fixture/documents_fixture",
            "--inference-slot",
            "worker=p-other",
        ],
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("selects another"), "{refused}");
    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        node.get_collection("FixtureJob").unwrap().is_none(),
        "refused preview must not publish the pack's schemas"
    );

    // Commit refuses the same binding even with the pinned digest.
    let refused = config_call(
        &tools,
        &[
            "pack",
            "install",
            "fixture/documents_fixture",
            "--digest",
            &digest,
            "--inference-slot",
            "worker=p-other",
        ],
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("selects another"), "{refused}");
    assert!(
        gents_pack::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        node.get_collection("FixtureJob").unwrap().is_none(),
        "refused commit must not publish the pack's schemas"
    );

    // The provider's default account and the account-free setup backend are
    // both admitted, and the pack installs under the pinned digest.
    let current_profile = format!("{}:inference", fixture.agent_did());
    for profile in ["p-default", current_profile.as_str()] {
        let preview: Value = serde_json::from_str(
            &config_call(
                &tools,
                &[
                    "pack",
                    "preview",
                    "install",
                    "fixture/documents_fixture",
                    "--inference-slot",
                    &format!("worker={profile}"),
                ],
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(preview["ready"], true, "{profile}: {preview}");
    }
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "install",
                "fixture/documents_fixture",
                "--digest",
                &digest,
                "--inference-slot",
                "worker=p-default",
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["effective"]["digest"], Value::String(digest));
    assert!(node.get_collection("FixtureJob").unwrap().is_some());
}
