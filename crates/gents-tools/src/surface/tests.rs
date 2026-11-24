use super::*;
use crate::plugin_host::{NoPluginHost, PluginHost};
use gents_documents::PluginToolRef;
use gents_pack::plugin::store::InstalledPlugin;
use gents_pack::plugin::PluginOutcome;
use identity::Did;

async fn selected_surface_runtime() -> (gents_store::test_support::TestStore, ToolRuntimeContext) {
    use gents_store::identity::NodeIdentity as _;
    let signed_home = gents_documents::test_support::SignedHome::start(
        "selected-surface",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, _user, _user_access) = signed_home.into_parts();
    let node = db.node.clone();
    let root = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let node_did = root.did().to_owned();
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let mut runtime = ToolRuntimeContext::oneshot_with_node_did(node, node_did);
    runtime.agent_identity = Some(agent);
    (db, runtime)
}

fn config_for_selected_surface(
    node_did: &str,
    entries: Vec<gents_documents::SurfaceToolDecl>,
) -> AgentToolSurfaceConfig {
    let tools = tools_document(serde_json::json!({"node_did":node_did,
        "datastore":{"datastore_tool_surface_ids":["test-surface"]}}));
    let surface: gents_documents::DatastoreToolSurfaceDocument =
        serde_json::from_value(serde_json::json!({
            "surface_id":"test-surface", "node_did":node_did, "entries":entries,
        }))
        .unwrap();
    AgentToolSurfaceConfig::from_tools_document_with_surfaces(
        "ops",
        &tools,
        &[surface],
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn empty_mcp_tool_selection_explains_why_no_tools_are_attached() {
    for style in [
        gents_documents::RemoteToolStyle::Discovery,
        gents_documents::RemoteToolStyle::Flat,
    ] {
        let config = AgentToolSurfaceConfig::from_selection(
            "worker",
            ResolvedToolSelection {
                enable_meta_tools: true,
                allowed_mcp_service_ids: vec!["research".into()],
                remote_tools: Some(gents_documents::RemoteTools {
                    services: vec![gents_documents::RemoteServiceTools {
                        mcp_service_id: "research".into(),
                        style,
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            },
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
        .unwrap();
        let explanation = config.explain_with_runtime(true, "did:test:owner", &Default::default());
        assert!(!explanation
            .tool_names
            .iter()
            .any(|name| gents_protocol::tool_names::META_TOOL_NAMES.contains(&name.as_str())));
        let warning = explanation
            .warnings
            .iter()
            .find(|warning| warning.code == "mcp_service_has_no_selected_tools")
            .expect("an available service with no selected names needs a selection diagnostic");
        assert!(warning.message.contains("research"));
        assert!(warning.message.contains("tool_names"));
        assert!(!explanation
            .warnings
            .iter()
            .any(|warning| warning.code == "meta_requested_no_online_mcp"));
    }
}

#[tokio::test]
async fn presented_mcp_tool_names_match_the_attached_tools() {
    use gents_documents::{RemoteServiceTools, RemoteToolStyle, RemoteTools};

    let (db, root) = crate::mcp::meta::tests::test_node().await;
    let node = db.node.clone();
    crate::mcp::meta::tests::seed_registry_service(&node, "research", "127.0.0.1", 9213).await;
    let node_did = node.node_identity_did().unwrap().to_string();
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let mut runtime = ToolRuntimeContext::oneshot_with_node_did(node.clone(), node_did.clone());
    runtime.agent_identity = Some(agent);

    let selection = |tool_names: Vec<&str>, background: Vec<&str>| RemoteServiceTools {
        mcp_service_id: "research".into(),
        tool_names: tool_names.into_iter().map(str::to_string).collect(),
        background_tool_names: background.into_iter().map(str::to_string).collect(),
        style: RemoteToolStyle::Discovery,
        ..Default::default()
    };

    for (case, services, attaches) in [
        (
            "selected names",
            vec![selection(vec!["search"], Vec::new())],
            true,
        ),
        (
            "omitted style selects discovery",
            vec![serde_json::from_value(serde_json::json!({
                "mcp_service_id": "research",
                "tool_names": ["search"]
            }))
            .unwrap()],
            true,
        ),
        (
            "no selected names",
            vec![selection(Vec::new(), Vec::new())],
            false,
        ),
        (
            "duplicate service reference",
            vec![
                selection(vec!["search"], Vec::new()),
                selection(vec!["search"], Vec::new()),
            ],
            false,
        ),
        (
            "background name outside the selection",
            vec![selection(vec!["search"], vec!["sweep"])],
            false,
        ),
    ] {
        let surface = AgentToolSurfaceConfig::from_selection(
            "worker",
            ResolvedToolSelection {
                enable_meta_tools: true,
                allowed_mcp_service_ids: vec!["research".into()],
                remote_tools: Some(RemoteTools { services }),
                ..Default::default()
            },
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
        .unwrap()
        .resolve(&node, &node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();

        let names = surface.tool_names();
        let built = surface.build_tools(&runtime).await.unwrap();
        for meta in gents_protocol::tool_names::META_TOOL_NAMES {
            let presented = names.iter().any(|name| name == meta);
            let attached = built.iter().any(|tool| tool.name() == meta);
            assert_eq!(
                presented, attached,
                "{case}: advertised {meta} must match the attached tool"
            );
            assert_eq!(attached, attaches, "{case}: {meta} attachment");
        }
    }
}

fn temp_root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn fingerprint_test_config(root: PathBuf, lsp_config: Option<String>) -> AgentToolSurfaceConfig {
    let enable_lsp = lsp_config.is_some();
    let file_tools = if enable_lsp {
        FileToolMode::ReadWrite
    } else {
        FileToolMode::Off
    };
    AgentToolSurfaceConfig::from_selection(
        "fingerprint",
        ResolvedToolSelection {
            file_tools,
            enable_lsp,
            lsp_config,
            ..ResolvedToolSelection::default()
        },
        &ToolCeiling::readwrite(root),
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn agent_and_effective_surface_fingerprints_include_lsp_configuration() {
    let root = temp_root("gents-fingerprint-lsp-tools");
    let baseline = fingerprint_test_config(root.clone(), Some("{}".to_string()));
    let configured = fingerprint_test_config(root, Some(r#"{"format_on_write":true}"#.to_string()));

    assert_ne!(format!("{baseline:?}"), format!("{configured:?}"));
    assert_ne!(
        format!(
            "{:?}",
            baseline.resolve_with_agent_tools_for_runtime_availability(
                RuntimeToolAvailability::all(),
                AgentToolConfig::default(),
            )
        ),
        format!(
            "{:?}",
            configured.resolve_with_agent_tools_for_runtime_availability(
                RuntimeToolAvailability::all(),
                AgentToolConfig::default(),
            )
        ),
    );
}

#[test]
fn command_timeout_ceiling_reaches_selected_bash_tool() {
    let operator_root = temp_root("gents-command-timeout-root");
    let ceiling = ToolCeiling::readonly_at(&operator_root).with_command_timeout_secs(120);
    let config = AgentToolSurfaceConfig::from_selection(
        "classifier",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ceiling,
        Vec::new(),
    )
    .unwrap();

    assert!(matches!(
        config.host_tools().native_tools(),
        [crate::native::NativeTool::BashReadOnly { timeout, .. }]
            if timeout.as_secs() == 120
    ));
}

#[test]
fn command_timeout_max_ceiling_reaches_selected_bash_tool() {
    let operator_root = temp_root("gents-command-timeout-max-root");
    let ceiling = ToolCeiling::readonly_at(&operator_root)
        .with_command_timeout_secs(600)
        .with_command_timeout_max_secs(3_600);
    let config = AgentToolSurfaceConfig::from_selection(
        "classifier",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ceiling,
        Vec::new(),
    )
    .unwrap();

    assert!(matches!(
        config.host_tools().native_tools(),
        [crate::native::NativeTool::BashReadOnly { timeout, timeout_max, .. }]
            if timeout.as_secs() == 600 && timeout_max.as_secs() == 3_600
    ));
}

#[test]
fn downgraded_off_selection_ignores_stale_file_tool_root() {
    let stale_root =
        std::env::temp_dir().join(format!("gents-stale-root-{}", uuid::Uuid::new_v4()));

    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: Some(stale_root),
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();

    assert!(config.host_tools().native_tools().is_empty());
    assert!(config.host_tools().build_native_tools().unwrap().is_empty());
}

#[test]
fn readonly_ceiling_clamps_unrestricted_background_bash_to_registered_tool() {
    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::ReadWrite,
            file_tool_root: None,
            bash: BashMode::Unrestricted,
            command_policy: Some(
                crate::native::CommandExecutionPolicy::write_capable()
                    .with_mode(crate::native::CommandExecutionMode::Unrestricted),
            ),
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ToolCeiling::readonly(),
        Vec::new(),
    )
    .unwrap();

    assert_eq!(config.host_tools(), &crate::native::ToolSet::readonly());
}

#[test]
fn selection_cli_tools_require_ceiling_entries() {
    let operator_root = temp_root("gents-operator-root");

    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: vec!["rg".to_string()],
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ToolCeiling::readwrite(operator_root),
        Vec::new(),
    )
    .unwrap();

    assert!(!config.host_tools().tool_names().contains(&"rg".to_string()));
    assert!(config
        .host_tools()
        .native_tools()
        .iter()
        .all(|tool| !matches!(tool, crate::native::NativeTool::Cli(_))));
}

#[test]
fn selection_cli_tools_expose_only_ceiling_entries() {
    let operator_root = temp_root("gents-operator-root");
    let ceiling = ToolCeiling::readwrite(operator_root).with_cli_tool(cli_tool(
        "rg",
        "/usr/bin/rg",
        "Search files with ripgrep",
    ));

    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: vec!["rg".to_string(), "cargo".to_string()],
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ceiling,
        Vec::new(),
    )
    .unwrap();

    let tool_names = config.host_tools().tool_names();
    assert!(tool_names.contains(&"rg".to_string()));
    assert!(!tool_names.contains(&"cargo".to_string()));
    assert_eq!(
        config
            .host_tools()
            .native_tools()
            .iter()
            .filter(|tool| matches!(tool, crate::native::NativeTool::Cli(_)))
            .count(),
        1
    );
}

#[test]
fn selection_mcp_service_allowlist_is_deduped() {
    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: true,
            enable_goal_tools: true,
            enable_goal_creation: false,
            allowed_mcp_service_ids: vec![
                "x-data".to_string(),
                "x-data".to_string(),
                "observability-mcp".to_string(),
            ],
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();

    assert_eq!(
        config.allowed_mcp_service_ids(),
        &["x-data".to_string(), "observability-mcp".to_string()]
    );
}

#[tokio::test]
async fn required_mcp_service_needs_agent_scoped_measured_availability() {
    let (db, _runtime) = crate::mcp::meta::tests::test_node().await;
    let node = db.node.clone();
    crate::mcp::meta::tests::seed_registry_service(&node, "research", "127.0.0.1", 9213).await;
    let node_did = node.node_identity_did().unwrap();
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node_did).unwrap(),
    );

    let config = AgentToolSurfaceConfig::from_selection(
        "investigator",
        ResolvedToolSelection {
            enable_meta_tools: true,
            enable_goal_tools: true,
            enable_goal_creation: false,
            allowed_mcp_service_ids: vec!["research".to_string()],
            required_mcp_service_ids: vec!["research".to_string()],
            remote_tools: Some(gents_documents::RemoteTools {
                services: vec![gents_documents::RemoteServiceTools {
                    mcp_service_id: "research".into(),
                    tool_names: vec!["search".into()],
                    required: true,
                    ..Default::default()
                }],
            }),
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();
    let active = rapidhash::RapidHashSet::from_iter(["investigator".to_string()]);
    let error = config
        .resolve_with_available_agent_targets(
            &node,
            node_did,
            &active,
            &crate::plugin_host::NoPluginHost,
        )
        .await
        .expect_err("missing health measurement must quarantine the agent");
    assert!(error.to_string().contains("not measured available"));

    let escaped = gents_store::graphql::escape_graphql_string(node_did);
    let health = format!(
        r#"mutation {{ create_ToolServiceHealthState(input: {{ service_id: "research", node_did: "{escaped}", endpoint: "http://127.0.0.1:9213/mcp", status: "healthy", tool_count: 2 }}) {{ _docID }} }}"#
    );
    access
        .transact_write("test.mcp.health", |txn| {
            let health = &health;
            Box::pin(async move {
                let response = txn.execute(health).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "ToolServiceHealthState",
                )?;
                gents_documents::publish_node_child_in_txn(
                    txn,
                    "ToolServiceHealthState",
                    &doc,
                    node_did,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let surface = config
        .resolve_with_available_agent_targets(
            &node,
            node_did,
            &active,
            &crate::plugin_host::NoPluginHost,
        )
        .await
        .expect("healthy required service makes the agent resolvable");
    assert!(surface.includes_meta_tools());
    assert_eq!(surface.allowed_mcp_service_ids(), &["research".to_string()]);

    access.write("test.mcp.change_endpoint", r#"mutation { update_ToolServiceRegistry(filter: { service_id: { _eq: "research" } }, input: { mcp_port: 9214 }) { _docID } }"#).await.unwrap();
    let error = config
        .resolve_with_available_agent_targets(
            &node,
            node_did,
            &active,
            &crate::plugin_host::NoPluginHost,
        )
        .await
        .expect_err("health for an old endpoint must not satisfy current registry config");
    assert!(error.to_string().contains("not measured available"));
}

#[test]
fn background_tool_allowlist_registers_r6_tools() {
    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: None,
            bash: BashMode::Unrestricted,
            command_policy: None,
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: vec![
                "bash_unrestricted".to_string(),
                "bash_unrestricted".to_string(),
            ],
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ToolCeiling::readonly(),
        Vec::new(),
    )
    .unwrap();

    assert_eq!(
        config.background_tools().allowlist,
        vec!["bash".to_string()]
    );
}

#[test]
fn background_tool_allowlist_rejects_non_backgroundable_tools() {
    let error = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: None,
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: vec!["read_file".to_string()],
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ToolCeiling::readonly(),
        Vec::new(),
    )
    .expect_err("read_file is intentionally not backgroundable");

    assert!(
        error
            .to_string()
            .contains("not a registered backgroundable tool"),
        "{error:#}"
    );
}

#[cfg(unix)]
#[test]
fn selection_file_tool_root_rejects_symlink_escape_for_missing_child() {
    let operator_root = temp_root("gents-operator-root");
    let outside_root = temp_root("gents-outside-root");
    let symlink_path = operator_root.join("link-out");
    std::os::unix::fs::symlink(&outside_root, &symlink_path).unwrap();

    let error = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            hidden_tool_names: Vec::new(),
            file_tools: FileToolMode::ReadWrite,
            file_tool_root: Some(symlink_path.join("workspace")),
            bash: BashMode::Unrestricted,
            command_policy: None,
            cli_tool_names: Vec::new(),
            command_output_limits: Default::default(),
            timeouts: Default::default(),
            file_limits: Default::default(),
            enable_meta_tools: false,
            enable_goal_tools: false,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_preview: false,
            enable_pack_install: false,
            enable_pack_authority: false,
            enable_lsp: false,
            lsp_config: None,
            plugin_tools: Vec::new(),
            remote_tools: None,
            max_instruction_chars: gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS,
            background_limit: Default::default(),
        },
        &ToolCeiling::readwrite(operator_root),
        Vec::new(),
    )
    .expect_err("selection root through symlink should fail");

    assert!(
        error.to_string().contains("escapes operator tool root"),
        "{error:#}"
    );
}

#[tokio::test]
async fn defra_query_tool_gated_by_selection() {
    let node_db = gents_store::test_support::TestStore::start(
        "defra-query-tool-gated-by-selection",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();

    let enabled = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_defra_query: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(enabled.tool_names().contains(&"query".to_string()));

    let disabled = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_defra_query: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(!disabled.tool_names().contains(&"query".to_string()));
}

#[tokio::test]
async fn context_budget_tool_gated_by_selection() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();

    let enabled = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_context_budget: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(enabled.tool_names().contains(&"context_budget".to_string()));

    let disabled = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_context_budget: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(!disabled
        .tool_names()
        .contains(&"context_budget".to_string()));

    let built = disabled.build_tools(&runtime).await.unwrap();
    assert!(!built.iter().any(|tool| tool.name() == "context_budget"));
}

#[tokio::test]
async fn write_tools_register_under_declared_names() {
    use gents_documents::{WriteToolDecl, WriteToolField};

    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    node.add_collection("type ActionRequest { summary: String }")
        .await
        .unwrap();

    let surface = config_for_selected_surface(
        &runtime.node_did,
        vec![gents_documents::SurfaceToolDecl::Create(WriteToolDecl {
            notification: None,
            tool_name: "request_action".to_string(),
            collection: "ActionRequest".to_string(),
            description: "Request an action".to_string(),
            fields: vec![WriteToolField {
                name: "summary".to_string(),
                required: true,
                fill: None,
            }],
            output_obligation: None,
        })],
    )
    .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
    .await
    .unwrap();

    let names = surface.tool_names();
    assert!(
        names.contains(&"request_action".to_string()),
        "declared write tool should be advertised under its tool_name; got {names:?}"
    );

    let built = surface.build_tools(&runtime).await.unwrap();
    assert!(
        built.iter().any(|tool| tool.name() == "request_action"),
        "registered dynamic tool should advertise decl.tool_name"
    );
}

#[tokio::test]
async fn write_tool_whose_count_field_resolves_no_schema_is_refused_at_registration() {
    use gents_documents::{
        WriteToolDecl, WriteToolField, WriteToolOutputObligation, WriteToolOutputObligationScope,
    };

    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    node.add_collection(
        "type TallyOwner { label: String tallies: [Tally] } type Tally { label: String owner: TallyOwner @primary }",
    )
        .await
        .unwrap();

    let surface = config_for_selected_surface(
        &runtime.node_did,
        vec![gents_documents::SurfaceToolDecl::Create(WriteToolDecl {
            notification: None,
            tool_name: "record_tally".to_string(),
            collection: "Tally".to_string(),
            description: "Record one tally".to_string(),
            fields: vec![WriteToolField {
                name: "owner".to_string(),
                required: true,
                fill: None,
            }],
            output_obligation: Some(WriteToolOutputObligation {
                scope: WriteToolOutputObligationScope::Request,
                minimum_writes: 1,
                expected_count_field: Some("owner".to_string()),
            }),
        })],
    )
    .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
    .await
    .unwrap();

    let error = match surface.build_tools(&runtime).await {
        Ok(_) => panic!("a relation-typed count field must not reach a registered tool"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("unsupported bounded write field type"),
        "{error:#}"
    );
}

#[tokio::test]
async fn plugin_only_bindings_are_captured_for_trigger_execution() {
    use gents_documents::{PluginToolRef, WriteToolField, WriteToolFieldFill};

    let node_db = gents_store::test_support::TestStore::start(
        "plugin-only-bindings-are-captured-for-tr",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let selected = AgentToolSurfaceConfig::from_selection(
        "evidence",
        ResolvedToolSelection {
            plugin_tools: vec![PluginToolRef {
                tool_calls: false,
                plugin: "team/page_reader".into(),
                digest: None,
                input_fields: vec![
                    WriteToolField {
                        name: "command".into(),
                        required: false,
                        fill: Some(WriteToolFieldFill::SourceField("page_command".into())),
                    },
                    WriteToolField {
                        name: "path".into(),
                        required: false,
                        fill: Some(WriteToolFieldFill::SourceField("book_path".into())),
                    },
                    WriteToolField {
                        name: "run_id".into(),
                        required: false,
                        fill: Some(WriteToolFieldFill::Correlation),
                    },
                ],
                job: None,
            }],
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert_eq!(
        selected.source_fill_fields(),
        ["book_path".to_string(), "page_command".to_string()]
            .into_iter()
            .collect()
    );
}

#[tokio::test]
async fn mailbox_surface_registers_stamped_tool_and_captures_owner_lineage() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();

    let granted = config_for_selected_surface(
        &runtime.node_did,
        vec![gents_documents::SurfaceToolDecl::Create(
            gents_documents::mailbox_write_decl::canonical_mailbox_write_decl(),
        )],
    )
    .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
    .await
    .unwrap();
    assert!(granted
        .tool_names()
        .contains(&gents_documents::mailbox_write_decl::FILE_MAILBOX_ITEM_TOOL_NAME.to_string()));
    assert!(granted.source_fill_fields().contains("requester_did"));
    let built = granted.build_tools(&runtime).await.unwrap();
    assert!(built.iter().any(
        |tool| tool.name() == gents_documents::mailbox_write_decl::FILE_MAILBOX_ITEM_TOOL_NAME
    ));

    let without_grant = AgentToolSurfaceConfig::meta_only()
        .resolve(
            &runtime.node,
            &runtime.node_did,
            &crate::plugin_host::NoPluginHost,
        )
        .await
        .unwrap();
    assert!(!without_grant
        .tool_names()
        .contains(&gents_documents::mailbox_write_decl::FILE_MAILBOX_ITEM_TOOL_NAME.to_string()));
    assert!(!without_grant.source_fill_fields().contains("requester_did"));
}

#[tokio::test]
async fn query_tool_is_advertised_and_registered() {
    use gents_documents::{QueryToolDecl, WriteToolField, WriteToolFieldFill};

    let db = gents_store::test_support::TestStore::start(
        "query-tool",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let identity = std::sync::Arc::new(gents_store::test_support::held_runtime_identity(
        db.node_identity.did(),
    ));
    let node_did = gents_store::identity::NodeIdentity::did(&*identity).to_owned();

    let surface = config_for_selected_surface(
        &node_did,
        vec![gents_documents::SurfaceToolDecl::Query(QueryToolDecl {
            tool_name: "query_candidate_finding".to_string(),
            collection: "CandidateFinding".to_string(),
            description: "Load candidates".to_string(),
            fields: vec!["finding_id".to_string()],
            filter_fields: vec![WriteToolField {
                name: "run_id".to_string(),
                required: false,
                fill: Some(WriteToolFieldFill::Correlation),
            }],
        })],
    )
    .resolve(&node, &node_did, &crate::plugin_host::NoPluginHost)
    .await
    .unwrap();

    let names = surface.tool_names();
    assert!(
        names.contains(&"query_candidate_finding".to_string()),
        "declared query tool should be advertised; got {names:?}"
    );
    assert!(
        !names.contains(&"query".to_string()),
        "bound query tools must not imply the generic defra_query console"
    );

    let mut runtime = ToolRuntimeContext::oneshot_with_node_did(node, node_did);
    let error = match surface.build_tools(&runtime).await {
        Ok(_) => panic!("query requires its selected agent identity"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("selected agent identity"));
    runtime.agent_identity = Some(identity.clone());
    let error = match surface.build_tools(&runtime).await {
        Ok(_) => panic!("node identity cannot substitute for selected agent identity"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("cannot use the runtime node identity"));
    let binding = gents_store::identity::provision_agent_identity(identity.as_ref()).unwrap();
    runtime.agent_identity =
        Some(gents_store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap());
    let built = surface.build_tools(&runtime).await.unwrap();
    assert!(built
        .iter()
        .any(|tool| tool.name() == "query_candidate_finding"));
}

#[test]
fn document_tool_config_expands_linked_datastore_surfaces() {
    use gents_documents::{DatastoreToolSurfaceDocument, QueryToolDecl, SurfaceToolDecl};

    let node_did = "did:key:zToolExplainSurface";
    let selection = tools_document(serde_json::json!({
        "node_did": node_did, "datastore": {"datastore_tool_surface_ids":["research-reads"]}
    }));
    let surfaces = vec![DatastoreToolSurfaceDocument {
        surface_id: "research-reads".to_string(),
        tags: Vec::new(),
        node_did: node_did.to_string(),
        display_name: None,
        enabled: true,
        entries: Some(vec![SurfaceToolDecl::Query(QueryToolDecl {
            tool_name: "read_research_source".to_string(),
            collection: "WebResearchSource".to_string(),
            description: "Read sources for this run".to_string(),
            fields: vec!["source_id".to_string()],
            filter_fields: Vec::new(),
        })]),
        created_at: None,
    }];

    let missing = AgentToolSurfaceConfig::from_tools_document(
        "reporter",
        &selection,
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(missing
        .to_string()
        .contains("missing same-agent DatastoreToolSurface"));

    let config = AgentToolSurfaceConfig::from_tools_document_with_surfaces(
        "reporter",
        &selection,
        &surfaces,
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();
    let explanation =
        config.explain_with_runtime(false, node_did, &rapidhash::RapidHashSet::default());
    assert_eq!(
        explanation.tool_names,
        vec![
            gents_protocol::tool_names::SESSION_HISTORY_TOOL_NAME,
            "read_research_source"
        ]
    );
}

#[test]
fn malformed_write_tool_is_rejected_during_configuration() {
    use gents_documents::WriteToolDecl;

    let error = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            write_tools: vec![WriteToolDecl {
                notification: None,
                tool_name: "broken_tool".to_string(),
                collection: "  ".to_string(),
                description: String::new(),
                fields: Vec::new(),
                output_obligation: None,
            }],
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("malformed"));
}

#[test]
fn write_tool_colliding_with_builtin_is_rejected_during_configuration() {
    use gents_documents::{WriteToolDecl, WriteToolField};

    let error = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_defra_query: false,
            write_tools: vec![WriteToolDecl {
                notification: None,
                tool_name: "context_budget".to_string(),
                collection: "ActionRequest".to_string(),
                description: String::new(),
                fields: vec![WriteToolField {
                    name: "summary".to_string(),
                    required: false,
                    fill: None,
                }],
                output_obligation: None,
            }],
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("collides with a built-in"));
}

#[test]
fn default_surface_enables_session_reads_and_disables_goal_creation() {
    assert!(ResolvedToolSelection::default().enable_session_history_tool);
    assert!(!AgentToolSurfaceConfig::meta_only().goal_creation_requested());
    assert!(!AgentToolSurfaceConfig::meta_only()
        .static_policy()
        .include_goal_creation());
}

#[tokio::test]
async fn explicitly_disabling_goal_tools_removes_them_from_names_and_runtime() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let surface = AgentToolSurfaceConfig::from_selection(
        "benchmark",
        ResolvedToolSelection {
            enable_meta_tools: true,
            enable_goal_tools: false,
            enable_goal_creation: false,
            enable_context_budget: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();

    let names = surface.tool_names();
    assert!(!names.contains(&gents_protocol::goal_names::GET_GOAL_TOOL_NAME.to_string()));
    assert!(!names.contains(&gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME.to_string()));

    let built_names = surface
        .build_tools(&runtime)
        .await
        .unwrap()
        .into_iter()
        .map(|tool| tool.name())
        .collect::<Vec<_>>();
    assert!(!built_names.contains(&gents_protocol::goal_names::GET_GOAL_TOOL_NAME.to_string()));
    assert!(!built_names.contains(&gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME.to_string()));
}

#[tokio::test]
async fn goal_tools_are_independent_from_generic_meta_tools() {
    let node_db = gents_store::test_support::TestStore::start(
        "goal-tools-are-independent-from-generic-",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let surface = AgentToolSurfaceConfig::from_selection(
        "durable",
        ResolvedToolSelection {
            enable_meta_tools: false,
            enable_goal_tools: true,
            enable_goal_creation: false,
            enable_context_budget: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();

    let names = surface.tool_names();
    assert!(!names.iter().any(|name| name == "discover_tools"));
    assert!(names.contains(&gents_protocol::goal_names::GET_GOAL_TOOL_NAME.to_string()));
    assert!(names.contains(&gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME.to_string()));
    assert!(!names.contains(&gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME.to_string()));
}

#[tokio::test]
async fn goal_creation_requires_its_separate_capability() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();

    let without_create = AgentToolSurfaceConfig::from_selection(
        "durable",
        ResolvedToolSelection {
            enable_goal_tools: true,
            enable_goal_creation: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(!without_create
        .tool_names()
        .contains(&gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME.to_string()));

    let with_create = AgentToolSurfaceConfig::from_selection(
        "durable",
        ResolvedToolSelection {
            enable_goal_tools: true,
            enable_goal_creation: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(with_create
        .tool_names()
        .contains(&gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME.to_string()));
    let built_names = with_create
        .build_tools(&runtime)
        .await
        .unwrap()
        .into_iter()
        .map(|tool| tool.name())
        .collect::<Vec<_>>();
    assert!(built_names.contains(&gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME.to_string()));
}

#[tokio::test]
async fn operator_ceiling_can_deny_goal_creation_only() {
    let node_db = gents_store::test_support::TestStore::start(
        "operator-ceiling-can-deny-goal-creation-",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let mut ceiling_policy = ToolPolicySurface::runtime_all();
    ceiling_policy.goal_create = false;
    let ceiling = ToolCeiling::meta_only().with_policy(ceiling_policy);
    let surface = AgentToolSurfaceConfig::from_selection(
        "durable",
        ResolvedToolSelection {
            enable_goal_tools: true,
            enable_goal_creation: true,
            ..Default::default()
        },
        &ceiling,
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();

    assert!(surface.includes_goal_tools());
    assert!(!surface.includes_goal_creation());
    assert!(!surface
        .tool_names()
        .contains(&gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME.to_string()));
}

#[tokio::test]
async fn operator_ceiling_denies_all_goal_mutation_when_base_capability_is_off() {
    let node_db = gents_store::test_support::TestStore::start(
        "operator-ceiling-denies-all-goal-mutatio",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let mut ceiling_policy = ToolPolicySurface::runtime_all();
    ceiling_policy.goal_tools = false;
    let ceiling = ToolCeiling::meta_only().with_policy(ceiling_policy);
    let surface = AgentToolSurfaceConfig::from_selection(
        "durable",
        ResolvedToolSelection {
            enable_goal_tools: true,
            enable_goal_creation: true,
            ..Default::default()
        },
        &ceiling,
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();

    assert!(!surface.includes_goal_tools());
    for denied in [
        gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
        gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
    ] {
        assert!(!surface.tool_names().contains(&denied.to_string()));
    }
}

#[test]
fn defra_query_deny_all_collection_scope_gates_tool_off() {
    use super::policy::{EndpointScope, ToolPolicySurface};

    let mut agent = ToolPolicySurface::runtime_all();
    agent.defra_collections = EndpointScope::<String, ()>::only_units(["a".to_string()]);
    let mut ceiling = ToolPolicySurface::runtime_all();
    ceiling.defra_collections = EndpointScope::<String, ()>::only_units(["b".to_string()]);

    let effective = agent.meet(&ceiling);
    assert!(
        effective.defra_collections.is_deny_all(),
        "disjoint collection scopes must meet to Only(empty) = deny-all"
    );
    assert!(effective.defra_query);
    assert!(
        !effective.include_defra_query(),
        "deny-all collection scope must gate the defra_query tool off, not surface it as allow-all"
    );
    assert!(
        effective.defra_query_collections_for_runtime().is_empty(),
        "deny-all and all both project to an empty list; the gate is what distinguishes them"
    );

    let mut all_ceiling = ToolPolicySurface::runtime_all();
    all_ceiling.defra_collections = EndpointScope::all();
    let permissive = agent.meet(&all_ceiling);
    assert!(permissive.include_defra_query());
    assert_eq!(
        permissive.defra_query_collections_for_runtime(),
        vec!["a".to_string()]
    );
}

#[test]
fn query_decls_for_runtime_drops_tool_when_filled_filter_would_be_stripped() {
    use std::collections::{BTreeMap, BTreeSet};

    use super::policy::{EndpointScope, ToolPolicySurface};
    use gents_documents::{QueryToolDecl, WriteToolField, WriteToolFieldFill};

    let decl = QueryToolDecl {
        tool_name: "query_candidate_finding".to_string(),
        collection: "CandidateFinding".to_string(),
        description: "Load candidates".to_string(),
        fields: vec!["finding_id".to_string(), "title".to_string()],
        filter_fields: vec![WriteToolField {
            name: "run_id".to_string(),
            required: false,
            fill: Some(WriteToolFieldFill::Correlation),
        }],
    };
    let mut policy = ToolPolicySurface::runtime_all();
    let mut grants = BTreeMap::new();
    grants.insert(
        (
            "query_candidate_finding".to_string(),
            "CandidateFinding".to_string(),
        ),
        BTreeSet::from(["finding_id".to_string(), "title".to_string()]),
    );
    policy.query_tools = EndpointScope::Only(grants);

    assert!(
        policy
            .query_decls_for_runtime(std::slice::from_ref(&decl))
            .is_empty(),
        "narrowing away a correlation filter must drop the tool, not un-scope the read"
    );

    let mut lossless = ToolPolicySurface::runtime_all();
    let mut full = BTreeMap::new();
    full.insert(
        (
            "query_candidate_finding".to_string(),
            "CandidateFinding".to_string(),
        ),
        BTreeSet::from([
            "finding_id".to_string(),
            "title".to_string(),
            "run_id".to_string(),
        ]),
    );
    lossless.query_tools = EndpointScope::Only(full);
    let kept = lossless.query_decls_for_runtime(std::slice::from_ref(&decl));
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].filter_fields.len(), 1);

    let mut write_all = ToolPolicySurface::runtime_all();
    write_all.write_tools = EndpointScope::all();
    write_all.query_tools = EndpointScope::none();
    assert!(
        write_all.query_decls_for_runtime(&[decl]).is_empty(),
        "write_tools: All must not grant bound read tools"
    );
}

#[test]
fn from_document_read_only_allowlist_override_and_fallback() {
    let mut doc = preset_tools_document("readonly-custom", "Off", "ReadOnly", false, false);
    doc.host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .read_only_commands = Some(vec!["cat".to_string()]);
    let selection = ResolvedToolSelection::from_document(&doc).unwrap();
    let policy = selection
        .command_policy
        .expect("custom read-only allowlist must produce Some(command_policy)");
    assert_eq!(policy.read_only_allowlist(), ["cat".to_string()]);

    let mut absent = doc.clone();
    absent
        .host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .read_only_commands = None;
    assert!(ResolvedToolSelection::from_document(&absent)
        .unwrap()
        .command_policy
        .is_none());

    let mut empty = doc;
    empty
        .host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .read_only_commands = Some(Vec::new());
    assert!(ResolvedToolSelection::from_document(&empty)
        .unwrap()
        .command_policy
        .is_none());
}

#[test]
fn absent_groups_grant_nothing_and_goal_controls_are_independent() {
    let absent =
        ResolvedToolSelection::from_document(&tools_document(serde_json::json!({}))).unwrap();
    assert!(!absent.enable_meta_tools);
    assert!(!absent.enable_goal_tools);
    assert!(!absent.enable_goal_creation);
    assert!(!absent.enable_defra_query);
    let independent = ResolvedToolSelection::from_document(&tools_document(serde_json::json!({
        "built_ins": {"enable_goal_tools":true,"enable_goal_creation":true}
    })))
    .unwrap();
    assert!(!independent.enable_meta_tools);
    assert!(independent.enable_goal_tools);
    assert!(independent.enable_goal_creation);
    assert!(
        serde_json::from_value::<gents_documents::Tools>(serde_json::json!({
            "tools_id":"legacy", "node_did":"did:test:test", "tool_policy_version":"v1"
        }))
        .is_err()
    );
}

#[tokio::test]
async fn session_history_tool_follows_selection() {
    let node_db = gents_store::test_support::TestStore::start(
        "session-history-tool-follows-selection",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();

    let disabled = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_session_history_tool: false,
            enable_schema_tool: false,
            enable_p2p_tool: false,
            enable_p2p_mutations: false,
            enable_context_budget: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(!disabled
        .tool_names()
        .contains(&gents_protocol::tool_names::SESSION_HISTORY_TOOL_NAME.to_string()));

    let enabled = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_session_history_tool: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();
    assert!(enabled
        .tool_names()
        .contains(&gents_protocol::tool_names::SESSION_HISTORY_TOOL_NAME.to_string()));
}

#[test]
fn tools_document_and_resolved_selection_agree_on_needing_a_filesystem_root() {
    for (groups, expected) in [
        (serde_json::json!({}), false),
        (serde_json::json!({"host": {}}), false),
        (
            serde_json::json!({"host": {"files": {"mode": "Off"}, "bash": {"mode": "Off"}}}),
            false,
        ),
        (
            serde_json::json!({"host": {"files": {"mode": "ReadOnly"}}}),
            true,
        ),
        (
            serde_json::json!({"host": {"bash": {"mode": "ReadOnly"}}}),
            true,
        ),
        (
            serde_json::json!({"host": {"cli": [{"name": "git"}]}}),
            true,
        ),
        (serde_json::json!({"integrations": {"lsp": {}}}), true),
    ] {
        let tools = tools_document(groups.clone());
        assert_eq!(tools.requires_filesystem_root(), expected, "{groups}");
        assert_eq!(
            ResolvedToolSelection::from_document(&tools)
                .unwrap()
                .requires_filesystem_root(),
            expected,
            "{groups}"
        );
    }
}

fn tools_document(mut value: serde_json::Value) -> gents_documents::Tools {
    let fields = value.as_object_mut().unwrap();
    fields
        .entry("tools_id")
        .or_insert(serde_json::json!("test-tools"));
    fields
        .entry("node_did")
        .or_insert(serde_json::json!("did:test:test"));
    serde_json::from_value(value).unwrap()
}

fn preset_tools_document(
    name: &str,
    file_mode: &str,
    bash_mode: &str,
    background: bool,
    query: bool,
) -> gents_documents::Tools {
    tools_document(serde_json::json!({
        "tools_id":format!("{name}-tools"), "display_name":name,
        "host": {"files":{"mode":file_mode},
            "bash":{"mode":bash_mode, "background_enabled": background}},
        "built_ins":{"enable_context_budget":true},
        "datastore":{"enable_defra_query":query}
    }))
}

async fn publish_test_root(
    node: &std::sync::Arc<defra_node::Node>,
    runtime: &ToolRuntimeContext,
    root: &str,
) -> (gents_store::config_client::ConfigAccess, String) {
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        runtime.node_did.parse().unwrap(),
    );
    let response = access.write("test.workspace.root", &format!(
        r#"mutation {{ create_WorkspaceRoot(input: {{node_did:"{}", root_path:"{}", enabled:true}}) {{_docID}} }}"#,
        gents_store::graphql::escape_graphql_string(&runtime.node_did),
        gents_store::graphql::escape_graphql_string(root),
    )).await.unwrap();
    let doc = gents_store::graphql::created_doc_id(&response, "WorkspaceRoot").unwrap();
    let agent = runtime.agent_identity.as_ref().unwrap();
    access
        .add_actor_relationship(
            "WorkspaceRoot",
            &doc,
            "reader",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    (access, doc)
}

#[tokio::test]
async fn tool_construction_rechecks_current_workspace_root_policy() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let ceiling = tempfile::tempdir().unwrap();
    let selected = ceiling.path().join("selected");
    std::fs::create_dir_all(&selected).unwrap();
    let selected_text = selected.to_string_lossy().into_owned();
    let (access, root_doc) = publish_test_root(&node, &runtime, &selected_text).await;
    let agent = runtime.agent_identity.as_ref().unwrap();
    let mut document = preset_tools_document("runtime-root", "ReadOnly", "Off", false, false);
    document.host.as_mut().unwrap().root = Some(selected_text.clone());
    let config = AgentToolSurfaceConfig::from_tools_document(
        agent.did(),
        &document,
        &ToolCeiling::readonly_at(ceiling.path()),
        Vec::new(),
    )
    .unwrap();
    let surface = config
        .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();
    surface
        .build_tools(&runtime)
        .await
        .expect("enabled published root admits tool construction");

    let agent_did = agent.did().parse().unwrap();
    access
        .delete_actor_relationship("WorkspaceRoot", &root_doc, "reader", &agent_did)
        .await
        .unwrap();
    assert!(
        surface.build_tools(&runtime).await.is_err(),
        "Agent reader revocation must deny tool construction"
    );
    access
        .add_actor_relationship("WorkspaceRoot", &root_doc, "reader", &agent_did)
        .await
        .unwrap();
    let disabled = access.write("test.workspace.disable", &format!(
        r#"mutation {{ update_WorkspaceRoot(docID: "{}", input: {{enabled:false}}) {{_docID}} }}"#,
        gents_store::graphql::escape_graphql_string(&root_doc),
    )).await.unwrap();
    assert_eq!(
        gents_store::graphql::mutation_documents(&disabled, "update_WorkspaceRoot")
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    let error = match surface.build_tools(&runtime).await {
        Ok(_) => panic!("fresh tool construction must observe root revocation"),
        Err(error) => error,
    };
    // The refusal is the typed root refusal and names the root it could not
    // admit; a disabled root is the operator's switch, not the Agent's reader.
    let resolved = std::fs::canonicalize(&selected).unwrap();
    assert!(
        matches!(
            tool_root_refusal(error.as_ref()),
            Some(ToolRootRefusal::Denied { root, .. }) if root == &resolved
        ),
        "{error:#}"
    );
}

/// The reported failure: an Agent created on its own, with a read-only Tools
/// document and host tools under a published root it holds no `reader` on. The
/// tool surface cannot admit the root, and the refusal must name that cause —
/// not "tool surface is unavailable" — with the grant that fixes it, including
/// this node's `--home`, since a grant applied to another node's home leaves
/// this one exactly as it was.
#[tokio::test]
async fn tool_construction_without_the_root_reader_names_the_grant_that_fixes_it() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let ceiling = tempfile::tempdir().unwrap();
    let root = ceiling.path().join("shared-root");
    std::fs::create_dir_all(&root).unwrap();
    let root_text = root.to_string_lossy().into_owned();

    // Publish the root with no reader: the state an Agent created separately
    // from its Tools document is in.
    let publisher = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        runtime.node_did.parse().unwrap(),
    );
    let published = publisher
        .write(
            "test.workspace.root",
            &format!(
                r#"mutation {{ create_WorkspaceRoot(input: {{node_did:"{}", root_path:"{}", enabled:true}}) {{_docID}} }}"#,
                gents_store::graphql::escape_graphql_string(&runtime.node_did),
                gents_store::graphql::escape_graphql_string(&root_text),
            ),
        )
        .await
        .unwrap();
    assert!(
        gents_store::graphql::created_doc_id(&published, "WorkspaceRoot").is_ok(),
        "the root is published"
    );

    let home = tempfile::tempdir().unwrap();
    let home_text = std::fs::canonicalize(home.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let agent_did = runtime.agent_identity.as_ref().unwrap().did().to_owned();
    let document = preset_tools_document("reader-missing", "ReadOnly", "Off", false, false);
    let mut document = document;
    document.host.as_mut().unwrap().root = Some(root_text.clone());
    let config = AgentToolSurfaceConfig::from_tools_document(
        &agent_did,
        &document,
        &ToolCeiling::readonly_at(ceiling.path()),
        Vec::new(),
    )
    .unwrap();
    let surface = config
        .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();
    let mut guard = surface
        .root_execution_guard()
        .cloned()
        .expect("host tools carry a root guard");
    assert!(
        guard.node_home.is_none(),
        "the parsed document carries no home; the runtime supplies it"
    );
    // What `build_tools` does from the runtime's plugin host before it
    // validates: the home is the run's, not the document's.
    guard.node_home = Some(std::path::PathBuf::from(&home_text));
    let held = runtime.agent_identity.clone().unwrap();
    let error = guard
        .validate(&node, held.as_ref())
        .await
        .expect_err("an Agent with no reader on its root cannot admit it");
    let refusal = tool_root_refusal(error.as_ref()).expect("the refusal is typed");
    let ToolRootRefusal::ReaderMissing {
        root: named_root,
        home: _named_home,
        ..
    } = refusal
    else {
        panic!("a published root the Agent cannot read is the reader-missing cause: {error:#}");
    };
    assert_eq!(
        named_root.as_deref(),
        Some(std::path::Path::new(&root_text)),
        "the refusal names the published root the grant matches"
    );
    let refusal_text = error.to_string();
    assert_eq!(
        gents_protocol::node_readiness::workspace_root_reader_missing_message(
            &agent_did, &root_text, &home_text
        ),
        refusal_text,
        "the refusal states the grant, its path and the Agent"
    );
    assert!(
        refusal_text.contains(&format!("--home {home_text}")),
        "the grant names this node's home: {refusal_text}"
    );
    assert!(
        !refusal_text.eq("tool surface is unavailable"),
        "the generic wording is not the cause"
    );
    assert!(
        gents_protocol::node_readiness::unavailable_rejection_reason(&refusal_text)
            .is_some_and(|reason| reason
                == gents_protocol::node_readiness::AgentReadinessUnavailableReason::WorkspaceRootReaderMissing),
        "a rejection carrying this text reads back as the reader-missing cause: {refusal_text}"
    );
    node.shutdown().await.unwrap();
}

/// A grant on the root admits the same Tools the refusal came from, so the
/// named fix is the one that works.
#[tokio::test]
async fn granting_the_named_reader_admits_the_same_tools() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let ceiling = tempfile::tempdir().unwrap();
    let root = ceiling.path().join("shared-root");
    std::fs::create_dir_all(&root).unwrap();
    let root_text = root.to_string_lossy().into_owned();
    let (access, root_doc) = publish_test_root(&node, &runtime, &root_text).await;
    let agent = runtime.agent_identity.clone().unwrap();
    access
        .delete_actor_relationship(
            "WorkspaceRoot",
            &root_doc,
            "reader",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let mut document = preset_tools_document("reader-granted", "ReadOnly", "Off", false, false);
    document.host.as_mut().unwrap().root = Some(root_text.clone());
    let config = AgentToolSurfaceConfig::from_tools_document(
        agent.did(),
        &document,
        &ToolCeiling::readonly_at(ceiling.path()),
        Vec::new(),
    )
    .unwrap();
    let surface = config
        .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();
    let guard = surface.root_execution_guard().cloned().unwrap();
    let before = guard.validate(&node, agent.as_ref()).await;
    assert!(
        before.as_ref().is_err_and(|error| {
            tool_root_refusal(error.as_ref()).is_some_and(ToolRootRefusal::is_reader_missing)
        }),
        "without the reader the guard names the reader: {before:?}"
    );
    access
        .add_actor_relationship(
            "WorkspaceRoot",
            &root_doc,
            "reader",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    guard
        .validate(&node, agent.as_ref())
        .await
        .expect("the grant the refusal named admits the same root");
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn normal_load_rejects_active_blank_root_under_explicit_policy_without_ceiling() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let allowed = tempfile::tempdir().unwrap();
    publish_test_root(&node, &runtime, &allowed.path().to_string_lossy()).await;
    let agent = runtime.agent_identity.as_ref().unwrap();

    let document = preset_tools_document("legacy-blank-root", "ReadOnly", "Off", false, false);
    assert!(document
        .host
        .as_ref()
        .and_then(|host| host.root.as_ref())
        .is_none());
    let config = AgentToolSurfaceConfig::from_tools_document(
        agent.did(),
        &document,
        &ToolCeiling::readonly(),
        Vec::new(),
    )
    .unwrap();
    let surface = config
        .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();
    let error = match surface.build_tools(&runtime).await {
        Ok(_) => panic!("active legacy Tools must not bypass explicit WorkspaceRoot policy"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("no explicit root"), "{error:#}");
}

fn explanation_has_warning(explanation: &ToolSurfaceExplanation, code: &str) -> bool {
    explanation
        .warnings
        .iter()
        .any(|warning| warning.code == code)
}

fn explanation_category_contains(
    map: &std::collections::BTreeMap<String, Vec<String>>,
    category: &str,
    name: &str,
) -> bool {
    map.get(category)
        .is_some_and(|names| names.contains(&name.to_string()))
}

#[test]
fn explain_init_package_document_matrix_resolves_expected_surfaces() {
    struct Case {
        name: &'static str,
        selection: gents_documents::Tools,
        ceiling: ToolCeiling,
        mcp_services_online: bool,
        expected_tool_names: Vec<&'static str>,
        absent_tool_names: Vec<&'static str>,
        expected_warnings: Vec<&'static str>,
        host_ceiling_warning: bool,
    }

    let readonly_root = temp_root("gents-readonly-package-root");
    let write_root = temp_root("gents-write-package-root");
    let cases = vec![
        Case {
            name: "minimal",
            selection: preset_tools_document("minimal", "Off", "Off", false, false),
            ceiling: ToolCeiling::meta_only(),
            mcp_services_online: false,
            expected_tool_names: vec!["context_budget"],
            absent_tool_names: vec!["call_tool", "query", "read_file", "spawn_process"],
            expected_warnings: vec!["host_ceiling_not_global"],
            host_ceiling_warning: true,
        },
        Case {
            name: "introspection-offline",
            selection: preset_tools_document("introspection", "Off", "Off", false, true),
            ceiling: ToolCeiling::meta_only(),
            mcp_services_online: false,
            expected_tool_names: vec!["context_budget", "query"],
            absent_tool_names: vec!["call_tool", "read_file", "spawn_process"],
            expected_warnings: vec!["host_ceiling_not_global", "defra_query_empty_scope_all"],
            host_ceiling_warning: true,
        },
        Case {
            name: "introspection-online",
            selection: preset_tools_document("introspection", "Off", "Off", false, true),
            ceiling: ToolCeiling::meta_only(),
            mcp_services_online: true,
            expected_tool_names: vec!["context_budget", "query"],
            absent_tool_names: vec![
                "discover_tools",
                "call_tool",
                "read_file",
                "spawn_process",
                "agent_new",
            ],
            expected_warnings: vec!["host_ceiling_not_global", "defra_query_empty_scope_all"],
            host_ceiling_warning: true,
        },
        Case {
            name: "readonly-online",
            selection: preset_tools_document("readonly", "ReadOnly", "ReadOnly", false, false),
            ceiling: ToolCeiling::readonly_at(readonly_root),
            mcp_services_online: true,
            expected_tool_names: vec![
                "list_files",
                "read_file",
                "glob",
                "grep",
                "bash",
                "context_budget",
            ],
            absent_tool_names: vec![
                "discover_tools",
                "call_tool",
                "write_file",
                "bash_unrestricted",
                "spawn_process",
                "query",
            ],
            expected_warnings: vec![],
            host_ceiling_warning: false,
        },
        Case {
            name: "write-online",
            selection: preset_tools_document("write", "ReadWrite", "Unrestricted", true, false),
            ceiling: ToolCeiling::readwrite(write_root),
            mcp_services_online: true,
            expected_tool_names: vec![
                "list_files",
                "read_file",
                "glob",
                "grep",
                "write_file",
                "edit_file",
                "bash_unrestricted",
                "spawn_process",
                "wait_process",
                "context_budget",
            ],
            absent_tool_names: vec!["discover_tools", "call_tool", "bash", "agent_new", "query"],
            expected_warnings: vec![],
            host_ceiling_warning: false,
        },
    ];

    for case in cases {
        let config = AgentToolSurfaceConfig::from_tools_document(
            case.name,
            &case.selection,
            &case.ceiling,
            Vec::new(),
        )
        .unwrap();
        let explanation = config.explain_with_runtime(
            case.mcp_services_online,
            "did:test:test",
            &rapidhash::RapidHashSet::default(),
        );

        for name in case.expected_tool_names {
            assert!(
                explanation.tool_names.contains(&name.to_string()),
                "{} should expose {name}; got {:?}",
                case.name,
                explanation.tool_names
            );
        }
        for name in case.absent_tool_names {
            assert!(
                !explanation.tool_names.contains(&name.to_string()),
                "{} should not expose {name}; got {:?}",
                case.name,
                explanation.tool_names
            );
        }
        for code in case.expected_warnings {
            assert!(
                explanation_has_warning(&explanation, code),
                "{} should warn {code}; got {:?}",
                case.name,
                explanation.warnings
            );
        }
        assert_eq!(
            explanation_has_warning(&explanation, "host_ceiling_not_global"),
            case.host_ceiling_warning,
            "{} host ceiling warning should match whether only built-in read tools survived",
            case.name
        );
    }
}

#[test]
fn explain_complex_document_combination_filters_agents_and_groups_surface() {
    let own_node_did = "did:test:local";
    let selection = tools_document(serde_json::json!({
        "node_did":own_node_did,
        "host":{"files":{"mode":"ReadOnly"},"bash":{"mode":"ReadOnly","background_enabled":true}},
        "remote":{"services":[
            {"mcp_service_id":"registry","tool_names":["read"]},
            {"mcp_service_id":"observability","tool_names":["query"]}]},
        "agents":{"target_ids":["worker","inactive","remote"],"enabled":true},
        "built_ins":{"enable_context_budget":true},
        "datastore":{"enable_defra_query":true,"defra_query_collections":["AgentRequest","AgentResponse"]}
    }));
    let targets: Vec<gents_documents::AgentTargetDocument> = [
        ("worker", own_node_did, "worker", "local worker"),
        (
            "inactive",
            own_node_did,
            "inactive",
            "inactive local worker",
        ),
        (
            "remote",
            "did:test:remote",
            "remote-worker",
            "remote worker",
        ),
    ]
    .into_iter()
    .map(|(name, destination, agent, description)| {
        serde_json::from_value(serde_json::json!({
            "target_id":name,"node_did":own_node_did,"target_node_did":destination,
            "agent_did":agent,"name":name,"description":description
        }))
        .unwrap()
    })
    .collect();
    let ceiling = ToolCeiling::readonly_at(temp_root("gents-complex-package-root"));
    let mut agents = AgentToolConfig::from_document(&selection).unwrap();
    agents.targets = targets.clone();
    let mut resolved = ResolvedToolSelection::from_document(&selection).unwrap();
    resolved.backgroundable_tool_names = vec!["bash".into()];
    let config = AgentToolSurfaceConfig::from_selection_with_agent_tools(
        "complex",
        resolved,
        &ceiling,
        agents,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        config.allowed_mcp_service_ids(),
        &["observability".to_string(), "registry".to_string()]
    );

    let active_agent_dids = rapidhash::RapidHashSet::from_iter(["worker".to_string()]);
    let surface = config.resolve_with_available_agent_targets_for_mcp_presence(
        true,
        own_node_did,
        &active_agent_dids,
    );
    assert_eq!(
        resolve_agent_target_descriptions(&surface),
        vec![
            ("worker".to_string(), "local worker".to_string()),
            ("remote".to_string(), "remote worker".to_string())
        ]
    );
    let explanation = ToolSurfaceExplanation::from_resolved(&config, &surface);

    for name in [
        "read_file",
        "bash",
        "call_tool",
        "spawn_process",
        "agent_new",
        "agent_message",
        "agent_interrupt",
        "agent_list",
        "agent_session_list",
        "agent_session_read",
        "query",
    ] {
        assert!(
            explanation.tool_names.contains(&name.to_string()),
            "complex surface should expose {name}; got {:?}",
            explanation.tool_names
        );
    }
    assert!(!explanation_has_warning(
        &explanation,
        "defra_query_empty_scope_all"
    ));
    assert!(explanation_category_contains(
        &explanation.included,
        "agent",
        "agent_new"
    ));
    assert!(explanation_category_contains(
        &explanation.included,
        "background_process",
        "spawn_process"
    ));
    assert!(explanation_category_contains(
        &explanation.included,
        "meta_mcp",
        "call_tool"
    ));
}

#[test]
fn explain_default_surface_calls_out_builtin_reads_and_defra_query_scope() {
    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection::default(),
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();
    let explanation =
        config.explain_with_runtime(false, "did:test:test", &rapidhash::RapidHashSet::default());

    assert!(explanation
        .tool_names
        .contains(&gents_protocol::tool_names::CONTEXT_BUDGET_TOOL_NAME.to_string()));
    assert!(explanation
        .tool_names
        .contains(&gents_protocol::tool_names::SESSION_HISTORY_TOOL_NAME.to_string()));
    assert!(!explanation
        .tool_names
        .contains(&gents_protocol::tool_names::DEFRA_QUERY_TOOL_NAME.to_string()));
    assert!(!explanation
        .warnings
        .iter()
        .any(|warning| warning.code == "defra_query_empty_scope_all"));
    assert!(explanation
        .policy
        .effective
        .get("built_in_read")
        .is_some_and(|names| names
            .contains(&gents_protocol::tool_names::CONTEXT_BUDGET_TOOL_NAME.to_string())));
    assert!(explanation
        .unavailable
        .get("meta_mcp")
        .is_some_and(|names| names.contains(&"discover_tools".to_string())));
}

#[test]
fn category_complete_ceiling_clamps_builtin_reads() {
    let mut ceiling_policy =
        ToolPolicySurface::ceiling_with_host_modes(FileToolMode::Off, BashMode::Off);
    ceiling_policy.context_budget = false;
    ceiling_policy.defra_query = false;
    ceiling_policy.defra_collections = EndpointScope::None;

    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection::default(),
        &ToolCeiling::meta_only().with_policy(ceiling_policy),
        Vec::new(),
    )
    .unwrap();
    let explanation =
        config.explain_with_runtime(false, "did:test:test", &rapidhash::RapidHashSet::default());

    assert!(!explanation
        .tool_names
        .contains(&gents_protocol::tool_names::CONTEXT_BUDGET_TOOL_NAME.to_string()));
    assert!(!explanation
        .tool_names
        .contains(&gents_protocol::tool_names::DEFRA_QUERY_TOOL_NAME.to_string()));
    assert!(explanation
        .excluded
        .get("built_in_read")
        .is_some_and(|names| names
            .contains(&gents_protocol::tool_names::CONTEXT_BUDGET_TOOL_NAME.to_string())));
}

#[test]
fn explain_mcp_empty_allowlist_denies_all_services_when_online() {
    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_meta_tools: true,
            allowed_mcp_service_ids: Vec::new(),
            required_mcp_service_ids: Vec::new(),
            enable_defra_query: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();
    let explanation =
        config.explain_with_runtime(true, "did:test:test", &rapidhash::RapidHashSet::default());

    assert!(!explanation.tool_names.contains(&"call_tool".to_string()));
    assert!(!explanation
        .tool_names
        .contains(&"discover_tools".to_string()));
    assert!(explanation
        .warnings
        .iter()
        .any(|warning| warning.code == "mcp_empty_allowlist_none"));
    assert!(explanation
        .unavailable
        .get("meta_mcp")
        .is_some_and(|names| names.contains(&"call_tool".to_string())));
}

#[tokio::test]
async fn agent_config_alias_expands_to_config_scope() {
    let node_db = gents_store::test_support::TestStore::start(
        "agent-config-alias-expands-to-config-sco",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();

    let surface = AgentToolSurfaceConfig::from_selection(
        "ops",
        ResolvedToolSelection {
            enable_defra_query: true,
            defra_query_collections: vec!["agent-config".to_string()],
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(
        &node,
        node.node_identity_did().unwrap(),
        &crate::plugin_host::NoPluginHost,
    )
    .await
    .unwrap();

    assert!(surface.tool_names().contains(&"query".to_string()));
    for allowed in ["Agent", "Tools", "Schedule", "AgentContext"] {
        assert!(
            surface.defra_query_scope.ensure_allowed(allowed).is_ok(),
            "{allowed} must be readable under the agent-config preset"
        );
    }
    for denied in [
        "AgentRequest",
        "AgentMessage",
        "OAuthCredential",
        "agent-config",
    ] {
        assert!(
            surface.defra_query_scope.ensure_allowed(denied).is_err(),
            "{denied} must stay outside the agent-config preset"
        );
    }
}

fn command_mode_wire(mode: crate::native::CommandExecutionMode) -> String {
    use crate::native::CommandExecutionMode as Mode;
    let wire = match mode {
        Mode::ReadOnly => "read_only",
        Mode::WorkspaceWrite => "workspace_write",
        Mode::Unrestricted => "unrestricted",
        Mode::ArtifactWrite => "artifact_write",
    };
    wire.to_string()
}

#[test]
fn artifact_selection_matches_the_lean_model() {
    use crate::native::{CommandNetworkMode, NativeTool};
    let root = temp_root("artifact-selection-ceilings");
    gents_lean_vocab::families::tool_surface_artifact_selection::replay(|input| {
        let bash_mode = if input.bash_unrestricted {
            "Unrestricted"
        } else {
            "ReadOnly"
        };
        let selection = ResolvedToolSelection::from_document(&tools_document(serde_json::json!({
            "host":{"bash":{"mode":bash_mode,"execution_mode":"artifact_write","network_mode":"disabled"}}
        }))).unwrap();
        let ceiling = if input.read_only_ceiling {
            ToolCeiling::readonly_at(&root)
        } else {
            ToolCeiling::readwrite(&root)
        };
        let config = AgentToolSurfaceConfig::from_selection(
            "artifact-test",
            selection,
            &ceiling,
            Vec::new(),
        )
        .unwrap();
        let static_bash = &config.static_policy().bash;
        gents_lean_vocab::families::tool_surface_artifact_selection::SelectionOutput {
            static_mode: command_mode_wire(static_bash.execution_mode),
            network_disabled: static_bash.network_mode == CommandNetworkMode::Disabled,
            native_modes: config
                .host_tools()
                .native_tools()
                .iter()
                .filter_map(|tool| match tool {
                    NativeTool::BashUnrestricted { policy, .. }
                    | NativeTool::BashReadOnly { policy, .. } => {
                        Some(command_mode_wire(policy.mode))
                    }
                    _ => None,
                })
                .collect(),
        }
    });
}

#[test]
fn direct_readonly_selection_cannot_advertise_an_artifact_static_grant() {
    let selection = ResolvedToolSelection {
        bash: BashMode::ReadOnly,
        command_policy: Some(
            crate::native::CommandExecutionPolicy::write_capable()
                .with_mode(crate::native::CommandExecutionMode::ArtifactWrite),
        ),
        ..Default::default()
    };
    assert_eq!(
        ToolPolicySurface::from_selection(&selection, &Default::default())
            .bash
            .execution_mode,
        crate::native::CommandExecutionMode::ReadOnly
    );
}

#[test]
fn flat_presentation_explanation_matches_selected_names_and_ceiling() {
    let document = tools_document(serde_json::json!({"remote":{"services":[
        {"mcp_service_id":"selected", "tool_names":["read"], "style":"flat"},
        {"mcp_service_id":"denied", "tool_names":["write"], "style":"flat"}
    ]}}));
    let mut ceiling_policy =
        ToolPolicySurface::ceiling_with_host_modes(FileToolMode::Off, BashMode::Off);
    ceiling_policy.mcp_services = EndpointScope::<String, ()>::only_units(["selected".to_string()]);
    let config = AgentToolSurfaceConfig::from_tools_document(
        "flat",
        &document,
        &ToolCeiling::meta_only().with_policy(ceiling_policy),
        Vec::new(),
    )
    .unwrap();
    let explanation =
        config.explain_with_runtime(true, "did:test:test", &rapidhash::RapidHashSet::default());
    let name = crate::mcp::meta::flat_tool_name("selected", "read");
    assert_eq!(
        explanation.tool_names,
        vec![
            name.clone(),
            gents_protocol::tool_names::SESSION_HISTORY_TOOL_NAME.to_string()
        ]
    );
    assert_eq!(explanation.included["meta_mcp"], vec![name]);
    assert!(!explanation
        .tool_names
        .contains(&crate::mcp::meta::flat_tool_name("denied", "write")));
    assert!(!explanation.tool_names.contains(&"call_tool".to_string()));
}

#[cfg(unix)]
#[tokio::test]
async fn configured_command_output_caps_bound_bash_and_cli_results() {
    let root = temp_root("gents-command-output-cap");
    let ceiling = ToolCeiling::readwrite(root)
        .with_cli_tool(cli_tool("sh", "/bin/sh", "shell"))
        .with_cli_tool(cli_tool("sh_default", "/bin/sh", "shell"));
    let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "capped", "node_did": "did:key:example",
        "host": {
            "bash": {"mode": "Unrestricted", "max_output_chars": 10},
            "cli": [{"name": "sh", "max_output_chars": 12}, {"name": "sh_default"}]
        }
    }))
    .unwrap();
    let config =
        AgentToolSurfaceConfig::from_tools_documents("capped", &tools, &[], &[], &ceiling, vec![])
            .unwrap();
    let cli_caps = config
        .host_tools()
        .native_tools()
        .iter()
        .filter_map(|tool| match tool {
            crate::native::NativeTool::Cli(tool) => {
                Some((tool.name.as_str(), tool.max_output_chars))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cli_caps,
        vec![
            ("sh", 12),
            ("sh_default", crate::native::DEFAULT_MAX_COMMAND_CHARS)
        ]
    );
    let built = config.host_tools().build_native_tools().unwrap();
    let envelope = gents_loop::tool::ToolCallEnvelope::default();
    let call = |name: &str, args: serde_json::Value| {
        let tool = built.iter().find(|tool| tool.name() == name).unwrap();
        tool.call(args.to_string(), &envelope)
    };

    let bash = call(
        "bash_unrestricted",
        serde_json::json!({"command": "printf '%100s' x", "raw_json": true}),
    )
    .await
    .unwrap();
    let text = |raw: String| serde_json::from_str::<String>(&raw).unwrap_or(raw);
    let bash: serde_json::Value = serde_json::from_str(&text(bash)).unwrap();
    let truncation = &bash["stdout_truncation"];
    assert_eq!(truncation["max_bytes"], 10, "{bash}");
    assert_eq!(truncation["total_bytes"], 100, "{bash}");
    assert_eq!(truncation["truncated"], true, "{bash}");
    assert_eq!(truncation["returned_bytes"], 10, "{bash}");
    assert!(
        bash["stdout"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{:10}\n", "")),
        "{bash}"
    );

    let cli = call(
        "sh",
        serde_json::json!({"argv": ["-c", "printf '%100s' x"]}),
    )
    .await
    .unwrap();
    let cli = text(cli);
    let stdout = cli
        .split_once("stdout:\n")
        .unwrap()
        .1
        .split_once("\nstderr:")
        .unwrap()
        .0;
    assert!(stdout.starts_with(&format!("{:12}\n", "")), "{cli}");
    assert!(stdout.contains("first 12 of 100 bytes"), "{cli}");
}

#[test]
fn plugin_tools_come_from_the_tools_document_and_are_named_after_the_plugin() {
    let selection = ResolvedToolSelection::from_document(&tools_document(serde_json::json!({
        "node_did": "did:key:zPlugins",
        "integrations": {"plugins": [{"plugin": "team/lint_diff"}]},
    })))
    .unwrap();
    let config = AgentToolSurfaceConfig::from_selection(
        "ops",
        selection,
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();
    assert!(config
        .static_policy()
        .plugin_tools
        .permits(&"lint_diff".to_string()));
    assert!(!config
        .static_policy()
        .plugin_tools
        .permits(&"other".to_string()));
}

#[test]
fn a_tools_document_naming_two_plugins_the_same_is_refused() {
    let tools = tools_document(serde_json::json!({
        "node_did": "did:key:zPlugins",
        "integrations": {"plugins": [{"plugin": "a/lint"}, {"plugin": "b/lint"}]},
    }));
    assert!(tools.validate().is_err());
    let tools = tools_document(serde_json::json!({
        "node_did": "did:key:zPlugins",
        "integrations": {"plugins": [{"plugin": "a/lint", "digest": "sha256:nope"}]},
    }));
    assert!(tools.validate().is_err());
}

#[test]
fn configured_tool_timeouts_reach_built_tools_within_host_ceilings() {
    let root = temp_root("gents-configured-timeouts");
    let ceiling = ToolCeiling::readwrite(root)
        .with_command_timeout_secs(120)
        .with_command_timeout_max_secs(900)
        .with_cli_tool(cli_tool("git", "/usr/bin/git", "git"))
        .with_cli_tool(cli_tool("rg", "/usr/bin/rg", "search"))
        .with_cli_tool(cli_tool("jq", "/usr/bin/jq", "json"));
    let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "timed", "node_did": "did:key:example",
        "host": {
            "files": {"mode": "ReadOnly"},
            "bash": {"mode": "Unrestricted", "background_enabled": true,
                "timeout_secs": 30, "max_timeout_secs": 3600,
                "background_timeout_secs": 90, "wait_timeout_secs": 5,
                "max_wait_timeout_secs": 1200},
            "cli": [{"name": "git", "timeout_secs": 45},
                    {"name": "rg", "timeout_secs": 5000}, {"name": "jq"}]
        },
        "remote": {"services": [{"mcp_service_id": "search", "tool_names": ["query"],
            "background_tool_names": ["query"], "background_timeout_secs": 600,
            "wait_timeout_secs": 60}]},
        "integrations": {"lsp": {"timeout_secs": 40, "max_timeout_secs": 90}}
    }))
    .unwrap();
    let config =
        AgentToolSurfaceConfig::from_tools_documents("timed", &tools, &[], &[], &ceiling, vec![])
            .unwrap();

    let bash = config
        .host_tools()
        .native_tools()
        .iter()
        .find_map(|tool| match tool {
            crate::native::NativeTool::BashUnrestricted {
                timeout,
                timeout_max,
                ..
            } => Some((timeout.as_secs(), timeout_max.as_secs())),
            _ => None,
        });
    assert_eq!(bash, Some((30, 900)));

    let cli = config
        .host_tools()
        .native_tools()
        .iter()
        .filter_map(|tool| match tool {
            crate::native::NativeTool::Cli(tool) => Some((tool.name.as_str(), tool.timeout_secs)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(cli, vec![("git", 45), ("rg", 900), ("jq", 10)]);

    let background = &config.background_tools().timeouts;
    let bash = background.for_execution("bash_unrestricted", None);
    assert_eq!(bash.lifetime.as_secs(), 90);
    assert_eq!(
        (bash.wait.default.as_secs(), bash.wait.maximum.as_secs()),
        (5, 600)
    );
    let remote = background.for_execution("call_tool", Some("search"));
    assert_eq!(remote.lifetime.as_secs(), 600);
    assert_eq!(
        (remote.wait.default.as_secs(), remote.wait.maximum.as_secs()),
        (60, 600)
    );

    let surface = config.resolve_with_agent_tools_for_runtime_availability(
        RuntimeToolAvailability::for_mcp_presence(true),
        AgentToolConfig::default(),
    );
    let lsp = surface.lsp_config().expect("lsp selected").action_timeout;
    assert_eq!((lsp.default.as_secs(), lsp.maximum.as_secs()), (40, 90));
    assert_eq!(lsp.action_for(Some(1)).as_secs(), 5);
    assert_eq!(lsp.action_for(Some(600)).as_secs(), 90);
}

#[test]
fn unconfigured_tool_timeouts_keep_host_defaults() {
    let root = temp_root("gents-unconfigured-timeouts");
    let ceiling = ToolCeiling::readwrite(root)
        .with_command_timeout_secs(200)
        .with_cli_tool(cli_tool("git", "/usr/bin/git", "git"));
    let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "plain", "node_did": "did:key:example",
        "host": {"bash": {"mode": "ReadOnly"}, "cli": [{"name": "git"}]}
    }))
    .unwrap();
    let config =
        AgentToolSurfaceConfig::from_tools_documents("plain", &tools, &[], &[], &ceiling, vec![])
            .unwrap();
    let timeouts = config
        .host_tools()
        .native_tools()
        .iter()
        .map(|tool| match tool {
            crate::native::NativeTool::BashReadOnly {
                timeout,
                timeout_max,
                ..
            } => (timeout.as_secs(), timeout_max.as_secs()),
            crate::native::NativeTool::Cli(tool) => (tool.timeout_secs, tool.timeout_secs),
            other => panic!("unexpected tool {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(timeouts, vec![(200, 200), (10, 10)]);
    assert_eq!(
        config.background_tools().timeouts,
        crate::surface::BackgroundTimeouts::default()
    );
}

#[tokio::test]
async fn bash_schema_advertises_the_configured_background_lifetime() {
    let root = temp_root("gents-background-lifetime-schema");
    let ceiling = ToolCeiling::readwrite(root);
    for (lifetime, expected) in [(Some(90), 90), (Some(36_001), 36_000), (None, 36_000)] {
        let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
            "tools_id": "timed", "node_did": "did:key:example",
            "host": {"bash": {"mode": "Unrestricted", "background_enabled": true,
                "background_timeout_secs": lifetime}}
        }))
        .unwrap();
        let config = AgentToolSurfaceConfig::from_tools_documents(
            "timed",
            &tools,
            &[],
            &[],
            &ceiling,
            vec![],
        )
        .unwrap();
        let built = config.host_tools().build_native_tools().unwrap();
        let bash = built
            .iter()
            .find(|tool| tool.name() == "bash_unrestricted")
            .unwrap();
        let schema = bash.definition(String::new()).await.parameters;
        let description = schema["properties"]["timeout_secs"]["description"]
            .as_str()
            .unwrap();
        assert!(
            description.contains(&format!("configured {expected}s lifetime")),
            "{description}"
        );
    }
}

#[tokio::test]
async fn configured_file_limits_reach_file_tools_as_default_and_maximum() {
    let root = temp_root("gents-file-limits");
    std::fs::write(root.join("big.txt"), "y".repeat(100)).unwrap();
    for index in 0..5 {
        std::fs::write(root.join(format!("f{index}.rs")), "fn main() {}\n").unwrap();
    }
    let ceiling = ToolCeiling::readwrite(root);
    let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "files", "node_did": "did:key:example",
        "host": {"files": {"mode": "ReadOnly", "max_read_chars": 10,
            "max_list_entries": 3, "max_matches": 2}}
    }))
    .unwrap();
    let config =
        AgentToolSurfaceConfig::from_tools_documents("files", &tools, &[], &[], &ceiling, vec![])
            .unwrap();
    let limits = config
        .host_tools()
        .native_tools()
        .iter()
        .map(|tool| match tool {
            crate::native::NativeTool::ListFiles { max_entries } => ("list_files", *max_entries),
            crate::native::NativeTool::ReadFile { max_chars } => ("read_file", *max_chars),
            crate::native::NativeTool::Glob { max_matches } => ("glob", *max_matches),
            crate::native::NativeTool::Grep { max_matches } => ("grep", *max_matches),
            other => panic!("unexpected tool {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        limits,
        vec![
            ("list_files", 3),
            ("read_file", 10),
            ("glob", 2),
            ("grep", 2)
        ]
    );
    let built = config.host_tools().build_native_tools().unwrap();
    let tool = |name: &str| built.iter().find(|tool| tool.name() == name).unwrap();
    let schema = tool("read_file").definition(String::new()).await.parameters;
    assert_eq!(schema["properties"]["max_chars"]["default"], 10);
    assert_eq!(schema["properties"]["max_chars"]["maximum"], 10);
    for args in [
        serde_json::json!({"path": "big.txt", "raw_json": true}),
        serde_json::json!({"path": "big.txt", "max_chars": 1000, "raw_json": true}),
    ] {
        let raw = tool("read_file")
            .call(
                args.to_string(),
                &gents_loop::tool::ToolCallEnvelope::default(),
            )
            .await
            .unwrap();
        let raw = serde_json::from_str::<String>(&raw).unwrap_or(raw);
        let output: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            output["content"]
                .as_str()
                .unwrap()
                .contains("[Showing first 10 of 104 bytes]"),
            "{output}"
        );
    }

    let unconfigured: gents_documents::Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "files", "node_did": "did:key:example",
        "host": {"files": {"mode": "ReadOnly"}}
    }))
    .unwrap();
    let config = AgentToolSurfaceConfig::from_tools_documents(
        "files",
        &unconfigured,
        &[],
        &[],
        &ceiling,
        vec![],
    )
    .unwrap();
    assert!(config
        .host_tools()
        .native_tools()
        .contains(&crate::native::NativeTool::ReadFile {
            max_chars: crate::native::DEFAULT_MAX_FILE_CHARS
        }));
}

#[test]
fn effective_surface_fingerprint_preserves_p2p_mutation() {
    let make = |mutations| {
        AgentToolSurfaceConfig::from_selection(
            "p2p",
            ResolvedToolSelection {
                enable_p2p_tool: true,
                enable_p2p_mutations: mutations,
                ..Default::default()
            },
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
        .unwrap()
        .resolve_with_agent_tools_for_mcp_presence(false, AgentToolConfig::default())
    };
    let read = make(false);
    let mutate = make(true);
    assert_ne!(format!("{read:?}"), format!("{mutate:?}"));
}

#[test]
fn application_write_is_exactly_granted_and_narrowed_by_the_existing_endpoint_meet() {
    let grant = ResolvedToolSelection {
        application_write_collections: vec!["Shipment".into()],
        ..Default::default()
    };
    let policy = ToolPolicySurface::from_selection(&grant, &AgentToolConfig::default());
    assert_eq!(
        policy.application_write_collections_for_runtime(&["Shipment".into(), "Other".into()]),
        vec!["Shipment"]
    );
    assert!(!policy.defra_query);
    let other = ResolvedToolSelection {
        application_write_collections: vec!["Other".into()],
        ..Default::default()
    };
    let narrowed = policy.meet(&ToolPolicySurface::from_selection(
        &other,
        &AgentToolConfig::default(),
    ));
    assert!(narrowed
        .application_write_collections_for_runtime(&grant.application_write_collections)
        .is_empty());
    let empty = ToolPolicySurface::from_selection(
        &ResolvedToolSelection::default(),
        &AgentToolConfig::default(),
    );
    assert!(empty
        .application_write_collections_for_runtime(&grant.application_write_collections)
        .is_empty());
}

#[test]
fn provider_limits_is_available_under_an_agent_self_config_grant() {
    let config = AgentToolSurfaceConfig::from_selection(
        "reporter",
        ResolvedToolSelection {
            enable_self_config: true,
            self_config_categories: Some(vec!["agent".to_string()]),
            ..Default::default()
        },
        &ToolCeiling::readwrite(temp_root("provider-limits-grant")),
        Vec::new(),
    )
    .unwrap();
    let explanation = config.explain_with_runtime(false, "did:key:limits", &Default::default());
    assert!(explanation
        .tool_names
        .iter()
        .any(|name| name == "provider_limits"));
}

/// A host that carries a home and delegates everything else, so a test sees the
/// runtime's home reach the guard exactly as `serve` gives it.
struct HomePluginHost(NoPluginHost, std::path::PathBuf);

#[async_trait::async_trait]
impl PluginHost for HomePluginHost {
    fn home(&self) -> Option<&std::path::Path> {
        Some(&self.1)
    }
    fn installed(&self) -> anyhow::Result<Vec<InstalledPlugin>> {
        PluginHost::installed(&self.0)
    }
    fn installed_record(&self, coordinate: &str) -> anyhow::Result<InstalledPlugin> {
        PluginHost::installed_record(&self.0, coordinate)
    }
    fn resolve(&self, coordinate: &str, pinned: Option<&str>) -> anyhow::Result<InstalledPlugin> {
        PluginHost::resolve(&self.0, coordinate, pinned)
    }
    async fn call_outcome(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> anyhow::Result<PluginOutcome> {
        PluginHost::call_outcome(&self.0, record, input, context).await
    }
    async fn run_plugin_once(
        self: std::sync::Arc<Self>,
        coordinate: &str,
        selected: &[PluginToolRef],
        agent: crate::plugin_host::PluginRunAgent<'_>,
        input: serde_json::Value,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> anyhow::Result<(serde_json::Value, Option<String>, Option<String>)> {
        std::sync::Arc::new(NoPluginHost)
            .run_plugin_once(coordinate, selected, agent, input, context)
            .await
    }
    fn application_write_tool(
        &self,
        access: gents_store::config_client::ConfigAccess,
        collections: std::collections::BTreeSet<String>,
        actor: Option<Did>,
    ) -> anyhow::Result<Box<dyn ToolDyn>> {
        PluginHost::application_write_tool(&self.0, access, collections, actor)
    }
    fn plugin_tool(
        self: std::sync::Arc<Self>,
        plugin: &PluginToolRef,
        read_root: Option<std::path::PathBuf>,
        own_folder: Option<std::path::PathBuf>,
        node: std::sync::Arc<defra_node::Node>,
        node_did: &str,
        agent_did: Option<&str>,
    ) -> anyhow::Result<Box<dyn ToolDyn>> {
        std::sync::Arc::new(NoPluginHost)
            .plugin_tool(plugin, read_root, own_folder, node, node_did, agent_did)
    }
}

/// `build_tools` copies the runtime's home onto the guard before it validates,
/// so the refusal the operator reads carries this node's `--home` — the fact a
/// grant run against another node's home gets wrong.
#[tokio::test]
async fn build_tools_passes_the_nodes_home_into_the_refusal() {
    let (db, mut runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let ceiling = tempfile::tempdir().unwrap();
    let root = ceiling.path().join("shared-root");
    std::fs::create_dir_all(&root).unwrap();
    let root_text = root.to_string_lossy().into_owned();
    let home_dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home_dir.path()).unwrap();
    let home_text = home.to_string_lossy().into_owned();
    // Published with no reader, as an Agent created apart from its Tools leaves it.
    let publisher = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        runtime.node_did.parse().unwrap(),
    );
    let published = publisher
        .write(
            "test.workspace.root",
            &format!(
                r#"mutation {{ create_WorkspaceRoot(input: {{node_did:"{}", root_path:"{}", enabled:true}}) {{_docID}} }}"#,
                gents_store::graphql::escape_graphql_string(&runtime.node_did),
                gents_store::graphql::escape_graphql_string(&root_text),
            ),
        )
        .await
        .unwrap();
    assert!(gents_store::graphql::created_doc_id(&published, "WorkspaceRoot").is_ok());
    let agent = runtime.agent_identity.clone().unwrap();
    let mut document = preset_tools_document("home-flow", "ReadOnly", "Off", false, false);
    document.host.as_mut().unwrap().root = Some(root_text.clone());
    let config = AgentToolSurfaceConfig::from_tools_document(
        agent.did(),
        &document,
        &ToolCeiling::readonly_at(ceiling.path()),
        Vec::new(),
    )
    .unwrap();
    let surface = config
        .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();
    // Exactly what `serve` does: the plugin host carries the node's home.
    runtime.plugins = std::sync::Arc::new(HomePluginHost(
        crate::plugin_host::NoPluginHost,
        home.clone(),
    ));
    let error = match surface.build_tools(&runtime).await {
        Ok(_) => panic!("an Agent with no reader on its root cannot build its surface"),
        Err(error) => error,
    };
    let refusal = tool_root_refusal(error.as_ref()).expect("the refusal is typed");
    let ToolRootRefusal::ReaderMissing { home: named, .. } = refusal else {
        panic!("a published root the Agent cannot read is the reader cause: {error:#}");
    };
    assert_eq!(
        named.as_deref(),
        Some(home.as_path()),
        "the guard carries the runtime's home"
    );
    assert!(
        error.to_string().contains(&format!("--home {home_text}")),
        "the refusal's grant applies to this node: {error}"
    );
    node.shutdown().await.unwrap();
}

/// An Agent that reads root A while its tools point at published, enabled root
/// B, which it does not read, is denied for B — the same missing grant on B.
#[tokio::test]
async fn an_agent_reading_one_root_and_working_in_another_names_the_grant_on_b() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let base = tempfile::tempdir().unwrap();
    let root_a = base.path().join("root-a");
    let root_b = base.path().join("root-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let agent = runtime.agent_identity.clone().unwrap();
    let publisher = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        runtime.node_did.parse().unwrap(),
    );
    for path in [&root_a, &root_b] {
        let published = publisher
            .write(
                "test.workspace.root",
                &format!(
                    r#"mutation {{ create_WorkspaceRoot(input: {{node_did:"{}", root_path:"{}", enabled:true}}) {{_docID}} }}"#,
                    gents_store::graphql::escape_graphql_string(&runtime.node_did),
                    gents_store::graphql::escape_graphql_string(&path.to_string_lossy()),
                ),
            )
            .await
            .unwrap();
        let doc = gents_store::graphql::created_doc_id(&published, "WorkspaceRoot").unwrap();
        if path == &root_a {
            publisher
                .add_actor_relationship(
                    "WorkspaceRoot",
                    &doc,
                    "reader",
                    &agent.did().parse().unwrap(),
                )
                .await
                .unwrap();
        }
    }
    let mut document = preset_tools_document("ab", "ReadOnly", "Off", false, false);
    document.host.as_mut().unwrap().root = Some(root_b.to_string_lossy().into_owned());
    let config = AgentToolSurfaceConfig::from_tools_document(
        agent.did(),
        &document,
        &ToolCeiling::readonly_at(base.path()),
        Vec::new(),
    )
    .unwrap();
    let surface = config
        .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();
    let guard = surface.root_execution_guard().cloned().unwrap();
    let error = guard
        .validate(&node, agent.as_ref())
        .await
        .expect_err("root B is published but this Agent does not read it");
    let refusal = tool_root_refusal(error.as_ref()).expect("the refusal is typed");
    let ToolRootRefusal::ReaderMissing { root, .. } = refusal else {
        panic!("a published root the Agent cannot read is the reader cause: {error:#}");
    };
    assert_eq!(
        root.as_deref(),
        Some(root_b.as_path()),
        "the grant names root B, not the root the Agent already reads"
    );
    assert!(
        error
            .to_string()
            .contains(&root_b.to_string_lossy().into_owned()),
        "{error}"
    );
    node.shutdown().await.unwrap();
}

/// With no home, the grant stays copy-pasteable and says what to add.
#[tokio::test]
async fn a_refusal_without_the_home_states_the_grant_and_says_to_add_the_home() {
    let refusal = ToolRootRefusal::ReaderMissing {
        agent_did: "did:key:agent".to_owned(),
        root: Some(std::path::PathBuf::from("/work")),
        tool_root: std::path::PathBuf::from("/work"),
        home: None,
        publishing: RootPublishing::PublishedEnabled,
        denial: RootDenialKind::NoAllowedRoots,
    };
    // The facts decide the cause, and here the model answers "missing reader".
    assert_eq!(
        tool_surface_reason(refusal.publishing(), refusal.denial()),
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::WorkspaceRootReaderMissing
    );
    let text = refusal.to_string();
    assert!(
        text.contains(
            "`gents config workspace-root grant-reader --path /work --agent-did did:key:agent`"
        ),
        "{text}"
    );
    assert!(!text.contains("<this node's home>"), "{text}");
    assert!(
        text.contains("adding `--home` with this node's home"),
        "{text}"
    );
    assert_eq!(
        gents_protocol::node_readiness::unavailable_rejection_reason(&text),
        Some(gents_protocol::node_readiness::AgentReadinessUnavailableReason::WorkspaceRootReaderMissing),
        "{text}"
    );
}

/// A reader read the actor may not make degrades the listing, never fails it:
/// `list` and `show` still return each root, saying the readers are unknown and
/// why. The cause is a signer-less Local access with no client signing key, the
/// one shape the unsigned read refuses.
#[tokio::test]
async fn a_reader_read_the_actor_may_not_make_reports_unknown_and_still_lists() {
    use gents_protocol::operations::workspace_root::WorkspaceRootReaders;
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let ceiling = tempfile::tempdir().unwrap();
    let root = ceiling.path().join("shared-root");
    std::fs::create_dir_all(&root).unwrap();
    let root_text = root.to_string_lossy().into_owned();
    let publisher = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        runtime.node_did.parse().unwrap(),
    );
    let published = publisher
        .write(
            "test.workspace.root",
            &format!(
                r#"mutation {{ create_WorkspaceRoot(input: {{node_did:"{}", root_path:"{}", enabled:true}}) {{_docID}} }}"#,
                gents_store::graphql::escape_graphql_string(&runtime.node_did),
                gents_store::graphql::escape_graphql_string(&root_text),
            ),
        )
        .await
        .unwrap();
    let doc = gents_store::graphql::created_doc_id(&published, "WorkspaceRoot").unwrap();
    let agent = runtime.agent_identity.clone().unwrap();
    publisher
        .add_actor_relationship(
            "WorkspaceRoot",
            &doc,
            "reader",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();

    // An access of the shape the runtime builds for an Agent: an asserted
    // Agent identity that reads the root, no Agent signer and no client
    // signing key, so the relationship read is refused.
    let refused = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        agent.did().parse().unwrap(),
    );
    let listed = super::list_workspace_roots_with_readers(&refused)
        .await
        .expect("a reader read it may not make still lists every root");
    let listed_root = listed
        .iter()
        .find(|listed| listed.root.root_path == root_text)
        .unwrap_or_else(|| panic!("the published root is listed: {listed:?}"));
    let WorkspaceRootReaders::Unknown { reason } = &listed_root.readers else {
        panic!("the readers are unknown: {:?}", listed_root.readers);
    };
    assert!(
        !reason.is_empty(),
        "the listing says why the readers are unknown"
    );

    let shown = super::show_workspace_root_with_readers(&refused, &root_text)
        .await
        .expect("show still returns the root");
    assert!(
        matches!(shown.readers, WorkspaceRootReaders::Unknown { .. }),
        "show reports the readers as unknown: {:?}",
        shown.readers
    );
    node.shutdown().await.unwrap();
}

/// The generic path is pinned too: a `Denied` refusal's facts answer
/// `toolSurfaceUnavailable`, so the split between the two causes is asserted on
/// both sides and not only for the reader.
#[test]
fn a_denied_refusal_keeps_the_generic_tool_surface_cause() {
    use gents_protocol::node_readiness::AgentReadinessUnavailableReason;
    let denied = ToolRootRefusal::Denied {
        agent_did: "did:key:agent".to_owned(),
        root: std::path::PathBuf::from("/outside"),
        denial: gents_session::workspace::root_resolution::RootDenialReason::NoAllowedRoots,
        publishing: RootPublishing::PublishedDisabled,
        denial_kind: RootDenialKind::NoAllowedRoots,
    };
    assert_eq!(
        tool_surface_reason(denied.publishing(), denied.denial()),
        AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
        "a denial over no enabled published root is the generic surface"
    );
    // Every way the policy fails before it publishes keeps the generic cause.
    for publishing in [RootPublishing::NoPolicy, RootPublishing::PublishedDisabled] {
        for denial in [
            RootDenialKind::Admitted,
            RootDenialKind::NoAllowedRoots,
            RootDenialKind::Outside,
        ] {
            assert_eq!(
                tool_surface_reason(publishing, denial),
                AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
                "only an enabled published root makes it the reader: {publishing:?} {denial:?}"
            );
        }
    }
    // And a refusal that never reached the policy states no premise.
    let early = ToolRootRefusal::NoExplicitRoot {
        agent_did: "did:key:agent".to_owned(),
    };
    assert_eq!(early.publishing(), RootPublishing::NoPolicy);
    assert_eq!(early.denial(), RootDenialKind::Admitted);
    assert_eq!(
        tool_surface_reason(early.publishing(), early.denial()),
        AgentReadinessUnavailableReason::ToolSurfaceUnavailable
    );
}

/// A published root nested *under* the tool root does not cover it: admission
/// requires the tools to sit inside an allowed root, so the grant such a root
/// would name admits nothing. Only a published root the tools sit inside does.
#[tokio::test]
async fn a_published_root_nested_under_a_broader_tool_root_does_not_cover_it() {
    let (db, runtime) = selected_surface_runtime().await;
    let node = db.node.clone();
    let ceiling = tempfile::tempdir().unwrap();
    // The tools sit at the ceiling root; the published root is a folder inside.
    let tool_root = ceiling.path().join("work");
    std::fs::create_dir_all(&tool_root).unwrap();
    let nested = tool_root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let tool_root_text = tool_root.to_string_lossy().into_owned();
    let nested_text = nested.to_string_lossy().into_owned();
    let owner = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        runtime.node_did.parse().unwrap(),
    );
    let published = owner
        .write(
            "test.workspace.root.nested",
            &format!(
                r#"mutation {{ create_WorkspaceRoot(input: {{node_did:"{}", root_path:"{}", enabled:true}}) {{_docID}} }}"#,
                gents_store::graphql::escape_graphql_string(&runtime.node_did),
                gents_store::graphql::escape_graphql_string(&nested_text),
            ),
        )
        .await
        .unwrap();
    assert!(gents_store::graphql::created_doc_id(&published, "WorkspaceRoot").is_ok());
    let agent = runtime.agent_identity.clone().unwrap();
    let mut document = preset_tools_document("nested", "ReadOnly", "Off", false, false);
    document.host.as_mut().unwrap().root = Some(tool_root_text.clone());
    let config = AgentToolSurfaceConfig::from_tools_document(
        agent.did(),
        &document,
        &ToolCeiling::readonly_at(ceiling.path()),
        Vec::new(),
    )
    .unwrap();
    let surface = config
        .resolve(&node, &runtime.node_did, &crate::plugin_host::NoPluginHost)
        .await
        .unwrap();
    let guard = surface.root_execution_guard().cloned().unwrap();
    let error = guard
        .validate(&node, agent.as_ref())
        .await
        .expect_err("no published root contains the tool root");
    let refusal = tool_root_refusal(error.as_ref()).expect("the refusal is typed");
    assert!(
        matches!(refusal, ToolRootRefusal::Denied { .. }),
        "a nested published root is not the reader cause, since its grant admits nothing: {error:#}"
    );
    assert!(!refusal.is_reader_missing());
    node.shutdown().await.unwrap();
}

#[test]
fn configured_host_limits_reach_the_surface_and_unset_keeps_the_defaults() {
    let ceiling = ToolCeiling::readwrite(temp_root("gents-host-limits"));
    let config = |host: serde_json::Value| {
        let tools: gents_documents::Tools = serde_json::from_value(serde_json::json!({
            "tools_id": "host-limits", "node_did": "did:key:example", "host": host
        }))
        .unwrap();
        AgentToolSurfaceConfig::from_tools_documents(
            "host-limits",
            &tools,
            &[],
            &[],
            &ceiling,
            vec![],
        )
        .unwrap()
    };
    let configured =
        config(serde_json::json!({"max_instruction_chars": 100, "max_background_tool_calls": 3}));
    assert_eq!(configured.max_instruction_chars(), 100);
    assert_eq!(configured.background_tools().limit.get(), 3);
    let unset = config(serde_json::json!({}));
    assert_eq!(
        unset.max_instruction_chars(),
        gents_documents::tool_limits::DEFAULT_MAX_INSTRUCTION_CHARS
    );
    assert_eq!(
        unset.background_tools().limit,
        gents_protocol::job::BackgroundLimit::default()
    );
}
