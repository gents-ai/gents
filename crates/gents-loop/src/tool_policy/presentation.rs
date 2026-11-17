use crate::tool::{
    BoxFuture, ToolDefinition, ToolDispatchResult, ToolDyn, ToolError, ToolFootprint,
    ToolRequestContext,
};

pub fn advertised(granted: bool, hidden: bool) -> bool {
    granted && !hidden
}

pub fn direct_admissible(granted: bool, hidden: bool) -> bool {
    advertised(granted, hidden)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationOrigin {
    Provider,
    Composition,
}

pub fn admissible(granted: bool, hidden: bool, origin: InvocationOrigin) -> bool {
    match origin {
        InvocationOrigin::Provider => direct_admissible(granted, hidden),
        InvocationOrigin::Composition => granted,
    }
}

pub fn is_gateway(name: &str) -> bool {
    matches!(name, "script" | "config" | "load_skill" | "spawn_process")
}

pub fn configuration_valid(
    has_hidden: bool,
    all_selected: bool,
    script_ready: bool,
    gateway_hidden: bool,
) -> bool {
    !has_hidden || (all_selected && script_ready && !gateway_hidden)
}

#[derive(Debug, thiserror::Error)]
pub enum AdmissionRefusal {
    #[error("tool '{name}' is script-only for direct model calls. Inspect `tools.{name}.parameters`, then call `await tools.{name}({{...}})` through script")]
    Hidden { name: String },
    #[error("tool '{name}' is not in this request's granted tool surface")]
    Unavailable { name: String },
}

pub fn admit(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    origin: InvocationOrigin,
) -> Result<(), AdmissionRefusal> {
    let tool = tools.iter().find(|tool| tool.name() == name);
    if admissible(
        tool.is_some(),
        tool.is_some_and(|tool| tool.hidden()),
        origin,
    ) {
        Ok(())
    } else if tool.is_some() {
        Err(AdmissionRefusal::Hidden { name: name.into() })
    } else {
        Err(AdmissionRefusal::Unavailable { name: name.into() })
    }
}

const SCRIPT_HINT: &str = "Some granted tools are script-only: use `Object.keys(tools)`, then `tools.NAME.description` and `.parameters`.";

pub struct PresentedTool {
    tool: Box<dyn ToolDyn>,
    hidden: bool,
    script_hint: bool,
}

impl PresentedTool {
    pub fn wrap(tool: Box<dyn ToolDyn>, hidden: bool, script_hint: bool) -> Box<dyn ToolDyn> {
        if !hidden && !script_hint {
            tool
        } else {
            Box::new(Self {
                tool,
                hidden,
                script_hint,
            })
        }
    }
}

impl ToolDyn for PresentedTool {
    fn name(&self) -> String {
        self.tool.name()
    }
    fn hidden(&self) -> bool {
        self.hidden
    }
    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            let mut definition = self.tool.definition(prompt).await;
            if self.script_hint {
                definition.description.push('\n');
                definition.description.push_str(SCRIPT_HINT);
            }
            definition
        })
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        self.tool.call(args)
    }
    fn call_with_receipt<'a>(&'a self, args: String) -> BoxFuture<'a, ToolDispatchResult> {
        self.tool.call_with_receipt(args)
    }
    fn call_with_request<'a>(
        &'a self,
        args: String,
        request: &'a ToolRequestContext,
    ) -> BoxFuture<'a, ToolDispatchResult> {
        self.tool.call_with_request(args, request)
    }
    fn footprint(&self, args: &str) -> ToolFootprint {
        self.tool.footprint(args)
    }
    fn emits_command_envelope(&self) -> bool {
        self.tool.emits_command_envelope()
    }
    fn admit(&self, args: &str) -> Result<(), ToolError> {
        self.tool.admit(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_conforms_to_lean() {
        for case in gents_lean_vocab::families::tool_presentation_gateways::cases() {
            case.check(is_gateway(&case.input));
        }
        for case in gents_lean_vocab::families::tool_presentation_advertised::cases() {
            case.check(advertised(case.input.granted, case.input.hidden));
        }
        for case in gents_lean_vocab::families::tool_presentation_direct_admissible::cases() {
            case.check(direct_admissible(case.input.granted, case.input.hidden));
        }
        for case in gents_lean_vocab::families::tool_presentation_admissible::cases() {
            let origin = if case.input.provider_origin {
                InvocationOrigin::Provider
            } else {
                InvocationOrigin::Composition
            };
            case.check(admissible(case.input.granted, case.input.hidden, origin));
        }
        for case in gents_lean_vocab::families::tool_presentation_configuration_valid::cases() {
            case.check(configuration_valid(
                case.input.has_hidden,
                case.input.all_selected,
                case.input.script_ready,
                case.input.gateway_hidden,
            ));
        }
    }
}
