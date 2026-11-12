use super::*;
pub struct InvalidProgressProbe;

impl ToolDyn for InvalidProgressProbe {
    fn name(&self) -> String {
        "invalid_progress_probe".into()
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "invalid_progress_probe".into(),
                description: "typed outcome fixture".into(),
                parameters: serde_json::json!({"type":"object","properties":{"outcome":{"type":"string"}},"required":["outcome"]}),
            }
        })
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            use gents_loop::tool_call_lifecycle::FailureClass;
            let value: serde_json::Value = serde_json::from_str(&args).unwrap();
            let class = match value["outcome"].as_str().unwrap() {
                "invalidArguments" => FailureClass::ArgumentInvalid,
                "policyDenied" => FailureClass::PolicyDenied,
                "ordinaryFailure" => FailureClass::ToolReturnedError,
                "success" => return Ok(r#"{"failure_class":"policyDenied","ok":false}"#.into()),
                other => panic!("unknown fixture outcome {other}"),
            };
            Err(ToolError::ReportedFailure {
                class,
                text: format!("fixture {}", class.as_str()),
            })
        })
    }
}

pub fn invalid_progress_call(index: usize, outcome: &str) -> RawStreamingChoice<()> {
    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        format!("invalid-progress-{index}"),
        if outcome == "unknownTool" {
            "missing"
        } else {
            "invalid_progress_probe"
        }
        .into(),
        serde_json::json!({"outcome":outcome}),
    ))
}
