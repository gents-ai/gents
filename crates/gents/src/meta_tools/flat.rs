//! Flat presentation is only an adapter: invocation uses the dispatcher owner.
use super::{CallToolArgs, CallToolTool, MetaToolContext};
use crate::document_config::{RemoteToolStyle, RemoteTools};
use crate::llm::tool::{BoxFuture, Tool, ToolDefinition, ToolDyn, ToolError};

/// Stable provider-safe name, resolved only against the configured selection.
/// Hashing avoids ambiguous separators and provider name-length restrictions.
pub(crate) fn flat_tool_name(service: &str, tool: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update((service.len() as u64).to_be_bytes());
    hash.update(service.as_bytes());
    hash.update(tool.as_bytes());
    format!("mcp_{:x}", hash.finalize())[..64].to_string()
}

pub(crate) fn selected_remote_identity(
    name: &str,
    args: &str,
    remote: &RemoteTools,
) -> Option<(String, String)> {
    if let Some(identity) = super::selected_tool_identity(name, args) {
        return Some(identity);
    }
    let mut matches = remote
        .services
        .iter()
        .filter(|service| service.style == RemoteToolStyle::Flat)
        .flat_map(|service| service.tool_names.iter().map(move |tool| (service, tool)))
        .filter(|(service, tool)| flat_tool_name(&service.mcp_service_id, tool) == name);
    let (service, tool) = matches.next()?;
    matches
        .next()
        .is_none()
        .then(|| (service.mcp_service_id.clone(), tool.clone()))
}

pub(crate) fn presented_tool_names(remote: &RemoteTools, allowed: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    let mut discovery = false;
    for service in &remote.services {
        if !allowed.contains(&service.mcp_service_id) || service.tool_names.is_empty() {
            continue;
        }
        match service.style {
            RemoteToolStyle::Discovery => discovery = true,
            RemoteToolStyle::Flat => names.extend(
                service
                    .tool_names
                    .iter()
                    .map(|tool| flat_tool_name(&service.mcp_service_id, tool)),
            ),
        }
    }
    if discovery {
        names.extend(super::META_TOOL_NAMES.iter().map(|name| name.to_string()));
    }
    names
}

pub(super) struct FlatRemoteTool {
    pub(super) definition: ToolDefinition,
    pub(super) service_id: String,
    pub(super) tool_name: String,
    pub(super) context: MetaToolContext,
}

impl ToolDyn for FlatRemoteTool {
    fn name(&self) -> String {
        self.definition.name.clone()
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async { self.definition.clone() })
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let arguments = crate::llm::tool::parse_tool_args(&args)?;
            Tool::call(
                &CallToolTool::new(self.context.clone()),
                CallToolArgs {
                    service_id: self.service_id.clone(),
                    tool_name: self.tool_name.clone(),
                    arguments,
                },
            )
            .await
            .map_err(CallToolTool::into_dyn_error)
        })
    }
}
