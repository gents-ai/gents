//! Unit tests for the self-config tool family. End-to-end lifecycle coverage
//! (identity-scoped writes, reconcile pickup) lives in
//! `tests/e2e_runtime/self_config_tools.rs`.

use super::command::{behavior_params, help_patch_contracts};
use super::*;

fn config(categories: &[&str]) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        behavior_id: "beh-test".to_string(),
        categories: categories.iter().map(|c| c.to_string()).collect(),
        no_lockout: false,
        dry_run: false,
        enable_pack_install: false,
        enable_graph_tools: false,
        process_ceiling: Default::default(),
    }
}

#[test]
fn tool_names_follow_enabled_categories() {
    let names = self_config_tool_names(&config(&["behavior", "tools", "profile"]));
    assert_eq!(
        names,
        vec![CONFIG_TOOL_NAME.to_string()],
        "all granted categories share one model-facing config command"
    );

    let disabled = SelfConfigToolConfig::default();
    assert!(self_config_tool_names(&disabled).is_empty());
}

#[test]
fn pack_install_requires_its_separate_opt_in() {
    let without_install = config(&["behavior"]);
    assert_eq!(self_config_tool_names(&without_install), [CONFIG_TOOL_NAME]);

    let mut with_install = without_install;
    with_install.enable_pack_install = true;
    assert_eq!(self_config_tool_names(&with_install), [CONFIG_TOOL_NAME]);
}

#[test]
fn every_tool_name_is_reserved_builtin() {
    for name in SELF_CONFIG_TOOL_NAMES {
        assert!(
            crate::document_config::is_reserved_builtin_tool_name(name),
            "{name} must be reserved so write_tools declarations cannot shadow it"
        );
    }
}

#[test]
fn help_contracts_conform_to_canonical_types_and_enum_vocabulary() {
    let all_contracts = [
        "behavior",
        "tools",
        "profile",
        "backend",
        "mcp-service",
        "automation",
        "datastore",
        "skill",
    ]
    .into_iter()
    .flat_map(|resource| {
        help_patch_contracts(Some(resource))
            .as_array()
            .unwrap()
            .clone()
    })
    .collect::<Vec<_>>();
    for target in crate::config_client::patch::ALL_SELF_CONFIG_TARGETS {
        let contract = all_contracts
            .iter()
            .find(|contract| contract["collection"] == target.collection_name())
            .unwrap_or_else(|| panic!("missing help contract for {}", target.collection_name()));
        let described = contract["field_shapes"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let writable = target
            .writable_fields()
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            described,
            writable,
            "{} help drift",
            target.collection_name()
        );
    }

    let tools = all_contracts
        .iter()
        .find(|contract| contract["collection"] == "Tools")
        .unwrap();
    let shapes = &tools["field_shapes"];
    let assert_fields = |shape: &Value, canonical: &[&str], name: &str| {
        let described = shape
            .as_object()
            .unwrap_or_else(|| panic!("{name} help shape must be an object"))
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let canonical = canonical.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(described, canonical, "{name} nested help drift");
    };
    use crate::config_client::canonical_struct_fields;
    use crate::document_config::*;
    assert_fields(
        &shapes["host"],
        canonical_struct_fields::<HostTools>().unwrap(),
        "host",
    );
    assert_fields(
        &shapes["host"]["files"],
        canonical_struct_fields::<FileTools>().unwrap(),
        "files",
    );
    assert_fields(
        &shapes["host"]["bash"],
        canonical_struct_fields::<BashTools>().unwrap(),
        "bash",
    );
    assert_fields(
        &shapes["remote"],
        canonical_struct_fields::<RemoteTools>().unwrap(),
        "remote",
    );
    assert_fields(
        &shapes["subagents"],
        canonical_struct_fields::<SubagentTools>().unwrap(),
        "subagents",
    );
    assert_fields(
        &shapes["built_ins"],
        canonical_struct_fields::<BuiltInTools>().unwrap(),
        "built_ins",
    );
    assert_fields(
        &shapes["datastore"],
        canonical_struct_fields::<DatastoreTools>().unwrap(),
        "datastore",
    );
    assert_fields(
        &shapes["integrations"],
        canonical_struct_fields::<IntegrationTools>().unwrap(),
        "integrations",
    );
    assert_fields(
        &shapes["integrations"]["lsp"],
        canonical_struct_fields::<LspTools>().unwrap(),
        "lsp",
    );
    assert_fields(
        &shapes["self_config"],
        canonical_struct_fields::<SelfConfigTools>().unwrap(),
        "self_config",
    );

    let rendered = serde_json::to_string(&all_contracts).unwrap();
    let serialized_values = crate::backend_provider::BackendProviderKind::ALL
        .into_iter()
        .map(|value| value.as_str())
        .chain(
            crate::config::ReasoningEffort::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            crate::openai_wire::OpenAiWireApi::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            crate::toolset::CommandNetworkMode::ALL
                .into_iter()
                .map(|value| value.as_str()),
        );
    for value in serialized_values {
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
    for value in crate::tool_surface::FileToolMode::ALL
        .into_iter()
        .map(|value| serde_json::to_value(value).unwrap())
        .chain(
            crate::tool_surface::BashMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::toolset::CommandExecutionMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::document_config::RemoteToolStyle::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::document_config::ConcurrencyMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::compaction::CompactionStrategy::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
    {
        let value = value.as_str().unwrap();
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
}

#[tokio::test]
async fn build_fails_closed_without_agent_did() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::new(node),
        String::new(),
        None,
        &config(&["behavior"]),
    );
    assert!(
        tools.is_empty(),
        "an empty agent DID must register no self-config tools"
    );
}

#[tokio::test]
async fn build_registers_gated_family() {
    // Smoke assertion: the builder registers whatever the sorted name table
    // lists (owned by `tool_names_follow_enabled_categories`); this only
    // proves the real registration path is wired for a gated config.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::new(node),
        "did:key:zSelfConfigTest".to_string(),
        None,
        &config(&["behavior", "backend"]),
    );
    assert_eq!(
        tools.len(),
        1,
        "self-configuration has one model-facing tool"
    );
    assert_eq!(tools[0].name(), CONFIG_TOOL_NAME);
    let definition = tools[0].definition(String::new()).await;
    assert!(definition.description.contains("Behavior -> Context"));
    assert!(definition.description.contains("one Tools document"));
    assert!(definition
        .description
        .contains("Behavior -> InferenceProfile -> Backend"));
    assert_eq!(definition.parameters["required"], json!(["argv"]));
}

#[tokio::test]
async fn pack_install_uses_current_principal_and_inference_chain() {
    let node = build_persona_node().await;
    let identity = persona_identity("pack-install");
    let agent_did = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &agent_did, "setup").await;
    for role in ["coordinator", "worker", "verifier"] {
        crate::test_support::install_test_behavior(&node, &agent_did, role).await;
    }

    let mut tool_config = config(&[]);
    tool_config.behavior_id = "setup".to_string();
    tool_config.enable_pack_install = true;
    tool_config.enable_graph_tools = true;
    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity),
        &tool_config,
    );
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    for name in [
        LIST_GRAPHS_TOOL_NAME,
        RUN_GRAPH_TOOL_NAME,
        GET_GRAPH_RUN_TOOL_NAME,
        GET_GRAPH_RESULT_TOOL_NAME,
        CANCEL_GRAPH_RUN_TOOL_NAME,
    ] {
        assert!(
            tools.iter().any(|tool| tool.name() == name),
            "{name} must share the explicit graph authority gate"
        );
    }
    let discovery: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "preview", "install", "code_review"]}).to_string())
            .await
            .expect("incomplete preview remains readable"),
    )
    .unwrap();
    assert_eq!(discovery["committed"], false);
    assert_eq!(discovery["ready"], false);
    assert_eq!(
        discovery["missing_inference_slots"],
        json!(["coordinator", "worker", "verifier"])
    );
    assert_eq!(
        discovery["inference"]["profiles"].as_array().unwrap().len(),
        4
    );
    let before = node
        .execute("{ GraphDefinition {graph_id} GraphRevision {digest} }")
        .await;
    assert!(!before.has_errors(), "{:?}", before.errors);
    assert!(before.data.as_ref().unwrap()["GraphDefinition"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(before.data.as_ref().unwrap()["GraphRevision"]
        .as_array()
        .unwrap()
        .is_empty());

    let slot_argv = [
        "--inference-slot",
        "coordinator=coordinator:inference",
        "--inference-slot",
        "worker=worker:inference",
        "--inference-slot",
        "verifier=verifier:inference",
    ];
    let mut preview_argv = vec!["pack", "preview", "install", "code_review"];
    preview_argv.extend(slot_argv);
    let preview: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": preview_argv}).to_string())
            .await
            .expect("complete preview succeeds"),
    )
    .unwrap();
    assert_eq!(preview["ready"], true);
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut install_argv = vec!["pack", "install", "code_review", "--digest", digest];
    install_argv.extend(slot_argv);
    let output = tool
        .call(json!({"argv": install_argv}).to_string())
        .await
        .expect("bundled pack installs");
    let output: serde_json::Value = serde_json::from_str(&output).expect("JSON receipt");
    assert_eq!(output["install"]["package_name"], "code_review");
    assert_eq!(output["artifact_digest"], preview["artifact_digest"]);
    assert_eq!(
        output["inference"]["bindings"],
        json!({
            "coordinator": "coordinator:inference",
            "worker": "worker:inference",
            "verifier": "verifier:inference",
        })
    );
    assert_eq!(output["activation"]["graph_id"], "code-review");
    assert_eq!(
        output["activation"]["active_digest"],
        output["install"]["revision_digest"]
    );

    let response = node
        .execute(&format!(
            "{{ GraphDefinition(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{graph_id agent_did active_revision_digest}} }}",
            crate::graphql::escape_graphql_string(&agent_did)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let definitions = response.data.unwrap()["GraphDefinition"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["agent_did"], agent_did);
    assert_eq!(definitions[0]["graph_id"], "code-review");
    assert_eq!(
        definitions[0]["active_revision_digest"],
        output["install"]["revision_digest"]
    );
    let inspected: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "get", "code_review"]}).to_string())
            .await
            .expect("installed pack is inspectable"),
    )
    .unwrap();
    assert_eq!(
        inspected["installed"]["digest"],
        output["install"]["revision_digest"]
    );

    let bad_digest = format!("sha256:{}", "0".repeat(64));
    let rejected = tool
        .call(
            json!({"argv": [
                "pack", "update", "code_review", "--digest", bad_digest,
                "--inference-slot", "coordinator=coordinator:inference",
                "--inference-slot", "worker=worker:inference",
                "--inference-slot", "verifier=verifier:inference"
            ]})
            .to_string(),
        )
        .await
        .expect_err("a digest not returned by preview is rejected");
    assert!(rejected.to_string().contains("preview again"));

    let owner = crate::graphql::escape_graphql_string(&agent_did);
    let tagged = node
        .execute(&format!(
            r#"mutation {{ update_AgentBehavior(filter: {{agent_did: {{_eq: "{owner}"}}, behavior_id: {{_eq: "review-recon"}}}}, input: {{tags: ["gents:pack:code_review", "user:favorite"]}}) {{_docID}} }}"#
        ))
        .await;
    assert!(!tagged.has_errors(), "{:?}", tagged.errors);
    let update_preview: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": [
                    "pack", "preview", "update", "code_review",
                    "--inference-slot", "coordinator=coordinator:inference",
                    "--inference-slot", "worker=worker:inference",
                    "--inference-slot", "verifier=verifier:inference"
                ]})
                .to_string(),
            )
            .await
            .expect("installed pack update previews"),
    )
    .unwrap();
    assert_eq!(update_preview["ready"], true);
    let update_digest = update_preview["artifact_digest"].as_str().unwrap();
    let updated: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": [
                    "pack", "update", "code_review", "--digest", update_digest,
                    "--inference-slot", "coordinator=coordinator:inference",
                    "--inference-slot", "worker=worker:inference",
                    "--inference-slot", "verifier=verifier:inference"
                ]})
                .to_string(),
            )
            .await
            .expect("same distribution update is idempotent"),
    )
    .unwrap();
    assert_eq!(
        updated["install"]["revision_digest"],
        output["install"]["revision_digest"]
    );
    let tags = node
        .execute(&format!(
            r#"{{ AgentBehavior(filter: {{agent_did: {{_eq: "{owner}"}}, behavior_id: {{_eq: "review-recon"}}}}) {{tags}} }}"#
        ))
        .await;
    assert!(!tags.has_errors(), "{:?}", tags.errors);
    assert_eq!(
        tags.data.unwrap()["AgentBehavior"][0]["tags"],
        json!(["gents:pack:code_review", "user:favorite"])
    );
    let list = tools
        .iter()
        .find(|tool| tool.name() == LIST_GRAPHS_TOOL_NAME)
        .unwrap()
        .call("{}".to_owned())
        .await
        .expect("installed graph is discoverable on the same node");
    let list: Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list["node_bound"], true);
    assert_eq!(list["agent_did"], agent_did);
    assert_eq!(list["graphs"][0]["definition"]["graph_id"], "code-review");
    assert_eq!(
        list["graphs"][0]["active_plan"]["digest"],
        output["install"]["revision_digest"]
    );
    let run_error = tools
        .iter()
        .find(|tool| tool.name() == RUN_GRAPH_TOOL_NAME)
        .unwrap()
        .call(json!({"package": "code_review"}).to_string())
        .await
        .expect_err("code review cannot bypass this behavior's disabled file authority");
    assert!(
        run_error
            .to_string()
            .contains("requires effective read authority"),
        "{run_error:#}"
    );
    let runs = node
        .execute(&format!(
            r#"{{ GraphRun(filter: {{owner_did: {{_eq: "{}"}}}}) {{run_id}} }}"#,
            crate::graphql::escape_graphql_string(&agent_did)
        ))
        .await;
    assert!(!runs.has_errors(), "{:?}", runs.errors);
    assert!(runs.data.unwrap()["GraphRun"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn graph_tools_start_observe_and_cancel_on_the_current_node() {
    let node = build_persona_node().await;
    let identity = persona_identity("graph-tools");
    let agent_did = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &agent_did, "setup").await;
    let repository = tempfile::tempdir().expect("repository");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "user.name", "Graph Tool Test"]);
    std::fs::write(repository.path().join("review.txt"), "before\n").unwrap();
    git(&["add", "review.txt"]);
    git(&["commit", "--quiet", "-m", "base"]);
    std::fs::write(repository.path().join("review.txt"), "after\n").unwrap();
    git(&["add", "review.txt"]);
    git(&["commit", "--quiet", "-m", "head"]);

    let mut tool_config = config(&["tools"]);
    tool_config.behavior_id = "setup".to_owned();
    tool_config.enable_pack_install = true;
    tool_config.enable_graph_tools = true;
    tool_config.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(repository.path().to_owned()),
    };
    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity.clone()),
        &tool_config,
    );
    let call = |name: &str, args: Value| {
        let tool = tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing tool {name}"));
        tool.call(args.to_string())
    };

    call(
        CONFIG_TOOL_NAME,
        json!({"argv": [
            "tools", "edit", "--set",
            format!("host={}", json!({
                "root": repository.path().to_string_lossy(),
                "files": {"mode": "ReadOnly"}
            }))
        ]}),
    )
    .await
    .expect("current behavior receives explicit read authority");
    let preview = call(
        CONFIG_TOOL_NAME,
        json!({"argv": [
            "pack", "preview", "install", "code_review",
            "--inference-slot", "coordinator=setup:inference",
            "--inference-slot", "worker=setup:inference",
            "--inference-slot", "verifier=setup:inference"
        ]}),
    )
    .await
    .expect("code-review pack previews");
    let preview: Value = serde_json::from_str(&preview).unwrap();
    let digest = preview["artifact_digest"].as_str().unwrap();
    call(
        CONFIG_TOOL_NAME,
        json!({"argv": [
            "pack", "install", "code_review", "--digest", digest,
            "--inference-slot", "coordinator=setup:inference",
            "--inference-slot", "worker=setup:inference",
            "--inference-slot", "verifier=setup:inference"
        ]}),
    )
    .await
    .expect("code-review pack installs");
    // Running an admitted pack needs neither installation nor self-config.
    tool_config.enabled = false;
    tool_config.enable_pack_install = false;
    let tools = build_self_config_tools(node, agent_did.clone(), Some(identity), &tool_config);
    assert!(!tools.iter().any(|t| t.name() == CONFIG_TOOL_NAME));
    let call = |name: &str, args: Value| {
        tools
            .iter()
            .find(|t| t.name() == name)
            .expect("native graph tool")
            .call(args.to_string())
    };
    let started = call(
        RUN_GRAPH_TOOL_NAME,
        json!({
            "package": "code_review",
            "repository": repository.path().to_string_lossy(),
            "base": "HEAD^",
            "head": "HEAD",
            "focus": "Review the changed text.",
        }),
    )
    .await
    .expect("node-bound graph starts");
    let started: Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["node_bound"], true);
    assert_eq!(started["principal"], agent_did);
    assert_eq!(started["observed"]["status"], "running");
    let run_id = started["receipt"]["run_id"].as_str().unwrap();

    let observed = call(GET_GRAPH_RUN_TOOL_NAME, json!({"run_id": run_id}))
        .await
        .expect("same-node status is readable");
    let observed: Value = serde_json::from_str(&observed).unwrap();
    assert_eq!(observed["run_id"], run_id);
    assert_eq!(observed["owner_did"], agent_did);
    assert_eq!(observed["status"], "running");

    let not_yet_a_result = call(GET_GRAPH_RESULT_TOOL_NAME, json!({"run_id": run_id}))
        .await
        .expect("nonterminal result view remains inspectable");
    let not_yet_a_result: Value = serde_json::from_str(&not_yet_a_result).unwrap();
    assert_eq!(not_yet_a_result["status"], "running");
    assert_eq!(not_yet_a_result["result_contract_satisfied"], false);

    let cancelled = call(
        CANCEL_GRAPH_RUN_TOOL_NAME,
        json!({"run_id": run_id, "reason": "test cleanup"}),
    )
    .await
    .expect("same-node cancellation is persisted");
    let cancelled: Value = serde_json::from_str(&cancelled).unwrap();
    assert_eq!(cancelled["run_id"], run_id);
    assert_eq!(cancelled["cancellation_requested_by"], agent_did);
    assert_eq!(cancelled["cancellation_reason"], "test cleanup");
    assert_ne!(cancelled["status"], "succeeded");
}

#[tokio::test]
async fn config_tools_cannot_self_grant_pack_install() {
    let node = build_persona_node().await;
    let identity = persona_identity("pack-self-grant");
    let agent_did = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &agent_did, "setup").await;

    let mut tool_config = config(&["tools"]);
    tool_config.behavior_id = "setup".to_string();
    let tools = build_self_config_tools(node, agent_did, Some(identity), &tool_config);
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

// -- behavior commands (#Task 5) --

#[test]
fn behavior_edit_arguments_distinguish_omission_clear_and_set() {
    let args: ConfigurePersonaParams = serde_json::from_value(json!({
        "action": "edit",
        "behavior_id": "review",
        "display_name": "Review reconnaissance",
        "root": null
    }))
    .expect("valid sparse edit arguments");
    assert_eq!(
        persona_edit_fields(&args),
        vec!["display_name".to_string(), "root".to_string()]
    );
    assert_eq!(
        args.display_name,
        StringUpdate::Set("Review reconnaissance".to_string())
    );
    assert_eq!(args.root, StringUpdate::Clear);
    assert_eq!(args.profile_id, StringUpdate::Omitted);
    assert_eq!(args.system_prompt, StringUpdate::Omitted);
}

async fn build_persona_node() -> std::sync::Arc<defra_node::EmbeddedNode> {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    crate::ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas register");
    std::sync::Arc::new(node)
}

fn persona_identity(label: &str) -> std::sync::Arc<dyn crate::AgentIdentity> {
    let tempdir = tempfile::tempdir().expect("identity tempdir");
    std::sync::Arc::new(
        crate::KeyIdentity::load_or_create(&tempdir.path().join(format!("{label}.key")), None)
            .expect("test identity"),
    )
}

#[tokio::test]
async fn automation_rejects_invalid_template_before_publication_and_can_recover() {
    let node = build_persona_node().await;
    let identity = persona_identity("template-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let mut grants = config(&["automation"]);
    grants.dry_run = true;
    let tools = build_self_config_tools(node.clone(), owner, Some(identity), &grants);
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
        let rows = node.execute("{ Task { task_id } }").await;
        assert!(!rows.has_errors());
        assert!(rows.data.unwrap()["Task"].as_array().unwrap().is_empty());
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
    let rows = node.execute("{ Task { task_id prompt_template } }").await;
    assert!(!rows.has_errors());
    let data = rows.data.unwrap();
    assert_eq!(data["Task"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["Task"][0]["prompt_template"],
        "Process {{ doc.message }}"
    );
}

#[tokio::test]
async fn schema_publication_matches_lean_grant_artifact_and_contract_guards() {
    use sha2::{Digest, Sha256};
    // Exhaust the Bool inputs of SelfConfig.schemaPublicationAllowed in
    // proofs/Proofs/SelfConfig/Auth.lean against the real publication path.
    let sdl = "type ConfiguratorTruthTable { message: String }";
    for granted in [false, true] {
        for artifact_matches in [false, true] {
            for compatible in [false, true] {
                let node = build_persona_node().await;
                let identity = persona_identity("schema-truth-table");
                let owner = identity.did().to_string();
                crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
                let access = crate::config_client::ConfigAccess::Local(node.clone());
                if !compatible {
                    access
                        .add_schema("type ConfiguratorTruthTable { message: Int }")
                        .await
                        .unwrap();
                }
                let before = access
                    .collection_version("ConfiguratorTruthTable")
                    .await
                    .unwrap();
                let grants = config(if granted { &["automation"] } else { &["tools"] });
                let tools = build_self_config_tools(node, owner, Some(identity), &grants);
                let digest = if artifact_matches {
                    format!("sha256:{:x}", Sha256::digest(sdl))
                } else {
                    "wrong".into()
                };
                let result = call_config_tool(
                    &tools,
                    vec![
                        "schema".into(),
                        "install".into(),
                        "--sdl".into(),
                        sdl.into(),
                        "--digest".into(),
                        digest,
                    ],
                )
                .await;
                let accepted = granted && artifact_matches && compatible;
                assert_eq!(result.is_ok(), accepted, "grant={granted}, artifact={artifact_matches}, compatible={compatible}: {result:?}");
                let after = access
                    .collection_version("ConfiguratorTruthTable")
                    .await
                    .unwrap();
                if accepted {
                    assert!(after.is_some());
                } else {
                    assert_eq!(after, before, "rejected publication changed schema");
                }
            }
        }
    }
}

#[tokio::test]
async fn schema_publication_requires_automation_and_previewed_artifact() {
    let node = build_persona_node().await;
    let identity = persona_identity("schema-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let sdl = "type ConfiguratorWorkItem { message: String }";
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let denied = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity.clone()),
        &config(&["tools"]),
    );
    assert!(call_config_tool(
        &denied,
        command(&["schema", "preview", "install", "--sdl", sdl])
    )
    .await
    .is_err());
    let mut grants = config(&["automation"]);
    grants.dry_run = true;
    let tools = build_self_config_tools(node.clone(), owner, Some(identity), &grants);
    let preview = call_config_tool(
        &tools,
        command(&["schema", "preview", "install", "--sdl", sdl]),
    )
    .await
    .unwrap();
    let preview: Value = serde_json::from_str(&preview).unwrap();
    assert_eq!(preview["committed"], false);
    assert!(node
        .get_collection("ConfiguratorWorkItem")
        .unwrap()
        .is_none());
    assert!(call_config_tool(
        &tools,
        command(&["schema", "install", "--sdl", sdl, "--digest", "wrong"])
    )
    .await
    .is_err());
    assert!(node
        .get_collection("ConfiguratorWorkItem")
        .unwrap()
        .is_none());
    let digest = preview["plan"]["artifact_digest"].as_str().unwrap();
    assert!(call_config_tool(
        &denied,
        command(&["schema", "install", "--sdl", sdl, "--digest", digest])
    )
    .await
    .is_err());
    call_config_tool(
        &tools,
        command(&["schema", "install", "--sdl", sdl, "--digest", digest]),
    )
    .await
    .unwrap();
    assert!(node
        .get_collection("ConfiguratorWorkItem")
        .unwrap()
        .is_some());
    let schema = call_config_tool(&tools, command(&["schema", "get", "ConfiguratorWorkItem"]))
        .await
        .unwrap();
    assert!(schema.contains("message"));
    assert!(call_config_tool(
        &tools,
        command(&[
            "schema",
            "preview",
            "install",
            "--sdl",
            "type ConfiguratorWorkItem { message: Int }"
        ])
    )
    .await
    .is_err());
}

#[tokio::test]
async fn skill_import_previews_without_writes_and_requires_file_authority() {
    let node = build_persona_node().await;
    let identity = persona_identity("skill-import");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("SKILL.md");
    std::fs::write(
        &file,
        "---\nname: Review\ndescription: Review code\n---\nCheck the diff carefully.",
    )
    .unwrap();
    let mut grants = config(&["tools", "behavior"]);
    grants.dry_run = true;
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let denied_tools =
        build_self_config_tools(node.clone(), owner.clone(), Some(identity.clone()), &grants);
    let denied = call_config_tool(
        &denied_tools,
        command(&["skill", "import", "review", file.to_str().unwrap()]),
    )
    .await
    .unwrap_err();
    assert!(denied.contains("file read permission"), "{denied}");
    grants.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "beh-test".into()).unwrap();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(), "files": {"mode": "ReadOnly"}
            })),
        )],
        false,
    ))
    .await
    .unwrap();
    let tools = build_self_config_tools(node.clone(), owner.clone(), Some(identity), &grants);
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
            "behavior",
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
    let node = build_persona_node().await;
    let identity = persona_identity("configuration-discovery");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("synthetic-codex");
    std::fs::create_dir_all(&source).unwrap();
    let marker = root.path().join("SHOULD_NEVER_RUN");
    let fixture =
        include_str!("../../tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &marker.to_string_lossy());
    std::fs::write(source.join("config.toml"), fixture).unwrap();

    let mut grants = config(&["tools"]);
    let denied_tools =
        build_self_config_tools(node.clone(), owner.clone(), Some(identity.clone()), &grants);
    let command = |args: &[&str]| args.iter().map(|value| (*value).to_owned()).collect();
    let help = call_config_tool(&denied_tools, command(&["help", "discovery"]))
        .await
        .unwrap();
    assert!(help.contains("discovery commands"), "{help}");
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

    grants.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "beh-test".into()).unwrap();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(), "files": {"mode": "ReadOnly"}
            })),
        )],
        false,
    ))
    .await
    .unwrap();
    let before = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    let tools = build_self_config_tools(node, owner, Some(identity), &grants);
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
    let inventory: crate::configuration_discovery::ConfigurationDiscoveryInventory =
        serde_json::from_str(&output).unwrap();
    assert_eq!(inventory.schema_version, 1);
    assert!(inventory.items.iter().any(|item| {
        item.category == crate::configuration_discovery::DiscoveryCategory::RemoteTool
            && item.display_label == "shared"
    }));
}

#[tokio::test]
async fn setup_discovery_clarification_apply_and_verification_preserve_disabled_settings() {
    use crate::configuration_discovery::{
        ConfigurationDiscoveryInventory, DiscoveryCategory, DiscoveryItemState,
        DiscoverySourceOutcome, MappingSupportLevel,
    };

    let node = build_persona_node().await;
    let identity = persona_identity("setup-discovery-flow");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;

    let root = tempfile::tempdir().unwrap();
    let user_root = root.path().join("synthetic-user/.codex");
    let project_root = root.path().join("synthetic-project");
    std::fs::create_dir_all(&user_root).unwrap();
    std::fs::create_dir_all(project_root.join(".codex")).unwrap();
    let marker = root.path().join("SHOULD_NEVER_RUN");
    std::fs::write(
        user_root.join("config.toml"),
        include_str!("../../tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &marker.to_string_lossy()),
    )
    .unwrap();
    std::fs::write(
        user_root.join("AGENTS.md"),
        include_str!("../../tests/fixtures/configuration_discovery/codex-user/AGENTS.md"),
    )
    .unwrap();
    std::fs::write(
        project_root.join(".codex/config.toml"),
        include_str!("../../tests/fixtures/configuration_discovery/project/.codex/config.toml"),
    )
    .unwrap();
    std::fs::write(
        project_root.join("AGENTS.md"),
        include_str!("../../tests/fixtures/configuration_discovery/project/AGENTS.md"),
    )
    .unwrap();

    let mut grants = config(&["tools", "behavior", "backend"]);
    grants.dry_run = true;
    grants.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "beh-test".into()).unwrap();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(),
                "files": {"mode": "ReadOnly"}
            })),
        )],
        false,
    ))
    .await
    .unwrap();
    let tools = build_self_config_tools(node.clone(), owner.clone(), Some(identity), &grants);
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

    // The user-approved source tuples produce the real, sanitized production shape.
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

    // Clarification selects only the project instruction. Conflicting model hints and the
    // disabled remote tool remain unresolved and therefore absent from the proposal.
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
        command(&["behavior", "context", "get", "--behavior", "beh-test"]),
    )
    .await
    .unwrap();
    let backend_before = call_config_tool(
        &tools,
        command(&["backend", "get", "--behavior", "beh-test"]),
    )
    .await
    .unwrap();
    assert!(!backend_before.contains("FAKE_DISCOVERY_SECRET_123"));
    let services_before = crate::registry::configured_mcp_services(&node, &owner)
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
                "behavior".into(),
                "context".into(),
                "preview".into(),
                "--behavior".into(),
                "beh-test".into(),
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
            command(&["behavior", "context", "get", "--behavior", "beh-test"]),
        )
        .await
        .unwrap(),
        context_before,
        "preview must not apply the proposal"
    );

    // This edit represents the separately approved minimal preview.
    call_config_tool(
        &tools,
        vec![
            "behavior".into(),
            "context".into(),
            "edit".into(),
            "--behavior".into(),
            "beh-test".into(),
            "--set".into(),
            prompt_patch,
        ],
    )
    .await
    .unwrap();

    let verified: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            command(&["behavior", "context", "get", "--behavior", "beh-test"]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["document"]["system_prompt"], approved_prompt);
    assert_eq!(
        call_config_tool(
            &tools,
            command(&["backend", "get", "--behavior", "beh-test"]),
        )
        .await
        .unwrap(),
        backend_before,
        "discovery-backed setup must not modify operator-owned inference credentials"
    );
    assert_eq!(
        crate::registry::configured_mcp_services(&node, &owner)
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
    let node = build_persona_node().await;
    let identity = persona_identity("datastore-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let mut grants = config(&["tools"]);
    grants.dry_run = true;
    let tools = build_self_config_tools(node.clone(), owner.clone(), Some(identity), &grants);
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let expected_help = call_config_tool(&tools, command(&["help", "datastore"]))
        .await
        .unwrap();
    for args in [
        vec!["datastore", "--help"],
        vec!["datastore", "preview", "create", "-h"],
        vec!["datastore", "help", "create"],
    ] {
        assert_eq!(
            call_config_tool(&tools, command(&args)).await.unwrap(),
            expected_help
        );
    }
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
        command(&[
            "datastore",
            "edit",
            "jobs",
            "--set",
            "agent_did=\"foreign\"",
        ])
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
    node.add_schema("type ConfiguredWork { message: String }")
        .await
        .unwrap();
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
    let core = SelfConfigCore::new(node.clone(), owner, "beh-test".into()).unwrap();
    core.apply(tools_request(
        &core,
        vec![(
            "datastore".into(),
            Some(json!({
                "datastore_tool_surface_ids": ["jobs"]
            })),
        )],
        false,
    ))
    .await
    .unwrap();
    core.apply(behavior_request(
        &core,
        vec![(
            "tags".into(),
            Some(json!([
                crate::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG
            ])),
        )],
    ))
    .await
    .unwrap();
    let denied = call_config_tool(
        &tools,
        command(&["datastore", "edit", "jobs", "--set", "enabled=false"]),
    )
    .await
    .unwrap_err();
    assert!(denied.contains("protected Setup"), "{denied}");
    let denied = call_config_tool(
        &tools,
        command(&[
            "datastore",
            "preview",
            "edit",
            "jobs",
            "--set",
            "enabled=false",
        ]),
    )
    .await
    .unwrap_err();
    assert!(denied.contains("protected Setup"), "{denied}");
}

#[tokio::test]
async fn structured_config_preview_and_apply_round_trip_literal_prompt() {
    let node = build_persona_node().await;
    let identity = persona_identity("structured-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "working").await;
    let mut settings = config(&["behavior"]);
    settings.behavior_id = "working".into();
    settings.dry_run = true;
    let tools = build_self_config_tools(node.clone(), owner, Some(identity), &settings);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let read = json!({"argv":["behavior","context","get"]}).to_string();
    let before = tool.call(read.clone()).await.unwrap();
    let prompt = "Quoted \"text\"\nActual newline; literal \\n; Unicode λ; {{ doc.message }}";
    let mut request =
        json!({"argv":["behavior","context","preview"],"set":{"system_prompt":prompt}});
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
    request["set"] = json!({"agent_did":"foreign"});
    assert!(tool.call(request.to_string()).await.is_err());
    assert_eq!(
        serde_json::from_str::<Value>(&tool.call(read).await.unwrap()).unwrap(),
        after
    );
    node.shutdown().await;
}

async fn call_config_tool(
    tools: &[Box<dyn crate::llm::tool::ToolDyn>],
    argv: Vec<String>,
) -> Result<String, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    tool.call(json!({"argv": argv}).to_string())
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tokio::test]
async fn connected_plan_preview_validates_pending_references_without_writes() {
    let node = build_persona_node().await;
    let identity = persona_identity("connected-preview");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let access = crate::ConfigAccess::Local(node.clone());
    let query = "{ AgentBehavior { behavior_id context_id } AgentContext { context_id tools_id } Tools { tools_id } }";
    let before = access.execute(query).await.unwrap();
    let documents = json!([
        {"collection":"AgentBehavior","document":{"agent_did":owner,"behavior_id":"proposed-behavior","context_id":"proposed-context","inference_profile_id":"beh-test:inference"}},
        {"collection":"AgentContext","document":{"agent_did":owner,"context_id":"proposed-context","tools_id":"proposed-tools","system_prompt":"Observe the host."}},
        {"collection":"Tools","document":{"agent_did":owner,"tools_id":"proposed-tools"}}
    ]);
    let mut grants = config(&["persona", "tools"]);
    grants.dry_run = true;
    let tools =
        build_self_config_tools(node.clone(), owner.clone(), Some(identity.clone()), &grants);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let args = |documents: Value| {
        json!({"argv":["plan","preview"],"options":{"documents":documents}}).to_string()
    };
    for resource in ["behavior", "tools", "datastore", "automation", "schema"] {
        let help: Value = serde_json::from_str(
            &tool
                .call(json!({"argv":[resource,"--help"]}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            help["connected_preview"]["preview_argv"],
            json!(["plan", "preview"])
        );
        assert_eq!(
            help["connected_preview"]["input_field"],
            "options.documents"
        );
        let plan_help: Value = serde_json::from_str(
            &tool
                .call(json!({"argv":help["connected_preview"]["help_argv"]}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(plan_help["ok"], true);
    }
    let error = tool.call(json!({"argv":["tools","preview"],"options":{"behavior":"proposed-behavior"},"set":{"host":{"bash":{"mode":"read_only"}}}}).to_string()).await.unwrap_err();
    let crate::llm::tool::ToolError::ToolCallError(error) = error else {
        panic!("missing typed config error: {error}");
    };
    let error: Value = serde_json::from_str(&error.to_string()).unwrap();
    assert_eq!(error["config_execution"]["mutation_entered"], false);
    assert_eq!(
        error["recovery"]["preview_argv"],
        json!(["plan", "preview"])
    );
    let response: Value =
        serde_json::from_str(&tool.call(args(documents.clone())).await.unwrap()).unwrap();
    assert_eq!(response["committed"], false);
    assert_eq!(response["config_execution"]["mutation_entered"], false);
    assert_eq!(response["documents"].as_array().unwrap().len(), 3);
    let mut with_mailbox = documents.clone();
    with_mailbox.as_array_mut().unwrap().push(json!({
        "collection":"DatastoreToolSurface",
        "document":{"agent_did":owner,"surface_id":"proposed-attention","enabled":true},
        "mailbox":{"identity":{"mode":"condition","key":"host-health"},"kind":"flag","action":"ack"}
    }));
    let response: Value =
        serde_json::from_str(&tool.call(args(with_mailbox.clone())).await.unwrap()).unwrap();
    let surface = response["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["collection"] == "DatastoreToolSurface")
        .unwrap();
    let mut canonical = crate::mailbox::canonical_mailbox_write_decl();
    canonical.notification =
        Some(serde_json::from_value(with_mailbox[3]["mailbox"].clone()).unwrap());
    let surface: crate::document_config::DatastoreToolSurfaceDocument =
        serde_json::from_value(surface["document"].clone()).unwrap();
    assert_eq!(
        surface.entries,
        Some(vec![crate::document_config::SurfaceToolDecl::Create(
            canonical
        )])
    );
    with_mailbox[3]["document"]["entries"] = json!([]);
    assert!(tool.call(args(with_mailbox)).await.is_err());
    let mut missing = documents.clone();
    missing.as_array_mut().unwrap().pop();
    assert!(tool.call(args(missing)).await.is_err());
    let mut foreign = documents.clone();
    foreign[2]["document"]["agent_did"] = "another-principal".into();
    assert!(tool.call(args(foreign)).await.is_err());
    let mut existing = documents.clone();
    existing[0]["document"]["behavior_id"] = "beh-test".into();
    assert!(tool.call(args(existing)).await.is_err());
    let mut duplicate = documents.clone();
    duplicate.as_array_mut().unwrap().push(documents[0].clone());
    assert!(tool.call(args(duplicate)).await.is_err());
    for (categories, dry_run) in [
        (&["persona"][..], true),
        (&["tools"][..], true),
        (&["persona", "tools"][..], false),
    ] {
        let mut denied = config(categories);
        denied.dry_run = dry_run;
        let denied =
            build_self_config_tools(node.clone(), owner.clone(), Some(identity.clone()), &denied);
        assert!(denied
            .iter()
            .find(|tool| tool.name() == CONFIG_TOOL_NAME)
            .unwrap()
            .call(args(documents.clone()))
            .await
            .is_err());
        let tool = denied
            .iter()
            .find(|tool| tool.name() == CONFIG_TOOL_NAME)
            .unwrap();
        let help: Value = serde_json::from_str(
            &tool
                .call(json!({"argv":["--help"]}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            help["connected_preview"].is_object(),
            dry_run && categories.contains(&"persona")
        );
    }
    assert_eq!(
        before,
        access.execute(query).await.unwrap(),
        "preview changed canonical documents"
    );
}

#[tokio::test]
async fn config_execution_receipts_separate_rejected_syntax_from_write_dispatch() {
    let node = build_persona_node().await;
    let identity = persona_identity("config-execution");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let mut grants = config(&[
        "persona",
        "tools",
        "automation",
        "profile",
        "backend",
        "mcp_service",
    ]);
    grants.dry_run = true;
    let tools = build_self_config_tools(node.clone(), owner, Some(identity), &grants);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    for (args, mutation) in [
        (
            json!({"argv":["behavior","create","preview"],"set":{"system_prompt":"literal"}}),
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
            true,
        ),
        (json!({"argv":["schema","install"]}), false),
        (
            json!({"argv":["automation","edit","task"],"target_id":"missing","set":{"display_name":"Missing behavior"},"options":{"behavior":"missing"}}),
            true,
        ),
    ] {
        let text = match tool.call(args.to_string()).await {
            Ok(text) => text,
            Err(crate::llm::tool::ToolError::ToolCallError(error)) => error.to_string(),
            Err(error) => panic!("missing typed result for {args}: {error}"),
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["config_execution"]["mutation_entered"], mutation,
            "{args}: {value}"
        );
    }
    // A write in one invocation cannot contaminate a later read's receipt.
    let read: Value = serde_json::from_str(
        &tool
            .call(json!({"argv":["datastore","--help"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read["config_execution"]["mutation_entered"], false);
    node.shutdown().await;
}

#[tokio::test]
async fn persona_category_gates_the_tool() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-gate");
    let agent_did = identity.did().to_string();

    let without_persona = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        None,
        &config(&["behavior"]),
    );
    let error = call_config_tool(&without_persona, vec!["behavior".into(), "list".into()])
        .await
        .expect_err("catalog read requires persona grant");
    assert!(error.contains("catalog grant"), "{error}");

    let with_persona =
        build_self_config_tools(node, agent_did, Some(identity), &config(&["persona"]));
    assert!(with_persona
        .iter()
        .any(|tool| tool.name() == CONFIG_TOOL_NAME));
}

#[tokio::test]
async fn persona_unknown_action_errors_cleanly() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-unknown");
    let tools = build_self_config_tools(
        node,
        identity.did().to_string(),
        Some(identity),
        &config(&["persona"]),
    );

    let error = call_config_tool(&tools, vec!["behavior".into(), "delete".into()])
        .await
        .expect_err("unknown action must error");
    assert!(
        error.contains("unknown behavior command"),
        "error should name the bad action: {error}"
    );
}

#[tokio::test]
async fn behavior_only_grant_cannot_change_default_and_writes_require_exact_signer() {
    let node = build_persona_node().await;
    let identity = persona_identity("behavior-only-owner");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "current").await;
    let mut tool_config = config(&["behavior"]);
    tool_config.behavior_id = "current".into();
    tool_config.dry_run = true;
    let tools = build_self_config_tools(node.clone(), owner.clone(), Some(identity), &tool_config);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let rejected = tool
        .call(
            json!({"argv":[
                "behavior", "preview", "edit", "--id", "current", "--default"
            ]})
            .to_string(),
        )
        .await
        .expect_err("current-only grant cannot change principal selection");
    assert!(rejected.to_string().contains("behavior catalog grant"));

    let foreign_identity = persona_identity("foreign-signer");
    let params = behavior_params(
        "edit",
        None,
        &[
            "--id".into(),
            "current".into(),
            "--display-name".into(),
            "Renamed".into(),
        ],
    )
    .unwrap();
    let rejected = persona_mutate(
        &node,
        &owner,
        foreign_identity.as_ref(),
        &params,
        &Default::default(),
    )
    .await
    .expect_err("foreign signer must fail before authoring a request");
    assert!(rejected
        .to_string()
        .contains("exact local principal signer"));
}

#[tokio::test]
async fn config_lists_are_bounded_paginated_and_inference_inventory_is_read_only() {
    let node = build_persona_node().await;
    let identity = persona_identity("config-inventory");
    let owner = identity.did().to_string();
    for behavior in ["alpha", "beta", "gamma"] {
        crate::test_support::install_test_behavior(&node, &owner, behavior).await;
    }
    let mut tool_config = config(&["persona", "profile", "backend"]);
    tool_config.behavior_id = "alpha".into();
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();

    let first: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["behavior", "list", "--limit", "1"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["page"]["returned"], 1);
    assert_eq!(first["page"]["truncated"], true);
    let cursor = first["page"]["next_cursor"].as_str().unwrap();
    let second: Value = serde_json::from_str(
        &config
            .call(
                json!({"argv":["behavior", "list", "--limit", "1", "--cursor", cursor]})
                    .to_string(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_ne!(first["behaviors"][0], second["behaviors"][0]);

    let behavior_id = first["behaviors"][0]["behavior_id"].as_str().unwrap();
    let inspected: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["behavior", "get", behavior_id]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inspected["behavior_id"], behavior_id);

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

    let node = build_persona_node().await;
    let identity = persona_identity("local-backend-create");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "setup").await;
    let mut tool_config = config(&["backend", "profile"]);
    tool_config.behavior_id = "setup".into();
    tool_config.dry_run = true;
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config);
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
        discovered["observation"]["catalogs"][0]["models"][0]["model_name"],
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
async fn config_targets_owned_working_behavior_for_all_bound_documents() {
    let node = build_persona_node().await;
    let identity = persona_identity("targeted-config");
    let owner = identity.did().to_string();
    for behavior in ["setup", "working"] {
        crate::test_support::install_test_behavior(&node, &owner, behavior).await;
    }
    let setup = SelfConfigCore::new(node.clone(), owner.clone(), "setup".into()).unwrap();
    setup
        .apply(behavior_request(
            &setup,
            vec![(
                "tags".into(),
                Some(json!([
                    crate::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG
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
            false,
        ))
        .await
        .unwrap();

    let mut tool_config = config(&["persona", "behavior", "tools", "profile", "backend"]);
    tool_config.behavior_id = "setup".into();
    tool_config.dry_run = true;
    tool_config.no_lockout = true;
    let tools = build_self_config_tools(node.clone(), owner.clone(), Some(identity), &tool_config);

    call_config_tool(
        &tools,
        vec![
            "behavior".into(),
            "edit".into(),
            "working".into(),
            "--set".into(),
            "tags=[\"ui:review\"]".into(),
        ],
    )
    .await
    .unwrap();
    call_config_tool(
        &tools,
        vec![
            "behavior".into(),
            "context".into(),
            "edit".into(),
            "--behavior".into(),
            "working".into(),
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
            "--behavior=working".into(),
            "--set".into(),
            "subagents={\"target_ids\":[],\"spawn_enabled\":false}".into(),
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
            "--behavior".into(),
            "working".into(),
            "--set".into(),
            "display_name=\"Working profile\"".into(),
        ],
    )
    .await
    .unwrap();

    let working: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["get".into(), "--behavior".into(), "working".into()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(working["behavior"]["tags"], json!(["ui:review"]));
    assert_eq!(working["context"]["system_prompt"], "Review carefully.");
    assert_eq!(
        working["documents"]["Tools"]["subagents"]["spawn_enabled"],
        false
    );
    assert!(working["documents"]["Tools"]["remote"]["services"].is_null());
    assert!(
        working["documents"]["Tools"]["self_config"].is_null(),
        "targeting a sibling must protect the invoking Setup chain without granting config to the sibling"
    );
    let working_core = SelfConfigCore::new(node.clone(), owner.clone(), "working".into()).unwrap();
    working_core
        .apply(behavior_request(
            &working_core,
            vec![(
                "inference_profile_id".into(),
                Some(json!("setup:inference")),
            )],
        ))
        .await
        .unwrap();
    let shared_backend = call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "edit".into(),
            "--behavior".into(),
            "working".into(),
            "--set".into(),
            "enabled=false".into(),
        ],
    )
    .await
    .expect_err("a sibling edit must not disable the invoking Setup backend");
    assert!(
        shared_backend.contains("backend disabled"),
        "{shared_backend}"
    );
    working_core
        .apply(behavior_request(
            &working_core,
            vec![(
                "inference_profile_id".into(),
                Some(json!("working:inference")),
            )],
        ))
        .await
        .unwrap();
    assert_eq!(
        working["inference_profile"]["display_name"],
        "Working profile"
    );

    let help: Value = serde_json::from_str(
        &call_config_tool(&tools, vec!["help".into(), "tools".into()])
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(help["patch_contracts"][0]["writable_fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field == "subagents"));
    assert!(help["patch_contracts"][0]["field_shapes"]["subagents"]
        .get("target_ids")
        .is_some());

    let mailbox_help: Value = serde_json::from_str(
        &call_config_tool(&tools, vec!["help".into(), "datastore".into()])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mailbox_help["canonical_mailbox_entries"],
        json!({"entries": [
            crate::document_config::SurfaceToolDecl::Create(crate::mailbox::canonical_mailbox_write_decl())
        ]})
    );
    assert_eq!(
        mailbox_help["mailbox_values"]["notification_identity"]["condition"],
        json!({"mode":"condition","key":"monitor-summary"})
    );

    let create_profile_args = vec![
        "profile".into(),
        "preview".into(),
        "create".into(),
        "review-profile".into(),
        "--set".into(),
        "backend_id=\"working:backend\"".into(),
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
                "behavior".into(),
                "preview".into(),
                "default".into(),
                "working".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(default_preview["committed"], false);
    assert_eq!(default_preview["admitted"], true);
    assert_eq!(default_preview["proposed_values"]["make_default"], true);

    let setup_error = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--behavior".into(),
            "setup".into(),
            "--set".into(),
            "tags=[\"changed\"]".into(),
        ],
    )
    .await
    .unwrap_err();
    assert!(setup_error.to_string().contains("protected Setup"));

    // Lean siblingToolsAllowed: both reference observations happen in the
    // same patch transaction. Sharing either the Context or Tools with Setup
    // must reject without mutating the shared document.
    working_core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![("tools_id".into(), Some(json!("setup:tools")))],
        ))
        .await
        .unwrap();
    let shared_tools = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--behavior".into(),
            "working".into(),
            "--set".into(),
            "display_name=\"must not land\"".into(),
        ],
    )
    .await
    .expect_err("Tools shared with Setup must be protected transactionally");
    assert!(
        shared_tools.contains("unshared Context and Tools"),
        "{shared_tools}"
    );
    working_core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![("tools_id".into(), Some(json!("working:tools")))],
        ))
        .await
        .unwrap();
    working_core
        .apply(behavior_request(
            &working_core,
            vec![("context_id".into(), Some(json!("setup:context")))],
        ))
        .await
        .unwrap();
    let shared_context = call_config_tool(
        &tools,
        vec![
            "behavior".into(),
            "context".into(),
            "edit".into(),
            "--behavior".into(),
            "working".into(),
            "--set".into(),
            "display_name=\"must not land\"".into(),
        ],
    )
    .await
    .expect_err("Context shared with Setup must be protected transactionally");
    assert!(
        shared_context.contains("unshared Context and Tools"),
        "{shared_context}"
    );
}

#[tokio::test]
async fn cleanup_previews_and_removes_exact_unreferenced_cycles_atomically() {
    let node = build_persona_node().await;
    let identity = persona_identity("config-cleanup");
    let owner = identity.did().to_string();
    for behavior in ["beh-test", "orphan"] {
        crate::test_support::install_test_behavior(&node, &owner, behavior).await;
    }
    let mut tool_config = config(&["persona", "tools", "profile", "backend"]);
    tool_config.dry_run = true;
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config);

    let referenced = call_config_tool(
        &tools,
        vec![
            "cleanup".into(),
            "preview".into(),
            "--target".into(),
            "backend=orphan:backend".into(),
        ],
    )
    .await
    .expect_err("cleanup must reject a retained profile's backend");
    assert!(
        referenced.contains("references missing InferenceBackend"),
        "{referenced}"
    );

    let targets = [
        "behavior=orphan",
        "context=orphan:context",
        "tools=orphan:tools",
        "profile=orphan:inference",
        "backend=orphan:backend",
    ];
    let argv = |verb: &str| {
        let mut argv = vec!["cleanup".to_owned(), verb.to_owned()];
        for target in targets {
            argv.push("--target".to_owned());
            argv.push(target.to_owned());
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
    call_config_tool(
        &tools,
        vec!["behavior".into(), "get".into(), "orphan".into()],
    )
    .await
    .expect("preview performs no writes");

    call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "edit".into(),
            "--behavior".into(),
            "orphan".into(),
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
    let missing = call_config_tool(
        &tools,
        vec!["behavior".into(), "get".into(), "orphan".into()],
    )
    .await
    .expect_err("removed behavior is no longer inspectable");
    assert!(
        missing.contains("missing")
            || missing.contains("no owned")
            || missing.contains("unknown behavior_id"),
        "{missing}"
    );

    call_config_tool(
        &tools,
        vec!["get".into(), "--behavior".into(), "beh-test".into()],
    )
    .await
    .expect("cleanup preserves unrelated configuration");
}

#[derive(serde::Deserialize)]
struct PersonaRequestRowForTest {
    request_key: Option<String>,
    requester_did: Option<String>,
    agent_did: Option<String>,
    op: Option<String>,
    clone_from: Option<String>,
    preset: Option<String>,
    edit_fields: Option<Vec<String>>,
}

async fn load_persona_rows_for_test(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
) -> Vec<PersonaRequestRowForTest> {
    let agent_did = crate::graphql::escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            PersonaConfigRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{
                request_key
                requester_did
                agent_did
                op
                clone_from
                preset
                edit_fields
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("PersonaConfigRequest"))
            .cloned()
            .unwrap_or(serde_json::Value::Array(Vec::new())),
    )
    .expect("decode rows")
}

/// Owns the tool as `Box<dyn ToolDyn>` so it can be moved into a spawned
/// task: `config behavior` polls for up to 5s internally, and this test
/// must run a manual reconciler tick concurrently — NOT a background task —
/// while that poll is in flight, so the tool's own call observes the
/// converged status instead of timing out at "pending".
fn take_persona_tool(
    tools: Vec<Box<dyn crate::llm::tool::ToolDyn>>,
) -> Box<dyn crate::llm::tool::ToolDyn> {
    tools
        .into_iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
}

#[tokio::test]
async fn behavior_default_uses_the_signed_persona_request_owner() {
    let node = build_persona_node().await;
    let identity = persona_identity("behavior-default");
    let agent_did = identity.did().to_string();
    for behavior in ["seed", "working"] {
        crate::test_support::install_test_behavior(&node, &agent_did, behavior).await;
    }
    let mut tool_config = config(&["persona"]);
    tool_config.behavior_id = "seed".into();
    tool_config.dry_run = true;
    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity.clone()),
        &tool_config,
    );
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "behavior".into(),
                "preview".into(),
                "default".into(),
                "working".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["admitted"], true);
    assert!(load_persona_rows_for_test(&node, &agent_did)
        .await
        .is_empty());

    let tool = take_persona_tool(tools);
    let call = tokio::spawn(async move {
        tool.call(json!({"argv":["behavior", "default", "working"]}).to_string())
            .await
    });
    let mut request_key = None;
    for _ in 0..50 {
        if let Some(row) = load_persona_rows_for_test(&node, &agent_did)
            .await
            .into_iter()
            .next()
        {
            assert!(row.edit_fields.as_deref().unwrap_or_default().is_empty());
            request_key = row.request_key;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request_key = request_key.expect("config behavior default authors a request row");
    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::with_local_identity(
        node.clone(),
        None,
        identity,
    );
    let outcome = crate::agent::p2p_reconcile::reconcile_persona_tick(&store, &node)
        .await
        .unwrap();
    assert!(outcome.applied.contains(&request_key), "{outcome:?}");
    let output: Value = serde_json::from_str(&call.await.unwrap().unwrap()).unwrap();
    assert_eq!(output["effective"]["is_default"], true);
    assert_eq!(
        principal_default_behavior(&node, &agent_did)
            .await
            .unwrap()
            .as_deref(),
        Some("working")
    );
}

#[tokio::test]
async fn persona_create_authors_row_and_applies_after_manual_tick() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-create");
    let agent_did = identity.did().to_string();

    crate::test_support::install_test_behavior(&node, &agent_did, "seed").await;
    let profile_id = "seed:inference";

    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity.clone()),
        &config(&["persona"]),
    );
    let rejected_preview = call_config_tool(
        &tools,
        vec![
            "behavior".into(),
            "preview".into(),
            "create".into(),
            "--display-name".into(),
            "Research Assistant".into(),
            "--preset".into(),
            "write".into(),
            "--profile".into(),
            profile_id.into(),
        ],
    )
    .await
    .expect("invalid preview is an inspectable no-write result");
    let rejected_preview: Value = serde_json::from_str(&rejected_preview).unwrap();
    assert_eq!(rejected_preview["committed"], false);
    assert_eq!(rejected_preview["admitted"], false);
    assert!(rejected_preview["rejection"]
        .as_str()
        .unwrap()
        .contains("system_prompt is required"));
    let admitted_preview = call_config_tool(
        &tools,
        vec![
            "behavior".into(),
            "preview".into(),
            "create".into(),
            "--display-name".into(),
            "Research Assistant".into(),
            "--description".into(),
            "Researches a focused question".into(),
            "--system-prompt".into(),
            "Research the question and cite evidence.".into(),
            "--preset".into(),
            "write".into(),
            "--profile".into(),
            profile_id.into(),
            "--default".into(),
        ],
    )
    .await
    .expect("complete preview succeeds");
    let admitted_preview: Value = serde_json::from_str(&admitted_preview).unwrap();
    assert_eq!(admitted_preview["committed"], false);
    assert_eq!(admitted_preview["admitted"], true);
    assert!(admitted_preview.get("preset_requested").is_some());
    assert!(admitted_preview.get("preset_effective").is_none());
    assert!(admitted_preview["note"]
        .as_str()
        .unwrap()
        .contains("does not verify materialization"));
    assert!(load_persona_rows_for_test(&node, &agent_did)
        .await
        .is_empty());
    let tool = take_persona_tool(tools);

    let args = json!({"argv": [
        "behavior", "create",
        "--display-name", "Research Assistant",
        "--description", "Researches a focused question",
        "--system-prompt", "Research the question and cite evidence.",
        "--preset", "write",
        "--profile", profile_id,
        "--default"
    ]})
    .to_string();

    // The tool call's internal poll runs to completion in the background
    // while THIS task drives one manual reconciler tick — the exact
    // production reconciler, called directly, never the spawned background
    // loop — so the call converges on "applied" instead of waiting out its
    // full 5s "still pending" ceiling.
    let call_handle = tokio::spawn(async move { tool.call(args).await });

    let mut request_key = None;
    for _ in 0..50 {
        let rows = load_persona_rows_for_test(&node, &agent_did).await;
        if let Some(row) = rows.into_iter().next() {
            assert_eq!(
                row.requester_did.as_deref(),
                Some(agent_did.as_str()),
                "self-authored requests set requester_did == agent_did"
            );
            assert_eq!(row.agent_did.as_deref(), Some(agent_did.as_str()));
            assert!(row.edit_fields.as_deref().unwrap_or_default().is_empty());
            request_key = row.request_key;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request_key = request_key.expect("config behavior authors a PersonaConfigRequest row");

    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::with_local_identity(
        node.clone(),
        None,
        identity.clone(),
    );
    let outcome = crate::agent::p2p_reconcile::reconcile_persona_tick(&store, &node)
        .await
        .expect("manual reconcile tick");
    assert!(
        outcome.applied.contains(&request_key),
        "manual tick must apply the pending request: {outcome:?}"
    );

    let behaviors = crate::list_agent_behaviors(&node, &agent_did)
        .await
        .expect("list behaviors");
    let created: Vec<_> = behaviors
        .iter()
        .filter(|behavior| behavior.behavior_id != "seed")
        .collect();
    assert_eq!(created.len(), 1, "exactly one new behavior materialized");
    assert_eq!(
        created[0].display_name,
        Some("Research Assistant".to_string())
    );
    let context_id = crate::graphql::escape_graphql_string(
        created[0]
            .context_id
            .as_deref()
            .expect("created behavior has context"),
    );
    let response = node
        .execute(&format!(
            r#"{{ AgentContext(filter: {{ context_id: {{ _eq: "{context_id}" }} }}) {{ description system_prompt }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    let prompt = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentContext"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("system_prompt"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(prompt, Some("Research the question and cite evidence."));
    let description = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentContext"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("description"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(description, Some("Researches a focused question"));

    let output = call_handle
        .await
        .expect("tool call task joins")
        .expect("config behavior call succeeds");
    assert!(
        output.contains("\"status\": \"applied\""),
        "the tool's own poll must observe the manual tick's outcome: {output}"
    );
    assert!(output.contains(&request_key), "{output}");
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        output["materialized_ids"]["behavior_id"],
        created[0].behavior_id
    );
    assert_eq!(
        output["effective"]["effective_config"]["context"]["system_prompt"],
        "Research the question and cite evidence."
    );
    assert_eq!(output["activation"]["durable"], "confirmed");
    assert_eq!(
        output["effective"]["effective_config"]["tool_grants"]["configured"],
        json!({"lsp": false, "native_graph_tools": false, "network_mode": "inherit"})
    );
    let grant_tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity.clone()),
        &config(&["persona", "tools"]),
    );
    let selected = call_config_tool(
        &grant_tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--behavior".into(),
            created[0].behavior_id.clone(),
            "--set".into(),
            r#"integrations={"lsp":{}}"#.into(),
            "--set".into(),
            r#"built_ins={"enable_graph_tools":true}"#.into(),
            "--set".into(),
            r#"host={"files":{"mode":"ReadOnly"},"bash":{"mode":"ReadOnly","network_mode":"disabled"}}"#.into(),
        ],
    )
    .await
    .expect("explicit second operation grants sibling tools");
    let applied: Value = serde_json::from_str(&selected).unwrap();
    assert_eq!(applied["committed"], true);
    let selected = call_config_tool(
        &grant_tools,
        vec![
            "get".into(),
            "--behavior".into(),
            created[0].behavior_id.clone(),
        ],
    )
    .await
    .expect("inspect the selected sibling after the canonical tools patch");
    let selected: Value = serde_json::from_str(&selected).unwrap();
    assert_eq!(
        selected["tool_grants"]["configured"],
        json!({"lsp": true, "native_graph_tools": true, "network_mode": "disabled"})
    );
    assert_eq!(
        selected["runtime_effective"]["effective"]["network_mode"],
        "disabled"
    );

    let snapshot = crate::agent::resolve_document_runtime_snapshot(
        node.as_ref(),
        &crate::agent::DocumentResolveContext {
            identity,
            tool_ceiling: crate::tool_surface::ToolCeiling::readonly(),
            backend_health: Default::default(),
        },
    )
    .await
    .expect("restart-style runtime resolution accepts the materialized behavior");
    assert_eq!(snapshot.default_behavior_id, created[0].behavior_id);
    let runtime_behavior = snapshot
        .behaviors
        .get(&created[0].behavior_id)
        .unwrap_or_else(|| {
            panic!(
                "new default is runnable after re-resolution: {:?}",
                snapshot.unavailable_behaviors
            )
        });
    assert_eq!(
        runtime_behavior.system_prompt,
        "Research the question and cite evidence."
    );
    let names = runtime_behavior
        .tools
        .resolve(node.as_ref(), &agent_did)
        .await
        .expect("new behavior tool surface resolves after restart")
        .tool_names();
    assert!(names.iter().any(|name| name == "lsp"), "{names:?}");
    assert!(
        names.iter().any(|name| name == RUN_GRAPH_TOOL_NAME),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name == CONFIG_TOOL_NAME),
        "{names:?}"
    );
}

#[tokio::test]
async fn persona_clone_accepts_sibling_behavior_id() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-clone");
    let agent_did = identity.did().to_string();
    let qualified_sibling_id = "sibling-behavior".to_owned();
    crate::test_support::install_test_behavior(&node, &agent_did, &qualified_sibling_id).await;
    let profile_id = "sibling-behavior:inference";

    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity.clone()),
        &config(&["persona"]),
    );
    let tool = take_persona_tool(tools);
    let args = json!({"argv": [
        "behavior", "clone",
        "--display-name", "Cloned Persona",
        "--from", "sibling-behavior",
        "--profile", profile_id
    ]})
    .to_string();
    let call_handle = tokio::spawn(async move { tool.call(args).await });

    let mut request_key = None;
    for _ in 0..50 {
        let rows = load_persona_rows_for_test(&node, &agent_did).await;
        if let Some(row) = rows.into_iter().next() {
            assert_eq!(row.op.as_deref(), Some("create"));
            assert_eq!(
                row.clone_from.as_deref(),
                Some(qualified_sibling_id.as_str()),
                "clone_from must resolve to the sibling's fully-qualified behavior_id"
            );
            assert!(row.preset.is_none(), "clone must not also set a preset");
            request_key = row.request_key;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request_key = request_key.expect("config behavior authors a PersonaConfigRequest row");

    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::with_local_identity(
        node.clone(),
        None,
        identity,
    );
    let outcome = crate::agent::p2p_reconcile::reconcile_persona_tick(&store, &node)
        .await
        .expect("manual reconcile tick");
    assert!(
        outcome.applied.contains(&request_key),
        "cloning from an enabled sibling must be admitted and applied: {outcome:?}"
    );

    let output = call_handle
        .await
        .expect("tool call task joins")
        .expect("config behavior clone call succeeds");
    assert!(output.contains("\"status\": \"applied\""), "{output}");
}

#[tokio::test]
async fn canonical_self_config_preview_and_apply_preserve_scope_and_reject_lockout() {
    let node = build_persona_node().await;
    let identity = persona_identity("canonical-self-config");
    let other = persona_identity("other-self-config");
    let owner = identity.did().to_string();
    let foreign = other.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "same").await;
    crate::test_support::install_test_behavior(&node, &foreign, "same").await;
    crate::test_support::install_test_behavior(&node, &owner, "unconfigured").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "same".into()).unwrap();
    let patch = vec![(
        "self_config".into(),
        Some(json!({"enable_self_config":true})),
    )];
    let preview = core
        .preview(tools_request(&core, patch.clone(), false))
        .await
        .unwrap();
    assert!(!preview.committed);
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(read["documents"]["Tools"]["self_config"].is_null());
    core.apply(tools_request(&core, patch, false))
        .await
        .unwrap();
    let guarded = core.clone().with_no_lockout(true);
    assert!(guarded
        .apply(tools_request(
            &guarded,
            vec![("self_config".into(), None)],
            false,
        ))
        .await
        .is_err());
    assert!(guarded
        .preview(behavior_request(
            &guarded,
            vec![("context_id".into(), Some(json!("unconfigured:context")))]
        ))
        .await
        .is_err());
    assert!(core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![("tools_id".into(), Some(json!("missing")))]
        ))
        .await
        .is_err());
    assert!(core
        .preview(tools_request(
            &core,
            vec![("host".into(), Some(json!({"unexpected":true})))],
            false,
        ))
        .await
        .is_err());
    let own = core
        .read_effective_config(&BTreeSet::new(), true, true)
        .await
        .unwrap();
    assert_eq!(own["context"]["tools_id"], "same:tools");
    assert_eq!(
        own["documents"]["Tools"]["self_config"]["enable_self_config"],
        true
    );
    let foreign_core = SelfConfigCore::new(node.clone(), foreign, "same".into()).unwrap();
    let foreign_read = foreign_core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    assert!(foreign_read["documents"]["Tools"]["self_config"].is_null());
}

#[tokio::test]
async fn backend_self_config_protects_raw_keys_in_writes_reads_and_diffs() {
    let node = build_persona_node().await;
    let identity = persona_identity("self-config-secret");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "secret").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "secret".into()).unwrap();
    let raw = vec![(
        "auth".into(),
        Some(json!({"kind":"api_key","key":"do-not-expose"})),
    )];
    assert!(core.preview(backend_request(raw.clone())).await.is_err());
    assert!(core.apply(backend_request(raw)).await.is_err());
    let owner = escape_graphql_string(&owner);
    let response=node.execute(&format!(r#"mutation {{update_InferenceBackend(filter:{{agent_did:{{_eq:"{owner}"}},backend_id:{{_eq:"secret:backend"}}}},input:{{auth:{{kind:"api_key",key:"operator-secret"}}}}) {{_docID}}}}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    // Lean authPatchAllowed permits preserving an existing raw key exactly.
    core.preview(backend_request(vec![(
        "auth".into(),
        Some(json!({"kind":"api_key","key":"operator-secret"})),
    )]))
    .await
    .unwrap();
    assert!(core
        .apply(backend_request(vec![(
            "auth".into(),
            Some(json!({"kind":"api_key","key":"replacement-secret"}))
        )]))
        .await
        .is_err());
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(!read.to_string().contains("operator-secret"));
    let patch = vec![(
        "auth".into(),
        Some(json!({"kind":"environment","variable":"GENTS_TEST_KEY_REFERENCE"})),
    )];
    let preview = core.preview(backend_request(patch)).await.unwrap();
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("operator-secret"));
}

#[tokio::test]
async fn explicit_tools_grant_preserves_lsp_settings_guard_for_preview_and_apply() {
    let node = build_persona_node().await;
    let identity = persona_identity("self-config-lsp-guard");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let mut tool_config = config(&["tools"]);
    tool_config.dry_run = true;
    let tools = build_self_config_tools(node, owner, None, &tool_config);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
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
        // Operator configuration keeps its existing broader admission.
        crate::toolset::lsp::LspConfigDocument::parse_operator(Some(&raw)).unwrap();
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
