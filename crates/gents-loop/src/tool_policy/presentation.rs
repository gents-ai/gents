use crate::tool::{
    BoxFuture, ToolCallEnvelope, ToolDefinition, ToolDispatchResult, ToolDyn, ToolError,
    ToolFootprint, ToolRequestContext,
};

/// Lean `ToolPresentation.Origin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Provider,
    Composition,
}

/// Lean `ToolPresentation.admissionDecision`, refusing what it does not allow.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionRefusal {
    #[error("tool '{name}' is not offered to direct calls; inspect `tools.{name}.parameters` in `script`, then call `await tools.{name}({{...}})` there")]
    Hidden { name: String },
    #[error("tool '{name}' is not in this request's granted tool surface")]
    Unavailable { name: String },
}

pub fn admit(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    origin: Origin,
) -> Result<(), AdmissionRefusal> {
    let tool = tools.iter().find(|tool| tool.name() == name);
    match (origin, tool) {
        (Origin::Provider, Some(tool)) if tool.hidden() => {
            Err(AdmissionRefusal::Hidden { name: name.into() })
        }
        (Origin::Composition, None) => Err(AdmissionRefusal::Unavailable { name: name.into() }),
        _ => Ok(()),
    }
}

/// Lean `ToolPresentation.isGateway`.
pub fn is_gateway(name: &str) -> bool {
    matches!(name, "script" | "config" | "load_skill" | "spawn_process")
}

/// Lean `ToolPresentation.effectiveHidden`.
pub fn effective_hidden(listed: bool, gateway: bool, script_present: bool) -> bool {
    listed && !gateway && script_present
}

/// Lean `ToolPresentation.configurationValid`.
pub fn configuration_valid(hides_others: bool, script_selected: bool, hides_gateway: bool) -> bool {
    !hides_gateway && (!hides_others || script_selected)
}

/// What `script` adds to its description while `hidden` are offered only
/// through it: their names, then how to describe and call one.
pub fn script_hint(hidden: &[String]) -> String {
    format!(
        "Offered only here, not as direct tools: {}. Read `tools.NAME.description` and `tools.NAME.parameters` before the first call, then `await tools.NAME({{...}})`.",
        hidden.join(", ")
    )
}

/// Lean `ToolPresentation.defaultHidden`.
pub fn default_hidden(name: &str) -> bool {
    matches!(name, "charts" | "data_tables" | "browser_exec")
}

/// Lean `ToolPresentation.listedAfterWiring`.
pub fn listed_after_wiring(
    default_hidden: bool,
    listed: bool,
    selected_before: bool,
    script_before: bool,
    selected_after: bool,
    script_after: bool,
) -> bool {
    script_after
        && (listed || (default_hidden && selected_after && !(selected_before && script_before)))
}

/// The names among `names` the model is not shown: those listed, while
/// `script` is among `names`.
pub fn hidden_names(names: &[String], listed: &[String]) -> Vec<String> {
    let script_present = names.iter().any(|name| name == "script");
    names
        .iter()
        .filter(|name| effective_hidden(listed.contains(name), is_gateway(name), script_present))
        .cloned()
        .collect()
}

/// `tools` as the model is shown them, with `script` saying that some are
/// offered only through it.
pub fn present_tools(tools: Vec<Box<dyn ToolDyn>>, listed: &[String]) -> Vec<Box<dyn ToolDyn>> {
    if listed.is_empty() {
        return tools;
    }
    let names: Vec<String> = tools.iter().map(|tool| tool.name()).collect();
    if !names.iter().any(|name| name == "script") {
        tracing::warn!(
            hidden_tool_names = ?listed,
            "hidden_tool_names is inactive because script is not on this agent's tool surface; every granted tool is shown"
        );
    }
    let hidden = hidden_names(&names, listed);
    tools
        .into_iter()
        .map(|tool| {
            let name = tool.name();
            let hide = hidden.contains(&name);
            let hint = (!hidden.is_empty() && name == "script").then(|| script_hint(&hidden));
            if hide || hint.is_some() {
                Box::new(PresentedTool {
                    tool,
                    hidden: hide,
                    hint,
                }) as Box<dyn ToolDyn>
            } else {
                tool
            }
        })
        .collect()
}

struct PresentedTool {
    tool: Box<dyn ToolDyn>,
    hidden: bool,
    hint: Option<String>,
}

impl PresentedTool {
    fn hinted(&self, mut definition: ToolDefinition) -> ToolDefinition {
        if let Some(hint) = &self.hint {
            definition.description.push('\n');
            definition.description.push_str(hint);
        }
        definition
    }
}

impl ToolDyn for PresentedTool {
    fn hidden(&self) -> bool {
        self.hidden
    }
    fn name(&self) -> String {
        self.tool.name()
    }
    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move { self.hinted(self.tool.definition(prompt).await) })
    }
    fn definition_for<'a>(
        &'a self,
        prompt: String,
        context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move { self.hinted(self.tool.definition_for(prompt, context).await) })
    }
    fn call<'a>(
        &'a self,
        args: String,
        context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        self.tool.call(args, context)
    }
    fn call_with_receipt<'a>(
        &'a self,
        args: String,
        context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, ToolDispatchResult> {
        self.tool.call_with_receipt(args, context)
    }
    fn call_with_request<'a>(
        &'a self,
        args: String,
        context: &'a ToolCallEnvelope,
        request: &'a ToolRequestContext,
    ) -> BoxFuture<'a, ToolDispatchResult> {
        self.tool.call_with_request(args, context, request)
    }
    fn footprint(&self, args: &str, context: &ToolCallEnvelope) -> ToolFootprint {
        self.tool.footprint(args, context)
    }
    fn emits_command_envelope(&self) -> bool {
        self.tool.emits_command_envelope()
    }
    fn admit(&self, args: &str, context: &ToolCallEnvelope) -> Result<(), ToolError> {
        self.tool.admit(args, context)
    }
}

#[cfg(test)]
mod tests;
