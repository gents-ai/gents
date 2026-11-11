use std::path::{Path, PathBuf};

use anyhow::Result;
use gents_service::args::ToolCeilingArg;
use gents_tools::surface::cli_tool;

pub fn parse_cli_tool_arg(value: &str) -> Result<gents_tools::native::CliToolConfig> {
    let (name, path) = value
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("--cli-tool must be NAME=/absolute/path"))?;
    let name = name.trim();
    let path = path.trim();
    if name.is_empty() || path.is_empty() {
        anyhow::bail!("--cli-tool must be NAME=/absolute/path");
    }

    Ok(cli_tool(
        name,
        PathBuf::from(path),
        format!("Run the approved {name} CLI."),
    ))
}

/// The host ceiling a `gents runtime server` enforces for this mode and root. Serve
/// resolves the readonly default root before calling; a readonly ceiling
/// without one leaves file tools unrooted.
pub fn host_tool_ceiling(
    mode: ToolCeilingArg,
    root: Option<&Path>,
) -> Result<gents_tools::surface::ToolCeiling> {
    Ok(match (mode, root) {
        (ToolCeilingArg::MetaOnly, _) => gents_tools::surface::ToolCeiling::meta_only(),
        (ToolCeilingArg::Readonly, Some(root)) => {
            gents_tools::surface::ToolCeiling::readonly_at(root)
        }
        (ToolCeilingArg::Readonly, None) => gents_tools::surface::ToolCeiling::readonly(),
        (ToolCeilingArg::Readwrite, Some(root)) => {
            gents_tools::surface::ToolCeiling::readwrite(root)
        }
        (ToolCeilingArg::Readwrite, None) => {
            anyhow::bail!("--tool-root is required when --tool-ceiling readwrite")
        }
    })
}

pub fn format_tool_ceiling(value: ToolCeilingArg) -> &'static str {
    match value {
        ToolCeilingArg::MetaOnly => "meta-only",
        ToolCeilingArg::Readonly => "readonly",
        ToolCeilingArg::Readwrite => "readwrite",
    }
}
