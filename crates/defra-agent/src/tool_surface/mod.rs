mod modes;
mod selection;
mod runtime_context;
mod behavior_config;
mod build;

pub use behavior_config::BehaviorToolConfig;
pub use modes::{BashMode, FileToolMode, ToolCeiling};
pub use runtime_context::ToolRuntimeContext;
pub use selection::{CustomToolFactory, ToolSelection};

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use rig::tool::ToolDyn;

use crate::meta_tools::{build_meta_tools, META_TOOL_NAMES};
use crate::toolset::{build_delegate_tool, CliToolConfig, ToolSet, DELEGATE_TOOL_NAME};

const DEFAULT_CLI_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
pub struct ToolSurface {
    host_tools: ToolSet,
    include_meta_tools: bool,
    delegate_to: Vec<String>,
    custom_tools: Vec<CustomToolFactory>,
}

impl ToolSurface {
    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn includes_meta_tools(&self) -> bool {
        self.include_meta_tools
    }

    pub fn delegate_to(&self) -> &[String] {
        &self.delegate_to
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names = self.host_tools.tool_names();
        if self.include_meta_tools {
            names.extend(META_TOOL_NAMES.iter().map(|name| (*name).to_string()));
        }
        if !self.delegate_to.is_empty() {
            names.push(DELEGATE_TOOL_NAME.to_string());
        }
        names.extend(self.custom_tools.iter().map(|tool| tool.name().to_string()));
        build::dedupe_strings(names)
    }

    pub fn build_tools(&self, runtime: &ToolRuntimeContext) -> Result<Vec<Box<dyn ToolDyn>>> {
        let mut tools = self.host_tools.build_native_tools()?;
        if !self.delegate_to.is_empty() {
            tools.push(build_delegate_tool(
                runtime.node.clone(),
                self.delegate_to.clone(),
            ));
        }
        if self.include_meta_tools {
            tools.extend(build_meta_tools(
                runtime.node.clone(),
                runtime.mcp_pool.clone(),
                runtime.health_map.clone(),
                runtime.local_hostname.clone(),
                runtime.local_subnet.clone(),
            ));
        }
        for tool in &self.custom_tools {
            tools.push(tool.build()?);
        }
        Ok(tools)
    }
}

impl std::fmt::Debug for ToolSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSurface")
            .field("host_tools", &self.host_tools)
            .field("include_meta_tools", &self.include_meta_tools)
            .field("delegate_to", &self.delegate_to)
            .field(
                "custom_tools",
                &self
                    .custom_tools
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

pub fn cli_tool(
    name: impl Into<String>,
    binary_path: impl Into<PathBuf>,
    description: impl Into<String>,
) -> CliToolConfig {
    CliToolConfig {
        name: name.into(),
        binary_path: binary_path.into(),
        description: description.into(),
        allowed_argv_prefixes: Vec::new(),
        env_vars: HashMap::new(),
        working_dir: None,
        timeout_secs: DEFAULT_CLI_TIMEOUT_SECS,
    }
}


#[cfg(test)]
mod tests {
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
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                delegate_to: Vec::new(),
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
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                delegate_to: Vec::new(),
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
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                delegate_to: Vec::new(),
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
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                delegate_to: Vec::new(),
            },
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
        .unwrap();

        assert!(config.host_tools().native_tools().is_empty());
        assert!(config.host_tools().build_native_tools().unwrap().is_empty());
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
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                delegate_to: Vec::new(),
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
                cli_tool_names: vec!["rg".to_string()],
                enable_meta_tools: false,
                delegate_to: Vec::new(),
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
                cli_tool_names: vec!["rg".to_string(), "cargo".to_string()],
                enable_meta_tools: false,
                delegate_to: Vec::new(),
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
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                delegate_to: Vec::new(),
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
}
