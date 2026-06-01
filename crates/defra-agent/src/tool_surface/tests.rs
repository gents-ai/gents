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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: vec!["bash".to_string(), "bash".to_string()],
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: vec!["read_file".to_string()],
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
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
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
