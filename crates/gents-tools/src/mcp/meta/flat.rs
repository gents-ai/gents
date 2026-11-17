use super::{CallToolArgs, CallToolTool, MetaToolContext};
use gents_documents::{RemoteToolStyle, RemoteTools};
use gents_loop::tool::{BoxFuture, Tool, ToolDefinition, ToolDyn, ToolError};

pub fn selected_remote_identity(
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
        .filter(|(service, tool)| {
            RemoteTools::flat_tool_name(&service.mcp_service_id, tool) == name
        });
    let (service, tool) = matches.next()?;
    matches
        .next()
        .is_none()
        .then(|| (service.mcp_service_id.clone(), tool.clone()))
}

pub(super) struct FlatRemoteTool {
    pub(super) definition: ToolDefinition,
    pub(super) service_id: String,
    pub(super) tool_name: String,
    pub(super) context: MetaToolContext,
}

impl ToolDyn for FlatRemoteTool {
    fn hidden(&self) -> bool {
        false
    }
    fn name(&self) -> String {
        self.definition.name.clone()
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async { self.definition.clone() })
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let arguments = gents_loop::tool::parse_tool_args(&args)?;
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

    fn admit(&self, args: &str) -> Result<(), ToolError> {
        let Ok(arguments) = gents_loop::tool::parse_tool_args::<serde_json::Value>(args) else {
            return Ok(());
        };
        Tool::admit(
            &CallToolTool::new(self.context.clone()),
            &CallToolArgs {
                service_id: self.service_id.clone(),
                tool_name: self.tool_name.clone(),
                arguments,
            },
        )
        .map_err(CallToolTool::into_dyn_error)
    }
}
