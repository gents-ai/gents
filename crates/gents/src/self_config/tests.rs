//! Unit tests for the self-config tool family. End-to-end lifecycle coverage
//! (identity-scoped writes, reconcile pickup) lives in
//! `tests/e2e_runtime/self_config_tools.rs`.

use super::command::{agent_params, help_patch_contracts};
use super::*;

fn test_plugins() -> Arc<crate::plugin::executor::PluginExecutor> {
    Arc::new(crate::plugin::executor::PluginExecutor::default())
}

#[derive(Clone)]
struct RootReadModel {
    path: String,
    turns: Arc<std::sync::atomic::AtomicUsize>,
    provider_inputs: Arc<std::sync::Mutex<Vec<String>>>,
}

#[allow(refining_impl_trait)]
impl rig::completion::CompletionModel for RootReadModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        unreachable!("the acceptance test constructs its deterministic model directly")
    }

    async fn completion(
        &self,
        _request: rig::completion::CompletionRequest,
    ) -> Result<rig::completion::CompletionResponse<()>, rig::completion::CompletionError> {
        Err(rig::completion::CompletionError::ProviderError(
            "non-streaming completion is unused".into(),
        ))
    }

    async fn stream(
        &self,
        request: rig::completion::CompletionRequest,
    ) -> Result<rig::streaming::StreamingCompletionResponse<()>, rig::completion::CompletionError>
    {
        use std::sync::atomic::Ordering;

        crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
        if !request.tools.iter().any(|tool| tool.name == "read_file") {
            let stream: rig::streaming::StreamingResult<()> = Box::pin(futures::stream::iter([
                Ok(rig::streaming::RawStreamingChoice::Message(
                    "workspace-root-check".into(),
                )),
                Ok(rig::streaming::RawStreamingChoice::FinalResponse(())),
            ]));
            return Ok(rig::streaming::StreamingCompletionResponse::stream(stream));
        }
        self.provider_inputs
            .lock()
            .expect("provider input capture")
            .push(serde_json::to_string(&request.chat_history).expect("serialize provider input"));
        let choices = if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                rig::streaming::RawStreamingChoice::ToolCall(
                    rig::streaming::RawStreamingToolCall::new(
                        "root-read".into(),
                        "read_file".into(),
                        json!({"path": self.path}),
                    ),
                ),
                rig::streaming::RawStreamingChoice::FinalResponse(()),
            ]
        } else {
            vec![
                rig::streaming::RawStreamingChoice::Message("root check complete".into()),
                rig::streaming::RawStreamingChoice::FinalResponse(()),
            ]
        };
        let stream: rig::streaming::StreamingResult<()> =
            Box::pin(futures::stream::iter(choices.into_iter().map(Ok)));
        Ok(rig::streaming::StreamingCompletionResponse::stream(stream))
    }
}

pub(super) fn config(categories: &[&str]) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        agent_id: "beh-test".to_string(),
        categories: categories.iter().map(|c| c.to_string()).collect(),
        no_lockout: false,
        preview: false,
        enable_pack_install: false,
        enable_graph_tools: false,
        process_ceiling: Default::default(),
    }
}

#[test]
fn tool_names_follow_enabled_categories() {
    let names = self_config_tool_names(&config(&["agent", "tools", "profile"]));
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
    let without_install = config(&["agent"]);
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
        "agent",
        "tools",
        "profile",
        "backend",
        "mcp-service",
        "automation",
        "datastore",
        "skill",
        "agent-target",
        "execution",
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
        &shapes["host"]["cli"][0],
        canonical_struct_fields::<CliTool>().unwrap(),
        "cli[]",
    );
    assert_fields(
        &shapes["remote"],
        canonical_struct_fields::<RemoteTools>().unwrap(),
        "remote",
    );
    assert_fields(
        &shapes["agents"],
        canonical_struct_fields::<AgentTools>().unwrap(),
        "agents",
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
async fn build_fails_closed_without_node_did() {
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
        &config(&["agent"]),
        test_plugins(),
    );
    assert!(
        tools.is_empty(),
        "an empty node DID must register no self-config tools"
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
        &config(&["agent", "backend"]),
        test_plugins(),
    );
    assert_eq!(
        tools.len(),
        1,
        "self-configuration has one model-facing tool"
    );
    assert_eq!(tools[0].name(), CONFIG_TOOL_NAME);
    let definition = tools[0].definition(String::new()).await;
    // The description is the small core of the layered text (#2088); fields,
    // recipes and the configuration model live in help and the Engineer prompt.
    assert!(
        definition.description.len() <= 700,
        "config description grew to {} chars",
        definition.description.len()
    );
    for core in [
        "native API",
        "\"<DID>:<slug>\"",
        "Preview writes nothing.",
        "[\"validate\"] audits saved configuration",
        "[\"help\"] lists granted resources",
    ] {
        assert!(definition.description.contains(core), "missing {core}");
    }
    assert_eq!(definition.parameters["required"], json!(["argv"]));
}

#[tokio::test]
async fn pack_install_uses_current_node_and_inference_chain() {
    let node = build_agent_node().await;
    let identity = agent_identity("pack-install");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    for role in ["coordinator", "worker", "verifier"] {
        crate::test_support::install_test_agent(&node, &node_did, role).await;
    }

    // The pack resolves from the home's store: the registry is never asked.
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let mut tool_config = config(&[]);
    tool_config.agent_id = "setup".to_string();
    tool_config.enable_pack_install = true;
    tool_config.enable_graph_tools = true;
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity),
        &tool_config,
        plugins,
    );
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    for name in [
        PREVIEW_GRAPH_TOOL_NAME,
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
            .call(
                json!({"argv": ["pack", "preview", "install", "fixture/review_graph"]}).to_string(),
            )
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
    let mut preview_argv = vec!["pack", "preview", "install", "fixture/review_graph"];
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
    let mut install_argv = vec![
        "pack",
        "install",
        "fixture/review_graph",
        "--digest",
        digest,
    ];
    install_argv.extend(slot_argv);
    let output = tool
        .call(json!({"argv": install_argv}).to_string())
        .await
        .expect("a stored pack installs");
    let output: serde_json::Value = serde_json::from_str(&output).expect("JSON receipt");
    assert_eq!(output["install"]["package_name"], "review_graph");
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
            "{{ GraphDefinition(filter: {{node_did: {{_eq: \"{}\"}}}}) {{graph_id node_did active_revision_digest}} }}",
            crate::graphql::escape_graphql_string(&node_did)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let definitions = response.data.unwrap()["GraphDefinition"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["node_did"], node_did);
    assert_eq!(definitions[0]["graph_id"], "code-review");
    assert_eq!(
        definitions[0]["active_revision_digest"],
        output["install"]["revision_digest"]
    );
    let inspected: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "get", "fixture/review_graph"]}).to_string())
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
                "pack", "update", "fixture/review_graph@1.0.0", "--digest", bad_digest,
                "--inference-slot", "coordinator=coordinator:inference",
                "--inference-slot", "worker=worker:inference",
                "--inference-slot", "verifier=verifier:inference"
            ]})
            .to_string(),
        )
        .await
        .expect_err("a digest not returned by preview is rejected");
    assert!(rejected.to_string().contains("preview again"));

    let owner = crate::graphql::escape_graphql_string(&node_did);
    let tagged = node
        .execute(&format!(
            r#"mutation {{ update_Agent(filter: {{node_did: {{_eq: "{owner}"}}, agent_id: {{_eq: "review-recon"}}}}, input: {{tags: ["gents:pack:review_graph", "user:favorite"]}}) {{_docID}} }}"#
        ))
        .await;
    assert!(!tagged.has_errors(), "{:?}", tagged.errors);
    let update_preview: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": [
                    "pack", "preview", "update", "fixture/review_graph@1.0.0",
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
                    "pack", "update", "fixture/review_graph@1.0.0", "--digest", update_digest,
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
            r#"{{ Agent(filter: {{node_did: {{_eq: "{owner}"}}, agent_id: {{_eq: "review-recon"}}}}) {{tags}} }}"#
        ))
        .await;
    assert!(!tags.has_errors(), "{:?}", tags.errors);
    assert_eq!(
        tags.data.unwrap()["Agent"][0]["tags"],
        json!(["gents:pack:review_graph", "user:favorite"])
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
    assert_eq!(list["node_did"], node_did);
    assert_eq!(list["graphs"][0]["definition"]["graph_id"], "code-review");
    assert_eq!(
        list["graphs"][0]["active_plan"]["digest"],
        output["install"]["revision_digest"]
    );
    // No run has been attempted here.
    let runs = node
        .execute(&format!(
            r#"{{ GraphRun(filter: {{owner_did: {{_eq: "{}"}}}}) {{run_id}} }}"#,
            crate::graphql::escape_graphql_string(&node_did)
        ))
        .await;
    assert!(!runs.has_errors(), "{:?}", runs.errors);
    assert!(runs.data.unwrap()["GraphRun"]
        .as_array()
        .unwrap()
        .is_empty());

    // `pack remove` is a real operation now, not a stale refusal: it deletes
    // the installed graph and its record.
    let removed = tool
        .call(json!({"argv": ["pack", "remove", "fixture/review_graph"]}).to_string())
        .await
        .expect("an installed graph package removes");
    let removed: Value = serde_json::from_str(&removed).unwrap();
    assert_eq!(removed["pack"], "fixture/review_graph");

    let response = node
        .execute(&format!(
            "{{ GraphDefinition(filter: {{node_did: {{_eq: \"{}\"}}}}) {{graph_id}} PackInstallation(filter: {{node_did: {{_eq: \"{}\"}}}}) {{_docID}} }}",
            crate::graphql::escape_graphql_string(&node_did),
            crate::graphql::escape_graphql_string(&node_did)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    assert!(data["GraphDefinition"].as_array().unwrap().is_empty());
    assert!(data["PackInstallation"].as_array().unwrap().is_empty());

    let again = tool
        .call(json!({"argv": ["pack", "remove", "fixture/review_graph"]}).to_string())
        .await
        .expect_err("a second remove finds no record");
    assert!(again.to_string().contains("is not installed"), "{again:#}");
}

/// A tool over `plugins` that may install packs, plus the node it acts on.
async fn pack_tool(
    label: &str,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
) -> (Arc<EmbeddedNode>, String, Vec<Box<dyn ToolDyn>>) {
    let node = build_agent_node().await;
    let identity = agent_identity(label);
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    let mut tool_config = config(&[]);
    tool_config.agent_id = "setup".to_string();
    tool_config.enable_pack_install = true;
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity),
        &tool_config,
        plugins,
    );
    (node, node_did, tools)
}

async fn config_call(
    tools: &[Box<dyn ToolDyn>],
    argv: &[&str],
) -> Result<String, crate::llm::tool::ToolError> {
    tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
        .call(json!({ "argv": argv }).to_string())
        .await
}

#[tokio::test]
async fn pack_list_pages_the_packs_the_home_store_holds() {
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let (_node, _did, tools) = pack_tool("pack-list", plugins).await;
    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["source"], "store");
    assert_eq!(page["page"]["total"], 1);
    assert_eq!(page["page"]["truncated"], false);
    assert_eq!(page["items"][0]["name"], "fixture/review_graph");
    assert_eq!(page["items"][0]["version"], "1.0.0");
    assert_eq!(page["items"][0]["installable"], true);
    assert_eq!(page["items"][0]["installed"], Value::Null);

    // The cursor names the last coordinate returned; nothing sorts after it.
    let after: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "list", "--cursor", "fixture/review_graph"],
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

    // A runtime with no home has no store to list.
    let (_node, _did, homeless) = pack_tool("pack-list-homeless", test_plugins()).await;
    let page: Value =
        serde_json::from_str(&config_call(&homeless, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["page"]["total"], 0);
    assert!(page["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn pack_list_reports_a_damaged_archive_as_an_error_row() {
    let (home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let (_node, _did, tools) = pack_tool("pack-list-damaged", plugins).await;
    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    let digest = page["items"][0]["artifact_digest"].as_str().unwrap();
    let path = crate::pack_store::PackStore::new(home.path())
        .path(digest)
        .unwrap();
    std::fs::write(path, b"not a pack archive").unwrap();
    // An open prefers the unpacked copy, so the damage must be all there is.
    std::fs::remove_dir_all(home.path().join("packs").join("unpacked")).unwrap();

    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["page"]["returned"], 1);
    assert_eq!(page["items"][0]["name"], "fixture/review_graph");
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
    let _registry = crate::test_support::EnvVarGuard::set("GENTS_REGISTRY", "http://127.0.0.1:9");
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let (_node, _did, tools) = pack_tool("pack-update-offline", plugins).await;
    let error = config_call(
        &tools,
        &["pack", "preview", "update", "fixture/review_graph"],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("the newest version of fixture/review_graph could not be looked up"),
        "{error}"
    );
    // A pinned version resolves from the store with no network call.
    let error = config_call(
        &tools,
        &["pack", "preview", "update", "fixture/review_graph@1.0.0"],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("is not installed"), "{error}");
}

#[tokio::test]
async fn pack_install_puts_a_sealed_plugin_in_the_home_and_refuses_one_that_asks_for_authority() {
    let slot = ["--inference-slot", "worker=setup:inference"];
    let (_fixture, dir) = crate::test_support::fixture_pack_copy("prepared_graph", &json!({}));
    let manifest_path = dir.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["plugins"][0]
        .as_object_mut()
        .unwrap()
        .remove("limits");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (home, plugins) = crate::test_support::home_with_pack_dir(&dir);
    let (_node, _did, tools) = pack_tool("pack-plugins", plugins).await;
    let mut preview_argv = vec!["pack", "preview", "install", "fixture/prepared_graph"];
    preview_argv.extend(slot);
    let preview: Value =
        serde_json::from_str(&config_call(&tools, &preview_argv).await.unwrap()).unwrap();
    assert_eq!(preview["ready"], true);
    let digest = preview["artifact_digest"].as_str().unwrap();
    assert_eq!(
        preview["apply_with"]["argv_prefix"],
        json!([
            "pack",
            "install",
            "fixture/prepared_graph@1.0.0",
            "--digest",
            digest
        ])
    );
    let mut install_argv = vec![
        "pack",
        "install",
        "fixture/prepared_graph",
        "--digest",
        digest,
    ];
    install_argv.extend(slot);
    config_call(&tools, &install_argv)
        .await
        .expect("a sealed plugin installs with its pack");
    let record = crate::plugin::store::read_record(home.path(), "fixture", "prepare_fixture")
        .expect("the pack's plugin is in the home's plugin store");
    assert_eq!(
        record.owner_pack_coordinate.as_deref(),
        Some("fixture/prepared_graph")
    );
    assert!(record.granted.is_none(), "a sealed plugin holds no grant");

    // The same pack with a plugin that asks for network access is refused,
    // and nothing is left behind.
    let (_guard, dir) =
        crate::test_support::fixture_pack_copy("prepared_graph", &serde_json::json!({}));
    let manifest_path = dir.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["plugins"][0]
        .as_object_mut()
        .unwrap()
        .remove("limits");
    manifest["plugins"][0]["manifold"] = json!({
        "fs": "None",
        "net": {"OutboundHttp": ["api.example.com"]},
        "env": "None",
        "crypto": false,
        "child_process": false,
    });
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let (home, plugins) = crate::test_support::home_with_pack_dir(&dir);
    let (node_2, node_did_2, tools) = pack_tool("pack-plugins-authority", plugins).await;
    let mut preview_argv = vec!["pack", "preview", "install", "fixture/prepared_graph"];
    preview_argv.extend(slot);
    let preview: Value =
        serde_json::from_str(&config_call(&tools, &preview_argv).await.unwrap()).unwrap();
    assert_eq!(preview["ready"], false);
    assert!(preview["apply_with"].is_null());
    assert!(preview["installation_blockers"]
        .to_string()
        .contains("api.example.com"));
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut install_argv = vec![
        "pack",
        "install",
        "fixture/prepared_graph",
        "--digest",
        digest,
    ];
    install_argv.extend(slot);
    let error = format!(
        "{:#}",
        config_call(&tools, &install_argv).await.unwrap_err()
    );
    assert!(
        error.contains("install it with `gents pack install --grant-authority`")
            && error.contains("api.example.com"),
        "{error}"
    );
    assert!(
        crate::plugin::store::read_record(home.path(), "fixture", "prepare_fixture").is_err(),
        "a refused plugin leaves no record"
    );
    let graphs = node_2
        .execute(&format!(
            "{{ GraphDefinition(filter: {{node_did: {{_eq: \"{}\"}}}}) {{graph_id}} }}",
            crate::graphql::escape_graphql_string(&node_did_2)
        ))
        .await;
    assert!(graphs.data.unwrap()["GraphDefinition"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn graph_tools_start_observe_and_cancel_on_the_current_node() {
    let node = build_agent_node().await;
    let identity = agent_identity("graph-tools");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
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
    tool_config.agent_id = "setup".to_owned();
    tool_config.enable_pack_install = true;
    tool_config.enable_graph_tools = true;
    tool_config.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(repository.path().to_owned()),
    };
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity.clone()),
        &tool_config,
        plugins.clone(),
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
    .expect("current agent receives explicit read authority");
    let preview = call(
        CONFIG_TOOL_NAME,
        json!({"argv": [
            "pack", "preview", "install", "fixture/review_graph",
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
            "pack", "install", "fixture/review_graph", "--digest", digest,
            "--inference-slot", "coordinator=setup:inference",
            "--inference-slot", "worker=setup:inference",
            "--inference-slot", "verifier=setup:inference"
        ]}),
    )
    .await
    .expect("code-review pack installs");
    // The `review_graph` fixture has no `prepare`, so this test passes the
    // entry's workspace and ref fields directly, provisioned through the
    // same production path as `git_diff`, and leaves out evidence. That is
    // enough to start, observe and cancel the run.
    let rev_parse = |rev: &str| -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args([
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{rev}^{{commit}}"),
            ])
            .output()
            .expect("run git rev-parse");
        assert!(output.status.success(), "git rev-parse {rev} failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let base_ref = rev_parse("HEAD^");
    let head_ref = rev_parse("HEAD");
    let access = graph_access(&node);
    let workspace = crate::workspace::provision_read_only_workspace(
        &access,
        repository.path(),
        &head_ref,
        &node_did,
    )
    .await
    .expect("read-only workspace provisions");
    let input = json!({
        "repository_path": ".",
        "base_ref": base_ref,
        "head_ref": head_ref,
        "workspace_id": workspace.workspace.workspace_id,
        "workspace_authority": "readOnly",
        "workspace_owner_node_did": workspace.workspace.owner_node_did,
        "lens_count": "4",
        "lens_min": "4",
        "lens_max": "4",
        "pr_number": "",
        "focus": "Review the changed text.",
    });
    // Running an admitted pack needs neither installation nor self-config.
    tool_config.enabled = false;
    tool_config.enable_pack_install = false;
    let tools = build_self_config_tools(
        node,
        node_did.clone(),
        Some(identity),
        &tool_config,
        plugins,
    );
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
            "package": "review_graph",
            "input": input,
        }),
    )
    .await
    .expect("node-bound graph starts");
    let receipt = crate::graph_pipeline::run_receipt_from_tool_result(&started)
        .expect("session observers decode the run from the durable reply");
    let started: Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["node_bound"], true);
    assert_eq!(started["node"], node_did);
    assert_eq!(started["observed"]["status"], "running");
    let run_id = started["receipt"]["run_id"].as_str().unwrap();
    assert_eq!(receipt.run_id, run_id);
    assert_eq!(
        crate::graph_pipeline::load_graph_run_view_with_access(&access, &node_did, &receipt.run_id)
            .await
            .expect("the decoded run loads through the canonical view owner")
            .run_id,
        run_id
    );

    let observed = call(GET_GRAPH_RUN_TOOL_NAME, json!({"run_id": run_id}))
        .await
        .expect("same-node status is readable");
    let observed: Value = serde_json::from_str(&observed).unwrap();
    assert_eq!(observed["run_id"], run_id);
    assert_eq!(observed["owner_did"], node_did);
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
    assert_eq!(cancelled["cancellation_requested_by"], node_did);
    assert_eq!(cancelled["cancellation_reason"], "test cleanup");
    assert_ne!(cancelled["status"], "succeeded");
}

/// A `graph_id` run goes through the same selection owner as a package run:
/// a digest that is not the active revision is refused there, naming the
/// active one, before any start transaction, and the `graph_id` and digest
/// `list_graphs` returns start the graph's only entry on the default input.
#[tokio::test]
async fn run_graph_by_graph_id_is_selected_through_the_shared_owner() {
    let node = build_agent_node().await;
    let identity = agent_identity("run-graph-by-id");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let mut tool_config = config(&[]);
    tool_config.agent_id = "setup".to_owned();
    tool_config.enable_pack_install = true;
    tool_config.enable_graph_tools = true;
    let tools = build_self_config_tools(
        node.clone(),
        node_did,
        Some(identity),
        &tool_config,
        plugins,
    );
    let call = |name: &str, args: Value| {
        let tool = tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing tool {name}"));
        tool.call(args.to_string())
    };
    let slots = [
        "--inference-slot",
        "coordinator=setup:inference",
        "--inference-slot",
        "worker=setup:inference",
        "--inference-slot",
        "verifier=setup:inference",
    ];
    let mut preview = vec!["pack", "preview", "install", "fixture/review_graph"];
    preview.extend(slots);
    let preview: Value = serde_json::from_str(
        &call(CONFIG_TOOL_NAME, json!({"argv": preview}))
            .await
            .expect("the pack previews"),
    )
    .unwrap();
    let digest = preview["artifact_digest"].as_str().unwrap().to_owned();
    let mut install = vec![
        "pack",
        "install",
        "fixture/review_graph",
        "--digest",
        digest.as_str(),
    ];
    install.extend(slots);
    call(CONFIG_TOOL_NAME, json!({"argv": install}))
        .await
        .expect("the pack installs");

    let stale = format!("sha256:{}", "0".repeat(64));
    let refused = call(
        RUN_GRAPH_TOOL_NAME,
        json!({"graph_id": "code-review", "revision_digest": stale.clone(), "input": {}}),
    )
    .await
    .expect_err("a digest that is not the active revision is refused")
    .to_string();
    assert!(
        refused.contains("graph code-review is active at revision sha256:"),
        "{refused}"
    );
    assert!(refused.contains(&format!("not {stale}")), "{refused}");

    let list: Value = serde_json::from_str(
        &call(LIST_GRAPHS_TOOL_NAME, json!({}))
            .await
            .expect("the installed graph is listed"),
    )
    .unwrap();
    let graph_id = list["graphs"][0]["definition"]["graph_id"].clone();
    let revision_digest = list["graphs"][0]["active_plan"]["digest"].clone();
    let started: Value = serde_json::from_str(
        &call(
            RUN_GRAPH_TOOL_NAME,
            json!({"graph_id": graph_id, "revision_digest": revision_digest}),
        )
        .await
        .expect("the listed graph_id and digest start without entry or input"),
    )
    .unwrap();
    assert_eq!(started["receipt"]["graph_id"], graph_id, "{started}");
    assert_eq!(
        started["receipt"]["revision_digest"], revision_digest,
        "{started}"
    );
    assert_eq!(started["receipt"]["entry_name"], "review", "{started}");
    call(
        CANCEL_GRAPH_RUN_TOOL_NAME,
        json!({"run_id": started["receipt"]["run_id"], "reason": "test cleanup"}),
    )
    .await
    .expect("the pinned run cancels");
}

/// Read from the registered tool, so it binds whichever source supplies the
/// in-session definitions: a `graph_id` run may omit entry and input too.
#[tokio::test]
async fn run_graph_description_makes_entry_optional_for_every_selection() {
    let mut tool_config = config(&[]);
    tool_config.enable_graph_tools = true;
    let tools = build_self_config_tools(
        build_agent_node().await,
        "did:key:zSelfConfigTest".to_owned(),
        None,
        &tool_config,
        test_plugins(),
    );
    let definition = tools
        .iter()
        .find(|tool| tool.name() == RUN_GRAPH_TOOL_NAME)
        .expect("run_graph registered")
        .definition(String::new())
        .await;
    assert!(
        definition
            .description
            .contains("Supply entry only when the graph has more than one"),
        "{}",
        definition.description
    );
}

/// The in-session `list_graphs` reply is the text of `list_graphs_value`
/// with the run tool; without one, as on `/mcp`, no listed graph names a
/// run tool.
#[tokio::test]
async fn list_graphs_value_is_the_in_session_reply_and_names_no_run_tool_without_one() {
    let node = build_agent_node().await;
    let identity = agent_identity("list-graphs-value");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    let mut tool_config = config(&["tools"]);
    tool_config.agent_id = "setup".to_owned();
    tool_config.enable_pack_install = true;
    tool_config.enable_graph_tools = true;
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity),
        &tool_config,
        plugins,
    );
    let call = |name: &str, args: Value| {
        let tool = tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing tool {name}"));
        tool.call(args.to_string())
    };
    let slots = [
        "--inference-slot",
        "coordinator=setup:inference",
        "--inference-slot",
        "worker=setup:inference",
        "--inference-slot",
        "verifier=setup:inference",
    ];
    let mut preview = vec!["pack", "preview", "install", "fixture/review_graph"];
    preview.extend(slots);
    let previewed = call(CONFIG_TOOL_NAME, json!({ "argv": preview }))
        .await
        .expect("review_graph previews");
    let previewed: Value = serde_json::from_str(&previewed).unwrap();
    let digest = previewed["artifact_digest"].as_str().unwrap().to_owned();
    let mut install = vec![
        "pack",
        "install",
        "fixture/review_graph",
        "--digest",
        digest.as_str(),
    ];
    install.extend(slots);
    call(CONFIG_TOOL_NAME, json!({ "argv": install }))
        .await
        .expect("review_graph installs");

    let in_session: Value =
        serde_json::from_str(&call(LIST_GRAPHS_TOOL_NAME, json!({})).await.unwrap()).unwrap();
    let access = graph_access(&node);
    let shared: Value = serde_json::from_str(
        &list_graphs_value(&access, &node_did, Some(RUN_GRAPH_TOOL_NAME))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(in_session, shared);
    let graphs = shared["graphs"].as_array().unwrap();
    assert!(!graphs.is_empty(), "{shared}");
    assert!(
        graphs
            .iter()
            .all(|graph| graph["run_with"]["tool"] == RUN_GRAPH_TOOL_NAME),
        "{shared}"
    );

    let read_only: Value =
        serde_json::from_str(&list_graphs_value(&access, &node_did, None).await.unwrap()).unwrap();
    let listed = read_only["graphs"].as_array().unwrap();
    assert_eq!(listed.len(), graphs.len());
    assert!(
        listed.iter().all(|graph| graph.get("run_with").is_none()),
        "a surface without a run tool names none: {read_only}"
    );
}

/// `RunGraphTool`'s host-ceiling gate for an entry whose `prepare` declares a
/// `git_diff` host step: refused when the current agent has no effective
/// read authority, and refused again once it does but the named repository
/// escapes the effective root. Both refusals happen before any plugin runs,
/// so the fixture's prepare plugin never needs to actually resolve, and both
/// go through the same entry selection the run itself uses (#RunGraphTool
/// ceiling gate).
#[tokio::test]
async fn run_graph_refuses_a_git_diff_prepare_without_ceiling_authority() {
    let node = build_agent_node().await;
    let identity = agent_identity("prepare-ceiling");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;

    let options = crate::graph_package::GraphPackageInstallBindings {
        node_did: node_did.clone(),
        inference_slots: BTreeMap::from([
            ("coordinator".to_owned(), "setup:inference".to_owned()),
            ("worker".to_owned(), "setup:inference".to_owned()),
            ("verifier".to_owned(), "setup:inference".to_owned()),
        ]),
    };
    let mut package = crate::test_support::load_test_graph_package("review_graph", &options);
    package.config.graph_intents[0].entries[0].prepare =
        Some(crate::graph_pipeline::EntryPrepare {
            host: vec![crate::graph_pipeline::HostInput::GitDiff {
                repository_field: "repository".to_owned(),
                base_field: "base".to_owned(),
                head_field: "head".to_owned(),
                unified_context_lines: 12,
                rename_similarity_percent: 50,
            }],
            plugin: "review-evidence".to_owned(),
            digest: Some(format!("sha256:{}", "0".repeat(64))),
            writes: vec!["CodeReviewEvidenceManifest".to_owned()],
        });
    let access = graph_access(&node);
    let receipt = crate::graph_package::install_loaded_graph_package(
        &access,
        &node_did,
        &package,
        &options,
        None,
        &crate::graph_package::GraphInstallRecord::default(),
    )
    .await
    .expect("mutated fixture installs");
    crate::graph_pipeline::activate_graph_revision_with_access(
        &access,
        &node_did,
        &receipt.graph_id,
        &receipt.revision_digest,
        None,
    )
    .await
    .expect("fixture revision activates");

    let mut tool_config = config(&["tools"]);
    tool_config.agent_id = "setup".to_owned();
    tool_config.enable_graph_tools = true;

    // No process ceiling has been granted at all: `process_ceiling` defaults
    // to `file_mode: Off`, so effective authority is `Off` regardless of
    // anything the agent itself requests.
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity.clone()),
        &tool_config,
        test_plugins(),
    );
    let off = tools
        .iter()
        .find(|tool| tool.name() == RUN_GRAPH_TOOL_NAME)
        .expect("run_graph registered")
        .call(json!({"package": "review_graph", "input": {}}).to_string())
        .await
        .expect_err("a git_diff prepare step requires effective read authority");
    assert!(
        off.to_string()
            .contains("requires effective read authority"),
        "{off:#}"
    );
    let pinned_off = tools
        .iter()
        .find(|tool| tool.name() == RUN_GRAPH_TOOL_NAME)
        .expect("run_graph registered")
        .call(
            json!({
                "graph_id": receipt.graph_id,
                "revision_digest": receipt.revision_digest,
            })
            .to_string(),
        )
        .await
        .expect_err("a graph_id run of the same entry meets the same ceiling");
    assert!(
        pinned_off
            .to_string()
            .contains("requires effective read authority"),
        "{pinned_off:#}"
    );

    // Grant a read-only process ceiling rooted at a directory that does not
    // contain the repository the operator is about to name, and request
    // that same root on the agent itself (the effective root is the meet
    // of the two).
    let allowed_root = tempfile::tempdir().expect("allowed root");
    let outside = tempfile::tempdir().expect("outside directory");
    tool_config.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(allowed_root.path().to_owned()),
    };
    let tools =
        build_self_config_tools(node, node_did, Some(identity), &tool_config, test_plugins());
    let call = |name: &str, args: Value| {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))
            .call(args.to_string())
    };
    call(
        CONFIG_TOOL_NAME,
        json!({"argv": [
            "tools", "edit", "--set",
            format!("host={}", json!({
                "root": allowed_root.path().to_string_lossy(),
                "files": {"mode": "ReadOnly"}
            }))
        ]}),
    )
    .await
    .expect("current agent receives effective read authority");

    let outside_of_ceiling = call(
        RUN_GRAPH_TOOL_NAME,
        json!({
            "package": "review_graph",
            "input": {
                "repository": outside.path().to_string_lossy(),
                "base": "HEAD",
                "head": "HEAD",
            },
        }),
    )
    .await
    .expect_err("a repository outside the effective root is refused");
    assert!(
        outside_of_ceiling
            .to_string()
            .contains("escapes operator tool root"),
        "{outside_of_ceiling:#}"
    );
}

/// A model-invoked `run_graph` of an entry whose `prepare` declares a
/// `git_diff` host step, end to end under a granted effective root: the
/// repository named by the input resolves inside that root, the pack's
/// prepare plugin runs on the collected facts, its evidence document is
/// persisted, and the run starts on the plugin's input rather than the
/// operator's.
#[tokio::test]
async fn run_graph_prepares_host_input_under_the_effective_root() {
    let node = build_agent_node().await;
    let identity = agent_identity("prepare-under-root");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;

    let options = crate::graph_package::GraphPackageInstallBindings {
        node_did: node_did.clone(),
        inference_slots: BTreeMap::from([("worker".to_owned(), "setup:inference".to_owned())]),
    };
    let plugin_output = json!({
        "input": {
            "repository_path": ".",
            "head_ref": "prepared-head",
            "evidence_id": "evidence-1",
            "summary": "prepared by the pack plugin",
        },
        "documents": [{
            "collection": "FixtureEvidence",
            "fields": {"evidence_id": "evidence-1", "head_ref": "prepared-head", "note": "prepared"},
        }],
    });
    let package = crate::test_support::load_test_graph_package_with_plugin_output(
        "prepared_graph",
        &options,
        &plugin_output,
    );
    let prepare = package.config.graph_intents[0].entries[0]
        .prepare
        .clone()
        .expect("the fixture entry prepares");
    let plugin_home = tempfile::tempdir().expect("plugin home");
    let declaration = package.manifest.metadata.plugins[0].clone();
    let afb = package
        .asset(&declaration.artifact)
        .expect("the plugin artifact")
        .to_vec();
    let digest = prepare.digest.clone().expect("the loader pins the plugin");
    let hex = digest.strip_prefix("sha256:").expect("sha256 pin");
    crate::plugin::store::store_bytes(plugin_home.path(), hex, &afb).expect("store artifact");
    let granted =
        crate::plugin::store::grant_on_install(plugin_home.path(), "fixture", &declaration, true)
            .expect("operator consents to fixture limits");
    crate::plugin::store::write_record(
        plugin_home.path(),
        &crate::plugin::store::InstalledPlugin {
            namespace: "fixture".into(),
            name: declaration.name.clone(),
            version: "0.1.0".into(),
            digest,
            language: "rust".into(),
            declaration,
            granted,
            instructions: None,
            owner_pack_coordinate: None,
            owner_pack_digest: None,
            model_binding: None,
        },
    )
    .expect("record the plugin");
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        plugin_home.path().to_owned(),
    )));

    let access = graph_access(&node);
    let receipt = crate::graph_package::install_loaded_graph_package(
        &access,
        &node_did,
        &package,
        &options,
        None,
        &crate::graph_package::GraphInstallRecord::default(),
    )
    .await
    .expect("fixture installs");
    crate::graph_pipeline::activate_graph_revision_with_access(
        &access,
        &node_did,
        &receipt.graph_id,
        &receipt.revision_digest,
        None,
    )
    .await
    .expect("fixture revision activates");

    let root = tempfile::tempdir().expect("root");
    let root_path = std::fs::canonicalize(root.path()).expect("canonical root");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&root_path)
            .args(["-c", "user.email=test@example.com", "-c", "user.name=Test"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {output:?}");
    };
    git(&["init", "--quiet"]);
    std::fs::write(root_path.join("a.txt"), "one\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "base"]);
    std::fs::write(root_path.join("a.txt"), "two\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "head"]);

    let mut tool_config = config(&["tools"]);
    tool_config.agent_id = "setup".to_owned();
    tool_config.enable_graph_tools = true;
    tool_config.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(root_path.clone()),
    };
    let tools = build_self_config_tools(
        node.clone(),
        node_did,
        Some(identity),
        &tool_config,
        plugins,
    );
    let call = |name: &str, args: Value| {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))
            .call(args.to_string())
    };
    call(
        CONFIG_TOOL_NAME,
        json!({"argv": [
            "tools", "edit", "--set",
            format!("host={}", json!({
                "root": root_path.to_string_lossy(),
                "files": {"mode": "ReadOnly"}
            }))
        ]}),
    )
    .await
    .expect("current agent receives effective read authority");

    let started = call(
        RUN_GRAPH_TOOL_NAME,
        json!({
            "package": "prepared_graph",
            "input": {
                "repository": root_path.to_string_lossy(),
                "base": "HEAD~1",
                "head": "HEAD",
            },
        }),
    )
    .await
    .expect("a repository under the effective root prepares and starts");
    let started: Value = serde_json::from_str(&started).expect("run_graph returns JSON");
    assert_eq!(started["node_bound"], true, "{started}");

    let evidence = access
        .execute("{ FixtureEvidence { evidence_id head_ref note } }")
        .await
        .expect("query evidence");
    assert_eq!(
        evidence["data"]["FixtureEvidence"],
        json!([{"evidence_id": "evidence-1", "head_ref": "prepared-head", "note": "prepared"}]),
        "the plugin's evidence document is persisted"
    );
    let jobs = access
        .execute("{ FixtureJob { summary head_ref evidence_id } }")
        .await
        .expect("query job");
    assert_eq!(
        jobs["data"]["FixtureJob"],
        json!([{
            "summary": "prepared by the pack plugin",
            "head_ref": "prepared-head",
            "evidence_id": "evidence-1",
        }]),
        "the run starts on the plugin's input, not the operator's"
    );
}

#[tokio::test]
async fn config_tools_cannot_self_grant_pack_install() {
    let node = build_agent_node().await;
    let identity = agent_identity("pack-self-grant");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;

    let mut tool_config = config(&["tools"]);
    tool_config.agent_id = "setup".to_string();
    let tools =
        build_self_config_tools(node, node_did, Some(identity), &tool_config, test_plugins());
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

#[test]
fn operator_grants_project_from_the_tools_group() {
    let project = |tools: Value| OperatorGrants::from_tools_json(tools.as_object().unwrap());
    assert_eq!(project(json!({})).unwrap(), OperatorGrants::default());
    assert_eq!(
        project(json!({"self_config": null})).unwrap(),
        OperatorGrants::default()
    );
    assert!(
        project(json!({"self_config": {"enable_self_config": true, "enable_pack_install": true}}))
            .unwrap()
            .pack_install
    );
    assert!(
        project(json!({"self_config": {"unknown": 1}})).is_err(),
        "a group that does not decode fails closed"
    );
}

/// Replace a Tools document through the operator route (no self-config
/// guard), as the desktop and `config apply` do.
async fn operator_replace_tools(node: &defra_node::EmbeddedNode, tools: Value) {
    use crate::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: crate::Collection::Tools,
        add: tools.clone(),
        update: tools,
    }])
    .unwrap();
    ConfigAccess::transact_local(node, None, "test.operator_tools", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

/// The operator grants pack installation on `tools_id`.
async fn operator_grant_pack_install(node: &defra_node::EmbeddedNode, owner: &str, tools_id: &str) {
    operator_replace_tools(
        node,
        json!({"node_did": owner, "tools_id": tools_id,
               "self_config": {"enable_self_config": true, "enable_pack_install": true}}),
    )
    .await;
}

#[tokio::test]
async fn config_tools_unrelated_edit_on_granted_tools_is_accepted() {
    let node = build_agent_node().await;
    let identity = agent_identity("granted-tools-edit");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "granted").await;
    operator_grant_pack_install(&node, &owner, "granted:tools").await;
    let mut tool_config = config(&["tools"]);
    tool_config.agent_id = "granted".to_string();
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins());

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
async fn built_ins_enable_graph_tools_remains_self_grantable() {
    let node = build_agent_node().await;
    let identity = agent_identity("graph-tools-self-grant");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    let mut tool_config = config(&["tools"]);
    tool_config.agent_id = "setup".to_string();
    let tools =
        build_self_config_tools(node, node_did, Some(identity), &tool_config, test_plugins());
    let applied = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--set".into(),
            r#"built_ins={"enable_graph_tools":true}"#.into(),
        ],
    )
    .await
    .expect("presenting graph run tools is not an operator-managed grant");
    assert_eq!(
        serde_json::from_str::<Value>(&applied).unwrap()["committed"],
        true
    );
}

#[tokio::test]
async fn config_tools_holder_may_grant_pack_install_to_a_sibling() {
    let node = build_agent_node().await;
    let identity = agent_identity("pack-holder-sibling");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "setup").await;
    crate::test_support::install_test_agent(&node, &node_did, "sibling").await;
    let mut tool_config = config(&["node", "tools"]);
    tool_config.agent_id = "setup".to_string();
    tool_config.enable_pack_install = true;
    let tools =
        build_self_config_tools(node, node_did, Some(identity), &tool_config, test_plugins());
    let applied = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            "sibling".into(),
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

/// The `error` message of a refused `config` call, read from its JSON
/// failure envelope, so quoted ids compare unescaped.
fn config_error_message(error: crate::llm::tool::ToolError) -> String {
    let crate::llm::tool::ToolError::ToolCallError(error) = error else {
        panic!("missing typed config error: {error}");
    };
    let envelope: Value =
        serde_json::from_str(&error.to_string()).expect("a config refusal is a JSON envelope");
    envelope["error"]
        .as_str()
        .expect("the envelope carries an error message")
        .to_owned()
}

/// `call_config_tool`, with a refusal reduced to its envelope's message.
async fn config_call_message(
    tools: &[Box<dyn crate::llm::tool::ToolDyn>],
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

/// `worker` (the invoker) and `sibling` with plain chains, and two chains
/// whose Tools the operator granted pack installation; the config tool runs
/// as `worker` with the agent catalog grant, holding the grant or not.
async fn reselection_tools(
    label: &str,
    hold_pack_install: bool,
) -> Vec<Box<dyn crate::llm::tool::ToolDyn>> {
    let node = build_agent_node().await;
    let identity = agent_identity(label);
    let owner = identity.did().to_string();
    for agent in ["worker", "sibling", "granted", "granted-2"] {
        crate::test_support::install_test_agent(&node, &owner, agent).await;
    }
    operator_grant_pack_install(&node, &owner, "granted:tools").await;
    operator_grant_pack_install(&node, &owner, "granted-2:tools").await;
    let mut tool_config = config(&["node", "agent", "tools"]);
    tool_config.agent_id = "worker".to_owned();
    tool_config.enable_pack_install = hold_pack_install;
    build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins())
}

#[tokio::test]
async fn context_reselect_cannot_acquire_grants() {
    let tools = reselection_tools("context-reselect", false).await;
    for target in [None, Some("sibling")] {
        let mut argv = argv_of(&["agent", "context", "edit"]);
        if let Some(target) = target {
            argv.extend(argv_of(&["--agent", target]));
        }
        argv.extend(argv_of(&["--set", r#"tools_id="granted:tools""#]));
        let refused = config_call_message(&tools, argv)
            .await
            .expect_err("re-pointing a Context at granted Tools must not acquire the grant");
        assert!(
            refused.contains(
                "tools_id \"granted:tools\" selects Tools carrying an operator grant this agent does not hold"
            ),
            "{target:?}: {refused}"
        );
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
        "the reference validator, not the grant guard, refuses a missing document: {missing}"
    );
    let holder = reselection_tools("context-reselect-holder", true).await;
    call_config_tool(
        &holder,
        argv_of(&[
            "agent",
            "context",
            "edit",
            "--set",
            r#"tools_id="granted:tools""#,
        ]),
    )
    .await
    .expect("an agent holding the grant may select Tools that carry it");
}

#[tokio::test]
async fn agent_reselect_cannot_acquire_grants() {
    let tools = reselection_tools("agent-reselect", false).await;
    for target in ["worker", "sibling"] {
        let refused = config_call_message(
            &tools,
            argv_of(&[
                "agent",
                "edit",
                target,
                "--set",
                r#"context_id="granted:context""#,
            ]),
        )
        .await
        .expect_err("re-pointing a Agent at a granted chain must not acquire the grant");
        assert!(
            refused.contains(
                "context_id \"granted:context\" selects Tools carrying an operator grant this agent does not hold"
            ),
            "{target}: {refused}"
        );
    }
    let holder = reselection_tools("agent-reselect-holder", true).await;
    call_config_tool(
        &holder,
        argv_of(&[
            "agent",
            "edit",
            "sibling",
            "--set",
            r#"context_id="granted:context""#,
        ]),
    )
    .await
    .expect("an agent holding the grant may select a chain that carries it");
}

/// A non-holder may move between two Tools documents that both carry the
/// grant: nothing is raised above the previous selection.
#[tokio::test]
async fn context_reselect_between_granted_tools_is_accepted() {
    let tools = reselection_tools("context-between-granted", false).await;
    call_config_tool(
        &tools,
        argv_of(&[
            "agent",
            "context",
            "edit",
            "--agent",
            "granted",
            "--set",
            r#"tools_id="granted-2:tools""#,
        ]),
    )
    .await
    .expect("re-pointing between granted Tools raises nothing");
}

#[tokio::test]
async fn context_create_selecting_granted_tools_requires_the_grant() {
    let create = argv_of(&[
        "context",
        "create",
        "fresh:context",
        "--set",
        r#"tools_id="granted:tools""#,
    ]);
    let tools = reselection_tools("context-create-granted", false).await;
    let refused = config_call_message(&tools, create.clone())
        .await
        .expect_err("a new Context must not select Tools whose grant the agent does not hold");
    assert!(
        refused.contains("selects Tools carrying an operator grant this agent does not hold"),
        "{refused}"
    );
    let holder = reselection_tools("context-create-holder", true).await;
    call_config_tool(&holder, create)
        .await
        .expect("an agent holding the grant may create a Context that selects it");
}

// -- agent commands (#Task 5) --

#[test]
fn agent_edit_arguments_distinguish_omission_clear_and_set() {
    let args: ConfigureAgentParams = serde_json::from_value(json!({
        "action": "edit",
        "agent_id": "review",
        "display_name": "Review reconnaissance",
        "root": null
    }))
    .expect("valid sparse edit arguments");
    assert_eq!(
        agent_edit_fields(&args),
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

pub(super) async fn build_agent_node() -> std::sync::Arc<defra_node::EmbeddedNode> {
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

pub(super) fn agent_identity(label: &str) -> std::sync::Arc<dyn crate::NodeIdentity> {
    let tempdir = tempfile::tempdir().expect("identity tempdir");
    std::sync::Arc::new(
        crate::KeyIdentity::load_or_create(&tempdir.path().join(format!("{label}.key")), None)
            .expect("test identity"),
    )
}

#[tokio::test]
async fn automation_rejects_invalid_template_before_publication_and_can_recover() {
    let node = build_agent_node().await;
    let identity = agent_identity("template-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    let mut grants = config(&["automation"]);
    grants.preview = true;
    let tools =
        build_self_config_tools(node.clone(), owner, Some(identity), &grants, test_plugins());
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
async fn automation_rejects_a_count_field_the_runtime_cannot_read_and_can_recover() {
    let node = build_agent_node().await;
    let identity = agent_identity("event-source-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    node.add_schema("type SelfConfigProbe { batch: String flag: Boolean total: Int }")
        .await
        .unwrap();
    let mut grants = config(&["automation"]);
    grants.preview = true;
    let tools =
        build_self_config_tools(node.clone(), owner, Some(identity), &grants, test_plugins());
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
        let rows = node.execute("{ EventSource { event_source_id } }").await;
        assert!(!rows.has_errors());
        assert!(
            rows.data.unwrap()["EventSource"]
                .as_array()
                .unwrap()
                .is_empty(),
            "a refused event source publishes no row"
        );
    }
    tool.call(set("total")).await.unwrap();
    let rows = node
        .execute("{ EventSource { event_source_id group } }")
        .await;
    assert!(!rows.has_errors());
    let rows = rows.data.unwrap()["EventSource"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["group"]["expected_count"]["source_field"], "total");
}

#[tokio::test]
async fn skill_import_previews_without_writes_and_requires_file_authority() {
    let node = build_agent_node().await;
    let identity = agent_identity("skill-import");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("SKILL.md");
    std::fs::write(
        &file,
        "---\nname: Review\ndescription: Review code\n---\nCheck the diff carefully.",
    )
    .unwrap();
    let mut grants = config(&["tools", "agent"]);
    grants.preview = true;
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let denied_tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity.clone()),
        &grants,
        test_plugins(),
    );
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
    ))
    .await
    .unwrap();
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &grants,
        test_plugins(),
    );
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
    let node = build_agent_node().await;
    let identity = agent_identity("configuration-discovery");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("synthetic-codex");
    std::fs::create_dir_all(&source).unwrap();
    let marker = root.path().join("SHOULD_NEVER_RUN");
    let fixture =
        include_str!("../../tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &marker.to_string_lossy());
    std::fs::write(source.join("config.toml"), fixture).unwrap();

    let mut grants = config(&["tools"]);
    let denied_tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity.clone()),
        &grants,
        test_plugins(),
    );
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
    ))
    .await
    .unwrap();
    let before = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    let tools = build_self_config_tools(node, owner, Some(identity), &grants, test_plugins());
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

    let node = build_agent_node().await;
    let identity = agent_identity("setup-discovery-flow");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;

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

    let mut grants = config(&["tools", "agent", "backend"]);
    grants.preview = true;
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
    ))
    .await
    .unwrap();
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &grants,
        test_plugins(),
    );
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
        command(&["agent", "context", "get", "--agent", "beh-test"]),
    )
    .await
    .unwrap();
    let backend_before =
        call_config_tool(&tools, command(&["backend", "get", "--agent", "beh-test"]))
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
                "agent".into(),
                "context".into(),
                "preview".into(),
                "--agent".into(),
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
            command(&["agent", "context", "get", "--agent", "beh-test"]),
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
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
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
            command(&["agent", "context", "get", "--agent", "beh-test"]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["document"]["system_prompt"], approved_prompt);
    assert_eq!(
        call_config_tool(&tools, command(&["backend", "get", "--agent", "beh-test"]),)
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
    let node = build_agent_node().await;
    let identity = agent_identity("datastore-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    let mut grants = config(&["tools"]);
    grants.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &grants,
        test_plugins(),
    );
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
    // A command's --help narrows to that command's syntax and field shapes.
    let create_help = call_config_tool(&tools, command(&["datastore", "preview", "create", "-h"]))
        .await
        .unwrap();
    assert!(
        create_help.starts_with("datastore:") && create_help.contains("create ID | update ID"),
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
    ))
    .await
    .unwrap();
    core.apply(agent_request(
        &core,
        vec![(
            "tags".into(),
            Some(json!([crate::self_config::ENGINEER_AGENT_TAG])),
        )],
    ))
    .await
    .unwrap();
    // The Engineer may edit surfaces its own Tools select (#1796).
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
    let node = build_agent_node().await;
    let identity = agent_identity("structured-config");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "working").await;
    let mut settings = config(&["agent"]);
    settings.agent_id = "working".into();
    settings.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner,
        Some(identity),
        &settings,
        test_plugins(),
    );
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
    node.shutdown().await;
}

pub(super) async fn call_config_tool(
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

/// Errors a model hit in the configurator skill-workflow eval name the call
/// that can proceed, not only what failed.
#[tokio::test]
async fn outcome_schema_error_returns_an_executable_schema_recovery() {
    use crate::llm::tool::Tool;
    let node = build_agent_node().await;
    let identity = agent_identity("schema-recovery");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    access
        .add_schema("type Delivery { body: String }")
        .await
        .unwrap();
    let tools = build_self_config_tools(
        node.clone(),
        owner,
        Some(identity),
        &config(&["automation"]),
        test_plugins(),
    );
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
    let crate::llm::tool::ToolError::ToolCallError(error) = error else {
        panic!("{error}")
    };
    let failure: Value = serde_json::from_str(&error.to_string()).unwrap();
    assert_eq!(failure["recovery"]["tool"], "schema");
    assert_eq!(
        failure["recovery"]["next_call"]["argv"],
        json!(["collection", "preview", "update", "Delivery"])
    );
    let schema = crate::schema_tool::SchemaTool::new(node.clone());
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
    let rows = access.execute("{ Trigger { trigger_id } }").await.unwrap();
    assert_eq!(rows["data"]["Trigger"].as_array().unwrap().len(), 1);
    node.shutdown().await;
}

#[tokio::test]
async fn config_errors_name_the_next_call() {
    let node = build_agent_node().await;
    let identity = agent_identity("next-call");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    crate::test_support::install_test_agent(&node, &owner, &format!("{owner}:builder")).await;
    let mut grants = config(&["node", "tools"]);
    grants.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &grants,
        test_plugins(),
    );
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let failure = |args: Value| async move {
        let error = tool.call(args.to_string()).await.unwrap_err();
        let crate::llm::tool::ToolError::ToolCallError(error) = error else {
            panic!("missing typed config error: {error}");
        };
        serde_json::from_str::<Value>(&error.to_string()).unwrap()
    };

    // A context preview cannot attach a skill that is only proposed.
    let skill = failure(json!({"argv":["agent","context","preview"],"options":{"agent":"builder"},"set":{"skill_ids":["eval-coding-check"]}})).await;
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

    let tools_ref = failure(json!({"argv":["agent","context","preview"],"options":{"agent":"builder"},"set":{"tools_id":"proposed-tools"}})).await;
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

    // preview edit is preview; a stray positional gets the whole correct call.
    let aliased = tool
        .call(json!({"argv":["agent","context","preview","edit"],"options":{"agent":"builder"},"set":{"skill_ids":[]}}).to_string())
        .await
        .unwrap();
    assert!(aliased.contains("\"committed\": false"), "{aliased}");
    let stray =
        failure(json!({"argv":["agent","context","edit","builder"],"set":{"skill_ids":[]}})).await;
    let message = stray["error"].as_str().unwrap();
    assert!(
        message.contains(r#"agent context edit takes no positional argument "builder""#)
            && message.contains(r#"{"argv":["agent","context","edit"],"options":{"agent":"AGENT_ID"},"set":{"FIELD":VALUE}}"#),
        "{message}"
    );
    assert_eq!(stray["config_execution"]["mutation_entered"], false);

    // A display name or differently cased slug suggests the slug.
    let named = failure(json!({"argv":["agent","get","Builder"]})).await;
    assert_eq!(
        named["error"],
        "unknown agent_id \"Builder\"; did you mean \"builder\"?"
    );
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "beh-test".into()).unwrap();
    core.apply(agent_request(
        &core,
        vec![("display_name".into(), Some(json!("Night Shift")))],
    ))
    .await
    .unwrap();
    let display = failure(json!({"argv":["agent","get","night shift"]})).await;
    assert_eq!(
        display["error"],
        "unknown agent_id \"night shift\"; did you mean \"beh-test\"?"
    );
    let unknown = failure(json!({"argv":["agent","get","nobody"]})).await;
    assert!(
        unknown["error"]
            .as_str()
            .unwrap()
            .contains(r#"copy an exact ID from ["agent","list"]"#),
        "{unknown}"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn config_execution_receipts_separate_rejected_syntax_from_write_dispatch() {
    let node = build_agent_node().await;
    let identity = agent_identity("config-execution");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    let mut grants = config(&[
        "node",
        "tools",
        "automation",
        "profile",
        "backend",
        "mcp_service",
    ]);
    grants.preview = true;
    let tools =
        build_self_config_tools(node.clone(), owner, Some(identity), &grants, test_plugins());
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
            .call(json!({"argv":["datastore","get"],"target_id":"notifications"}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read["config_execution"]["mutation_entered"], false);
    node.shutdown().await;
}

#[tokio::test]
async fn agent_category_gates_the_tool() {
    let node = build_agent_node().await;
    let identity = agent_identity("agent-gate");
    let node_did = identity.did().to_string();

    let without_agent = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        None,
        &config(&["tools"]),
        test_plugins(),
    );
    let error = call_config_tool(&without_agent, vec!["agent".into(), "list".into()])
        .await
        .expect_err("catalog read requires agent grant");
    assert!(error.contains("not granted"), "{error}");

    let with_agent = build_self_config_tools(
        node,
        node_did,
        Some(identity),
        &config(&["node"]),
        test_plugins(),
    );
    assert!(with_agent
        .iter()
        .any(|tool| tool.name() == CONFIG_TOOL_NAME));
}

#[tokio::test]
async fn agent_unknown_action_errors_cleanly() {
    let node = build_agent_node().await;
    let identity = agent_identity("agent-unknown");
    let tools = build_self_config_tools(
        node,
        identity.did().to_string(),
        Some(identity),
        &config(&["node"]),
        test_plugins(),
    );

    let error = call_config_tool(&tools, vec!["agent".into(), "unknown-action".into()])
        .await
        .expect_err("unknown action must error");
    assert!(
        error.contains("unknown agent verb"),
        "error should name the bad action: {error}"
    );
}

#[tokio::test]
async fn agent_only_grant_cannot_acquire_node_authority_and_writes_require_exact_signer() {
    let node = build_agent_node().await;
    let identity = agent_identity("agent-only-owner");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "current").await;
    let mut tool_config = config(&["agent"]);
    tool_config.agent_id = "current".into();
    tool_config.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &tool_config,
        test_plugins(),
    );
    for argv in [
        vec!["agent", "list"],
        vec!["agent", "preview", "default", "current"],
        vec!["agent", "default", "current"],
        vec![
            "agent",
            "create",
            "--display-name",
            "Other",
            "--system-prompt",
            "Other instructions",
            "--profile",
            "current:inference",
        ],
    ] {
        let error = call_config_tool(&tools, argv.into_iter().map(str::to_owned).collect())
            .await
            .expect_err("agent-only authority cannot manage the node catalog");
        assert!(error.contains("category node"), "{error}");
    }
    let edited = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "update".into(),
            "current".into(),
            "--set".into(),
            r#"display_name="Renamed""#.into(),
        ],
    )
    .await
    .expect("agent authority can edit its own document");
    let edited: Value = serde_json::from_str(&edited).unwrap();
    assert_eq!(edited["committed"], true);
    let foreign_identity = agent_identity("foreign-signer");
    let params = agent_params(
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
    let node = build_agent_node().await;
    let identity = agent_identity("config-inventory");
    let owner = identity.did().to_string();
    for agent in ["alpha", "beta", "gamma"] {
        crate::test_support::install_test_agent(&node, &owner, agent).await;
    }
    let mut tool_config = config(&["node", "profile", "backend"]);
    tool_config.agent_id = "alpha".into();
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins());
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

    let agent_id = first["agents"][0]["agent_id"].as_str().unwrap();
    let inspected: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["agent", "get", agent_id]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inspected["agent_id"], agent_id);

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

    let node = build_agent_node().await;
    let identity = agent_identity("local-backend-create");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "setup").await;
    let mut tool_config = config(&["backend", "profile"]);
    tool_config.agent_id = "setup".into();
    tool_config.preview = true;
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins());
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
    let node = build_agent_node().await;
    let identity = agent_identity("targeted-config");
    let owner = identity.did().to_string();
    for agent in ["setup", "working"] {
        crate::test_support::install_test_agent(&node, &owner, agent).await;
    }
    let setup = SelfConfigCore::new(node.clone(), owner.clone(), "setup".into()).unwrap();
    setup
        .apply(agent_request(
            &setup,
            vec![(
                "tags".into(),
                Some(json!([crate::self_config::ENGINEER_AGENT_TAG])),
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
    tool_config.agent_id = "setup".into();
    tool_config.preview = true;
    tool_config.no_lockout = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &tool_config,
        test_plugins(),
    );

    call_config_tool(
        &tools,
        vec![
            "agent".into(),
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
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
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
            "--agent=working".into(),
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
            vec!["get".into(), "--agent".into(), "working".into()],
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
    let working_core = SelfConfigCore::new(node.clone(), owner.clone(), "working".into()).unwrap();
    working_core
        .apply(agent_request(
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
            "--agent".into(),
            "working".into(),
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
                Some(json!("working:inference")),
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
    let shapes = call_config_tool(&tools, vec!["tools".into(), "edit".into(), "--help".into()])
        .await
        .unwrap();
    assert!(shapes.contains("\"agents\":{\"enabled\""), "{shapes}");
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
                "agent".into(),
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
    assert_eq!(default_preview["make_default"], true);

    // The Engineer edits its own Tools; only a lockout is refused (#1796).
    call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            "setup".into(),
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

    // Lean siblingToolsAllowed: both reference observations happen in the
    // same patch transaction. Sharing either the Context or Tools with Engineer
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
            "--agent".into(),
            "working".into(),
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
            vec![("tools_id".into(), Some(json!("working:tools")))],
        ))
        .await
        .unwrap();
    working_core
        .apply(agent_request(
            &working_core,
            vec![("context_id".into(), Some(json!("setup:context")))],
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
            "working".into(),
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
    let node = build_agent_node().await;
    let identity = agent_identity("config-cleanup");
    let owner = identity.did().to_string();
    for agent in ["beh-test", "orphan"] {
        crate::test_support::install_test_agent(&node, &owner, agent).await;
    }
    let mut tool_config = config(&["node", "tools", "profile", "backend"]);
    tool_config.preview = true;
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins());

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
        "agent=orphan",
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
    call_config_tool(&tools, vec!["agent".into(), "get".into(), "orphan".into()])
        .await
        .expect("preview performs no writes");

    call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "edit".into(),
            "--agent".into(),
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
    let missing = call_config_tool(&tools, vec!["agent".into(), "get".into(), "orphan".into()])
        .await
        .expect_err("removed agent is no longer inspectable");
    assert!(
        missing.contains("missing")
            || missing.contains("no owned")
            || missing.contains("unknown agent_id"),
        "{missing}"
    );

    call_config_tool(
        &tools,
        vec!["get".into(), "--agent".into(), "beh-test".into()],
    )
    .await
    .expect("cleanup preserves unrelated configuration");
}

fn take_agent_tool(
    tools: Vec<Box<dyn crate::llm::tool::ToolDyn>>,
) -> Box<dyn crate::llm::tool::ToolDyn> {
    tools
        .into_iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
}

#[tokio::test]
async fn agent_default_commits_through_the_agent_owner() {
    let node = build_agent_node().await;
    let identity = agent_identity("agent-default");
    let node_did = identity.did().to_string();
    for agent in ["seed", "working"] {
        crate::test_support::install_test_agent(&node, &node_did, agent).await;
    }
    crate::document_config::upsert_node(&node, &node_did, None, Some("seed"), true)
        .await
        .unwrap();
    let mut tool_config = config(&["node"]);
    tool_config.agent_id = "seed".into();
    tool_config.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity.clone()),
        &tool_config,
        test_plugins(),
    );
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
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
    assert_eq!(
        node_default_agent(&node, &node_did)
            .await
            .unwrap()
            .as_deref(),
        Some("seed")
    );
    let output: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["agent".into(), "default".into(), "working".into()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(output["committed"], true);
    assert_eq!(output["agent_id"], "working");
    assert_eq!(
        node_default_agent(&node, &node_did)
            .await
            .unwrap()
            .as_deref(),
        Some("working")
    );
}

#[tokio::test]
async fn agent_create_commits_and_resolves_after_restart() {
    let node = build_agent_node().await;
    let identity = agent_identity("agent-create");
    let node_did = identity.did().to_string();

    crate::test_support::install_test_agent(&node, &node_did, "seed").await;
    let profile_id = "seed:inference";

    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity.clone()),
        &config(&["node"]),
        test_plugins(),
    );
    let rejected_preview = call_config_tool(
        &tools,
        vec![
            "agent".into(),
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
        .contains("fresh agents require system_prompt"));
    let admitted_preview = call_config_tool(
        &tools,
        vec![
            "agent".into(),
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
    assert_eq!(admitted_preview["operation"], "create");
    assert_eq!(admitted_preview["make_default"], true);
    assert_eq!(
        admitted_preview["agent"]["display_name"],
        "Research Assistant"
    );
    assert_eq!(
        admitted_preview["agent"]["inference_profile_id"],
        profile_id
    );
    assert_eq!(
        admitted_preview["agent"]["context_id"],
        format!("{node_did}:research-assistant:context")
    );
    assert_eq!(crate::list_agents(&node, &node_did).await.unwrap().len(), 1);
    let tool = take_agent_tool(tools);

    let args = json!({"argv": [
        "agent", "create",
        "--display-name", "Research Assistant",
        "--description", "Researches a focused question",
        "--system-prompt", "Research the question and cite evidence.",
        "--preset", "write",
        "--profile", profile_id,
        "--default"
    ]})
    .to_string();

    let output = tool.call(args).await.expect("direct agent create commits");
    let agents = crate::list_agents(&node, &node_did)
        .await
        .expect("list agents");
    let created: Vec<_> = agents
        .iter()
        .filter(|agent| agent.agent_id != "seed")
        .collect();
    assert_eq!(created.len(), 1, "exactly one new agent materialized");
    assert_eq!(
        created[0].display_name,
        Some("Research Assistant".to_string())
    );
    let context_id = crate::graphql::escape_graphql_string(
        created[0]
            .context_id
            .as_deref()
            .expect("created agent has context"),
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

    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["committed"], true);
    assert_eq!(output["agent_id"], created[0].agent_id);
    let effective: Value = serde_json::from_str(
        &agent_inspect(&node, &node_did, &created[0].agent_id, &Default::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        effective["effective_config"]["context"]["system_prompt"],
        "Research the question and cite evidence."
    );
    assert_eq!(
        effective["effective_config"]["tool_grants"]["configured"],
        json!({"lsp": false, "native_graph_tools": false, "network_mode": "inherit"})
    );
    let grant_tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity.clone()),
        &config(&["node", "tools"]),
        test_plugins(),
    );
    let selected = call_config_tool(
        &grant_tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            created[0].agent_id.clone(),
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
        vec!["get".into(), "--agent".into(), created[0].agent_id.clone()],
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
        &crate::agent::DocumentResolveContext::for_tests(
            identity,
            crate::tool_surface::ToolCeiling::readonly(),
            Default::default(),
        ),
    )
    .await
    .expect("restart-style runtime resolution accepts the materialized agent");
    assert_eq!(snapshot.default_agent_id, created[0].agent_id);
    let runtime_agent = snapshot
        .agents
        .get(&created[0].agent_id)
        .unwrap_or_else(|| {
            panic!(
                "new default is runnable after re-resolution: {:?}",
                snapshot.unavailable_agents
            )
        });
    assert_eq!(
        runtime_agent.system_prompt,
        "Research the question and cite evidence."
    );
    let names = runtime_agent
        .tools
        .resolve(node.as_ref(), &node_did, &Default::default())
        .await
        .expect("new agent tool surface resolves after restart")
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
async fn agent_clone_accepts_sibling_agent_id() {
    let node = build_agent_node().await;
    let identity = agent_identity("agent-clone");
    let node_did = identity.did().to_string();
    let qualified_sibling_id = "sibling-agent".to_owned();
    crate::test_support::install_test_agent(&node, &node_did, &qualified_sibling_id).await;
    let profile_id = "sibling-agent:inference";

    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity.clone()),
        &config(&["node"]),
        test_plugins(),
    );
    let tool = take_agent_tool(tools);
    let args = json!({"argv": [
        "agent", "clone",
        "--display-name", "Cloned Agent",
        "--from", "sibling-agent",
        "--profile", profile_id
    ]})
    .to_string();
    let output: Value =
        serde_json::from_str(&tool.call(args).await.expect("clone commits")).unwrap();
    assert_eq!(output["committed"], true);
    let source = crate::list_agents(&node, &node_did)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.agent_id == qualified_sibling_id)
        .unwrap();
    let cloned = crate::list_agents(&node, &node_did)
        .await
        .unwrap()
        .into_iter()
        .find(|a| Some(a.agent_id.as_str()) == output["agent_id"].as_str())
        .unwrap();
    assert_eq!(source.inference_profile_id, cloned.inference_profile_id);
    assert_ne!(source.context_id, cloned.context_id);
}

#[tokio::test]
async fn agent_clone_cannot_copy_unheld_grants() {
    let node = build_agent_node().await;
    let identity = agent_identity("persona-clone-grants");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "granted").await;
    operator_grant_pack_install(&node, &node_did, "granted:tools").await;
    let mut tool_config = config(&["node"]);
    tool_config.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity.clone()),
        &tool_config,
        test_plugins(),
    );
    let tool = take_agent_tool(tools);
    for argv in [
        json!([
            "agent",
            "clone",
            "--display-name",
            "Granted Copy",
            "--from",
            "granted",
            "--profile",
            "granted:inference"
        ]),
        json!([
            "agent",
            "preview",
            "clone",
            "--display-name",
            "Granted Copy",
            "--from",
            "granted",
            "--profile",
            "granted:inference"
        ]),
    ] {
        let refused = if argv[1] == "preview" {
            let output: Value =
                serde_json::from_str(&tool.call(json!({"argv": argv}).to_string()).await.unwrap())
                    .unwrap();
            assert_eq!(output["admitted"], false);
            assert_eq!(output["committed"], false);
            output["rejection"].as_str().unwrap().to_owned()
        } else {
            config_error_message(
                tool.call(json!({"argv": argv}).to_string())
                    .await
                    .expect_err("a clone must not copy grants the invoking agent does not hold"),
            )
        };
        assert!(
            refused.contains(
                "clone source \"granted\" carries an operator grant this agent does not hold"
            ),
            "{argv}: {refused}"
        );
        assert!(
            refused.contains("cannot be self-granted"),
            "{argv}: {refused}"
        );
    }
    assert!(
        crate::list_agents(&node, &node_did)
            .await
            .unwrap()
            .iter()
            .all(|agent| agent.display_name.as_deref() != Some("Granted Copy")),
        "the refused clone must not publish an Agent"
    );
}

#[tokio::test]
async fn agent_clone_by_holder_copies_granted_source() {
    let node = build_agent_node().await;
    let identity = agent_identity("persona-clone-holder");
    let node_did = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &node_did, "granted").await;
    operator_grant_pack_install(&node, &node_did, "granted:tools").await;
    let mut tool_config = config(&["node"]);
    tool_config.enable_pack_install = true;
    tool_config.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        node_did.clone(),
        Some(identity),
        &tool_config,
        test_plugins(),
    );
    let tool = take_agent_tool(tools);
    tool.call(
        json!({"argv": ["agent", "preview", "clone", "--display-name", "Granted Copy",
                        "--from", "granted", "--profile", "granted:inference"]})
        .to_string(),
    )
    .await
    .expect("a holder may preview copying a granted source");
    let args = json!({"argv": ["agent", "clone", "--display-name", "Granted Copy",
                               "--from", "granted", "--profile", "granted:inference"]})
    .to_string();
    let output: Value =
        serde_json::from_str(&tool.call(args).await.expect("a holder's clone commits")).unwrap();
    assert_eq!(output["committed"], true);
    assert!(
        crate::list_agents(&node, &node_did)
            .await
            .unwrap()
            .iter()
            .any(|agent| Some(agent.agent_id.as_str()) == output["agent_id"].as_str()),
        "a holder's clone publishes its Agent"
    );
}

#[tokio::test]
async fn canonical_self_config_preview_and_apply_preserve_scope_and_reject_lockout() {
    let node = build_agent_node().await;
    let identity = agent_identity("canonical-self-config");
    let other = agent_identity("other-self-config");
    let owner = identity.did().to_string();
    let foreign = other.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "same").await;
    crate::test_support::install_test_agent(&node, &foreign, "same").await;
    crate::test_support::install_test_agent(&node, &owner, "unconfigured").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "same".into()).unwrap();
    let patch = vec![(
        "self_config".into(),
        Some(json!({"enable_self_config":true})),
    )];
    let preview = core
        .preview(tools_request(&core, patch.clone()))
        .await
        .unwrap();
    assert!(!preview.committed);
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(read["documents"]["Tools"]["self_config"].is_null());
    core.apply(tools_request(&core, patch)).await.unwrap();
    let guarded = core.clone().with_no_lockout(true);
    assert!(guarded
        .apply(tools_request(&guarded, vec![("self_config".into(), None)],))
        .await
        .is_err());
    assert!(guarded
        .preview(agent_request(
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
async fn direct_tools_preview_and_apply_enforce_and_persist_canonical_workspace_root() {
    let node = build_agent_node().await;
    let identity = agent_identity("tools-root-policy");
    let owner = identity.did().to_string();
    let agent_id = "root-policy";
    crate::test_support::install_test_agent(&node, &owner, agent_id).await;
    let ceiling = tempfile::tempdir().unwrap();
    let selected = ceiling.path().join("selected");
    let sibling = ceiling.path().join("sibling");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let selected_text = selected.to_string_lossy();
    let escaped = crate::graphql::escape_graphql_string(&selected_text);
    crate::config_client::ConfigAccess::write_local(
        &node,
        "test.self_config.workspace_root",
        &format!(
            r#"mutation {{ create_WorkspaceRoot(input: {{root_path:"{escaped}", enabled:true}}) {{_docID}} }}"#
        ),
    )
    .await
    .unwrap();
    let core = SelfConfigCore::new(node.clone(), owner.clone(), agent_id.into())
        .unwrap()
        .with_process_ceiling(crate::tool_surface::SelfConfigProcessCeiling {
            file_mode: crate::tool_surface::FileToolMode::ReadOnly,
            bash_mode: crate::tool_surface::BashMode::Off,
            root: Some(ceiling.path().to_path_buf()),
        });
    let patch = |root: &std::path::Path| {
        vec![(
            "host".into(),
            Some(json!({
                "root": root.to_string_lossy(),
                "files": {"mode": "ReadOnly"}
            })),
        )]
    };

    let authored_inside = selected.join("detour").join("..");
    let preview = core
        .preview(tools_request(&core, patch(&authored_inside)))
        .await
        .expect("preview admits a descendant and does not persist it");
    assert!(!preview.committed);
    assert!(core
        .preview(tools_request(&core, patch(&sibling)))
        .await
        .is_err());
    assert!(core
        .apply(tools_request(&core, patch(&sibling)))
        .await
        .is_err());

    core.apply(tools_request(&core, patch(&authored_inside)))
        .await
        .expect("apply admits the selected root");
    let tools_id = format!("{agent_id}:tools");
    let persisted: crate::document_config::Tools =
        crate::config_client::ConfigAccess::Local(node.clone())
            .transact("test.self_config.read_tools", |txn| {
                let owner = owner.clone();
                let tools_id = tools_id.clone();
                Box::pin(async move {
                    let value = crate::config_client::read_desired_state_document_in_txn(
                        txn,
                        crate::Collection::Tools,
                        &owner,
                        &tools_id,
                    )
                    .await?
                    .context("persisted Tools")?;
                    Ok(serde_json::from_value(value)?)
                })
            })
            .await
            .unwrap();
    assert_eq!(
        persisted
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(
            std::fs::canonicalize(&selected)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        )
    );
}

#[tokio::test]
async fn descendant_root_preview_apply_reconcile_reaches_fresh_request_file_tools() {
    let node = build_agent_node().await;
    let identity = agent_identity("descendant-root-self-config");
    let owner = identity.did().to_string();
    let seed_agent = "beh-test";
    crate::test_support::install_test_agent(&node, &owner, seed_agent).await;
    crate::upsert_node(&node, &owner, None, Some(seed_agent), true)
        .await
        .expect("bind default agent");

    let operator_root = tempfile::tempdir().expect("operator root");
    let selected_root = operator_root.path().join("projects").join("mandrake");
    std::fs::create_dir_all(&selected_root).expect("selected descendant root");
    std::fs::write(
        selected_root.join("marker.txt"),
        "fresh request sees descendant\n",
    )
    .expect("marker file");
    let sibling_root = operator_root.path().join("projects").join("other");
    std::fs::create_dir_all(&sibling_root).expect("sibling root");
    let sibling_marker = sibling_root.join("not-authorized.txt");
    std::fs::write(
        &sibling_marker,
        "operator ceiling must not widen selection\n",
    )
    .expect("sibling marker");
    let original_transcript = operator_root.path().join("original-transcript.txt");
    std::fs::write(&original_transcript, "private transcript fixture\n")
        .expect("original transcript fixture");

    let selected_text = selected_root.to_string_lossy().into_owned();
    let escaped_selected = crate::graphql::escape_graphql_string(&selected_text);
    let fixture_actor = ::identity::Did::new(owner.clone()).expect("fixture creator DID");
    let workspace_root_mutation = format!(
        r#"mutation {{ create_WorkspaceRoot(input: {{root_path:"{escaped_selected}", enabled:true}}) {{_docID}} }}"#
    );
    crate::config_client::ConfigAccess::transact_local(
        &node,
        Some(fixture_actor.clone()),
        "test.agent.workspace_root",
        |txn| {
            let mutation = &workspace_root_mutation;
            Box::pin(async move { txn.execute_local_response(mutation).await.map(|_| ()) })
        },
    )
    .await
    .expect("publish selected WorkspaceRoot");

    let mut tool_config = config(&["node"]);
    tool_config.agent_id = seed_agent.into();
    tool_config.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(operator_root.path().to_path_buf()),
    };
    let agent_tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity.clone()),
        &tool_config,
        test_plugins(),
    );
    let profile_id = format!("{seed_agent}:inference");
    let command = |preview: bool| {
        let mut argv = vec!["agent".to_string()];
        if preview {
            argv.push("preview".to_string());
        }
        argv.extend([
            "create".to_string(),
            "--display-name".to_string(),
            "Mandrake descendant".to_string(),
            "--system-prompt".to_string(),
            "Use only the selected workspace.".to_string(),
            "--root".to_string(),
            selected_text.clone(),
            "--preset".to_string(),
            "readonly".to_string(),
            "--profile".to_string(),
            profile_id.clone(),
            "--default".to_string(),
        ]);
        argv
    };
    let preview: Value = serde_json::from_str(
        &call_config_tool(&agent_tools, command(true))
            .await
            .expect("agent preview accepts the published descendant"),
    )
    .expect("preview json");
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["admitted"], true);
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap().len(), 1);
    let output = call_config_tool(&agent_tools, command(false))
        .await
        .expect("direct create commits");
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["committed"], true);
    let agents = crate::list_agents(&node, &owner)
        .await
        .expect("list materialized agents");
    let created = agents
        .iter()
        .find(|agent| agent.agent_id != seed_agent)
        .expect("agent reconciler materialized one agent");
    let runtime_view = crate::agent::document_view::load_document_runtime_view(&node, &owner)
        .await
        .expect("fresh runtime view after agent publication");
    let context_id = created.context_id.as_ref().expect("created context");
    let tools_id = runtime_view.contexts[context_id]
        .value
        .tools_id
        .as_ref()
        .expect("created tools");
    let canonical_selected =
        std::fs::canonicalize(&selected_root).expect("canonical selected root");
    assert_eq!(
        runtime_view.tools[tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(canonical_selected.to_string_lossy().as_ref()),
        "agent apply must persist the canonical selected root, not the operator ceiling"
    );

    let snapshot = crate::agent::resolve_document_runtime_snapshot(
        node.as_ref(),
        &crate::agent::DocumentResolveContext::for_tests(
            identity.clone(),
            crate::tool_surface::ToolCeiling::readonly_at(operator_root.path()),
            Default::default(),
        ),
    )
    .await
    .expect("fresh request runtime snapshot");
    let runtime_agent = snapshot
        .agents
        .get(&created.agent_id)
        .expect("fresh request agent is runnable")
        .clone();
    let surface = Arc::new(
        runtime_agent
            .tools
            .resolve(node.as_ref(), &owner, &Default::default())
            .await
            .expect("resolve fresh-request tool surface"),
    );
    let runtime =
        crate::tool_surface::ToolRuntimeContext::oneshot_with_node_did(node.clone(), owner.clone());

    let cases = [
        (
            "selected descendant",
            "marker.txt".to_string(),
            "fresh request sees descendant",
            None,
        ),
        (
            "sibling under operator ceiling",
            sibling_marker.to_string_lossy().into_owned(),
            "outside the allowed tool root",
            Some("operator ceiling must not widen selection"),
        ),
        (
            "original transcript path",
            original_transcript.to_string_lossy().into_owned(),
            "outside the allowed tool root",
            Some("private transcript fixture"),
        ),
    ];
    for (label, path, expected_result, forbidden_content) in cases {
        let request_doc_id = crate::write_manual_agent_request(
            &node,
            fixture_actor.clone(),
            &owner,
            &created.agent_id,
            &format!("descendant-root-{label}"),
            "Read the requested workspace path.",
            json!({}),
        )
        .await
        .expect("enqueue a fresh request for the reconciled agent");
        let request = crate::request_admission::load_request_for_admission_test(
            node.as_ref(),
            &request_doc_id,
        )
        .await
        .expect("load the fresh request through the admission representation");
        assert_eq!(request.agent_id, created.agent_id);
        let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));

        crate::agent::process_owned_request_with_model_for_test(
            node.clone(),
            runtime_agent.clone(),
            surface.clone(),
            &runtime,
            RootReadModel {
                path,
                turns: turns.clone(),
                provider_inputs: provider_inputs.clone(),
            },
            request,
        )
        .await
        .expect("run the persisted request through the production owned loop");
        let escaped_request_doc = crate::graphql::escape_graphql_string(&request_doc_id);
        let observed = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{_docID: {{_eq:"{escaped_request_doc}"}}}}) {{lifecycle_state failure_reason terminal_output}} }}"#
            ))
            .await;
        assert!(!observed.has_errors(), "{label}: {:?}", observed.errors);
        let data = observed.data.as_ref().expect("owned-loop observations");
        assert!(
            turns.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "{label}: provider did not receive the tool-result turn; request: {}",
            data["AgentRequest"][0]
        );
        assert!(
            provider_inputs
                .lock()
                .expect("provider inputs")
                .iter()
                .any(|input| input.contains(expected_result)),
            "{label}: provider did not receive the expected tool result/denial: {:?}",
            provider_inputs.lock().expect("provider inputs")
        );
        if let Some(forbidden_content) = forbidden_content {
            assert!(
                provider_inputs
                    .lock()
                    .expect("provider inputs")
                    .iter()
                    .all(|input| !input.contains(forbidden_content)),
                "{label}: denied file contents reached provider input: {:?}",
                provider_inputs.lock().expect("provider inputs")
            );
        }

        assert_eq!(
            data["AgentRequest"][0]["lifecycle_state"], "completed",
            "{label}: fresh owned request did not complete"
        );
    }
    let persisted_after_requests =
        crate::agent::document_view::load_document_runtime_view(&node, &owner)
            .await
            .expect("reload persisted Tools after fresh owned requests");
    assert_eq!(
        persisted_after_requests.tools[tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(canonical_selected.to_string_lossy().as_ref()),
        "fresh request acceptance must not widen persisted/effective root"
    );
}

#[tokio::test]
async fn backend_self_config_protects_raw_keys_in_writes_reads_and_diffs() {
    let node = build_agent_node().await;
    let identity = agent_identity("self-config-secret");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "secret").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "secret".into()).unwrap();
    let raw = vec![(
        "auth".into(),
        Some(json!({"kind":"api_key","key":"do-not-expose"})),
    )];
    assert!(core.preview(backend_request(raw.clone())).await.is_err());
    assert!(core.apply(backend_request(raw)).await.is_err());
    let owner = escape_graphql_string(&owner);
    let response=node.execute(&format!(r#"mutation {{update_InferenceBackend(filter:{{node_did:{{_eq:"{owner}"}},backend_id:{{_eq:"secret:backend"}}}},input:{{auth:{{kind:"api_key",key:"operator-secret"}}}}) {{_docID}}}}"#)).await;
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

/// A self-config write merges onto the raw stored document, so an unrelated
/// backend field edit must keep an environment credential as its reference
/// and never resolve the variable into storage, previews, or reads.
#[tokio::test]
async fn unrelated_backend_patch_keeps_environment_reference_unresolved() {
    const VARIABLE: &str = "GENTS_TEST_UNRELATED_PATCH_ENV_REFERENCE";
    const SENTINEL: &str = "sentinel-env-secret-never-stored";
    let node = build_agent_node().await;
    let identity = agent_identity("self-config-env-reference");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "envref").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "envref".into()).unwrap();
    let reference = json!({"kind":"environment","variable":VARIABLE});
    core.apply(backend_request(vec![(
        "auth".into(),
        Some(reference.clone()),
    )]))
    .await
    .unwrap();
    let _variable = crate::test_support::EnvVarGuard::set(VARIABLE, SENTINEL);

    let stored_backend = || {
        let node = node.clone();
        let owner = escape_graphql_string(&owner);
        async move {
            let response = node
                .execute(&format!(
                    r#"{{ InferenceBackend(filter: {{node_did: {{_eq: "{owner}"}}, backend_id: {{_eq: "envref:backend"}}}}) {{ name connect_timeout_secs auth }} }}"#
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            response.data.expect("backend data")["InferenceBackend"][0].clone()
        }
    };

    for patch in [
        vec![("name".into(), Some(json!("Renamed inference")))],
        vec![("connect_timeout_secs".into(), Some(json!(17)))],
    ] {
        let preview = core.preview(backend_request(patch.clone())).await.unwrap();
        let applied = core.apply(backend_request(patch)).await.unwrap();
        for outcome in [&preview, &applied] {
            let rendered = serde_json::to_string(outcome).unwrap();
            assert!(!rendered.contains(SENTINEL), "{rendered}");
            assert!(
                !rendered.contains("\"auth\""),
                "an unrelated patch must not report an auth delta: {rendered}"
            );
        }
    }

    let stored = stored_backend().await;
    assert_eq!(stored["name"], "Renamed inference");
    assert_eq!(stored["connect_timeout_secs"], 17);
    let auth = match &stored["auth"] {
        Value::String(encoded) => serde_json::from_str::<Value>(encoded).unwrap(),
        other => other.clone(),
    };
    assert_eq!(auth, reference, "stored auth must remain the reference");
    assert!(!stored.to_string().contains(SENTINEL));
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(!read.to_string().contains(SENTINEL));
    assert!(read.to_string().contains(VARIABLE));
}

/// The account fence lives in `validate`, so it holds with no-lockout off.
#[tokio::test]
async fn backend_self_config_cannot_set_or_change_an_oauth_account() {
    let node = build_agent_node().await;
    let identity = agent_identity("self-config-account");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "acct").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "acct".into()).unwrap();
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let store = |kind: &str, auth: Value| {
        let backend: crate::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": "acct:backend", "name": "Test inference",
            "provider_kind": kind, "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap();
        let access = &access;
        async move {
            crate::config_client::write_inference_backend_document(access, &backend)
                .await
                .unwrap();
        }
    };
    let to_oauth = |auth: Value| {
        vec![
            ("provider_kind".into(), Some(json!("ChatGptCodex"))),
            ("auth".into(), Some(auth)),
        ]
    };
    let refused = |patch: SelfConfigPatch| {
        let core = &core;
        async move {
            let preview = core.preview(backend_request(patch.clone())).await;
            let apply = core.apply(backend_request(patch)).await;
            for result in [preview, apply] {
                let error = result.expect_err("account reference change must be refused");
                assert!(
                    format!("{error:#}").contains("OAuth account references are operator-managed"),
                    "{error:#}"
                );
            }
        }
    };

    store(
        "OpenAiCompatible",
        json!({"kind":"environment","variable":"KEY"}),
    )
    .await;
    refused(to_oauth(json!({"kind":"node_oauth","account_ref":"a1"}))).await;
    let original = to_oauth(json!({"kind":"node_oauth"}));
    core.preview(backend_request(original.clone()))
        .await
        .unwrap();
    core.apply(backend_request(original)).await.unwrap();

    store(
        "ChatGptCodex",
        json!({"kind":"node_oauth","account_ref":"a1"}),
    )
    .await;
    refused(to_oauth(json!({"kind":"node_oauth","account_ref":"a2"}))).await;
    refused(to_oauth(json!({"kind":"node_oauth"}))).await;
    let endpoint = vec![("endpoint".into(), Some(json!("http://127.0.0.1:2/v1")))];
    core.preview(backend_request(endpoint.clone()))
        .await
        .unwrap();
    core.apply(backend_request(endpoint)).await.unwrap();
}

/// Two ChatGPT accounts and Grok's original account, a profile on each
/// (`p-original`, `p-chat-b`, `p-grok`); the agent's own profile is on the
/// original ChatGPT account and its context has an unset-profile compaction.
/// Spare contexts `ctx-original` and `ctx-chat-b` use compactions `c-original`
/// and `c-chat-b` on those profiles. No-lockout stays off.
async fn account_choice_core(
    agent: &str,
) -> (
    std::sync::Arc<defra_node::EmbeddedNode>,
    std::sync::Arc<dyn crate::NodeIdentity>,
    SelfConfigCore,
) {
    let node = build_agent_node().await;
    let identity = agent_identity(agent);
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, agent).await;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    for (id, kind, auth) in [
        (
            "chat-original",
            "ChatGptCodex",
            json!({"kind":"node_oauth"}),
        ),
        (
            "chat-b",
            "ChatGptCodex",
            json!({"kind":"node_oauth","account_ref":"acct-b"}),
        ),
        (
            "grok-original",
            "XaiGrokOAuth",
            json!({"kind":"node_oauth"}),
        ),
    ] {
        let backend: crate::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": id, "name": id, "provider_kind": kind,
            "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap();
        crate::config_client::write_inference_backend_document(&access, &backend)
            .await
            .unwrap();
    }
    for (id, backend) in [
        (format!("{agent}:inference"), "chat-original"),
        ("p-original".into(), "chat-original"),
        ("p-chat-b".into(), "chat-b"),
        ("p-grok".into(), "grok-original"),
    ] {
        let profile: crate::document_config::InferenceProfile = serde_json::from_value(json!({
            "node_did": owner, "profile_id": id, "backend_id": backend,
            "model_name": "test-model",
        }))
        .unwrap();
        crate::config_client::write_inference_profile_document(&access, &profile)
            .await
            .unwrap();
    }
    let compaction = format!("{agent}:compaction");
    let plan = crate::config_client::DesiredStateApplyPlan::new(
        [
            (
                crate::Collection::Compaction,
                json!({"node_did": owner, "compaction_id": compaction}),
            ),
            (
                crate::Collection::AgentContext,
                json!({"node_did": owner, "context_id": format!("{agent}:context"),
                    "tools_id": format!("{agent}:tools"), "compaction_id": compaction}),
            ),
        ]
        .into_iter()
        .chain(["original", "chat-b"].into_iter().flat_map(|account| {
            let compaction = format!("c-{account}");
            [
                (
                    crate::Collection::Compaction,
                    json!({"node_did": owner, "compaction_id": compaction,
                        "inference_profile_id": format!("p-{account}")}),
                ),
                (
                    crate::Collection::AgentContext,
                    json!({"node_did": owner, "context_id": format!("ctx-{account}"),
                        "compaction_id": compaction}),
                ),
            ]
        }))
        .map(
            |(collection, value)| crate::config_client::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    crate::config_client::ConfigAccess::transact_local(&node, None, "test.compaction", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
    let core = SelfConfigCore::new(node.clone(), owner, agent.into()).unwrap();
    (node, identity, core)
}

async fn assert_account_choice_refused(
    core: &SelfConfigCore,
    request: impl Fn() -> ApplyRequest<'static>,
) {
    for result in [core.preview(request()).await, core.apply(request()).await] {
        let error = result.expect_err("switching to another account must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn profile_self_config_cannot_pick_another_account() {
    let (_, _, core) = account_choice_core("pick").await;
    let owner = core.node_did().to_owned();
    let to = |backend: &str| vec![("backend_id".into(), Some(json!(backend)))];
    assert_account_choice_refused(&core, || profile_request(to("chat-b"))).await;
    core.preview(profile_request(to("grok-original")))
        .await
        .unwrap();
    core.apply(profile_request(to("grok-original")))
        .await
        .unwrap();
    assert_account_choice_refused(&core, || {
        let mut patch = to("chat-b");
        patch.push(("model_name".into(), Some(json!("test-model"))));
        profile_create_request(owner.clone(), "p-new".into(), patch)
    })
    .await;
}

#[tokio::test]
async fn agent_profile_pick_cannot_switch_account() {
    let (_, _, core) = account_choice_core("agent-pick").await;
    let to = |profile: &str| vec![("inference_profile_id".into(), Some(json!(profile)))];
    assert_account_choice_refused(&core, || agent_request(&core, to("p-chat-b"))).await;
    core.apply(agent_request(&core, to("p-original")))
        .await
        .unwrap();
    core.preview(agent_request(&core, to("p-grok")))
        .await
        .unwrap();
    core.apply(agent_request(&core, to("p-grok")))
        .await
        .unwrap();
}

/// [`account_choice_core`] plus Grok backend `grok-g2` on account `g2` (profile
/// `p-grok-g2`) and the node's Grok sign-ins: the original row (no
/// connection time) and `g2`, each enabled as given.
async fn grok_accounts_core(agent: &str, original: bool, g2: bool) -> SelfConfigCore {
    let (node, _, core) = account_choice_core(agent).await;
    let owner = core.node_did().to_owned();
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let backend: crate::InferenceBackend = serde_json::from_value(json!({
        "node_did": owner, "backend_id": "grok-g2", "name": "grok-g2",
        "provider_kind": "XaiGrokOAuth", "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind":"node_oauth","account_ref":"g2"},
    }))
    .unwrap();
    crate::config_client::write_inference_backend_document(&access, &backend)
        .await
        .unwrap();
    let profile: crate::document_config::InferenceProfile = serde_json::from_value(json!({
        "node_did": owner, "profile_id": "p-grok-g2", "backend_id": "grok-g2",
        "model_name": "test-model",
    }))
    .unwrap();
    crate::config_client::write_inference_profile_document(&access, &profile)
        .await
        .unwrap();
    let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
    for (account_ref, enabled) in [(None, original), (Some("g2"), g2)] {
        let original_id = crate::oauth_credential::oauth_credential_id(&owner, provider);
        let credential = crate::oauth_credential::OAuthCredential {
            doc_id: None,
            credential_id: match account_ref {
                Some(account_ref) => format!("{original_id}:{account_ref}"),
                None => original_id,
            },
            node_did: owner.clone(),
            provider: provider.to_string(),
            access_token: "access-TEST".into(),
            refresh_token: "refresh-TEST".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled,
            account_ref: account_ref.map(str::to_string),
            connected_at: account_ref.map(|_| chrono::Utc::now()),
            provider_account_key: None,
            label: None,
        };
        crate::oauth_credential::upsert_oauth_credential(&node, &credential)
            .await
            .unwrap();
    }
    core
}

#[tokio::test]
async fn default_account_follows_the_resolver() {
    let to = |profile: &str| vec![("inference_profile_id".into(), Some(json!(profile)))];
    let accepted = |core: SelfConfigCore, profile: &'static str| async move {
        core.preview(agent_request(&core, to(profile)))
            .await
            .unwrap();
        core.apply(agent_request(&core, to(profile))).await.unwrap();
    };

    // The original is disabled, so g2 is Grok's default account.
    let core = grok_accounts_core("default-g2", false, true).await;
    assert_account_choice_refused(&core, || agent_request(&core, to("p-grok"))).await;
    accepted(core, "p-grok-g2").await;

    // Both enabled: the original has no connection time, so it sorts first.
    let core = grok_accounts_core("default-original", true, true).await;
    assert_account_choice_refused(&core, || agent_request(&core, to("p-grok-g2"))).await;
    accepted(core, "p-grok").await;

    // None enabled: the no-reference backend is allowed and fails at run time.
    let core = grok_accounts_core("default-none", false, false).await;
    accepted(core, "p-grok").await;
}

/// An unset compaction profile reuses the agent's, so that is its current.
#[tokio::test]
async fn compaction_profile_pick_cannot_switch_account() {
    let (_, _, core) = account_choice_core("compaction-pick").await;
    let to = |profile: &str| {
        profile_target_request(
            Some("compaction"),
            vec![("inference_profile_id".into(), Some(json!(profile)))],
        )
        .unwrap()
    };
    assert_account_choice_refused(&core, || to("p-chat-b")).await;
    core.apply(to("p-original")).await.unwrap();
    core.preview(to("p-grok")).await.unwrap();
    core.apply(to("p-grok")).await.unwrap();
}

/// Compaction runs on its own profile, else the agent's; re-pointing the
/// context's compaction or the agent's context is a pick too.
#[tokio::test]
async fn compaction_reference_pick_cannot_switch_account() {
    let (_, _, core) = account_choice_core("compaction-ref-pick").await;
    let context = |compaction: &str| {
        protect_working_agent(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![("compaction_id".into(), Some(json!(compaction)))],
        ))
    };
    let agent = |context: &str| {
        protect_working_agent(agent_request(
            &core,
            vec![("context_id".into(), Some(json!(context)))],
        ))
    };
    assert_account_choice_refused(&core, || context("c-chat-b")).await;
    assert_account_choice_refused(&core, || agent("ctx-chat-b")).await;
    core.preview(context("c-original")).await.unwrap();
    core.apply(context("c-original")).await.unwrap();
    core.preview(agent("ctx-original")).await.unwrap();
    core.apply(agent("ctx-original")).await.unwrap();
}

/// Create and clone have no current backend; edit's is the target agent's.
/// The fence runs in the configuration publication transaction.
#[tokio::test]
async fn agent_management_profile_pick_cannot_switch_account() {
    let (node, identity, core) = account_choice_core("agent-pick").await;
    let owner = core.node_did().to_owned();
    let params = |action: &str, argv: &[&str]| {
        agent_params(
            action,
            None,
            &argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let create = |profile: &str| {
        params(
            "create",
            &[
                "--display-name",
                "Picker",
                "--description",
                "Picks",
                "--system-prompt",
                "Pick.",
                "--preset",
                "write",
                "--profile",
                profile,
            ],
        )
    };
    let clone = |profile: &str| {
        params(
            "clone",
            &[
                "--from",
                "agent-pick",
                "--display-name",
                "Picker clone",
                "--profile",
                profile,
            ],
        )
    };
    let edit = params("edit", &["--id", "agent-pick", "--profile", "p-chat-b"]);
    for refused in [create("p-chat-b"), clone("p-chat-b"), edit] {
        let error = agent_mutate(
            &node,
            &owner,
            identity.as_ref(),
            &refused,
            &Default::default(),
        )
        .await
        .expect_err("switching to another account must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap().len(), 1);
    for accepted in [
        create("p-grok"),
        clone("p-grok"),
        params("edit", &["--id", "agent-pick", "--profile", "p-original"]),
        params("edit", &["--id", "agent-pick", "--profile", "p-grok"]),
    ] {
        agent_mutate(
            &node,
            &owner,
            identity.as_ref(),
            &accepted,
            &Default::default(),
        )
        .await
        .unwrap();
    }
}

/// Preview runs the write's account check, so the two agree.
#[tokio::test]
async fn agent_preview_agrees_with_the_account_choice_fence() {
    let (node, _, core) = account_choice_core("preview-pick").await;
    let owner = core.node_did().to_owned();
    let create = [
        "--display-name",
        "Picker",
        "--description",
        "Picks",
        "--system-prompt",
        "Pick.",
        "--preset",
        "write",
    ];
    let clone = ["--from", "preview-pick", "--display-name", "Picker clone"];
    let edit = ["--id", "preview-pick"];
    for (operation, argv, profile, admitted) in [
        ("create", &create[..], "p-chat-b", false),
        ("clone", &clone[..], "p-chat-b", false),
        ("edit", &edit[..], "p-chat-b", false),
        ("create", &create[..], "p-grok", true),
        ("clone", &clone[..], "p-grok", true),
        ("edit", &edit[..], "p-original", true),
    ] {
        let argv: Vec<String> = argv
            .iter()
            .chain(&["--profile", profile])
            .map(|arg| (*arg).to_owned())
            .collect();
        let params = agent_params("preview", Some(operation.into()), &argv).unwrap();
        let preview: Value = serde_json::from_str(
            &agent_preview(&node, &owner, &params, &Default::default())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            preview["admitted"], admitted,
            "{operation} {profile}: {preview}"
        );
        if !admitted {
            assert!(
                preview["rejection"]
                    .as_str()
                    .is_some_and(|rejection| rejection.contains("selects another")),
                "{preview}"
            );
        }
    }
}

/// A clone keeps its source's context, so its compaction may run on another
/// backend than the clone's own profile; that inherited backend is a pick
/// from the profile's, as when re-pointing the context.
#[tokio::test]
async fn agent_clone_inherited_compaction_cannot_switch_account() {
    let (node, identity, core) = account_choice_core("clone-pick").await;
    let owner = core.node_did().to_owned();
    let sources = ["original", "chat-b"]
        .into_iter()
        .map(|account| {
            let value = json!({"node_did": owner, "agent_id": format!("src-{account}"),
                "context_id": format!("ctx-{account}"), "inference_profile_id": "p-original"});
            crate::config_client::DesiredStateApplyDocument {
                collection: crate::Collection::Agent,
                add: value.clone(),
                update: value,
            }
        })
        .collect();
    let plan = crate::config_client::DesiredStateApplyPlan::new(sources).unwrap();
    crate::config_client::ConfigAccess::transact_local(&node, None, "test.sources", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
    let before = crate::list_agents(&node, &owner).await.unwrap();
    let clone = |action: &str, source: &str, profile: &str| {
        let name = format!("{source} on {profile}");
        let argv = [
            "--from",
            source,
            "--display-name",
            &name,
            "--profile",
            profile,
        ];
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let operation = (action == "preview").then(|| "clone".to_owned());
        agent_params(action, operation, &argv).unwrap()
    };
    for (source, profile) in [("src-chat-b", "p-original"), ("src-chat-b", "p-grok")] {
        let preview: Value = serde_json::from_str(
            &agent_preview(
                &node,
                &owner,
                &clone("preview", source, profile),
                &Default::default(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(preview["admitted"], false, "{source} {profile}: {preview}");
        let error = agent_mutate(
            &node,
            &owner,
            identity.as_ref(),
            &clone("clone", source, profile),
            &Default::default(),
        )
        .await
        .expect_err("inheriting another account's compaction must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap(), before);
    for profile in ["p-original", "p-grok"] {
        agent_mutate(
            &node,
            &owner,
            identity.as_ref(),
            &clone("clone", "src-original", profile),
            &Default::default(),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn pack_inference_slot_cannot_pick_another_account() {
    let (node, identity, core) = account_choice_core("pack-pick").await;
    let mut tool_config = config(&[]);
    tool_config.agent_id = "pack-pick".into();
    tool_config.enable_pack_install = true;
    tool_config.enable_graph_tools = true;
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("review_graph");
    let tools = build_self_config_tools(
        node,
        core.node_did().to_owned(),
        Some(identity),
        &tool_config,
        plugins,
    );
    let argv = |prefix: &[&str], profile: &str| {
        let mut argv: Vec<String> = prefix.iter().map(|arg| (*arg).to_owned()).collect();
        for slot in ["coordinator", "worker", "verifier"] {
            argv.push("--inference-slot".into());
            argv.push(format!("{slot}={profile}"));
        }
        argv
    };
    let error = call_config_tool(
        &tools,
        argv(
            &["pack", "preview", "install", "fixture/review_graph"],
            "p-chat-b",
        ),
    )
    .await
    .expect_err("preview binding another account must be refused");
    assert!(error.contains("selects another"), "{error}");
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            argv(
                &["pack", "preview", "install", "fixture/review_graph"],
                "p-grok",
            ),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], true);
    let digest = preview["artifact_digest"].as_str().unwrap();
    let install = [
        "pack",
        "install",
        "fixture/review_graph",
        "--digest",
        digest,
    ];
    let error = call_config_tool(&tools, argv(&install, "p-chat-b"))
        .await
        .expect_err("install binding another account must be refused");
    assert!(error.contains("selects another"), "{error}");
    call_config_tool(&tools, argv(&install, "p-grok"))
        .await
        .unwrap();
}

#[tokio::test]
async fn explicit_tools_grant_preserves_lsp_settings_guard_for_preview_and_apply() {
    let node = build_agent_node().await;
    let identity = agent_identity("self-config-lsp-guard");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "beh-test").await;
    let mut tool_config = config(&["tools"]);
    tool_config.preview = true;
    let tools = build_self_config_tools(node, owner, None, &tool_config, test_plugins());
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

#[tokio::test]
async fn profile_edit_rejects_a_context_window_above_the_advertised_maximum() {
    let node = build_agent_node().await;
    let identity = agent_identity("profile-context-window");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "setup").await;
    let backend: crate::document_config::InferenceBackend = serde_json::from_value(json!({
        "node_did": owner, "backend_id": "setup:backend", "name": "Test inference",
        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind": "unauthenticated"},
    }))
    .unwrap();
    crate::backend_registry::record_model_catalog(
        &node,
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
    let mut tool_config = config(&["profile"]);
    tool_config.agent_id = "setup".into();
    tool_config.preview = true;
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins());
    let edit = |verb: &str, window: u64| -> Vec<String> {
        vec![
            "profile".into(),
            verb.into(),
            "--agent".into(),
            "setup".into(),
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
            "setup".into(),
        ],
    )
    .await
    .unwrap();
    assert!(stored.contains("500000"), "{stored}");
}

/// #1796: the Engineer builds its crew and edits itself through `config`,
/// and only a self-lockout is refused.
#[tokio::test]
async fn engineer_configures_targets_executions_and_itself_but_cannot_lock_out() {
    let node = build_agent_node().await;
    let identity = agent_identity("engineer-god-mode");
    let owner = identity.did().to_string();
    for agent in ["setup", "lead", "caller"] {
        crate::test_support::install_test_agent(&node, &owner, agent).await;
    }
    let setup = SelfConfigCore::new(node.clone(), owner.clone(), "setup".into()).unwrap();
    setup
        .apply(agent_request(
            &setup,
            vec![(
                "tags".into(),
                Some(json!([crate::self_config::ENGINEER_AGENT_TAG])),
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
    grants.agent_id = "setup".into();
    grants.preview = true;
    grants.no_lockout = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &grants,
        test_plugins(),
    );
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let ok = |result: Result<String, String>| -> Value {
        serde_json::from_str(&result.unwrap_or_else(|error| panic!("{error}"))).unwrap()
    };

    // #2058: AgentTarget through the desired-state owner.
    let target_patch = [
        "--set",
        "name=\"gatekeeper\"",
        "--set",
        &format!("target_node_did={}", json!(owner)),
        "--set",
        "agent_id=\"lead\"",
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
            "agent_id=\"missing\"",
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
            command(&["tools", "edit", "--agent", "caller", "--set", selection]),
        )
        .await);
        assert_eq!(receipt["committed"], true);
        assert!(receipt["effect"]
            .as_str()
            .unwrap()
            .contains("Selected targets are inactive"));
        let stored =
            ok(call_config_tool(&tools, command(&["tools", "get", "--agent", "caller"])).await);
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
            "caller",
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
        ok(call_config_tool(&tools, command(&["tools", "get", "--agent", "caller"])).await);
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
            "caller",
            "--set",
            r#"agents={"enabled":true}"#,
        ]),
    )
    .await);

    // Own Tools: select the target, the sessions tool and read-only query.
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
        command(&["agent", "edit", "setup", "--set", "enabled=false"]),
    )
    .await
    .unwrap_err();
    assert!(disable.contains("no-lockout"), "{disable}");
    // Two-step self-disable: the Engineer tag and the agent disable path.
    let untag = call_config_tool(
        &tools,
        command(&["agent", "edit", "setup", "--set", "tags=[\"ui:engineer\"]"]),
    )
    .await
    .unwrap_err();
    assert!(untag.contains("Engineer tag must remain"), "{untag}");
    ok(call_config_tool(
        &tools,
        command(&[
            "agent",
            "edit",
            "setup",
            "--set",
            &format!(
                "tags={}",
                json!([crate::self_config::ENGINEER_AGENT_TAG, "ui:engineer"])
            ),
        ]),
    )
    .await);
    let agent_disable = call_config_tool(&tools, command(&["agent", "disable", "--id", "setup"]))
        .await
        .unwrap_err();
    assert!(agent_disable.contains("no-lockout"), "{agent_disable}");
    // A selected target the runtime could not resolve is refused at publication.
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

    // #2059: create an execution, bind it, then edit its limits normally.
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
        crate::config::DEFAULT_MAX_TURNS
    );
    assert_eq!(
        defaults["effective"]["provider_idle_timeout_secs"],
        crate::config::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS
    );
    assert_eq!(
        defaults["effective"]["deadline_duration_secs"],
        crate::config::DEFAULT_DEADLINE_DURATION_SECS
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
            "lead",
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
            "lead",
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
        command(&["profile", "get", "execution", "--agent", "lead"]),
    )
    .await);
    assert_eq!(bound["effective"], execution["effective"]);

    // Automation that targets the Engineer itself.
    ok(call_config_tool(
        &tools,
        command(&[
            "automation",
            "edit",
            "task",
            "engineer-inbox",
            "--agent",
            "setup",
            "--set",
            "prompt_template=\"Review {{ doc.message }}\"",
        ]),
    )
    .await);

    // Delete through the reference-aware cleanup owner once deselected.
    assert!(call_config_tool(
        &tools,
        command(&["cleanup", "preview", "--target", "agent-target=gatekeeper"]),
    )
    .await
    .is_err());
    let refused = call_config_tool(
        &tools,
        command(&["tools", "edit", "--set", r#"agents={"enabled":true}"#]),
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("drop existing settings from your own Tools: agents.target_ids")
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
    let node = build_agent_node().await;
    let identity = agent_identity("backend-catalog-read");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "setup").await;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let backend = |id: &str, kind: &str, auth: Value| -> crate::document_config::InferenceBackend {
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
    let models: Vec<crate::document_config::AdvertisedModel> = serde_json::from_value(json!([{
        "model_name": "claude-opus-5-5", "display_name": "Claude Opus 5.5",
        "context_window": 1_000_000, "max_context_window": null, "max_output_tokens": 128_000,
        "reasoning_efforts": ["low", "high"],
    }]))
    .unwrap();
    for document in [&claude, &keyed] {
        crate::config_client::write_inference_backend_document(&access, document)
            .await
            .unwrap();
    }
    assert_eq!(
        crate::backend_registry::record_connection_catalog_on(
            &access,
            &owner,
            claude.provider_kind,
            &claude.endpoint,
            &claude.auth,
            models.clone(),
        )
        .await
        .unwrap(),
        1
    );
    crate::backend_registry::record_discovered_catalog_on(&access, &keyed, models)
        .await
        .unwrap();

    let mut tool_config = config(&["backend"]);
    tool_config.agent_id = "setup".into();
    tool_config.preview = true;
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins());
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
        let model = &get["observation"]["catalog"]["models"][0];
        assert_eq!(model["context_window"], 1_000_000);
        assert_eq!(model["reasoning_efforts"], json!(["low", "high"]));
        // The endpoint is unreachable: a provider call would fail discover.
        let discover = call(&["discover", id]).await;
        assert_eq!(discover["refreshed"], false);
        assert_eq!(
            discover["observation"]["catalog"]["models"][0]["model_name"],
            "claude-opus-5-5"
        );
    }
    let bound = call(&["get"]).await;
    assert!(bound["observation"].get("catalog").is_some(), "{bound}");
}

#[tokio::test]
async fn pack_search_uses_registry_pagination_without_installing() {
    let _registry_lock = PACK_REGISTRY_ENV.lock().await;
    let app = axum::Router::new().route("/api/v1/packs", axum::routing::get(
        |axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>| async move {
            assert_eq!(query.get("q").map(String::as_str), Some("code review"));
            let page = &query["page"];
            axum::Json(json!({"packs":[{"namespace":"fixture","name":"review_graph","latest":"1.0.0","description":"Code review"}],"has_more":page == "1"}))
        }
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let _registry =
        crate::test_support::EnvVarGuard::set("GENTS_REGISTRY", format!("http://{address}"));
    let (node, did, tools) = pack_tool("registry-search", test_plugins()).await;
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
        json!({"argv":["pack","get","fixture/review_graph"]})
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
    let access = crate::config_client::ConfigAccess::Local(node);
    assert!(
        crate::pack::read_installed_pack(&access, &did, "fixture/review_graph")
            .await
            .unwrap()
            .is_none()
    );
    server.abort();
}

#[tokio::test]
async fn pack_documents_install_and_release_graph_dependencies() {
    let (home, plugins) = crate::test_support::home_with_fixture_pack("dependent_fixture");
    let (_fixture, directory) = crate::test_support::fixture_pack_copy("review_graph", &json!({}));
    let (bytes, _) = crate::pack_archive::pack_dir(&directory).unwrap();
    crate::pack_store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap();
    let (node, did, tools) = pack_tool("documents-dependencies", plugins).await;
    let slots = [
        "--inference-slot",
        "worker=setup:inference",
        "--inference-slot",
        "coordinator=setup:inference",
        "--inference-slot",
        "verifier=setup:inference",
    ];
    let mut args = vec!["pack", "preview", "install", "fixture/dependent_fixture"];
    args.extend(slots);
    let preview: Value = serde_json::from_str(&config_call(&tools, &args).await.unwrap()).unwrap();
    assert_eq!(preview["ready"], true, "{preview}");
    assert_eq!(
        preview["dependencies"][0]["coordinate"],
        "fixture/review_graph"
    );
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut args = vec![
        "pack",
        "install",
        "fixture/dependent_fixture",
        "--digest",
        digest,
    ];
    args.extend(slots);
    config_call(&tools, &args).await.unwrap();
    let access = crate::config_client::ConfigAccess::Local(node);
    assert!(
        crate::pack::read_installed_pack(&access, &did, "fixture/review_graph")
            .await
            .unwrap()
            .is_some()
    );
    config_call(&tools, &["pack", "remove", "fixture/dependent_fixture"])
        .await
        .unwrap();
    assert!(
        crate::pack::read_installed_pack(&access, &did, "fixture/review_graph")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn pack_document_installation_uses_existing_lifecycle() {
    let (_fixture, directory) =
        crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let manifest_path = directory.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (_home, plugins) = crate::test_support::home_with_pack_dir(&directory);
    let (node, did, tools) = pack_tool("inspect-documents-pack", plugins).await;
    let result: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", "fixture/documents_fixture"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["installable"], true);
    let missing: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "preview", "install", "fixture/documents_fixture"],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(missing["ready"], false);
    assert_eq!(missing["missing_inference_slots"], json!(["worker"]));
    let preview: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                "fixture/documents_fixture",
                "--inference-slot",
                "worker=setup:inference",
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], true, "{preview}");
    let digest = preview["artifact_digest"].as_str().unwrap();
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    assert!(
        crate::pack::read_installed_pack(&access, &did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none(),
        "preview must not install documents"
    );
    let rejected = config_call(
        &tools,
        &[
            "pack",
            "install",
            "fixture/documents_fixture",
            "--digest",
            "sha256:wrong",
            "--inference-slot",
            "worker=setup:inference",
        ],
    )
    .await
    .unwrap_err();
    assert!(rejected.to_string().contains("digest"), "{rejected}");
    assert!(
        crate::pack::read_installed_pack(&access, &did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "install",
                "fixture/documents_fixture",
                "--digest",
                digest,
                "--inference-slot",
                "worker=setup:inference",
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["effective"]["digest"], digest);
    assert!(
        node.get_collection("FixtureJob").unwrap().is_some(),
        "custom schema asset must be published"
    );

    assert!(
        crate::pack::read_installed_pack(&access, &did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_some()
    );
    let updated: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "update",
                "fixture/documents_fixture@1.0.0",
                "--digest",
                digest,
                "--inference-slot",
                "worker=setup:inference",
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["effective"]["digest"], digest);
    config_call(&tools, &["pack", "remove", "fixture/documents_fixture"])
        .await
        .unwrap();
    assert!(
        crate::pack::read_installed_pack(&access, &did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
#[ignore = "uses the public pack registry; installs into a disposable node, never runs the graph"]
async fn published_review_pack_preserves_installation_when_upgrade_needs_authority() {
    let home = tempfile::tempdir().unwrap();
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_path_buf(),
    )));
    let (node, did, tools) = pack_tool("published-review-pack", plugins).await;
    let tool = tools.iter().find(|t| t.name() == CONFIG_TOOL_NAME).unwrap();
    for (verb, version) in [("install", "1.6.0"), ("update", "1.7.0")] {
        let package = format!("gents/code_review@{version}");
        let inspected: Value = serde_json::from_str(
            &config_call(&tools, &["pack", "get", &package])
                .await
                .unwrap(),
        )
        .unwrap();
        let slots = inspected["manifest"]["inference_slots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|slot| format!("{}=setup:inference", slot["name"].as_str().unwrap()))
            .collect::<Vec<_>>();
        let options = json!({"inference-slot":slots});
        let preview: Value = serde_json::from_str(
            &tool
                .call(
                    json!({"argv":["pack","preview",verb,&package],"options":options}).to_string(),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(preview["ready"], verb == "install", "{preview}");
        if verb == "update" {
            assert!(preview["apply_with"].is_null());
            assert!(preview["installation_blockers"]
                .to_string()
                .contains("increased resource limits"));
        }
        let mut options = options;
        options["digest"] = preview["artifact_digest"].clone();
        let applied = tool
            .call(json!({"argv":["pack",verb,&package],"options":options}).to_string())
            .await;
        if verb == "update" {
            assert!(applied
                .unwrap_err()
                .to_string()
                .contains("--grant-authority"));
        } else {
            applied.unwrap();
        }
        let access = crate::config_client::ConfigAccess::Local(node.clone());
        let installed = crate::pack::read_installed_pack(&access, &did, "gents/code_review")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(installed.version, "1.6.0");
    }
    config_call(&tools, &["pack", "remove", "gents/code_review"])
        .await
        .unwrap();
    let access = crate::config_client::ConfigAccess::Local(node);
    assert!(
        crate::pack::read_installed_pack(&access, &did, "gents/code_review")
            .await
            .unwrap()
            .is_none()
    );
    let owner = crate::graphql::escape_graphql_string(&did);
    let rows = access
        .execute(&format!(
            "{{GraphRun(filter: {{owner_did: {{_eq: \"{owner}\"}}}}) {{run_id}}}}"
        ))
        .await
        .unwrap();
    assert_eq!(rows["data"]["GraphRun"], json!([]));
}

/// Every `OAuthCredential`, `InferenceBackend` and `ProviderAccountUsage` row, as stored.
async fn account_state_rows(node: &std::sync::Arc<defra_node::EmbeddedNode>) -> Value {
    let response = node
        .execute(
            "{ OAuthCredential { _docID credential_id enabled label access_token } \
               InferenceBackend { _docID backend_id enabled auth } \
               ProviderAccountUsage { _docID usage_key report read_at } }",
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.expect("data")
}

async fn seed_account_view(node: &std::sync::Arc<defra_node::EmbeddedNode>, owner: &str) {
    use crate::oauth_credential::{oauth_credential_id, upsert_oauth_credential, OAuthCredential};
    let provider = crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
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
    let access = crate::config_client::ConfigAccess::Local(node.clone());
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
        let backend: crate::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner,
            "backend_id": backend_id,
            "name": backend_id,
            "provider_kind": kind,
            "endpoint": endpoint,
            "auth": auth,
        }))
        .unwrap();
        crate::config_client::write_inference_backend_document(&access, &backend)
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
    crate::config_client::write_inference_profile_document(&access, &profile)
        .await
        .unwrap();
    crate::usage_observation::record_usage(
        node,
        &crate::usage_observation::UsageAccount::Credential {
            doc_id: None,
            node_did: owner.to_string(),
            provider: provider.to_string(),
            account_ref: None,
        },
        crate::usage_observation::account_usage::UsageReport {
            windows: vec![crate::usage_observation::account_usage::UsageWindow {
                label: "5h".into(),
                window_minutes: Some(300),
                used_pct: 42.0,
                resets_at: None,
                source: crate::usage_observation::account_usage::UsageSource::Header,
                observed_at: chrono::Utc::now(),
            }],
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn config_backend_accounts_lists_accounts_and_usage_read_only() {
    let node = build_agent_node().await;
    let identity = agent_identity("config-accounts");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "alpha").await;
    seed_account_view(&node, &owner).await;
    let mut tool_config = config(&["node", "profile", "backend"]);
    tool_config.agent_id = "alpha".into();
    let tools = build_self_config_tools(
        node.clone(),
        owner,
        Some(identity),
        &tool_config,
        test_plugins(),
    );
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let before = account_state_rows(&node).await;

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
        "did:key",
        "credential_id",
        "account_ref",
        "acct-l2",
    ] {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    assert_eq!(account_state_rows(&node).await, before);

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
    let node = build_agent_node().await;
    let identity = agent_identity("config-accounts-grant");
    let owner = identity.did().to_string();
    crate::test_support::install_test_agent(&node, &owner, "alpha").await;
    let mut tool_config = config(&["node", "profile"]);
    tool_config.agent_id = "alpha".into();
    let tools = build_self_config_tools(node, owner, Some(identity), &tool_config, test_plugins());
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    assert!(config
        .call(json!({"argv":["backend", "accounts"]}).to_string())
        .await
        .is_err());
}

#[tokio::test]
async fn agent_management_clears_optional_fields_and_refuses_ignored_tool_modifiers() {
    let node = build_agent_node().await;
    let identity = agent_identity("agent-clear");
    let owner = identity.did().to_owned();
    crate::test_support::install_test_agent(&node, &owner, "seed").await;
    let created: Value = serde_json::from_str(
        &agent_mutate(
            &node,
            &owner,
            identity.as_ref(),
            &ConfigureAgentParams {
                action: "create".into(),
                display_name: StringUpdate::Set("Clearable".into()),
                description: StringUpdate::Set("Original description".into()),
                system_prompt: StringUpdate::Set("Original instructions".into()),
                profile_id: StringUpdate::Set("seed:inference".into()),
                ..Default::default()
            },
            &Default::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let id = created["agent_id"].as_str().unwrap().to_owned();
    let edit = ConfigureAgentParams {
        action: "edit".into(),
        agent_id: Some(id.clone()),
        display_name: StringUpdate::Clear,
        description: StringUpdate::Clear,
        system_prompt: StringUpdate::Clear,
        ..Default::default()
    };
    agent_mutate(&node, &owner, identity.as_ref(), &edit, &Default::default())
        .await
        .unwrap();
    let stored = crate::list_agents(&node, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_id == id)
        .unwrap();
    assert_eq!(stored.display_name, None);
    assert_eq!(stored.description, None);
    let effective = SelfConfigCore::new(node.clone(), owner.clone(), id.clone())
        .unwrap()
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    assert!(
        effective["context"]["system_prompt"].is_null(),
        "{effective}"
    );
    assert_eq!(stored.inference_profile_id, "seed:inference");
    let invalid = ConfigureAgentParams {
        root: StringUpdate::Set("/ignored".into()),
        ..edit
    };
    let error = agent_mutate(
        &node,
        &owner,
        identity.as_ref(),
        &invalid,
        &Default::default(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("tools update"), "{error:#}");
    let after = crate::list_agents(&node, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_id == id)
        .unwrap();
    assert_eq!(stored, after);
}

#[tokio::test]
async fn no_lockout_rejects_current_agent_disable_in_preview_and_commit() {
    let node = build_agent_node().await;
    let identity = agent_identity("disable-preview-parity");
    let owner = identity.did().to_owned();
    for id in ["current", "default"] {
        crate::test_support::install_test_agent(&node, &owner, id).await;
    }
    crate::document_config::upsert_node(&node, &owner, None, Some("default"), true)
        .await
        .unwrap();
    let mut grants = config(&["node"]);
    grants.agent_id = "current".into();
    grants.no_lockout = true;
    grants.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &grants,
        test_plugins(),
    );
    for argv in [
        vec!["agent", "preview", "disable", "--id", "current"],
        vec!["agent", "disable", "--id", "current"],
    ] {
        let error = call_config_tool(&tools, argv.into_iter().map(str::to_owned).collect())
            .await
            .unwrap_err();
        assert!(
            error.contains("no-lockout guard: agent must remain enabled"),
            "{error}"
        );
    }
    let current = crate::list_agents(&node, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_id == "current")
        .unwrap();
    assert!(current.enabled);
}

#[tokio::test]
async fn agent_only_edits_preserve_an_optional_absent_context() {
    let node = build_agent_node().await;
    let identity = agent_identity("contextless-agent-edit");
    let owner = identity.did().to_owned();
    for id in ["current", "other"] {
        crate::test_support::install_test_agent(&node, &owner, id).await;
    }
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "current".into()).unwrap();
    core.apply(agent_request(&core, vec![("context_id".into(), None)]))
        .await
        .unwrap();
    let mut grants = config(&["agent"]);
    grants.agent_id = "current".into();
    grants.preview = true;
    let tools = build_self_config_tools(
        node.clone(),
        owner.clone(),
        Some(identity),
        &grants,
        test_plugins(),
    );
    for (prefix, committed) in [
        (vec!["agent", "preview", "edit", "current"], false),
        (vec!["agent", "edit", "current"], true),
    ] {
        let mut argv: Vec<String> = prefix.into_iter().map(str::to_owned).collect();
        argv.extend(
            [
                "--set",
                r#"display_name="Contextless""#,
                "--set",
                r#"inference_profile_id="other:inference""#,
            ]
            .into_iter()
            .map(str::to_owned),
        );
        let result: Value =
            serde_json::from_str(&call_config_tool(&tools, argv).await.unwrap()).unwrap();
        assert_eq!(result["committed"], committed);
    }
    let current = crate::list_agents(&node, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_id == "current")
        .unwrap();
    assert_eq!(current.display_name.as_deref(), Some("Contextless"));
    assert_eq!(current.inference_profile_id, "other:inference");
    assert_eq!(current.context_id, None);
}

#[tokio::test]
async fn public_agent_management_previews_without_publishing_and_checks_signer() {
    let node = build_agent_node().await;
    let identity = agent_identity("public-agent-management");
    let owner = identity.did().to_owned();
    crate::test_support::install_test_agent(&node, &owner, "seed").await;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let mut args = ConfigureAgentParams {
        action: "preview".into(),
        operation: Some("create".into()),
        display_name: StringUpdate::Set("Public agent".into()),
        system_prompt: StringUpdate::Set("Complete the requested work.".into()),
        profile_id: StringUpdate::Set("seed:inference".into()),
        ..Default::default()
    };
    let preview: Value = serde_json::from_str(
        &configure_agent(
            &access,
            &owner,
            identity.as_ref(),
            &args,
            &Default::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["admitted"], true);
    assert_eq!(preview["committed"], false);
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap().len(), 1);
    args.action = "create".into();
    args.operation = None;
    let foreign = agent_identity("public-agent-foreign");
    let error = configure_agent(
        &access,
        &owner,
        foreign.as_ref(),
        &args,
        &Default::default(),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("exact local node identity"),
        "{error:#}"
    );
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap().len(), 1);
    let committed: Value = serde_json::from_str(
        &configure_agent(
            &access,
            &owner,
            identity.as_ref(),
            &args,
            &Default::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(committed["admitted"], true);
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["agent_id"], preview["agent_id"]);
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap().len(), 2);
}

#[test]
fn agent_decision_rejects_fields_outside_the_edit_contract() {
    let view = agent::AgentCatalogView {
        agents: BTreeMap::from([("target".into(), true)]),
        ..Default::default()
    };
    for field in ["enabled", "node_did", "context_id", "unexpected"] {
        let error = agent::decide_agent_operation(
            &view,
            &agent::AgentOperation::Edit(vec![(field.into(), Some(json!(true)))]),
            "target",
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsupported agent edit field"));
    }
}

#[tokio::test]
async fn public_agent_edit_preserves_disabled_state_and_rejects_clone_options() {
    let node = build_agent_node().await;
    let identity = agent_identity("disabled-agent-edit");
    let owner = identity.did().to_owned();
    crate::test_support::install_test_agent(&node, &owner, "target").await;
    let mut stored = crate::load_agent(&node, "target").await.unwrap().unwrap();
    stored.enabled = false;
    crate::upsert_agent(&node, &stored).await.unwrap();
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let mut args = ConfigureAgentParams {
        action: "preview".into(),
        operation: Some("edit".into()),
        agent_id: Some("target".into()),
        display_name: StringUpdate::Set("Edited while disabled".into()),
        ..Default::default()
    };
    let preview: Value = serde_json::from_str(
        &configure_agent(
            &access,
            &owner,
            identity.as_ref(),
            &args,
            &Default::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["agent"]["enabled"], false);
    assert_eq!(
        crate::load_agent(&node, "target").await.unwrap(),
        Some(stored.clone())
    );
    args.action = "edit".into();
    args.operation = None;
    args.clone_from = Some("target".into());
    let error = configure_agent(
        &access,
        &owner,
        identity.as_ref(),
        &args,
        &Default::default(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("does not accept clone_from"));
    assert_eq!(
        crate::load_agent(&node, "target").await.unwrap(),
        Some(stored.clone())
    );
    args.clone_from = None;
    configure_agent(
        &access,
        &owner,
        identity.as_ref(),
        &args,
        &Default::default(),
    )
    .await
    .unwrap();
    stored.display_name = Some("Edited while disabled".into());
    assert_eq!(
        crate::load_agent(&node, "target").await.unwrap(),
        Some(stored.clone())
    );
    args.profile_id = StringUpdate::Set("missing-profile".into());
    assert!(configure_agent(
        &access,
        &owner,
        identity.as_ref(),
        &args,
        &Default::default()
    )
    .await
    .is_err());
    assert_eq!(
        crate::load_agent(&node, "target").await.unwrap(),
        Some(stored.clone())
    );
    args.profile_id = StringUpdate::Omitted;
    args.make_default = true;
    assert!(configure_agent(
        &access,
        &owner,
        identity.as_ref(),
        &args,
        &Default::default()
    )
    .await
    .is_err());
    assert_eq!(
        crate::load_agent(&node, "target").await.unwrap(),
        Some(stored)
    );
}

#[tokio::test]
async fn public_prompt_edits_require_an_existing_unshared_context_without_partial_writes() {
    let node = build_agent_node().await;
    let identity = agent_identity("prompt-context-ownership");
    let owner = identity.did().to_owned();
    for id in ["missing", "shared", "sibling", "private"] {
        crate::test_support::install_test_agent(&node, &owner, id).await;
    }
    let mut missing = crate::load_agent(&node, "missing").await.unwrap().unwrap();
    missing.context_id = None;
    crate::upsert_agent(&node, &missing).await.unwrap();
    let shared = crate::load_agent(&node, "shared").await.unwrap().unwrap();
    let mut sibling = crate::load_agent(&node, "sibling").await.unwrap().unwrap();
    sibling.context_id = shared.context_id;
    crate::upsert_agent(&node, &sibling).await.unwrap();
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let query = "query { Agent(order: {agent_id: ASC}) { agent_id display_name context_id inference_profile_id enabled } AgentContext(order: {context_id: ASC}) { context_id display_name system_prompt tools_id } }";
    let before = access.execute(query).await.unwrap();
    for (target, expected) in [
        ("missing", "agent has no context"),
        ("shared", "unshared context"),
    ] {
        for preview in [true, false] {
            let args = ConfigureAgentParams {
                action: if preview { "preview" } else { "edit" }.into(),
                operation: preview.then(|| "edit".into()),
                agent_id: Some(target.into()),
                display_name: StringUpdate::Set("Must not be published".into()),
                system_prompt: StringUpdate::Set("Replacement instructions".into()),
                ..Default::default()
            };
            let error = configure_agent(
                &access,
                &owner,
                identity.as_ref(),
                &args,
                &Default::default(),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(access.execute(query).await.unwrap(), before);
        }
    }
    let private = crate::load_agent(&node, "private").await.unwrap().unwrap();
    let args = ConfigureAgentParams {
        action: "edit".into(),
        agent_id: Some("private".into()),
        system_prompt: StringUpdate::Set("Private replacement instructions".into()),
        ..Default::default()
    };
    configure_agent(
        &access,
        &owner,
        identity.as_ref(),
        &args,
        &Default::default(),
    )
    .await
    .unwrap();
    let after = access.execute(query).await.unwrap();
    assert_eq!(after["data"]["Agent"], before["data"]["Agent"]);
    let mut expected_contexts = before["data"]["AgentContext"].clone();
    let changed = expected_contexts
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|context| context["context_id"].as_str() == private.context_id.as_deref())
        .unwrap();
    changed["system_prompt"] = json!("Private replacement instructions");
    assert_eq!(after["data"]["AgentContext"], expected_contexts);
}

#[test]
fn operator_grants_decode_and_preserve_main_bounds() {
    use super::ops::{guard_tools_keep_grants, OperatorGrants};
    let empty = json!({});
    let granted = json!({"self_config":{"enable_self_config":true,"enable_pack_install":true}});
    let malformed = json!({"self_config":{"unknown":true}});
    assert_eq!(
        OperatorGrants::from_tools_json(empty.as_object().unwrap()).unwrap(),
        OperatorGrants::default()
    );
    assert!(OperatorGrants::from_tools_json(malformed.as_object().unwrap()).is_err());
    assert!(guard_tools_keep_grants(
        &OperatorGrants::default(),
        None,
        granted.as_object().unwrap()
    )
    .is_err());
    assert!(guard_tools_keep_grants(
        &OperatorGrants::default(),
        Some(granted.as_object().unwrap()),
        granted.as_object().unwrap()
    )
    .is_ok());
    assert!(guard_tools_keep_grants(
        &OperatorGrants { pack_install: true },
        None,
        granted.as_object().unwrap()
    )
    .is_ok());
    assert!(guard_tools_keep_grants(
        &OperatorGrants::default(),
        Some(granted.as_object().unwrap()),
        empty.as_object().unwrap()
    )
    .is_ok());
}

async fn grant_pack_install_for_test(node: &defra_node::EmbeddedNode, owner: &str, tools_id: &str) {
    let value = json!({"node_did":owner,"tools_id":tools_id,"self_config":{"enable_self_config":true,"enable_pack_install":true}});
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::Tools,
            add: value.clone(),
            update: value,
        },
    ])
    .unwrap();
    crate::config_client::ConfigAccess::transact_local(node, None, "test.operator_grant", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn model_clone_is_atomically_grant_bounded_but_operator_clone_is_not() {
    let node = build_agent_node().await;
    let identity = agent_identity("clone-grant-bound");
    let owner = identity.did().to_owned();
    for id in ["worker", "source"] {
        crate::test_support::install_test_agent(&node, &owner, id).await;
    }
    grant_pack_install_for_test(&node, &owner, "source:tools").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "worker".into()).unwrap();
    let args = ConfigureAgentParams {
        action: "clone".into(),
        display_name: StringUpdate::Set("Bound clone".into()),
        clone_from: Some("source".into()),
        profile_id: StringUpdate::Set("source:inference".into()),
        ..Default::default()
    };
    let before = crate::list_agents(&node, &owner).await.unwrap();
    let preview: Value = serde_json::from_str(
        &agent_management::model_agent_preview(&core, &args)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["admitted"], false);
    assert!(preview["rejection"]
        .as_str()
        .unwrap()
        .contains("cannot be self-granted"));
    let error = agent_management::model_agent_mutate(&core, identity.as_ref(), &args)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot be self-granted"));
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap(), before);
    let holder = core.with_held_grants(ops::OperatorGrants { pack_install: true });
    let accepted: Value = serde_json::from_str(
        &agent_management::model_agent_mutate(&holder, identity.as_ref(), &args)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(accepted["committed"], true);
    let mut operator_args = args;
    operator_args.display_name = StringUpdate::Set("Operator clone".into());
    let operator: Value = serde_json::from_str(
        &configure_agent(
            &crate::config_client::ConfigAccess::Local(node.clone()),
            &owner,
            identity.as_ref(),
            &operator_args,
            &Default::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(operator["committed"], true);
}

#[tokio::test]
async fn model_reselection_and_existing_sibling_grants_use_invoker_bounds() {
    let node = build_agent_node().await;
    let identity = agent_identity("reselection-grant-bound");
    let owner = identity.did().to_owned();
    for id in ["worker", "sibling", "granted"] {
        crate::test_support::install_test_agent(&node, &owner, id).await;
    }
    grant_pack_install_for_test(&node, &owner, "granted:tools").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "sibling".into())
        .unwrap()
        .with_lockout_agent_id("worker".into());
    for preview in [true, false] {
        let request = protect_working_agent(agent_request(
            &core,
            vec![("context_id".into(), Some(json!("granted:context")))],
        ));
        let result = if preview {
            core.preview(request).await
        } else {
            core.apply(request).await
        };
        assert!(format!("{:#}", result.unwrap_err()).contains("cannot be self-granted"));
    }
    let holder = core.with_held_grants(ops::OperatorGrants { pack_install: true });
    holder
        .apply(protect_working_agent(agent_request(
            &holder,
            vec![("context_id".into(), Some(json!("granted:context")))],
        )))
        .await
        .unwrap();
    let no_grant = SelfConfigCore::new(node.clone(), owner, "sibling".into()).unwrap();
    no_grant
        .apply(tools_request(
            &no_grant,
            vec![("built_ins".into(), Some(json!({"enable_graph_tools":true})))],
        ))
        .await
        .unwrap();
    no_grant.apply(tools_request(&no_grant, vec![("self_config".into(), Some(json!({"enable_self_config":true,"enable_pack_install":true,"self_config_preview":true})))])).await.unwrap();
}
