mod modes;
mod selection;
mod runtime_context;
mod behavior_config;
mod build;

pub use modes::{BashMode, FileToolMode, ToolCeiling};
pub use runtime_context::ToolRuntimeContext;
pub use selection::{CustomToolFactory, ToolSelection};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use rig::tool::ToolDyn;

use crate::meta_tools::{build_meta_tools, META_TOOL_NAMES};
use crate::toolset::{
    build_delegate_tool, CliToolConfig, ToolSet, ToolSetBuilder, DELEGATE_TOOL_NAME,
};

const DEFAULT_CLI_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
pub struct BehaviorToolConfig {
    host_tools: ToolSet,
    enable_meta_tools: bool,
    delegate_to: Vec<String>,
    custom_tools: Vec<CustomToolFactory>,
}

impl BehaviorToolConfig {
    pub fn meta_only() -> Self {
        Self {
            host_tools: ToolSet::meta_only(),
            enable_meta_tools: true,
            delegate_to: Vec::new(),
            custom_tools: Vec::new(),
        }
    }

    pub fn from_selection(
        behavior_name: &str,
        selection: ToolSelection,
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        let ToolSelection {
            file_tools: requested_file_tools,
            file_tool_root,
            bash: requested_bash,
            cli_tool_names,
            enable_meta_tools,
            delegate_to,
        } = selection;
        let file_tools =
            downgrade_file_tools(behavior_name, requested_file_tools, ceiling.file_tools());
        let bash = downgrade_bash(behavior_name, requested_bash, ceiling.bash());
        let host_tools = build_host_tools(
            behavior_name,
            file_tools,
            bash,
            file_tool_root.as_deref(),
            &cli_tool_names,
            ceiling,
        )?;

        Ok(Self {
            host_tools,
            enable_meta_tools,
            delegate_to: dedupe_strings(delegate_to),
            custom_tools,
        })
    }

    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn meta_tools_requested(&self) -> bool {
        self.enable_meta_tools
    }

    pub fn delegate_to(&self) -> &[String] {
        &self.delegate_to
    }

    pub fn custom_tool_names(&self) -> Vec<String> {
        self.custom_tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    pub async fn resolve(&self, node: &EmbeddedNode) -> Result<ToolSurface> {
        let include_meta_tools = if self.enable_meta_tools {
            has_registered_mcp_services(node).await?
        } else {
            false
        };

        Ok(ToolSurface {
            host_tools: self.host_tools.clone(),
            include_meta_tools,
            delegate_to: self.delegate_to.clone(),
            custom_tools: self.custom_tools.clone(),
        })
    }
}

impl Default for BehaviorToolConfig {
    fn default() -> Self {
        Self::meta_only()
    }
}

impl std::fmt::Debug for BehaviorToolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BehaviorToolConfig")
            .field("host_tools", &self.host_tools)
            .field("enable_meta_tools", &self.enable_meta_tools)
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
        dedupe_strings(names)
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

fn downgrade_file_tools(
    behavior_name: &str,
    requested: FileToolMode,
    ceiling: FileToolMode,
) -> FileToolMode {
    if requested.rank() <= ceiling.rank() {
        return requested;
    }

    tracing::warn!(
        behavior_id = %behavior_name,
        requested = ?requested,
        ceiling = ?ceiling,
        "downgrading file tool mode to fit tool ceiling"
    );
    ceiling
}

fn downgrade_bash(behavior_name: &str, requested: BashMode, ceiling: BashMode) -> BashMode {
    if requested.rank() <= ceiling.rank() {
        return requested;
    }

    tracing::warn!(
        behavior_id = %behavior_name,
        requested = ?requested,
        ceiling = ?ceiling,
        "downgrading bash mode to fit tool ceiling"
    );
    ceiling
}

fn build_host_tools(
    behavior_name: &str,
    file_tools: FileToolMode,
    bash: BashMode,
    file_tool_root: Option<&Path>,
    cli_tool_names: &[String],
    ceiling: &ToolCeiling,
) -> Result<ToolSet> {
    let mut builder = ToolSetBuilder::default();
    let needs_file_tool_root =
        !matches!(file_tools, FileToolMode::Off) || !matches!(bash, BashMode::Off);
    let effective_root = if needs_file_tool_root {
        resolve_effective_tool_root(behavior_name, file_tool_root, ceiling.root())?
    } else {
        None
    };
    if let Some(root) = effective_root.clone() {
        builder = builder.read_root(root.clone());
    }

    if !matches!(file_tools, FileToolMode::Off) {
        builder = builder.list_files().read_file().glob().grep();
    }

    if matches!(file_tools, FileToolMode::ReadWrite) {
        let root = effective_root
            .clone()
            .ok_or_else(|| anyhow!("readwrite file tools require a configured tool root"))?;
        builder = builder.write_file(root.clone()).edit_file(root);
    }

    match bash {
        BashMode::Off => {}
        BashMode::ReadOnly => {
            builder = builder.bash_read_only();
        }
        BashMode::Unrestricted => {
            let root = effective_root
                .clone()
                .ok_or_else(|| anyhow!("unrestricted bash requires a configured tool root"))?;
            builder = builder.bash_unrestricted(root);
        }
    }

    let cli_tools = ceiling
        .cli_tools()
        .iter()
        .map(|tool| (tool.name.clone(), tool.clone()))
        .collect::<HashMap<_, _>>();
    for tool_name in dedupe_strings(cli_tool_names.to_vec()) {
        match cli_tools.get(&tool_name) {
            Some(tool) => builder = builder.cli_tool(tool.clone()),
            None => tracing::warn!(
                behavior_id = %behavior_name,
                cli_tool = %tool_name,
                "dropping CLI tool not present in tool ceiling"
            ),
        }
    }

    Ok(builder.build())
}

fn resolve_effective_tool_root(
    behavior_name: &str,
    selection_root: Option<&Path>,
    ceiling_root: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let selection_root = selection_root
        .map(resolve_configured_tool_root)
        .transpose()?;
    let ceiling_root = ceiling_root.map(resolve_configured_tool_root).transpose()?;

    match (selection_root, ceiling_root) {
        (Some(selection_root), Some(ceiling_root)) => {
            if selection_root.starts_with(&ceiling_root) {
                Ok(Some(selection_root))
            } else {
                bail!(
                    "behavior {behavior_name} file tool root {} escapes operator tool root {}",
                    selection_root.display(),
                    ceiling_root.display()
                );
            }
        }
        (Some(selection_root), None) => Ok(Some(selection_root)),
        (None, Some(ceiling_root)) => Ok(Some(ceiling_root)),
        (None, None) => Ok(None),
    }
}

fn resolve_configured_tool_root(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .with_context(|| format!("resolving relative tool root {}", path.display()))?
            .join(path)
    };

    resolve_path_with_canonical_prefix(&absolute)
}

fn resolve_path_with_canonical_prefix(path: &Path) -> Result<PathBuf> {
    let mut resolved = PathBuf::new();
    let mut missing_tail = false;

    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                resolved.push(component.as_os_str());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(name) => {
                let candidate = resolved.join(name);
                if !missing_tail && candidate.exists() {
                    resolved = std::fs::canonicalize(&candidate).with_context(|| {
                        format!("canonicalizing tool root {}", candidate.display())
                    })?;
                } else {
                    missing_tail = true;
                    resolved.push(name);
                }
            }
        }
    }

    Ok(resolved)
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            deduped.push(trimmed.to_string());
        }
    }
    deduped
}

async fn has_registered_mcp_services(node: &EmbeddedNode) -> Result<bool> {
    let query = r#"{
  ToolServiceRegistry(
    filter: { status: { _eq: "online" } }
    limit: 1
  ) {
    service_id
  }
}"#;

    let response = node.execute(query).await;
    if response.has_errors() {
        bail!(
            "query ToolServiceRegistry for tool-surface resolution failed: {:?}",
            response.errors
        );
    }

    let services = response
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(!services.is_empty())
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
