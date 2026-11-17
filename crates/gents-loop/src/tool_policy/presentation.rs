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

pub fn presentation_refused(granted: bool, hidden: bool, provider_origin: bool) -> bool {
    granted && hidden && provider_origin
}

pub fn effective_hidden(hidden: bool, script_ready: bool) -> bool {
    hidden && script_ready
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
    #[error("tool '{name}' is script-only for direct model calls. Inspect `tools.{name}.parameters`, then call `await tools.{name}({{...}})` through `script`")]
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
    let granted = tool.is_some();
    let hidden = tool.is_some_and(|tool| tool.hidden());
    if presentation_refused(granted, hidden, origin == InvocationOrigin::Provider) {
        Err(AdmissionRefusal::Hidden { name: name.into() })
    } else if origin == InvocationOrigin::Composition && !granted {
        Err(AdmissionRefusal::Unavailable { name: name.into() })
    } else {
        // Unknown provider names retain on_tool_call and the existing dispatch owner.
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("hidden_tool_names requires a selected, visible, working script with tool_calls: true after ceilings and availability; presentation gateways must stay visible")]
pub struct PresentationError;

pub fn present_tools(
    tools: Vec<Box<dyn ToolDyn>>,
    hidden_names: &[String],
    script_ready: bool,
) -> Result<Vec<Box<dyn ToolDyn>>, PresentationError> {
    let gateway_hidden = hidden_names.iter().any(|name| is_gateway(name));
    let script_ready = script_ready && tools.iter().any(|tool| tool.name() == "script");
    if gateway_hidden {
        return Err(PresentationError);
    }
    if hidden_names.is_empty() {
        return Ok(tools);
    }
    if !script_ready {
        tracing::warn!("hidden_tool_names inactive: script is unavailable; advertising all granted tools so configuration can be repaired");
        return Ok(tools);
    }
    let has_hidden = tools.iter().any(|tool| hidden_names.contains(&tool.name()));
    Ok(tools
        .into_iter()
        .map(|tool| {
            let name = tool.name();
            PresentedTool::wrap(
                tool,
                effective_hidden(hidden_names.contains(&name), script_ready),
                has_hidden && name == "script",
            )
        })
        .collect())
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

    struct Named(&'static str);
    impl ToolDyn for Named {
        fn hidden(&self) -> bool {
            false
        }
        fn name(&self) -> String {
            self.0.into()
        }
        fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
            Box::pin(async move {
                ToolDefinition {
                    name: self.name(),
                    description: "contract".into(),
                    parameters: serde_json::json!({"type":"object"}),
                }
            })
        }
        fn call<'a>(&'a self, _: String) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(async { Ok("called".into()) })
        }
    }

    #[tokio::test]
    async fn hidden_presentation_has_one_conditional_hint_and_never_constructs_missing_tools() {
        let surface = || -> Vec<Box<dyn ToolDyn>> {
            vec![Box::new(Named("script")), Box::new(Named("query"))]
        };
        let empty = present_tools(surface(), &[], false).unwrap();
        assert_eq!(
            empty[0].definition(String::new()).await.description,
            "contract"
        );
        let hidden = present_tools(surface(), &["query".into()], true).unwrap();
        assert!(hidden[1].hidden());
        assert!(!hidden[0].hidden());
        assert_eq!(
            hidden[0].definition(String::new()).await.description,
            format!("contract\n{SCRIPT_HINT}")
        );
        assert_eq!(hidden[1].call("{}".into()).await.unwrap(), "called");
        assert_eq!(
            hidden[1].definition(String::new()).await.description,
            "contract"
        );
        let unavailable = present_tools(surface(), &["unavailable".into()], true).unwrap();
        assert_eq!(unavailable.len(), 2);
        assert_eq!(
            unavailable[0].definition(String::new()).await.description,
            "contract"
        );
        assert!(matches!(
            admit(&unavailable, "unavailable", InvocationOrigin::Composition),
            Err(AdmissionRefusal::Unavailable { .. })
        ));
        for recovered in [
            present_tools(surface(), &["query".into()], false).unwrap(),
            present_tools(vec![Box::new(Named("query"))], &["query".into()], true).unwrap(),
        ] {
            assert!(recovered.iter().all(|tool| !tool.hidden()));
            assert!(admit(&recovered, "query", InvocationOrigin::Provider).is_ok());
        }
        for gateway in ["script", "config", "load_skill", "spawn_process"] {
            assert!(present_tools(surface(), &[gateway.into()], true).is_err());
        }
    }

    #[test]
    fn presentation_conforms_to_lean() {
        gents_lean_vocab::families::tool_presentation_presentation_refused::replay(|i| {
            presentation_refused(i.granted, i.hidden, i.provider_origin)
        });
        gents_lean_vocab::families::tool_presentation_effective_hidden::replay(|i| {
            effective_hidden(i.hidden, i.script_ready)
        });
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
