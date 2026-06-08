use super::*;

fn temp_root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn selection_file_tool_root_clamps_within_operator_root() {
    let operator_root = temp_root("defra-agent-operator-root");
    let scoped_root = operator_root.join("bench").join("results");
    std::fs::create_dir_all(&scoped_root).unwrap();

    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadWrite,
            file_tool_root: Some(scoped_root.clone()),
            bash: BashMode::Unrestricted,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
        },
        &ToolCeiling::readwrite(operator_root.clone()),
        Vec::new(),
    )
    .unwrap();

    let canonical_scoped_root = std::fs::canonicalize(&scoped_root).unwrap();
    let native_tools = config.host_tools().native_tools();
    assert!(matches!(
        native_tools[0],
        crate::toolset::NativeTool::ListFiles { .. }
    ));
    assert!(matches!(
        native_tools[1],
        crate::toolset::NativeTool::ReadFile { .. }
    ));
    assert!(matches!(
        native_tools[2],
        crate::toolset::NativeTool::Glob { .. }
    ));
    assert!(matches!(
        native_tools[3],
        crate::toolset::NativeTool::Grep { .. }
    ));
    assert!(matches!(
        native_tools[4],
        crate::toolset::NativeTool::WriteFile { ref root } if root == &canonical_scoped_root
    ));
    assert!(matches!(
        native_tools[5],
        crate::toolset::NativeTool::EditFile { ref root } if root == &canonical_scoped_root
    ));
    assert!(matches!(
        native_tools[6],
        crate::toolset::NativeTool::BashUnrestricted { ref root, .. } if root == &canonical_scoped_root
    ));
}

#[test]
fn selection_file_tool_root_rejects_escape_outside_operator_root() {
    let operator_root = temp_root("defra-agent-operator-root");
    let outside_root = temp_root("defra-agent-outside-root");

    let error = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: Some(outside_root),
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
        },
        &ToolCeiling::readwrite(operator_root),
        Vec::new(),
    )
    .expect_err("selection root outside operator ceiling should fail");

    assert!(
        error.to_string().contains("escapes operator tool root"),
        "{error:#}"
    );
}

#[test]
fn readonly_selection_file_tool_root_rejects_escape_outside_operator_root() {
    let operator_root = temp_root("defra-agent-operator-root");
    let outside_root = temp_root("defra-agent-outside-root");

    let error = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: Some(outside_root),
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
        },
        &ToolCeiling::readonly_at(operator_root),
        Vec::new(),
    )
    .expect_err("readonly selection root outside operator ceiling should fail");

    assert!(
        error.to_string().contains("escapes operator tool root"),
        "{error:#}"
    );
}

#[test]
fn downgraded_off_selection_ignores_stale_file_tool_root() {
    let stale_root =
        std::env::temp_dir().join(format!("defra-agent-stale-root-{}", uuid::Uuid::new_v4()));

    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: Some(stale_root),
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap();

    assert!(config.host_tools().native_tools().is_empty());
    assert!(config.host_tools().build_native_tools().unwrap().is_empty());
}

#[test]
fn readonly_ceiling_clamps_unrestricted_bash_policy() {
    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadWrite,
            file_tool_root: None,
            bash: BashMode::Unrestricted,
            command_policy: Some(
                crate::toolset::CommandExecutionPolicy::write_capable()
                    .with_mode(crate::toolset::CommandExecutionMode::Unrestricted),
            ),
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
        },
        &ToolCeiling::readonly(),
        Vec::new(),
    )
    .unwrap();

    assert_eq!(config.host_tools(), &crate::toolset::ToolSet::readonly());
}

#[test]
fn selection_without_root_inherits_operator_root() {
    let operator_root = temp_root("defra-agent-operator-root");

    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadWrite,
            file_tool_root: None,
            bash: BashMode::Unrestricted,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
        },
        &ToolCeiling::readwrite(operator_root.clone()),
        Vec::new(),
    )
    .unwrap();

    let canonical_operator_root = std::fs::canonicalize(&operator_root).unwrap();
    let native_tools = config.host_tools().native_tools();
    assert!(matches!(
        native_tools[4],
        crate::toolset::NativeTool::WriteFile { ref root } if root == &canonical_operator_root
    ));
    assert!(matches!(
        native_tools[5],
        crate::toolset::NativeTool::EditFile { ref root } if root == &canonical_operator_root
    ));
    assert!(matches!(
        native_tools[6],
        crate::toolset::NativeTool::BashUnrestricted { ref root, .. } if root == &canonical_operator_root
    ));
}

#[test]
fn selection_cli_tools_require_ceiling_entries() {
    let operator_root = temp_root("defra-agent-operator-root");

    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: vec!["rg".to_string()],
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
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
        .all(|tool| !matches!(tool, crate::toolset::NativeTool::Cli(_))));
}

#[test]
fn selection_cli_tools_expose_only_ceiling_entries() {
    let operator_root = temp_root("defra-agent-operator-root");
    let ceiling = ToolCeiling::readwrite(operator_root).with_cli_tool(cli_tool(
        "rg",
        "/usr/bin/rg",
        "Search files with ripgrep",
    ));

    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: vec!["rg".to_string(), "cargo".to_string()],
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
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
            .filter(|tool| matches!(tool, crate::toolset::NativeTool::Cli(_)))
            .count(),
        1
    );
}

#[test]
fn selection_mcp_service_allowlist_is_deduped() {
    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: true,
            allowed_mcp_service_ids: vec![
                "x-data".to_string(),
                "x-data".to_string(),
                "observability-mcp".to_string(),
            ],
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
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

#[test]
fn background_tool_allowlist_registers_r6_tools() {
    let config = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: None,
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: vec!["bash".to_string(), "bash".to_string()],
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
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
    let error = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadOnly,
            file_tool_root: None,
            bash: BashMode::ReadOnly,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: vec!["read_file".to_string()],
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
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
    let operator_root = temp_root("defra-agent-operator-root");
    let outside_root = temp_root("defra-agent-outside-root");
    let symlink_path = operator_root.join("link-out");
    std::os::unix::fs::symlink(&outside_root, &symlink_path).unwrap();

    let error = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            file_tools: FileToolMode::ReadWrite,
            file_tool_root: Some(symlink_path.join("workspace")),
            bash: BashMode::Unrestricted,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
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
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let enabled = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_defra_query: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();
    assert!(enabled.tool_names().contains(&"defra_query".to_string()));

    let disabled = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_defra_query: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();
    assert!(!disabled.tool_names().contains(&"defra_query".to_string()));
}

#[tokio::test]
async fn write_tools_register_under_declared_names() {
    use crate::document_config::{WriteToolDecl, WriteToolField};

    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let surface = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_defra_query: false,
            write_tools: vec![
                WriteToolDecl {
                    tool_name: "request_action".to_string(),
                    collection: "ActionRequest".to_string(),
                    description: "Request an action".to_string(),
                    fields: vec![WriteToolField {
                        name: "summary".to_string(),
                        required: true,
                    }],
                },
                // Malformed: empty collection — must be skipped, not advertised.
                WriteToolDecl {
                    tool_name: "broken_tool".to_string(),
                    collection: "  ".to_string(),
                    description: String::new(),
                    fields: Vec::new(),
                },
            ],
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();

    let names = surface.tool_names();
    assert!(
        names.contains(&"request_action".to_string()),
        "declared write tool should be advertised under its tool_name; got {names:?}"
    );
    assert!(
        !names.contains(&"broken_tool".to_string()),
        "malformed write tool (empty collection) must be skipped; got {names:?}"
    );

    // The built dynamic tools must carry the per-decl name too.
    let runtime = ToolRuntimeContext::oneshot(std::sync::Arc::new(node));
    let built = surface.build_tools(&runtime).unwrap();
    assert!(
        built.iter().any(|tool| tool.name() == "request_action"),
        "registered dynamic tool should advertise decl.tool_name"
    );
    assert!(
        !built.iter().any(|tool| tool.name() == "broken_tool"),
        "malformed decl must not produce a registered tool"
    );
}

#[tokio::test]
async fn write_tool_colliding_with_builtin_is_not_registered_twice() {
    use crate::document_config::{WriteToolDecl, WriteToolField};

    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    // `context_budget` is always registered by build_tools. A write tool that
    // reuses that name (here bypassing apply-time validation by constructing the
    // surface directly) must be dropped by the runtime guard, not registered as
    // a second ToolDyn under the same name.
    let surface = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_defra_query: false,
            write_tools: vec![WriteToolDecl {
                tool_name: "context_budget".to_string(),
                collection: "ActionRequest".to_string(),
                description: String::new(),
                fields: vec![WriteToolField {
                    name: "summary".to_string(),
                    required: false,
                }],
            }],
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();

    let runtime = ToolRuntimeContext::oneshot(std::sync::Arc::new(node));
    let built = surface.build_tools(&runtime).unwrap();
    let count = built
        .iter()
        .filter(|tool| tool.name() == "context_budget")
        .count();
    assert_eq!(
        count, 1,
        "a write tool colliding with a built-in must not register a second impl under that name"
    );
}

#[test]
fn memory_tool_defaults_disabled() {
    assert!(!ToolSelection::default().enable_memory);
    assert!(!ToolSelection::default().enable_session_history_tool);
}

#[tokio::test]
async fn session_history_tool_requires_selection_opt_in() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let disabled = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_session_history_tool: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();
    assert!(!disabled
        .tool_names()
        .contains(&crate::toolset::SESSION_HISTORY_TOOL_NAME.to_string()));

    let enabled = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_session_history_tool: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();
    assert!(enabled
        .tool_names()
        .contains(&crate::toolset::SESSION_HISTORY_TOOL_NAME.to_string()));
}

#[cfg(feature = "agent-memory")]
#[tokio::test]
async fn memory_tool_requires_selection_opt_in() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let disabled = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_memory: false,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();
    assert!(!disabled
        .tool_names()
        .contains(&crate::toolset::MEMORY_TOOL_NAME.to_string()));

    let enabled = BehaviorToolConfig::from_selection(
        "ops",
        ToolSelection {
            enable_memory: true,
            ..Default::default()
        },
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node)
    .await
    .unwrap();
    assert!(enabled
        .tool_names()
        .contains(&crate::toolset::MEMORY_TOOL_NAME.to_string()));
}
