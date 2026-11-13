use super::*;
pub struct RepeatProbe {
    pub script: Arc<std::sync::Mutex<std::collections::VecDeque<serde_json::Value>>>,
}

impl ToolDyn for RepeatProbe {
    fn name(&self) -> String {
        "repeat_probe".into()
    }
    fn emits_command_envelope(&self) -> bool {
        true
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "repeat_probe".into(),
                description: "scripted repeated-failure fixture".into(),
                parameters: serde_json::json!({"type":"object","properties":{"call":{"type":"integer"}},"required":["call"]}),
            }
        })
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            use gents_loop::tool_call_lifecycle::FailureClass;
            let args: serde_json::Value = serde_json::from_str(&args).unwrap();
            let event = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("dispatched undispatchable call {args}"));
            assert_eq!(event["call"], args["call"], "dispatched out of model order");
            let (class, text) = match (event["outcome"].as_str().unwrap(), &event["error"]) {
                ("success", _) => return Ok("ok".into()),
                ("ordinaryFailure", serde_json::Value::Null) => (
                    FailureClass::ToolReturnedError,
                    "gents_exec: {unparseable\nfixture error".to_string(),
                ),
                ("ordinaryFailure", error) => (
                    FailureClass::ToolReturnedError,
                    format!("fixture error {error}"),
                ),
                ("invalidArguments", _) => {
                    (FailureClass::ArgumentInvalid, "fixture invalid".to_string())
                }
                ("policyDenied", _) => (FailureClass::PolicyDenied, "fixture denied".to_string()),
                (other, _) => panic!("fixture outcome {other} is not dispatched to the probe"),
            };
            Err(ToolError::ReportedFailure { class, text })
        })
    }
}

pub async fn tool_call_rows(
    reader: &gents_store::config_client::ConfigAccess,
) -> Vec<serde_json::Value> {
    let response = reader
        .execute(
            "query { AgentToolCall { _docID tool_call_id lifecycle_state tool_failure_class } }",
        )
        .await
        .unwrap();
    response["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone()
}

pub fn repeat_call(index: usize, event: &serde_json::Value) -> RawStreamingChoice<()> {
    let (tool, args) = match event["outcome"].as_str().unwrap() {
        "skipped" => ("list_processes", serde_json::json!({})),
        "unknownTool" => ("missing", serde_json::json!({"call": event["call"]})),
        _ => ("repeat_probe", serde_json::json!({"call": event["call"]})),
    };
    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        format!("repeat-{index}"),
        tool.into(),
        args,
    ))
}

pub struct EnvelopeShapedTool {
    pub calls: Arc<std::sync::atomic::AtomicUsize>,
    pub command_envelope: bool,
    pub varying_hint: bool,
}

impl ToolDyn for EnvelopeShapedTool {
    fn name(&self) -> String {
        "envelope_shaped".into()
    }
    fn emits_command_envelope(&self) -> bool {
        self.command_envelope
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "envelope_shaped".into(),
                description: "non-command tool with envelope-shaped errors".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let hint = if self.varying_hint { call } else { 0 };
            Err(ToolError::ReportedFailure {
                class: gents_loop::tool_call_lifecycle::FailureClass::ToolReturnedError,
                text: format!(
                    "gents_exec: {{\"ok\":false,\"duration_ms\":1,\"hint\":\"step {hint}\"}}\nstdout:\n(empty)\nstderr:\nfailed"
                ),
            })
        })
    }
}
